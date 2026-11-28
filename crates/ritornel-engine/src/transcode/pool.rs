//! Encodes made on dedicated threads, and the transfers prepared from
//! them. Only the callers await dependencies. A worker never waits for
//! another encode or a free worker.
//!
//! A batch warms the cache and answers one result for each item, so one
//! blocked or failed track never fails the rest. A prepared transfer is
//! what a sync hands a device: an ID, an exact length, a digest of the
//! bytes and a reader. The tagged copy behind it is a scratch file the
//! transfer owns, and the encode it was made from stays held until the
//! transfer ends.

mod workers;

use super::cache::Object;
use super::{
    Artwork, AudioAsset, CacheEntry, Codec, CopySource, Derivative, Encoder, EncoderId, GainTags,
    Pcm, Pin, PinOwner, Profile, TaggedCopy, Tags, TranscodeCache,
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
    /// The key of the encode this makes of `master`.
    pub fn key(&self, master: &Master) -> Result<blake3::Hash, Error> {
        Ok(self.derivative(master)?.key)
    }

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

    /// BLAKE3 of the source picture's bytes.
    pub fn hash(&self) -> blake3::Hash {
        self.hash
    }
}

/// What a tagged copy carries beside its audio.
#[derive(Clone, Debug, Default)]
pub struct Tagging {
    pub tags: Tags,
    pub cover: Option<Cover>,
    pub id3v1: bool,
}

impl Tagging {
    fn copy(&self) -> TaggedCopy<'_> {
        TaggedCopy {
            tags: &self.tags,
            artwork: self.cover.as_ref().map(|c| Artwork {
                bytes: &c.bytes,
                max_side: c.max_side,
            }),
            id3v1: self.id3v1,
        }
    }
}

/// Where a transfer's audio comes from, as the source policy chose.
#[derive(Clone, Debug)]
pub enum TransferAudio {
    /// Encoded from the master, through the cache.
    Encode(Encoding),
    /// The master's own audio, read directly.
    PassThrough(Codec),
}

/// One encode to make or find: the caller selects the master.
#[derive(Clone, Debug)]
pub struct BatchItem {
    pub master: Master,
    pub encoding: Encoding,
}

/// What a sync asks for by key: one tagged copy of one master.
#[derive(Clone, Debug)]
pub struct TransferRequest {
    pub master: Master,
    pub audio: TransferAudio,
    pub tagging: Arc<Tagging>,
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
        let object = item.encoding.derivative(&item.master)?;
        self.derivative(item.master, item.encoding, object).await
    }

    fn pin(&self) -> Result<Pin, Error> {
        Ok(Pin {
            id: self.store.new_id()?,
            owner: PinOwner::Batch,
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
            .run(Some(slot), move |cancel| {
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
            .run(Some(slot), move |_| {
                let pin = ctx.pin()?;
                let hit = ctx.cache.lookup(&ctx.store, object, pin)?;
                Ok(hit.map(|entry| ctx.lease(pin, entry)))
            })
            .await
    }
}

/// Keeps a cached encode pinned while a response is being sent.
pub struct Transfer {
    lease: Arc<Lease>,
    // A response owns its body. Release the pin before the last owner can join the index thread.
    _pool: Arc<TranscodePool>,
}
impl Transfer {
    pub fn entry(&self) -> &CacheEntry {
        &self.lease.entry
    }
}
impl fmt::Debug for Transfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.entry().fmt(f)
    }
}

/// One tagged copy made ready to send: what a sync hands a put. It has an
/// identity, an exact length and a digest of the bytes sent, and a caller
/// gets a reader and never a path. The scratch file behind it is this
/// transfer's own and goes when it is dropped; the encode it was tagged
/// from stays held until then, so a transfer never evicts its own audio.
pub struct PreparedTransfer {
    id: crate::Id,
    codec: Codec,
    len: u64,
    digest: blake3::Hash,
    sampled: blake3::Hash,
    scratch: tempfile::TempPath,
    _encode: Option<Arc<Lease>>,
    // The lease's release joins the pool's index thread: keep the pool.
    _pool: Arc<TranscodePool>,
}

impl PreparedTransfer {
    /// What names this transfer.
    pub fn id(&self) -> crate::Id {
        self.id
    }
    pub fn codec(&self) -> Codec {
        self.codec
    }
    /// The exact length of the bytes a reader gives.
    pub fn len(&self) -> u64 {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// BLAKE3 of those bytes. It is not the key the copy was asked for by.
    pub fn digest(&self) -> blake3::Hash {
        self.digest
    }
    /// BLAKE3 of the first and last MiB, counting overlapping bytes once.
    pub fn sampled_hash(&self) -> blake3::Hash {
        self.sampled
    }
    /// The bytes, from the start. Each reader holds the copy open.
    pub fn reader(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::open(&self.scratch)
    }
}

impl fmt::Debug for PreparedTransfer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedTransfer")
            .field("id", &self.id)
            .field("codec", &self.codec)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

/// One result for each item, in request order. An encode is pinned until
/// the caller drops the batch.
pub struct Batch<'pool> {
    entries: Vec<Result<Arc<Lease>, Arc<Error>>>,
    _pool: std::marker::PhantomData<&'pool TranscodePool>,
}

impl Batch<'_> {
    pub fn results(&self) -> impl ExactSizeIterator<Item = Result<&CacheEntry, &Error>> {
        self.entries.iter().map(|entry| match entry {
            Ok(lease) => Ok(&lease.entry),
            Err(error) => Err(error.as_ref()),
        })
    }
}

impl fmt::Debug for Batch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.results()).finish()
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

    /// Pin a cached encode until its response body is dropped, including on disconnect.
    pub async fn transfer(self: &Arc<Self>, key: blake3::Hash) -> Result<Option<Transfer>, Error> {
        let ctx = self.context.clone();
        let lease = self
            .context
            .index
            .run(None, move |_| {
                let pin = Pin {
                    id: ctx.store.new_id()?,
                    owner: PinOwner::HttpTransfer,
                };
                Ok(ctx
                    .cache
                    .transfer(&ctx.store, key, pin)?
                    .map(|entry| ctx.lease(pin, entry)))
            })
            .await?;
        Ok(lease.map(|lease| Transfer {
            lease,
            _pool: self.clone(),
        }))
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

    /// Makes one tagged copy ready to send. The audio is the master's own
    /// or its encode, found in the cache or made there; the copy is written
    /// to a scratch file the transfer owns. Asked again with the same
    /// request, it makes the same bytes. Dropping the future withdraws its
    /// waits.
    pub async fn prepare(
        self: &Arc<Self>,
        request: TransferRequest,
    ) -> Result<PreparedTransfer, Arc<Error>> {
        let ctx = self.context.clone();
        let TransferRequest {
            master,
            audio,
            tagging,
        } = request;
        let (encode, codec) = match audio {
            TransferAudio::Encode(encoding) => {
                let object = encoding.derivative(&master)?;
                tagging.copy().check(object.codec).map_err(Error::Io)?;
                let lease = ctx.derivative(master.clone(), encoding, object).await?;
                (Some(lease), object.codec)
            }
            TransferAudio::PassThrough(codec) => {
                tagging.copy().check(codec).map_err(Error::Io)?;
                (None, codec)
            }
        };
        let id = ctx.store.new_id()?;
        let held = encode.clone();
        let cache = ctx.cache.clone();
        let (scratch, len, digest, sampled) = ctx
            .workers
            .run(None, move |cancel| {
                let source = match &held {
                    Some(lease) => CopySource::Encode(&lease.entry),
                    None => CopySource::Master {
                        asset: master.asset,
                        path: &master.path,
                        facts: master.facts,
                        codec,
                    },
                };
                // The scratch name is reserved, and the copy is written to
                // it afresh: the writer refuses a file that is there.
                let scratch = cache.scratch().map_err(Error::Io)?;
                std::fs::remove_file(&scratch).map_err(Error::Io)?;
                tagging
                    .copy()
                    .write(source, &scratch, cancel)
                    .map_err(Error::Io)?;
                let mut file = std::fs::File::open(&scratch).map_err(Error::Io)?;
                let (len, digest, sampled) =
                    super::cache::hashes(&mut file, cancel).map_err(Error::Io)?;
                Ok((scratch, len, digest, sampled))
            })
            .await?;
        Ok(PreparedTransfer {
            id,
            codec,
            len,
            digest,
            sampled,
            scratch,
            _encode: encode,
            _pool: self.clone(),
        })
    }

    /// Warms the cache. Jobs and disconnected callers can stop
    /// independently. Dropping this future withdraws its waits and releases
    /// any results collected so far. One item's failure is its own result.
    pub async fn ensure_batch(
        &self,
        items: Vec<BatchItem>,
        cancel: &JobCancel,
        mut progress: impl FnMut(BatchProgress),
    ) -> Result<Batch<'_>, Arc<Error>> {
        let total = items.len();
        let mut entries = Vec::with_capacity(total);
        let mut pending = stream::iter(items.into_iter().enumerate())
            .map(|(index, item)| async move { (index, self.context.item(item).await) })
            .buffer_unordered(self.workers.get());
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            cancel.check()?;
            tokio::select! {
                _ = tick.tick() => {},
                result = pending.next() => {
                    let Some((index, entry)) = result else { break };
                    progress(BatchProgress {
                        index,
                        completed: entries.len() + 1,
                        total,
                        size_bytes: entry.as_ref().map_or(0, |lease| lease.entry.size_bytes),
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
