//! Covers, designed for fast touchpad flicks: in a recorded trace, a median
//! of about 4,000 points a second, a tenth of them past 16,000 and peaks
//! near 47,000, and most of every flick the system's momentum.
//!
//! - **What is wanted** is said by the wall each frame, in order: where the
//!   flick will come to rest first (it is predicted from the momentum),
//!   then what is on screen that will stay there long enough to be seen,
//!   then the way there in the direction of travel. The look-ahead is
//!   simply the distance to the predicted rest, so it grows with speed.
//! - **Stale requests are dropped.** The loader holds the latest list
//!   only. A fetch whose cover has left the list is aborted, which closes
//!   its connection, and the server drops the cover from its own queue if
//!   it has not begun it. A fetched cover that is no longer wanted is kept
//!   compressed and not decoded.
//! - **Decoding is bounded**: a few threads at most, by a semaphore, and
//!   each cover is resized there to the pixels it is drawn at.
//! - **The caches are bounded**: decoded pixels by bytes, least recently
//!   drawn first out (and out of GPUI's sprite atlas too), and the
//!   compressed bytes by bytes.
//! - **No tile is ever blank.** Every cover decoded leaves its average
//!   colour behind, kept for good (16 bytes a record), so a tile shows its
//!   own colour once its cover has been seen, and a colour from its name
//!   before that.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::{App, RenderImage, Rgba};
use ritornel_contract::Id;
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

use crate::net::Net;

/// Fetches at once. The server renders a cover on its own threads, newest
/// first, when its size is not cached on disk yet.
const MAX_FETCHES: usize = 8;
/// Compressed covers kept: about 1,000 at 384 pixels.
const COMPRESSED_BUDGET: usize = 32 * 1024 * 1024;
/// A cover that arrives while its tile is on screen comes up over this.
pub(crate) const FADE: Duration = Duration::from_millis(120);

/// What the wall wants, most wanted first, and the pixels it draws them at.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Wants {
    pub ids: Vec<Id>,
    pub px: u32,
}

/// A cover decoded, or one that is not there.
pub(crate) enum Arrived {
    Ready {
        art: Id,
        px: u32,
        image: Arc<RenderImage>,
        color: Rgba,
        bytes: usize,
    },
    Missing {
        art: Id,
    },
}

/// Counts for the measure.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct LoaderCounts {
    pub fetched: u64,
    pub aborted: u64,
    pub decoded: u64,
    pub skipped_decode: u64,
}

pub(crate) fn decoders() -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |count| count.get());
    cores.saturating_sub(2).clamp(2, 4)
}

/// Shared concurrency limit for cover decoding and blurring across the wall and
/// every view, so bursts of covers do not spawn dozens of blocking threads or
/// hold dozens of full-size decode buffers in memory at once.
fn decode_semaphore() -> Arc<Semaphore> {
    static DECODE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(DECODE.get_or_init(|| Arc::new(Semaphore::new(decoders()))))
}

/// Runs participating cover decoding or blurring on the blocking pool under
/// the shared concurrency limit. The owned permit moves into the blocking
/// closure so aborting the async waiter cannot release a permit while the
/// blocking thread is still running.
pub(crate) async fn decode<T, F>(work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    decode_with(decode_semaphore(), work).await
}

async fn decode_with<T, F>(sem: Arc<Semaphore>, work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let permit = sem.acquire_owned().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .ok()
    .flatten()
}

/// Abort handles for in-flight page art tasks; dropping the bag aborts every
/// task still queued or running so closed pages do not stall later views.
#[derive(Default)]
pub(crate) struct FetchBag(Vec<tokio::task::AbortHandle>);

impl FetchBag {
    pub(crate) fn push(&mut self, handle: tokio::task::AbortHandle) {
        self.0.retain(|existing| !existing.is_finished());
        self.0.push(handle);
    }
}

impl Drop for FetchBag {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
}

/// Starts the loader on the network runtime. It runs until `wants` is dropped.
pub(crate) fn start(
    net: Arc<Net>,
) -> (
    watch::Sender<Arc<Wants>>,
    UnboundedReceiver<Arrived>,
    Arc<std::sync::Mutex<LoaderCounts>>,
) {
    let (wants, wanted) = watch::channel(Arc::new(Wants::default()));
    let (arrive, arrived) = unbounded();
    let counts = Arc::new(std::sync::Mutex::new(LoaderCounts::default()));
    let loader = Loader {
        net: Arc::clone(&net),
        wanted,
        arrive,
        decode: decode_semaphore(),
        compressed: Arc::new(std::sync::Mutex::new(Compressed::default())),
        counts: Arc::clone(&counts),
    };
    net.rt().spawn(loader.run());
    (wants, arrived, counts)
}

struct Loader {
    net: Arc<Net>,
    wanted: watch::Receiver<Arc<Wants>>,
    arrive: UnboundedSender<Arrived>,
    decode: Arc<Semaphore>,
    compressed: Arc<std::sync::Mutex<Compressed>>,
    counts: Arc<std::sync::Mutex<LoaderCounts>>,
}

#[derive(Default)]
struct Compressed {
    map: HashMap<(Id, u32), Arc<Vec<u8>>>,
    order: VecDeque<(Id, u32)>,
    bytes: usize,
}

impl Compressed {
    fn get(&self, key: (Id, u32)) -> Option<Arc<Vec<u8>>> {
        self.map.get(&key).cloned()
    }

    fn put(&mut self, key: (Id, u32), bytes: Arc<Vec<u8>>) {
        if self.map.contains_key(&key) {
            return;
        }
        self.bytes += bytes.len();
        self.map.insert(key, bytes);
        self.order.push_back(key);
        while self.bytes > COMPRESSED_BUDGET {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(gone) = self.map.remove(&old) {
                self.bytes -= gone.len();
            }
        }
    }
}

impl Loader {
    async fn run(mut self) {
        let mut tasks: JoinSet<Id> = JoinSet::new();
        let mut running: HashMap<Id, tokio::task::AbortHandle> = HashMap::new();
        // Delivered a moment ago and not yet out of the wall's list.
        let mut delivered: HashMap<Id, Instant> = HashMap::new();
        loop {
            let wants = Arc::clone(&self.wanted.borrow_and_update());
            let wanted: HashSet<Id> = wants.ids.iter().copied().collect();
            // Stale: no longer wanted.
            running.retain(|art, handle| {
                let keep = wanted.contains(art);
                if !keep {
                    handle.abort();
                    if let Ok(mut counts) = self.counts.lock() {
                        counts.aborted += 1;
                    }
                }
                keep
            });
            let now = Instant::now();
            delivered.retain(|_, at| now.duration_since(*at) < Duration::from_millis(400));
            for art in &wants.ids {
                if running.len() >= MAX_FETCHES {
                    break;
                }
                if running.contains_key(art) || delivered.contains_key(art) {
                    continue;
                }
                let handle = tasks.spawn(self.fetch(*art, wants.px));
                running.insert(*art, handle);
            }
            tokio::select! {
                changed = self.wanted.changed() => {
                    if changed.is_err() {
                        tasks.abort_all();
                        return;
                    }
                }
                Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                    if let Ok(art) = done {
                        running.remove(&art);
                        delivered.insert(art, Instant::now());
                    }
                }
            }
        }
    }

    fn fetch(&self, art: Id, px: u32) -> impl Future<Output = Id> + Send + 'static {
        let net = Arc::clone(&self.net);
        let decode = Arc::clone(&self.decode);
        let compressed = Arc::clone(&self.compressed);
        let arrive = self.arrive.clone();
        let wanted = self.wanted.clone();
        let counts = Arc::clone(&self.counts);
        async move {
            let bucket = net.bucket(px);
            let key = (art, bucket);
            let held = compressed.lock().ok().and_then(|held| held.get(key));
            let bytes = match held {
                Some(bytes) => bytes,
                None => {
                    let Some(bytes) = net.art.get(art, bucket).await else {
                        let _ = arrive.unbounded_send(Arrived::Missing { art });
                        return art;
                    };
                    if let Ok(mut counts) = counts.lock() {
                        counts.fetched += 1;
                    }
                    let bytes = Arc::new(bytes);
                    if let Ok(mut held) = compressed.lock() {
                        held.put(key, Arc::clone(&bytes));
                    }
                    bytes
                }
            };
            // Not square, and too short once cut square: once more, larger.
            let larger = sharper(&bytes, px, bucket, &net.art_sizes)
                // Not if the wall has moved on meanwhile.
                .filter(|_| wanted.borrow().ids.contains(&art));
            let bytes = match larger {
                Some(larger) => {
                    let key = (art, larger);
                    let held = compressed.lock().ok().and_then(|held| held.get(key));
                    match held {
                        Some(bytes) => bytes,
                        None => match net.art.get(art, larger).await {
                            Some(again) => {
                                if let Ok(mut counts) = counts.lock() {
                                    counts.fetched += 1;
                                }
                                let again = Arc::new(again);
                                if let Ok(mut held) = compressed.lock() {
                                    held.put(key, Arc::clone(&again));
                                }
                                again
                            }
                            None => bytes,
                        },
                    }
                }
                None => bytes,
            };
            let Ok(permit) = Arc::clone(&decode).acquire_owned().await else {
                return art;
            };
            // Waiting for a decoder, the wall may have moved on.
            if !wanted.borrow().ids.contains(&art) {
                if let Ok(mut counts) = counts.lock() {
                    counts.skipped_decode += 1;
                }
                return art;
            }
            let decoded = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                decode_cover(&bytes, px)
            })
            .await
            .ok()
            .flatten();
            match decoded {
                Some((image, color, size)) => {
                    if let Ok(mut counts) = counts.lock() {
                        counts.decoded += 1;
                    }
                    let _ = arrive.unbounded_send(Arrived::Ready {
                        art,
                        px,
                        image: Arc::new(image),
                        color,
                        bytes: size,
                    });
                }
                None => {
                    let _ = arrive.unbounded_send(Arrived::Missing { art });
                }
            }
            art
        }
    }
}

/// Images evicted or released while another borrower (a flying cover, an
/// ebbing wash in `Shell`, a lifted search thumbnail, or `frost::LAST`) still
/// held a clone, waiting until only this queue holds the last reference.
#[derive(Default)]
struct PendingDrops {
    images: Vec<Arc<RenderImage>>,
}

impl PendingDrops {
    fn push(&mut self, image: Arc<RenderImage>) {
        if !self.images.iter().any(|held| Arc::ptr_eq(held, &image)) {
            self.images.push(image);
        }
    }

    fn take_unshared(&mut self) -> Vec<Arc<RenderImage>> {
        let mut ready = Vec::new();
        let mut i = 0;
        while i < self.images.len() {
            let unshared = self
                .images
                .get(i)
                .is_some_and(|image| Arc::strong_count(image) == 1);
            if unshared {
                ready.push(self.images.swap_remove(i));
            } else {
                i += 1;
            }
        }
        ready
    }
}

thread_local! {
    static PENDING_DROPS: std::cell::RefCell<PendingDrops> = Default::default();
}

/// Drops `image` from every window's sprite atlas immediately when nothing else
/// still holds a clone, or queues it for `flush_dropped` once the last external
/// borrower lets go so it is never stranded in the atlas.
pub(crate) fn drop_unshared(
    image: Arc<RenderImage>,
    window: Option<&mut gpui::Window>,
    cx: &mut App,
) {
    if Arc::strong_count(&image) == 1 {
        cx.drop_image(image, window);
    } else {
        PENDING_DROPS.with(|pending| pending.borrow_mut().push(image));
    }
}

/// Drops any queued view images whose external borrowers have released them.
pub(crate) fn flush_dropped(window: &mut gpui::Window, cx: &mut App) {
    let ready = PENDING_DROPS.with(|pending| {
        let mut queue = pending.borrow_mut();
        if queue.images.is_empty() {
            return Vec::new();
        }
        queue.take_unshared()
    });
    if ready.is_empty() {
        return;
    }
    for image in ready {
        cx.drop_image(image, Some(window));
    }
    crate::platform::give_back();
}

/// Decodes a JPEG whole, keeping its shape, as RGBA: an artist's portrait,
/// which the banner crops and melts itself (`banner::melted`).
pub(crate) fn decode_picture(jpeg: &[u8]) -> Option<image::RgbaImage> {
    Some(
        image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
            .ok()?
            .into_rgba8(),
    )
}

/// A picture cut square as `object-fit: cover` shows it in a square: at the
/// middle across, and `down` of the way down (0.3 for `center 30%`, as the
/// artist's banner cuts their portrait, so that a tall portrait keeps its
/// face in the wall's circle; 0.5 for the middle).
pub(crate) fn squared(image: image::DynamicImage, down: f32) -> image::DynamicImage {
    let (w, h) = (image.width(), image.height());
    if w > h {
        image.crop_imm((w - h) / 2, 0, h, h)
    } else if h > w {
        image.crop_imm(0, ((h - w) as f32 * down).round() as u32, w, w)
    } else {
        image
    }
}

/// Where a picture is cut square for its tile (`squared`).
const TILE_DOWN: f32 = 0.3;

/// The size to ask for again when a picture fetched at `bucket`, `jpeg`, is
/// not square and so too short once cut square for a slot `px` pixels: the
/// server sizes a picture by its longer side. `None` when it is square, when
/// it is sharp enough already, or when the server sent it smaller than it
/// asked, so it has no larger one.
pub(crate) fn sharper(jpeg: &[u8], px: u32, bucket: u32, sizes: &[u32]) -> Option<u32> {
    let (w, h) =
        image::ImageReader::with_format(std::io::Cursor::new(jpeg), image::ImageFormat::Jpeg)
            .into_dimensions()
            .ok()?;
    let (short, long) = (w.min(h).max(1), w.max(h));
    if short >= px || long < bucket {
        return None;
    }
    let wanted = (u64::from(px) * u64::from(long)).div_ceil(u64::from(short));
    let wanted = u32::try_from(wanted).unwrap_or(u32::MAX);
    let next = crate::net::bucket_in(sizes, wanted);
    (next > bucket).then_some(next)
}

/// A picture for a square slot `px` pixels: fetched at the size the slot
/// asks for, and once more at a larger one when it is not square and so too
/// short once cut square (`sharper`); then decoded (`decode_cover`).
pub(crate) async fn fetch_square(
    http: &crate::net::ArtHttp,
    sizes: &[u32],
    art: Id,
    px: u32,
) -> Option<(RenderImage, Rgba, usize)> {
    let bucket = crate::net::bucket_in(sizes, px);
    let mut bytes = http.get(art, bucket).await?;
    if let Some(larger) = sharper(&bytes, px, bucket, sizes)
        && let Some(again) = http.get(art, larger).await
    {
        bytes = again;
    }
    decode(move || decode_cover(&bytes, px)).await
}

/// Decodes a JPEG, cuts it square as it is shown (`squared`), resizes it
/// to `px` square when it is larger, swaps it to BGRA for GPUI and reads its
/// average colour.
pub(crate) fn decode_cover(jpeg: &[u8], px: u32) -> Option<(RenderImage, Rgba, usize)> {
    let decoded = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    // A picture larger than `px` either way is made `px` square, as it was
    // before it was cut, so that a thin one is not left a few pixels wide.
    let larger = decoded.width() > px || decoded.height() > px;
    let decoded = squared(decoded, TILE_DOWN);
    let decoded = if larger {
        decoded.resize_exact(px, px, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let mut pixels = decoded.into_rgba8();
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for (at, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if at % 7 == 0 {
            r += u64::from(px[0]);
            g += u64::from(px[1]);
            b += u64::from(px[2]);
            n += 1;
        }
        px.swap(0, 2);
    }
    let n = n.max(1) as f32 * 255.;
    let color = Rgba {
        r: r as f32 / n,
        g: g as f32 / n,
        b: b as f32 / n,
        a: 1.,
    };
    let bytes = pixels.len();
    Some((
        RenderImage::new(vec![image::Frame::new(pixels)]),
        color,
        bytes,
    ))
}

/// The covers the wall holds, decoded.
pub(crate) struct Cache {
    ready: HashMap<Id, Ready>,
    /// Each cover's average colour, once it has been seen.
    pub colors: HashMap<Id, Rgba>,
    pub missing: HashSet<Id>,
    bytes: usize,
    budget: usize,
    pub evicted: u64,
}

pub(crate) struct Ready {
    pub image: Arc<RenderImage>,
    pub px: u32,
    /// When it arrived, and whether it arrived on screen and so fades in.
    pub arrived: Instant,
    pub fades: bool,
    bytes: usize,
    used: u64,
}

impl Cache {
    pub(crate) fn new(budget: usize) -> Self {
        Self {
            ready: HashMap::new(),
            colors: HashMap::new(),
            missing: HashSet::new(),
            bytes: 0,
            budget,
            evicted: 0,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn len(&self) -> usize {
        self.ready.len()
    }

    /// The cover at `px`, marked as drawn in `frame`.
    pub(crate) fn get(&mut self, art: Id, px: u32, frame: u64) -> Option<&Ready> {
        let ready = self.ready.get_mut(&art)?;
        if ready.px != px {
            return None;
        }
        ready.used = frame;
        Some(ready)
    }

    /// The cover held for `art`, at whatever size.
    pub(crate) fn image(&self, art: Id) -> Option<Arc<RenderImage>> {
        self.ready.get(&art).map(|ready| Arc::clone(&ready.image))
    }

    pub(crate) fn has(&self, art: Id, px: u32) -> bool {
        self.ready.get(&art).is_some_and(|ready| ready.px == px)
    }

    /// Takes a cover in. `on_screen` says whether its tile is in view now.
    pub(crate) fn land(
        &mut self,
        arrived: Arrived,
        on_screen: impl Fn(Id) -> bool,
        frame: u64,
        cx: &mut App,
    ) {
        match arrived {
            Arrived::Missing { art } => {
                self.missing.insert(art);
            }
            Arrived::Ready {
                art,
                px,
                image,
                color,
                bytes,
            } => {
                self.colors.insert(art, color);
                if let Some(old) = self.ready.remove(&art) {
                    self.bytes -= old.bytes;
                    cx.drop_image(old.image, None);
                }
                self.bytes += bytes;
                self.ready.insert(
                    art,
                    Ready {
                        image,
                        px,
                        arrived: Instant::now(),
                        fades: on_screen(art),
                        bytes,
                        used: frame,
                    },
                );
                self.evict(frame, cx);
            }
        }
    }

    /// At rest, keeps little more than what is on screen.
    pub(crate) fn trim(
        &mut self,
        frame: u64,
        keep: usize,
        window: &mut gpui::Window,
        cx: &mut App,
    ) {
        if self.bytes > keep {
            let before = self.bytes;
            // Inside a window's update, GPUI only drops the image from that
            // window's atlas if it is handed the window.
            self.shrink(frame, keep, Some(window), cx);
            crate::platform::give_back();
            crate::debug(format_args!(
                "trim: {} MB to {} MB",
                before >> 20,
                self.bytes >> 20
            ));
        }
    }

    /// Lets the least recently drawn go, never one drawn in the last two frames.
    fn evict(&mut self, frame: u64, cx: &mut App) {
        if self.bytes > self.budget {
            self.shrink(frame, self.budget * 9 / 10, None, cx);
        }
    }

    fn shrink(
        &mut self,
        frame: u64,
        to: usize,
        mut window: Option<&mut gpui::Window>,
        cx: &mut App,
    ) {
        let mut old: Vec<(u64, Id)> = self
            .ready
            .iter()
            .filter(|(_, ready)| ready.used + 2 < frame)
            .map(|(art, ready)| (ready.used, *art))
            .collect();
        old.sort_unstable_by_key(|(used, _)| *used);
        for (_, art) in old {
            if self.bytes <= to {
                break;
            }
            if let Some(gone) = self.ready.remove(&art) {
                self.bytes -= gone.bytes;
                self.evicted += 1;
                cx.drop_image(gone.image, window.as_deref_mut());
            }
        }
    }
}

#[cfg(test)]
mod drop_tests {
    use super::*;

    fn tiny_image() -> Arc<RenderImage> {
        Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(1, 1),
        )]))
    }

    #[test]
    fn pending_drops_waits_until_external_borrower_releases() {
        let mut queue = PendingDrops::default();
        let original = tiny_image();
        let borrower = Arc::clone(&original);
        queue.push(Arc::clone(&original));
        // Duplicate push of the same image is deduplicated.
        queue.push(original);
        assert_eq!(queue.images.len(), 1);
        assert!(queue.take_unshared().is_empty());
        drop(borrower);
        let ready = queue.take_unshared();
        assert_eq!(ready.len(), 1);
        assert!(queue.images.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn decode_bounds_concurrent_work_to_decoders() -> Result<(), Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        rt.block_on(async {
            let sem = Arc::new(Semaphore::new(decoders()));
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let mut tasks = Vec::new();
            for i in 0..24 {
                let sem = Arc::clone(&sem);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                tasks.push(tokio::spawn(async move {
                    decode_with(sem, move || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(15));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Some(i)
                    })
                    .await
                }));
            }
            for (i, task) in tasks.into_iter().enumerate() {
                assert_eq!(task.await?, Some(i));
            }
            assert!(peak.load(Ordering::SeqCst) <= decoders());
            Ok(())
        })
    }

    #[test]
    fn decode_holds_permit_until_blocking_work_exits_after_abort()
    -> Result<(), Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        rt.block_on(async {
            let limit = decoders();
            let sem = Arc::new(Semaphore::new(limit));
            let active = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(tokio::sync::Barrier::new(limit + 1));
            let release = Arc::new(std::sync::Barrier::new(limit + 1));

            let mut aborted = Vec::new();
            for _ in 0..limit {
                let sem = Arc::clone(&sem);
                let active = Arc::clone(&active);
                let peak = Arc::clone(&peak);
                let entered = Arc::clone(&entered);
                let release = Arc::clone(&release);
                let handle = tokio::spawn(async move {
                    decode_with(sem, move || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::runtime::Handle::current().block_on(entered.wait());
                        release.wait();
                        active.fetch_sub(1, Ordering::SeqCst);
                        Some(())
                    })
                    .await
                });
                aborted.push(handle);
            }

            // Wait until every decoder slot is occupied inside spawn_blocking,
            // then abort all async waiters.
            tokio::time::timeout(Duration::from_secs(5), entered.wait()).await?;
            for handle in aborted {
                handle.abort();
            }

            // A new decode must not enter while the aborted blocking closures
            // still hold their owned permits.
            let extra_entered = Arc::new(AtomicUsize::new(0));
            let extra_flag = Arc::clone(&extra_entered);
            let extra_active = Arc::clone(&active);
            let extra_peak = Arc::clone(&peak);
            let extra_sem = Arc::clone(&sem);
            let extra = tokio::spawn(async move {
                decode_with(extra_sem, move || {
                    extra_flag.store(1, Ordering::SeqCst);
                    let now = extra_active.fetch_add(1, Ordering::SeqCst) + 1;
                    extra_peak.fetch_max(now, Ordering::SeqCst);
                    extra_active.fetch_sub(1, Ordering::SeqCst);
                    Some(())
                })
                .await
            });
            tokio::time::sleep(Duration::from_millis(40)).await;
            assert_eq!(extra_entered.load(Ordering::SeqCst), 0);

            // Release the blocking closures; now the queued decode completes.
            release.wait();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), extra).await??,
                Some(())
            );
            assert_eq!(extra_entered.load(Ordering::SeqCst), 1);
            assert!(peak.load(Ordering::SeqCst) <= limit);
            Ok(())
        })
    }

    #[test]
    fn fetch_bag_aborts_pending_tasks_on_drop() -> Result<(), Box<dyn std::error::Error>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let mut bag = FetchBag::default();
            let task = tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
            });
            bag.push(task.abort_handle());
            drop(bag);
            let joined = tokio::time::timeout(Duration::from_secs(2), task).await?;
            assert!(joined.is_err());
            Ok(())
        })
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;

    /// A JPEG of three bands down it, green, red and blue, each a third.
    fn banded(w: u32, h: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(w, h, |_, y| match 3 * y / h {
            0 => image::Rgb([0, 255, 0]),
            1 => image::Rgb([255, 0, 0]),
            _ => image::Rgb([0, 0, 255]),
        });
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap_or_default();
        bytes
    }

    /// The pixel at `(x, y)` of a decoded picture `side` square, in BGRA.
    fn at(image: &RenderImage, side: usize, x: usize, y: usize) -> Option<[u8; 4]> {
        let bytes = image.as_bytes(0)?;
        let at = (y * side + x) * 4;
        bytes.get(at..at + 4)?.try_into().ok()
    }

    fn red(pixel: Option<[u8; 4]>) -> bool {
        matches!(pixel, Some([b, g, r, _]) if r > 200 && g < 60 && b < 60)
    }

    fn green(pixel: Option<[u8; 4]>) -> bool {
        matches!(pixel, Some([b, g, r, _]) if g > 200 && r < 60 && b < 60)
    }

    #[test]
    fn a_tall_picture_is_cut_square_30_percent_down_not_squashed() {
        // 100 by 300, made 50 square: rows 60 to 160, green above red, the
        // edge at row 20 of 50. Cut at the middle it would be all red; at
        // 25% or 35% the edge would be at 25 or 15; squashed, its foot blue.
        let decoded = decode_cover(&banded(100, 300), 50);
        assert!(decoded.is_some(), "not decoded");
        let Some((image, _, _)) = decoded else { return };
        let size = image.size(0);
        assert_eq!((size.width.0, size.height.0), (50, 50));
        for y in [2, 17] {
            assert!(
                green(at(&image, 50, 25, y)),
                "{y}: {:?}",
                at(&image, 50, 25, y)
            );
        }
        for y in [23, 47] {
            assert!(
                red(at(&image, 50, 25, y)),
                "{y}: {:?}",
                at(&image, 50, 25, y)
            );
        }
    }

    #[test]
    fn a_wide_picture_is_cut_square_at_its_middle() {
        // Three bands across, 300 by 100, made 50 square: the middle band.
        let image = image::RgbImage::from_fn(300, 100, |x, _| match x / 100 {
            0 => image::Rgb([0, 255, 0]),
            1 => image::Rgb([255, 0, 0]),
            _ => image::Rgb([0, 0, 255]),
        });
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 95)
            .encode_image(&image)
            .unwrap_or_default();
        let decoded = decode_cover(&bytes, 50);
        assert!(decoded.is_some(), "not decoded");
        let Some((image, _, _)) = decoded else { return };
        for x in [3, 25, 46] {
            assert!(
                red(at(&image, 50, x, 25)),
                "{x}: {:?}",
                at(&image, 50, x, 25)
            );
        }
    }

    #[test]
    fn a_picture_too_short_once_cut_square_is_asked_for_again_larger() {
        let sizes = [64, 96, 128, 192, 256, 384, 512, 768, 1024];
        // Tall, sent at 384 on its longer side: 192 wide, short of a 300
        // slot; 600 wanted along its longer side, so 768.
        assert_eq!(sharper(&banded(192, 384), 300, 384, &sizes), Some(768));
        // Wide, the same.
        assert_eq!(sharper(&banded(384, 192), 300, 384, &sizes), Some(768));
        // Square, or wide enough already: none.
        assert_eq!(sharper(&banded(384, 384), 300, 384, &sizes), None);
        assert_eq!(sharper(&banded(320, 384), 300, 384, &sizes), None);
        // Sent smaller than asked: the server has no larger one.
        assert_eq!(sharper(&banded(100, 200), 300, 384, &sizes), None);
        // At the largest size already: none larger.
        assert_eq!(sharper(&banded(512, 1024), 760, 1024, &sizes), None);
        // Thinner than the largest size can cover: the largest.
        assert_eq!(sharper(&banded(96, 384), 300, 384, &sizes), Some(1024));
        // A slot larger than every size: none larger to ask for.
        assert_eq!(sharper(&banded(512, 1024), 1200, 1024, &sizes), None);
    }

    #[test]
    fn a_thin_picture_is_still_made_its_slots_size() {
        for (w, h) in [(1, 300), (300, 1), (40, 40)] {
            let decoded = decode_cover(&banded(w, h), 50);
            assert!(decoded.is_some(), "{w}x{h} not decoded");
            let Some((image, _, _)) = decoded else { return };
            let size = image.size(0);
            let side = if w > 50 || h > 50 {
                50
            } else {
                w.min(h) as i32
            };
            assert_eq!((size.width.0, size.height.0), (side, side), "{w}x{h}");
        }
    }
}
