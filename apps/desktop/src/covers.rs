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
use std::sync::Arc;
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
        decode: Arc::new(Semaphore::new(decoders())),
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
            let Ok(_permit) = decode.acquire().await else {
                return art;
            };
            // Waiting for a decoder, the wall may have moved on.
            if !wanted.borrow().ids.contains(&art) {
                if let Ok(mut counts) = counts.lock() {
                    counts.skipped_decode += 1;
                }
                return art;
            }
            let decoded = tokio::task::spawn_blocking(move || decode_cover(&bytes, px))
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

/// Decodes a JPEG, resizes it to `px` square when it is larger, swaps it to
/// BGRA for GPUI and reads its average colour.
pub(crate) fn decode_cover(jpeg: &[u8], px: u32) -> Option<(RenderImage, Rgba, usize)> {
    let decoded = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg).ok()?;
    let decoded = if decoded.width() > px || decoded.height() > px {
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
