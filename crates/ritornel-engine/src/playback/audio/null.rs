//! The null output: a thread that asks the callback for frames as a device
//! would, at real-time speed or a multiple of it, and hands them to
//! nothing. It makes no sound and needs no device, so the tests, a server
//! with no speakers and a silent trial run all use it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Starts the null output, which drives `callback` at `speed` times real
/// time. With a tap, every frame the callback hands over is kept there, for
/// a test to hold to what was decoded.
pub(super) fn start(
    mut callback: Callback,
    spec: OutputSpec,
    speed: u32,
    tap: Option<Tap>,
) -> Result<NullOutput, RenderError> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let stop = Arc::clone(&stop);
        std::thread::Builder::new()
            .name("ritornel-null-output".to_owned())
            .spawn(move || {
                let frames = usize::try_from(spec.rate * PERIOD_MS / 1000).unwrap_or(480);
                let mut buffer = vec![0.0_f32; frames * spec.channels];
                let started = Instant::now();
                let mut rendered: u64 = 0;
                let per_second = f64::from(spec.rate) * f64::from(speed);
                while !stop.load(Ordering::SeqCst) {
                    let written = callback.render(&mut buffer);
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
