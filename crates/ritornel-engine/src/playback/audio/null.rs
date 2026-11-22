//! The null output: a thread that asks the callback for frames as a device
//! would, at real-time speed or a multiple of it, and hands them to
//! nothing. It makes no sound and needs no device, so the tests, a server
//! with no speakers and a silent trial run all use it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::Running;
use super::decode::OutputSpec;
use super::output::Callback;
use crate::playback::renderer::RenderError;

/// How much audio each pass asks for, as a device's buffer would.
const PERIOD_MS: u32 = 10;

/// The running null output. Dropping it stops the thread.
#[derive(Debug)]
pub(super) struct NullOutput {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Running for NullOutput {}

impl Drop for NullOutput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What a test reads the frames handed to the null output from.
pub(super) type Tap = Arc<std::sync::Mutex<Vec<f32>>>;

/// For a test: a hold on the null output. Once the output has handed over
/// `at` frames it asks for no more, as a device that has stopped would,
/// until it is let go, and meanwhile it says how many samples wait in the
/// ring it plays. A test then acts at a frame it chose, however slowly the
/// machine runs. Held, it takes no messages, so a call that waits for the
/// output to take one, as a load, a seek or an unload does, waits in vain.
#[derive(Debug)]
pub(super) struct Hold {
    at: u64,
    let_go: AtomicBool,
    /// The samples waiting while held; `usize::MAX` until it holds.
    waiting: AtomicUsize,
}

impl Hold {
    #[cfg(test)]
    pub(super) fn at(frames: u64) -> Arc<Self> {
        Arc::new(Self {
            at: frames,
            let_go: AtomicBool::new(false),
            waiting: AtomicUsize::new(usize::MAX),
        })
    }

    /// The samples waiting in the ring, once the output holds.
    #[cfg(test)]
    pub(super) fn waiting(&self) -> Option<usize> {
        Some(self.waiting.load(Ordering::SeqCst)).filter(|waiting| *waiting != usize::MAX)
    }

    #[cfg(test)]
    pub(super) fn let_go(&self) {
        self.let_go.store(true, Ordering::SeqCst);
    }

    fn holds(&self, handed: u64) -> bool {
        handed >= self.at && !self.let_go.load(Ordering::SeqCst)
    }
}

/// Starts the null output, which drives `callback` at `speed` times real
/// time. With a tap, every frame the callback hands over is kept there, for
/// a test to hold to what was decoded; with a hold, it stops where the
/// test says.
pub(super) fn start(
    mut callback: Callback,
    spec: OutputSpec,
    speed: u32,
    tap: Option<Tap>,
    hold: Option<Arc<Hold>>,
) -> Result<NullOutput, RenderError> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("ritornel-null-output".to_owned())
            .spawn(move || {
                let frames = usize::try_from(spec.rate * PERIOD_MS / 1000).unwrap_or(480);
                let mut buffer = vec![0.0_f32; frames * spec.channels];
                let mut started = Instant::now();
                let mut rendered: u64 = 0;
                let mut handed: u64 = 0;
                let per_second = f64::from(spec.rate) * f64::from(speed);
                while !stop.load(Ordering::SeqCst) {
                    if let Some(hold) = &hold
                        && hold.holds(handed)
                    {
                        hold.waiting.store(callback.waiting(), Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(1));
                        // Paced afresh once let go.
                        (started, rendered) = (Instant::now(), 0);
                        continue;
                    }
                    let written = callback.render(&mut buffer);
                    handed += written as u64;
                    if let Some(tap) = &tap
                        && let Some(frames) = buffer.get(..written * spec.channels)
                    {
                        tap.lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .extend_from_slice(frames);
                    }
                    rendered += frames as u64;
                    // Paced against the start, so that the rate holds
                    // however long each pass takes.
                    let due = Duration::from_secs_f64(rendered as f64 / per_second);
                    if let Some(wait) = due.checked_sub(started.elapsed()) {
                        std::thread::sleep(wait);
                    }
                }
            })
            .map_err(|error| RenderError::new("output", error.to_string()))?
    };
    Ok(NullOutput {
        stop,
        thread: Some(thread),
    })
}
