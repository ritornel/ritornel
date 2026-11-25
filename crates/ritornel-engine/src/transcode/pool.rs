//! Batch materialization. Only the callers await dependencies. A worker
//! never waits for another encode, a package, or a free worker.

mod workers;

use super::cache::Object;
use super::{
    Artwork, AudioAsset, CacheEntry, Codec, Derivative, Encoder, EncoderId, GainTags, Package,
    PackageSource, Pcm, Pin, PinOwner, Profile, Tags, TranscodeCache,
};
use crate::playback::FileFacts;
use crate::{Error, JobCancel, ServerStore};
use futures_util::future::{BoxFuture, FutureExt, Shared, WeakShared};
use futures_util::{StreamExt, stream};
use std::collections::HashMap;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;
use workers::{JoinWorkers, Workers};

#[derive(Clone, Debug)]
pub struct Master {
    pub path: PathBuf,
    pub asset: AudioAsset,
    pub facts: FileFacts,
}

/// The selected backend and its identity from the startup probe.
#[derive(Clone)]
pub struct Encoding {
    pub backend: Arc<dyn Encoder>,
    pub id: EncoderId,
    pub profile: Profile,
    pub gain_tags: GainTags,
}

impl fmt::Debug for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encoding")
            .field("id", &self.id)
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl Encoding {
    fn derivative(&self, master: &Master) -> Result<Object, Error> {
        if !self.backend.supports(self.profile) {
            return Err(Error::Invalid("unsupported encoder profile"));
        }
        Derivative {
            asset: master.asset,
            encoder: &self.id,
            profile: self.profile,
            gain_tags: self.gain_tags,
        }
        .object()
    }
}

/// An immutable source picture. Compute its digest once when loading the art.
#[derive(Clone, Debug)]
pub struct Cover {
    bytes: Arc<[u8]>,
    hash: blake3::Hash,
    max_side: u32,
}

impl Cover {
    pub fn new(bytes: Arc<[u8]>, max_side: u32) -> Self {
        Self {
            hash: blake3::hash(&bytes),
            bytes,
            max_side,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Packaging {
    pub tags: Tags,
    pub cover: Option<Cover>,
    pub id3v1: bool,
}

impl Packaging {
    fn package(&self) -> Package<'_> {
        Package {
            tags: &self.tags,
            artwork: self.cover.as_ref().map(|c| Artwork {
                bytes: &c.bytes,
                max_side: c.max_side,
            }),
            id3v1: self.id3v1,
        }
    }
}

#[derive(Clone, Debug)]
pub enum PackageAudio {
    Encode(Encoding),
    PassThrough(Codec),
}

/// The caller selects the master and output using the source policy.
/// Pass-through packages read the master directly.
#[derive(Clone, Debug)]
pub enum BatchItem {
    Derivative {
        master: Master,
        encoding: Encoding,
    },
    Package {
        master: Master,
        audio: PackageAudio,
        packaging: Arc<Packaging>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct BatchProgress {
    pub index: usize,
    pub completed: usize,
    pub total: usize,
    pub size_bytes: u64,
}

struct Lease {
    entry: CacheEntry,
    pin: Pin,
    context: Arc<Context>,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let (cache, store, pin) = (
            self.context.cache.clone(),
            self.context.store.clone(),
            self.pin,
        );
        let error = self.context.release_error.clone();
        self.context.index.release(move || {
            if let Err(failed) = cache.release(&store, pin.id) {
                error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get_or_insert(failed);
            }
        });
    }
}

type ResultEntry = Result<Arc<Lease>, Arc<Error>>;
type Pending = BoxFuture<'static, ResultEntry>;
type Keys = Mutex<HashMap<blake3::Hash, Weak<KeyWork>>>;

struct KeyWork {
    key: blake3::Hash,
    keys: Arc<Keys>,
    future: Mutex<Option<WeakShared<Pending>>>,
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl Drop for KeyWork {
    fn drop(&mut self) {
        let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
        if keys
            .get(&self.key)
            .is_some_and(|w| std::ptr::eq(w.as_ptr(), self))
        {
            keys.remove(&self.key);
        }
    }
}

#[derive(Default)]
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Context {
    store: Arc<ServerStore>,
    cache: Arc<TranscodeCache>,
    keys: Arc<Keys>,
    workers: Workers,
    index: Workers,
    release_error: Arc<Mutex<Option<Error>>>,
}

impl Context {
    /// Weak references in the registry let the last waiter drop the shared
    /// future, including its upstream wait and its worker cancellation guard.
    fn flight<F: Future<Output = ResultEntry> + Send + 'static>(
        &self,
        key: blake3::Hash,
        make: impl FnOnce(Arc<KeyWork>) -> F,
    ) -> Shared<Pending> {
        let slot = {
            let mut keys = self.keys.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(slot) = keys.get(&key).and_then(Weak::upgrade) {
                slot
            } else {
                let slot = Arc::new(KeyWork {
                    key,
                    keys: self.keys.clone(),
                    future: Mutex::default(),
                    gate: Arc::default(),
                });
                keys.insert(key, Arc::downgrade(&slot));
                slot
            }
        };
        let mut current = slot.future.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = current.as_ref().and_then(WeakShared::upgrade)
            && !matches!(running.peek(), Some(Err(_)))
        {
            return running;
        }
        let running = make(slot.clone()).boxed().shared();
        *current = running.downgrade();
        running
    }

    fn derivative(
        self: &Arc<Self>,
        master: Master,
        encoding: Encoding,
        object: Object,
    ) -> Shared<Pending> {
        let ctx = self.clone();
        self.flight(object.key, move |slot| async move {
            if let Some(hit) = ctx.lookup(slot.clone(), object).await? {
                return Ok(hit);
            }
            ctx.ensure(slot, object, move |out, cancel| {
                let pcm = Pcm::open(&master.path, Some(master.facts))?;
                if encoding
                    .id
                    .derivative_key(master.asset, encoding.profile, pcm.gain_tags())?
                    != object.key
                {
                    return Err(std::io::Error::other(
                        "master gain tags changed since the scan",
                    ));
                }
                encoding
                    .backend
                    .encode(pcm, encoding.profile, object.key, out, cancel)
            })
            .await
        })
    }

    async fn item(self: &Arc<Self>, item: BatchItem) -> ResultEntry {
        let (master, audio, packaging) = match item {
            BatchItem::Derivative { master, encoding } => {
                let object = encoding.derivative(&master)?;
                return self.derivative(master, encoding, object).await;
            }
            BatchItem::Package {
                master,
                audio,
                packaging,
            } => (master, audio, packaging),
        };
        let (derivative, hash, scheme, codec) = match audio {
            PackageAudio::Encode(e) => {
                let object = e.derivative(&master)?;
                (Some((e, object)), object.key, None, object.codec)
            }
            PackageAudio::PassThrough(codec) => {
                (None, master.asset.hash, Some(master.asset.scheme), codec)
            }
        };
        let object = packaging
            .package()
            .object_for(
                hash,
                scheme,
                codec,
                packaging.cover.as_ref().map(|c| (c.hash, c.max_side)),
            )
            .map_err(Error::Io)?;
        let ctx = self.clone();
        self.flight(object.key, move |slot| async move {
            // A package can outlive its derivative. Try it before opening the master.
            if let Some(hit) = ctx.lookup(slot.clone(), object).await? {
                return Ok(hit);
            }
            let audio = if let Some((encoding, base)) = derivative {
                Some(ctx.derivative(master.clone(), encoding, base).await?)
            } else {
                None
            };
            ctx.ensure(slot, object, move |path, cancel| {
                let source = match &audio {
                    Some(audio) => PackageSource::Derivative(&audio.entry),
                    None => PackageSource::Master {
                        asset: master.asset,
                        path: &master.path,
                        facts: master.facts,
                        codec,
                    },
                };
                packaging.package().write(source, path, cancel)
            })
            .await
        })
        .await
    }

    fn pin(&self) -> Result<Pin, Error> {
        Ok(Pin {
            id: self.store.new_id()?,
            owner: PinOwner::SyncBatch,
        })
    }

    fn lease(self: &Arc<Self>, pin: Pin, entry: CacheEntry) -> Arc<Lease> {
        Arc::new(Lease {
            context: self.clone(),
            pin,
            entry,
        })
    }

    async fn ensure(
        self: &Arc<Self>,
        slot: Arc<KeyWork>,
        object: Object,
        produce: impl FnOnce(&std::path::Path, &AtomicBool) -> std::io::Result<()> + Send + 'static,
    ) -> ResultEntry {
        let ctx = self.clone();
        Ok(self
            .workers
            .run(slot, move |cancel| {
                let pin = ctx.pin()?;
                let entry = ctx
                    .cache
                    .ensure(&ctx.store, object, pin, cancel, |out| produce(out, cancel))?;
                Ok(ctx.lease(pin, entry))
            })
            .await?)
    }

    async fn lookup(
        self: &Arc<Self>,
        slot: Arc<KeyWork>,
        object: Object,
    ) -> Result<Option<Arc<Lease>>, Error> {
        let ctx = self.clone();
        self.index
            .run(slot, move |_| {
                let pin = ctx.pin()?;
                let hit = ctx.cache.lookup(&ctx.store, object, pin)?;
                Ok(hit.map(|entry| ctx.lease(pin, entry)))
            })
            .await
    }
}

/// Files in request order, pinned until the caller drops the batch.
pub struct Batch<'pool> {
    entries: Vec<Arc<Lease>>,
    _pool: std::marker::PhantomData<&'pool TranscodePool>,
}

impl Batch<'_> {
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &CacheEntry> {
        self.entries.iter().map(|lease| &lease.entry)
    }
}

impl fmt::Debug for Batch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.entries()).finish()
    }
}

/// Dropping the pool drains workers before the data directory can be unlocked.
/// Async servers call `close` on a blocking thread during shutdown.
pub struct TranscodePool {
    workers: NonZeroUsize,
    context: Arc<Context>,
    _join: [JoinWorkers; 2],
}

impl fmt::Debug for TranscodePool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscodePool")
            .field("workers", &self.workers)
            .finish_non_exhaustive()
    }
}

impl TranscodePool {
    pub fn default_workers() -> NonZeroUsize {
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        NonZeroUsize::new(cores.saturating_sub(1)).unwrap_or(NonZeroUsize::MIN)
    }

    /// Threads start on demand. One pool owns all flights for the cache.
    pub fn new(
        store: Arc<ServerStore>,
        cache: TranscodeCache,
        workers: NonZeroUsize,
    ) -> Result<Self, Error> {
        if workers.get() > tokio::sync::Semaphore::MAX_PERMITS {
            return Err(Error::Invalid("too many transcode workers"));
        }
        let (threads, join) = Workers::new(workers.get(), "transcode");
        let (index, join_index) = Workers::new(1, "transcode cache");
        Ok(Self {
            workers,
            context: Arc::new(Context {
                workers: threads,
                index,
                store,
                cache: Arc::new(cache),
                release_error: Arc::default(),
                keys: Arc::default(),
            }),
            _join: [join, join_index],
        })
    }

    pub fn cache(&self) -> &TranscodeCache {
        &self.context.cache
    }

    /// Join all workers and report any deferred pin-release error.
    pub fn close(self) -> Result<(), Error> {
        let error = self.context.release_error.clone();
        drop(self);
        match error.lock().unwrap_or_else(PoisonError::into_inner).take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Jobs and disconnected callers can stop independently. Dropping this
    /// future withdraws its waits and releases any results collected so far.
    pub async fn ensure_batch(
        &self,
        items: Vec<BatchItem>,
        cancel: &JobCancel,
        mut progress: impl FnMut(BatchProgress),
    ) -> Result<Batch<'_>, Arc<Error>> {
        let total = items.len();
        let mut entries = Vec::with_capacity(total);
        let mut pending = stream::iter(items.into_iter().enumerate())
            .map(|(index, item)| async move {
                self.context.item(item).await.map(|entry| (index, entry))
            })
            .buffer_unordered(self.workers.get());
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            cancel.check()?;
            tokio::select! {
                _ = tick.tick() => {},
                result = pending.next() => {
                    let Some(result) = result else { break };
                    let (index, entry) = result?;
                    progress(BatchProgress {
                        index,
                        completed: entries.len() + 1,
                        total,
                        size_bytes: entry.entry.size_bytes,
                    });
                    entries.push((index, entry));
                }
            }
        }
        entries.sort_unstable_by_key(|(index, _)| *index);
        Ok(Batch {
            entries: entries.into_iter().map(|(_, entry)| entry).collect(),
            _pool: std::marker::PhantomData,
        })
    }
}

#[cfg(test)]
mod tests;
