//! Hub-local transcode jobs and the encode cache served to linked clients.

use super::settings::set_by;
use super::{Core, Stores};
use ritornel_contract::methods::{Method, TranscodePrune, TranscodeStart};
use ritornel_contract::transcode::{
    EncoderCapability, JobStarted, TranscodeOutcome, TranscodeParams, TranscodeResult,
    TranscodeStatus,
};
use ritornel_contract::{ErrorCode, Id, Receipt, ReceiptStatus, RpcError, Setting};
use ritornel_engine::transcode::{
    BatchItem, Codec, Encoder, EncoderId, Encoding, LameEncoder, OpusEncoder, Pcm, Profile,
    ReplayGain, Sourced, Target, TranscodePool,
};
use ritornel_engine::{
    Command, Error, JobCancel, JobState, JobStop, LibraryStore, NewJob, Progress, ServerStore,
};
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
}

/// The codec a profile makes.
pub(crate) fn codec_of(target: Target) -> Codec {
    match target {
        Target::Mp3 { .. } => Codec::Mp3,
        Target::Opus { .. } => Codec::Opus,
        Target::Flac {} => Codec::Flac,
    }
}

impl Transcodes {
    /// The encoder that makes `profile`, with the identity its startup
    /// probe found: `None` where the program is not there, and for native
    /// FLAC, which needs none.
    pub(crate) fn encoding(&self, profile: Profile) -> Option<Encoding> {
        let (backend, family): (Arc<dyn Encoder>, &str) = match profile.target {
            Target::Mp3 { .. } => (Arc::new(LameEncoder), "lame"),
            Target::Opus { .. } => (Arc::new(OpusEncoder), "opusenc"),
            Target::Flac {} => return None,
        };
        let id = self.encoders.iter().find(|e| e.family == family)?.clone();
        Some(Encoding {
            backend,
            id,
            profile,
            gain_tags: Default::default(),
        })
    }

    /// The detected encoder programs, as the contract lists them.
    pub(crate) fn capabilities(&self) -> Vec<EncoderCapability> {
        self.encoders
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
            .collect()
    }
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
        let (used_bytes, derivatives) = s
            .pool
            .cache()
            .usage(stores.server())
            .map_err(|e| super::failed(crate::Error::Store(e)))?;
        Ok(Some(TranscodeStatus {
            used_bytes,
            derivatives,
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
            encoders: s.capabilities(),
        }))
    }

    pub(super) fn start_transcode(
        &self,
        stores: &Stores,
        p: TranscodeParams,
        show_paths: bool,
    ) -> Result<JobStarted, RpcError> {
        self.enqueue_transcode::<TranscodeStart>(stores, &p, p.idempotency_key, show_paths, || {
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
        let request = BatchRequest {
            appearances: p.appearances.clone(),
            profile,
        };
        serde_json::to_string(&request)
            .map_err(|_| RpcError::new(ErrorCode::Internal, "cannot serialize transcode request"))
    }

    pub(super) fn enqueue_transcode<M: Method>(
        &self,
        stores: &Stores,
        p: &M::Params,
        key: Id,
        show_paths: bool,
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
            job: super::calls::job_for(job, show_paths),
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
            // One outcome for each appearance: what is not encoded is
            // settled here, and the rest by the batch.
            let (mut outcomes, items) = self
                .with_library(|store, lib| s.items(lib, store, self.server_id, &request, &cancel))
                .ok_or(Error::NoLibrary)??;
            let (slots, items): (Vec<usize>, Vec<BatchItem>) = items.into_iter().unzip();
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
            for (slot, result) in slots.into_iter().zip(batch.results()) {
                let result = match result {
                    Ok(entry) => TranscodeResult::Encoded {
                        key: entry.key().to_hex().to_string(),
                        size_bytes: entry.size_bytes,
                        file_hash: entry.file_hash.to_hex().to_string(),
                        sampled_hash: entry.sampled_hash.to_hex().to_string(),
                    },
                    Err(error) => TranscodeResult::Failed {
                        reason: error.to_string(),
                    },
                };
                if let Some(outcome) = outcomes.get_mut(slot) {
                    outcome.result = result;
                }
            }
            let done = outcomes.len() as i64;
            let failed = outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome.result,
                        TranscodeResult::Blocked { .. } | TranscodeResult::Failed { .. }
                    )
                })
                .count() as i64;
            let progress = Progress {
                step: 1,
                done,
                total: Some(done),
                bytes,
                failed,
                checkpoint_json: Some(serde_json::to_string(&outcomes).map_err(Error::Json)?),
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
    /// One outcome for each appearance, and the encodes still to make,
    /// each with the outcome it fills in.
    #[allow(clippy::type_complexity)]
    fn items(
        &self,
        library: &LibraryStore,
        store: &ServerStore,
        server: Id,
        request: &BatchRequest,
        cancel: &JobCancel,
    ) -> Result<(Vec<TranscodeOutcome>, Vec<(usize, BatchItem)>), Error> {
        let paths = store
            .roots()?
            .into_iter()
            .map(|(n, p)| (n, PathBuf::from(p)))
            .collect();
        let made = codec_of(request.profile.target);
        let mut outcomes = Vec::new();
        let mut items = Vec::new();
        for id in &request.appearances {
            if let Some(stop) = cancel.stopped() {
                return Err(Error::JobStopped(stop));
            }
            // A lossy master serves only a profile of its own codec.
            let sourced =
                library.transcode_source(*id, server, &paths, request.profile, |codec| {
                    codec == made.extension()
                })?;
            let result = match sourced {
                Sourced::Blocked(reason) => TranscodeResult::Blocked {
                    reason: reason.to_owned(),
                },
                Sourced::PassThrough(_, codec) => TranscodeResult::PassedThrough { codec },
                Sourced::Encode(master) => match self.encoding(request.profile) {
                    None => TranscodeResult::Blocked {
                        reason: "no_encoder".to_owned(),
                    },
                    Some(mut encoding) => {
                        if request.profile.dsp.replay_gain != ReplayGain::Off {
                            encoding.gain_tags = Pcm::open(&master.path, Some(master.facts))
                                .map_err(Error::Io)?
                                .gain_tags();
                        }
                        items.push((outcomes.len(), BatchItem { master, encoding }));
                        // Filled in when its encode ends.
                        TranscodeResult::Failed {
                            reason: "not encoded".to_owned(),
                        }
                    }
                },
            };
            outcomes.push(TranscodeOutcome {
                appearance_id: *id,
                result,
            });
        }
        Ok((outcomes, items))
    }
}
