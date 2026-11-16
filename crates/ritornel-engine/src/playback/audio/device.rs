//! The system's default output device, through cpal, in shared mode: one
//! stream at the device's own rate and channels, opened as 32-bit float.
//! The stream lives on a thread of its own, which holds it until the
//! output is dropped. See decisions 4 and 5 and "The pipeline" in
//! docs/design/playback.md.
//!
//! Only the default device is used, and a stream that errors or a default
//! that changes is not rebuilt yet: that arrives with the full player.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rtrb::{Consumer, Producer};

use super::Running;
use super::decode::OutputSpec;
use super::output::{Callback, Message, Shared};
use crate::playback::renderer::RenderError;

/// The running device. Dropping it stops the stream.
#[derive(Debug)]
pub(super) struct DeviceOutput {
    name: String,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Running for DeviceOutput {}

impl DeviceOutput {
    pub(super) fn name(&self) -> String {
        self.name.clone()
    }
}

impl Drop for DeviceOutput {
    fn drop(&mut self) {
        // The thread waits on this channel, and ends when it closes.
        self.stop = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn output_error(error: impl std::fmt::Display) -> RenderError {
    RenderError::new("output", error.to_string())
}

type Queues = (Producer<Message>, Consumer<Consumer<f32>>, Callback);

/// The device running, what it takes, and the control side's two queues.
type Started = (
    DeviceOutput,
    OutputSpec,
    Producer<Message>,
    Consumer<Consumer<f32>>,
);

/// Opens the default output device and starts its stream. `queues` makes
/// the callback for the device's rate and channels once they are known.
pub(super) fn start(
    shared: &Arc<Shared>,
    queues: fn(OutputSpec, &Arc<Shared>) -> Queues,
) -> Result<Started, RenderError> {
    let (found_tx, found) = mpsc::channel::<Result<(OutputSpec, String), RenderError>>();
    let (callback_tx, callback_rx) = mpsc::channel::<Callback>();
    let (started_tx, started) = mpsc::channel::<Result<(), RenderError>>();
    let (stop, stopped) = mpsc::channel::<()>();
    let errors = Arc::clone(shared);
    let thread = std::thread::Builder::new()
        .name("ritornel-output".to_owned())
        .spawn(move || {
            let host = cpal::default_host();
            let Some(device) = host.default_output_device() else {
                let _ = found_tx.send(Err(output_error("there is no default output device")));
                return;
            };
            let config = match device.default_output_config() {
                Ok(config) => config,
                Err(error) => {
                    let _ = found_tx.send(Err(output_error(error)));
                    return;
                }
            };
            let spec = OutputSpec {
                rate: config.sample_rate(),
                channels: usize::from(config.channels()),
            };
            let name = device.description().map_or_else(
                |_| "default".to_owned(),
                |description| description.name().to_owned(),
            );
            if found_tx.send(Ok((spec, name))).is_err() {
                return;
            }
            let Ok(mut callback) = callback_rx.recv() else {
                return;
            };
            let stream_config = cpal::StreamConfig {
                channels: config.channels(),
                sample_rate: config.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            let stream = device.build_output_stream::<f32, _, _>(
                stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| callback.render(data),
                move |_error| {
                    errors.stream_errors.fetch_add(1, Ordering::Relaxed);
                },
                None,
            );
            let stream = match stream.and_then(|stream| stream.play().map(|()| stream)) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = started_tx.send(Err(output_error(error)));
                    return;
                }
            };
            let _ = started_tx.send(Ok(()));
            // Held until the output is dropped.
            let _ = stopped.recv();
            drop(stream);
        })
        .map_err(output_error)?;
    let output = |name: String| DeviceOutput {
        name,
        stop: Some(stop),
        thread: Some(thread),
    };
    let (spec, name) = found
        .recv()
        .map_err(|_| output_error("the output thread stopped"))??;
    let (control, retired, callback) = queues(spec, shared);
    let output = output(name);
    callback_tx
        .send(callback)
        .map_err(|_| output_error("the output thread stopped"))?;
    started
        .recv()
        .map_err(|_| output_error("the output thread stopped"))??;
    Ok((output, spec, control, retired))
}
