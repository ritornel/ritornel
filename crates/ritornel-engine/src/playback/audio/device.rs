//! The system's default output device, through cpal, in shared mode: one
//! stream at the device's own rate and channels, opened as 32-bit float.
//! The stream lives on a thread of its own, which holds it until the
//! output is dropped. See "Audio pipeline" in docs/internals/playback.md.
//!
//! Only the default device is used. A stream that errors, or a default
//! that changes, is not followed while the stream is open; the player
//! closes an idle or stale output, and play opens the default again.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::decode::OutputSpec;
use super::output::{Callback, Shared};
use super::{Ends, Running};
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

/// The device's default configuration, taken as 32-bit float. CoreAudio's
/// default is always float, and so is WASAPI's shared-mode mix format and
/// that of ALSA's `default` device behind PipeWire or PulseAudio. A device whose default is another format
/// (an ALSA hardware device with no sound server) is asked for float at
/// the same rate and channels, and refused if it has none.
fn float_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, RenderError> {
    let config = device.default_output_config().map_err(output_error)?;
    if config.sample_format() == cpal::SampleFormat::F32 {
        return Ok(config);
    }
    device
        .supported_output_configs()
        .map_err(output_error)?
        .filter(|range| {
            range.sample_format() == cpal::SampleFormat::F32
                && range.channels() == config.channels()
        })
        .find_map(|range| range.try_with_sample_rate(config.sample_rate()))
        .ok_or_else(|| {
            output_error(format!(
                "the default output takes {} samples, not 32-bit float",
                config.sample_format()
            ))
        })
}

type Queues = (Ends, Callback);

/// The device running, what it takes, and the control side's queues.
type Started = (DeviceOutput, OutputSpec, Ends);

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
            let config = match float_config(&device) {
                Ok(config) => config,
                Err(error) => {
                    let _ = found_tx.send(Err(error));
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
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    callback.render(data);
                },
                move |error: cpal::Error| {
                    errors.stream_errors.fetch_add(1, Ordering::Relaxed);
                    if matches!(
                        error.kind(),
                        cpal::ErrorKind::DeviceNotAvailable
                            | cpal::ErrorKind::StreamInvalidated
                            | cpal::ErrorKind::HostUnavailable
                    ) {
                        errors.lost.store(true, Ordering::SeqCst);
                    }
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
    let (ends, callback) = queues(spec, shared);
    let output = output(name);
    callback_tx
        .send(callback)
        .map_err(|_| output_error("the output thread stopped"))?;
    started
        .recv()
        .map_err(|_| output_error("the output thread stopped"))??;
    Ok((output, spec, ends))
}

#[cfg(test)]
mod tests {
    use cpal::traits::{DeviceTrait, HostTrait};

    use super::float_config;

    /// Opens the system's default output and builds a stream on it, never
    /// starting it, so nothing is played. It touches a real device, so no
    /// run of the tests does it unasked:
    /// `cargo test --package ritornel-engine -- --ignored the_default_output_opens`.
    #[test]
    #[ignore = "opens the system's default output device"]
    fn the_default_output_opens() {
        let host = cpal::default_host();
        let device = host.default_output_device().unwrap();
        let config = float_config(&device).unwrap();
        let stream = device
            .build_output_stream::<f32, _, _>(
                config.config(),
                |data: &mut [f32], _: &cpal::OutputCallbackInfo| data.fill(0.0),
                |_: cpal::Error| {},
                None,
            )
            .unwrap();
        drop(stream);
    }
}
