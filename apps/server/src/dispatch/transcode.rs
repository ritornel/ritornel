//! Hub-local transcode jobs and the cache served to linked clients.

use super::settings::set_by;
use super::{Core, Stores};
use ritornel_contract::methods::{Method, TranscodePrune, TranscodeStart};
use ritornel_contract::transcode::{
    ArtifactLayer, EncoderCapability, JobStarted, TranscodeArtifact, TranscodeParams,
    TranscodeStatus,
};
use ritornel_contract::{ErrorCode, Id, Receipt, ReceiptStatus, RpcError, Setting};
use ritornel_engine::transcode::{
    BatchItem, Codec, Cover, Encoder, EncoderId, Encoding, LameEncoder, OpusEncoder, PackageAudio,
    Packaging, Pcm, Profile, ReplayGain, Tags, Target, TranscodePool,
};
use ritornel_engine::{
    Command, Error, JobCancel, JobState, JobStop, LibraryStore, NewJob, Progress, ServerStore,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Weak, atomic::AtomicBool};

#[derive(Debug)]
pub(crate) struct Transcodes {
    pub(crate) pool: Arc<TranscodePool>,
    pub(crate) config: crate::TranscodeConfig,
    pub(crate) encoders: Vec<EncoderId>,
    pub(crate) origins: crate::Origins,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BatchRequest {
    appearances: Vec<Id>,
    profile: Profile,
    layer: ArtifactLayer,
}

impl Core {
    pub(crate) fn attach_transcodes(&self, service: &Arc<Transcodes>) {
        let _ = self.transcodes.set(Arc::downgrade(service));
    }
    pub(crate) fn transcodes(&self) -> Option<Arc<Transcodes>> {
        self.transcodes.get().and_then(Weak::upgrade)
    }

    pub(super) fn transcode_status(
        &self,
        stores: &Stores,
    ) -> Result<Option<TranscodeStatus>, RpcError> {
        let Some(s) = self.transcodes() else {
            return Ok(None);
        };
        let (used_bytes, derivatives, packages) = s
            .pool
            .cache()
            .usage(stores.server())
            .map_err(|e| super::failed(crate::Error::Store(e)))?;
        Ok(Some(TranscodeStatus {
            used_bytes,
            derivatives,
            packages,
            budget_bytes: Setting {
                value: s.config.budget,
                set_by: set_by(s.origins.transcode_cache_size),
            },
            workers: Setting {
                value: u32::try_from(s.config.workers.get())
                    .map_err(|_| RpcError::new(ErrorCode::Internal, "worker count out of range"))?,
                set_by: set_by(s.origins.transcode_workers),
            },
            profiles: Setting {
                value: s
                    .config
                    .profiles
                    .iter()
                    .map(|(name, profile)| (name.clone(), (*profile).into()))
                    .collect(),
                set_by: set_by(s.origins.transcode_profiles),
            },
            encoders: s
                .encoders
                .iter()
                .map(|e| EncoderCapability {
                    family: e.family.into(),
                    version: e.version.clone(),
                    formats: match e.family {
                        "lame" => vec!["mp3".into()],
                        "opusenc" => vec!["opus".into()],
                        _ => Vec::new(),
                    },
                })
                .collect(),
        }))
    }

    pub(super) fn start_transcode(
        &self,
        stores: &Stores,
        p: TranscodeParams,
    ) -> Result<JobStarted, RpcError> {
        self.enqueue_transcode::<TranscodeStart>(stores, &p, p.idempotency_key, || {
            self.batch_request(&p)
        })
    }

    fn batch_request(&self, p: &TranscodeParams) -> Result<String, RpcError> {
        let service = self.transcodes().ok_or_else(|| {
            RpcError::new(ErrorCode::Unavailable, "transcode pool is unavailable")
        })?;
        if p.appearances.is_empty() || p.appearances.len() > 128 {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "select 1 to 128 appearances",
            ));
        }
        let profile = *service
            .config
            .profiles
            .get(&p.profile)
            .ok_or_else(|| RpcError::new(ErrorCode::Invalid, "unknown transcode profile"))?;
        if matches!(
            (profile.target, p.layer),
            (Target::Flac {}, ArtifactLayer::Derivative)
        ) {
            return Err(RpcError::new(
                ErrorCode::Invalid,
                "FLAC pass-through needs the packaged layer",
            ));
        }
        let request = BatchRequest {
            appearances: p.appearances.clone(),
            profile,
            layer: p.layer,
        };
        serde_json::to_string(&request)
            .map_err(|_| RpcError::new(ErrorCode::Internal, "cannot serialize transcode request"))
    }

    pub(super) fn enqueue_transcode<M: Method>(
        &self,
        stores: &Stores,
        p: &M::Params,
        key: Id,
        request: impl FnOnce() -> Result<String, RpcError>,
    ) -> Result<JobStarted, RpcError> {
        let command = Command {
            id: key,
            fingerprint: super::fingerprint::<M>(p)?,
        };
        let response = |job: &ritornel_engine::Job| JobStarted {
            receipt: Receipt {
                command_id: key,
                status: ReceiptStatus::Accepted,
                revision: None,
            },
            job: super::calls::job(job),
        };
        // A retry can arrive after a startup profile was renamed or removed.
        if let Some(job) = stores
            .server()
            .job_of_command(command)
            .map_err(|e| super::failed(crate::Error::Store(e)))?
        {
            return Ok(response(&job));
        }
        Self::library(stores, |_| Ok(()))?;
        let json = request()?;
        let (saved, created) = stores
            .server()
            .start_job(
                command,
                M::NAME,
                NewJob {
                    kind: M::NAME,
                    dedupe_key: key.to_string(),
                    params_json: json,
                    target_kind: "server",
                    target: self.server_id,
                    session: self.local_credential.session(),
                },
            )
            .map_err(|e| super::failed(crate::Error::Store(e)))?;
        if created {
            self.publish_stored_job(stores, &saved)
        }
        self.jobs.wake();
        Ok(response(&saved))
    }

    pub(crate) fn run_transcode(&self, job: ritornel_engine::Job, shutdown: &Arc<AtomicBool>) {
        let Some(result) = crate::jobs::cancel_handle(self, job.id, shutdown) else {
            return;
        };
        let outcome = result.map_err(Arc::new).and_then(|cancel| {
            let s = self.transcodes().ok_or(Error::WriterStopped)?;
            if let Some(stop) = cancel.stopped() {
                return Err(Arc::new(Error::JobStopped(stop)));
            }
            if job.kind == TranscodePrune::NAME {
                self.with_stores(|stores| s.pool.cache().prune_unpinned(stores.server()))
                    .ok_or(Error::WriterStopped)??;
                return Ok(());
            }
            let request: BatchRequest =
                serde_json::from_str(&job.params_json).map_err(Error::Json)?;
            let items = self
                .with_library(|store, lib| s.items(lib, store, self.server_id, &request, &cancel))
                .ok_or(Error::NoLibrary)??;
            let runtime = self.runtime.as_ref().ok_or(Error::WriterStopped)?;
            let mut bytes = 0i64;
            let mut saved = std::time::Instant::now();
            let batch = runtime.block_on(s.pool.ensure_batch(items, &cancel, |event| {
                bytes = bytes.saturating_add(i64::try_from(event.size_bytes).unwrap_or(i64::MAX));
                if saved.elapsed() >= crate::jobs::SAVE_EVERY {
                    saved = std::time::Instant::now();
                    let progress = Progress {
                        done: event.completed as i64,
                        total: Some(event.total as i64),
                        bytes,
                        ..Default::default()
                    };
                    if let Some(Ok(job)) =
                        self.with_stores(|s| s.server().save_progress(job.id, progress))
                    {
                        self.publish_job(&job);
                    }
                }
            }))?;
            let artifacts: Vec<_> = batch
                .entries()
                .zip(request.appearances)
                .map(|(entry, appearance_id)| TranscodeArtifact {
                    appearance_id,
                    layer: request.layer,
                    key: entry.key().to_hex().to_string(),
                    size_bytes: entry.size_bytes,
                    file_hash: entry.file_hash.to_hex().to_string(),
                    sampled_hash: entry.sampled_hash.to_hex().to_string(),
                })
                .collect();
            let done = artifacts.len() as i64;
            let progress = Progress {
                step: 1,
                done,
                total: Some(done),
                bytes,
                checkpoint_json: Some(serde_json::to_string(&artifacts).map_err(Error::Json)?),
                ..Default::default()
            };
            self.with_stores(|stores| stores.server().save_progress(job.id, progress))
                .ok_or(Error::WriterStopped)??;
            Ok(())
        });
        let (state, error) = match outcome {
            Ok(()) => (JobState::Succeeded, None),
            Err(e) => match e.as_ref() {
                Error::JobStopped(JobStop::Shutdown) => return,
                Error::JobStopped(JobStop::Cancelled) => (JobState::Cancelled, None),
                e => (JobState::Failed, Some(e.to_string())),
            },
        };
        if let Some(Ok(job)) = self.with_stores(|s| s.server().finish_job(job.id, state, error)) {
            self.publish_job(&job);
        }
    }
}

impl Transcodes {
    fn items(
        &self,
        library: &LibraryStore,
        store: &ServerStore,
        server: Id,
        request: &BatchRequest,
        cancel: &JobCancel,
    ) -> Result<Vec<BatchItem>, Error> {
        let roots = store.roots()?;
        let paths = roots
            .iter()
            .map(|(n, p)| (n.clone(), PathBuf::from(p)))
            .collect();
        let mut covers: HashMap<Id, Option<Cover>> = HashMap::new();
        let backend: Option<(Arc<dyn Encoder>, &str)> = match request.profile.target {
            Target::Mp3 { .. } => Some((Arc::new(LameEncoder), "lame")),
            Target::Opus { .. } => Some((Arc::new(OpusEncoder), "opusenc")),
            Target::Flac {} => None,
        };
        let mut items = Vec::new();
        for id in &request.appearances {
            if let Some(stop) = cancel.stopped() {
                return Err(Error::JobStopped(stop));
            }
            let master = library
                .transcode_master(*id, server, &paths, backend.is_none())?
                .ok_or_else(|| {
                    Error::Io(std::io::Error::other(format!(
                        "appearance {id} has no eligible local lossless master"
                    )))
                })?;
            let audio = if let Some((backend, family)) = &backend {
                let encoder = self
                    .encoders
                    .iter()
                    .find(|e| e.family == *family)
                    .ok_or(Error::Invalid("the profile's encoder is unavailable"))?;
                let gain_tags = if request.profile.dsp.replay_gain == ReplayGain::Off {
                    Default::default()
                } else {
                    Pcm::open(&master.path, Some(master.facts))
                        .map_err(Error::Io)?
                        .gain_tags()
                };
                let encoding = Encoding {
                    backend: backend.clone(),
                    id: encoder.clone(),
                    profile: request.profile,
                    gain_tags,
                };
                if request.layer == ArtifactLayer::Derivative {
                    items.push(BatchItem::Derivative { master, encoding });
                    continue;
                }
                PackageAudio::Encode(encoding)
            } else {
                PackageAudio::PassThrough(Codec::Flac)
            };
            let track = library
                .track(*id)?
                .ok_or(Error::NotFound("unknown track appearance"))?;
            let cover = match track.art {
                Some(art) => {
                    if let std::collections::hash_map::Entry::Vacant(slot) = covers.entry(art) {
                        let bytes = library.original_cover(art, &roots)?;
                        slot.insert(bytes.map(|bytes| Cover::new(bytes.into(), 500)));
                    }
                    covers.get(&art).cloned().flatten()
                }
                None => None,
            };
            let tags = Tags {
                title: track.title,
                artist: track.artist,
                album: track.album,
                disc: track.disc.and_then(|v| v.try_into().ok()),
                track: track.track.and_then(|v| v.try_into().ok()),
                ..Tags::default()
            };
            items.push(BatchItem::Package {
                master,
                audio,
                packaging: Arc::new(Packaging {
                    tags,
                    cover,
                    id3v1: matches!(request.profile.target, Target::Mp3 { .. }),
                }),
            });
        }
        Ok(items)
    }
}
