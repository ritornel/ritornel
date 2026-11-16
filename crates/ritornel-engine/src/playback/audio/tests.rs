//! The pipeline's decoding, held to reference decodes, without a sound:
//! the fixtures go through the decoder, the channel step and the resampler
//! into a sample sink, and through the real output's callback into the
//! null output. The references in `fixtures/decoded` were made once with
//! the ffmpeg that `mise.toml` pins (`fixtures/audio/generate.sh`), so no
//! test needs ffmpeg.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::decode::{self, OutputSpec, Source};
use super::{AudioRenderer, Output};
use crate::playback::renderer::{FileFacts, Renderer};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

/// Every sample the pipeline makes of a file, from `start_ms`, for an
/// output of this rate and these channels.
fn decoded(name: &str, start_ms: u64, to: OutputSpec) -> Vec<f32> {
    let source = Source::open(&fixture(name), start_ms).unwrap();
    let mut samples = Vec::new();
    decode::run(source, to, &AtomicBool::new(false), &mut |frames| {
        samples.extend_from_slice(frames);
        true
    })
    .unwrap();
    samples
}

/// The fixtures' own rate and channels: no step changes a sample.
const AS_IS: OutputSpec = OutputSpec {
    rate: 16_000,
    channels: 1,
};

fn decoded_by_ffmpeg(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/decoded")
            .join(name),
    )
    .unwrap()
}

fn flac_reference() -> Vec<i16> {
    decoded_by_ffmpeg("a.flac.s16le")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect()
}

#[test]
fn a_flac_comes_out_of_the_pipeline_bit_for_bit_as_the_reference_decode() {
    let samples = decoded("a.flac", 0, AS_IS);
    let reference = flac_reference();
    assert_eq!(samples.len(), reference.len());
    assert_eq!(samples.len(), 8000);
    for (at, (sample, expected)) in samples.iter().zip(&reference).enumerate() {
        // A 16-bit sample is exact in a float: scaled back, it is the same
        // integer, with nothing to round.
        let scaled = sample * 32_768.0;
        assert_eq!(scaled.fract(), 0.0, "sample {at} is not a 16-bit value");
        assert_eq!(scaled as i16, *expected, "sample {at}");
    }
}

#[test]
fn a_flac_played_from_a_position_is_the_rest_of_the_same_samples() {
    let reference = flac_reference();
    let samples = decoded("a.flac", 250, AS_IS);
    let from = 250 * 16;
    assert_eq!(samples.len(), reference.len() - from);
    for (sample, expected) in samples.iter().zip(&reference[from..]) {
        assert_eq!((sample * 32_768.0) as i16, *expected);
    }
}

#[test]
fn an_mp3_comes_out_with_the_reference_decodes_length_and_within_a_float_rounding_of_it() {
    let reference: Vec<f32> = decoded_by_ffmpeg("a.mp3.f32le")
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect();
    let samples = decoded("a.mp3", 0, AS_IS);
    // The encoder's delay and padding are trimmed as ffmpeg trims them.
    assert_eq!(samples.len(), reference.len());
    let worst = samples
        .iter()
        .zip(&reference)
        .map(|(sample, expected)| (sample - expected).abs())
        .fold(0.0_f32, f32::max);
    assert!(worst < 1e-4, "the decoders differ by {worst}");
}

#[test]
fn a_track_at_another_rate_is_resampled_to_the_outputs_length_and_a_mono_one_fills_both_channels() {
    let stereo_48k = OutputSpec {
        rate: 48_000,
        channels: 2,
    };
    let samples = decoded("a.flac", 0, stereo_48k);
    // Half a second at 48 kHz, in two channels.
    assert_eq!(samples.len(), 24_000 * 2);
    let (left, right): (Vec<f32>, Vec<f32>) = samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[left, right]| (*left, *right))
        .unzip();
    assert_eq!(left, right);
    // Still a 440 Hz tone: about 440 sign changes in half a second, away
    // from the edges the resampler's filter touches.
    let middle = &left[2400..21_600];
    let crossings = middle
        .windows(2)
        .filter(|pair| (pair[0] < 0.0) != (pair[1] < 0.0))
        .count();
    assert!((350..=360).contains(&crossings), "{crossings} crossings");
}

/// The loudness of each 10 ms of one channel, as root mean square.
fn loudness(samples: &[f32], channels: usize, rate: usize) -> Vec<f32> {
    let one: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    one.chunks_exact(rate / 100)
        .map(|window| (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt())
        .collect()
}

#[test]
fn resampling_keeps_every_frame_of_a_track_whatever_size_its_packets_are() {
    // A FLAC's packets are 4,608 frames and an MP3's 1,152, both more than
    // the resampler takes at a time: none of the rest may be lost.
    let stereo_48k = OutputSpec {
        rate: 48_000,
        channels: 2,
    };
    for name in ["a.flac", "a.mp3"] {
        let samples = decoded(name, 0, stereo_48k);
        assert_eq!(samples.len(), 24_000 * 2, "{name}");
        // A steady tone stays as loud from end to end, away from the
        // edges, where the resampler's filter and the encoder's fade sit.
        let windows = loudness(&samples, 2, 48_000);
        let steady = &windows[5..45];
        let (quietest, loudest) = steady.iter().fold((f32::MAX, 0.0_f32), |(low, high), rms| {
            (low.min(*rms), high.max(*rms))
        });
        assert!(quietest > 0.9 * loudest, "{name}: {quietest} to {loudest}");
        // And it matches the track at its own rate.
        let own = loudness(&decoded(name, 0, AS_IS), 1, 16_000);
        let (own_mean, mean) = (
            own[5..45].iter().sum::<f32>() / 40.0,
            steady.iter().sum::<f32>() / 40.0,
        );
        assert!(
            (own_mean - mean).abs() < 0.02 * own_mean,
            "{name}: {own_mean} and {mean}"
        );
    }
}

#[test]
fn a_file_that_is_not_audio_or_not_there_is_refused_with_the_reason_the_queue_records() {
    let refused = Source::open(&fixture("not-audio.flac"), 0).unwrap_err();
    assert_eq!(refused.reason, "unsupported_format");
    let refused = Source::open(&fixture("no-such-file.flac"), 0).unwrap_err();
    assert_eq!(refused.reason, "missing");
    // Opus has no decoder in this Symphonia: the player moves on.
    let refused = Source::open(&fixture("a.opus"), 0).unwrap_err();
    assert_eq!(refused.reason, "unsupported_format");
}

/// What a scan would record of the file at `path`.
fn facts(path: &Path) -> FileFacts {
    let metadata = std::fs::metadata(path).unwrap();
    FileFacts {
        size_bytes: metadata.len(),
        mtime_ms: crate::walk::mtime_ms(&metadata),
    }
}

/// Polls the renderer until `done` says so, for at most ten seconds. The
/// null output runs at many times real time, so this is milliseconds.
fn until(renderer: &mut AudioRenderer, done: impl Fn(&crate::playback::Segment) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let segment = renderer.poll();
        if done(&segment) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the output did not get there: {segment:?}"
        );
        std::thread::yield_now();
    }
}

#[test]
fn the_null_output_plays_a_track_to_its_end_and_counts_every_frame_it_was_handed() {
    let mut renderer = AudioRenderer::open(Output::Null { speed: 50 }).unwrap();
    assert_eq!((renderer.rate(), renderer.channels()), (48_000, 2));
    let format = renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0)
        .unwrap();
    assert_eq!(
        (
            format.codec.as_str(),
            format.sample_rate_hz,
            format.channels
        ),
        ("flac", 16_000, 1)
    );
    assert_eq!(format.duration_ms, Some(500));
    // Loaded and not playing: nothing is heard.
    assert_eq!(renderer.poll().frames, 0);
    renderer.play();
    until(&mut renderer, |segment| segment.ended);
    let last = renderer.unload();
    assert_eq!(last.frames, 24_000);
    assert!(last.ended);
}

#[test]
fn a_seek_ends_the_segment_with_its_count_and_the_next_plays_the_rest() {
    let mut renderer = AudioRenderer::open(Output::Null { speed: 50 }).unwrap();
    renderer
        .load(&fixture("a.mp3"), facts(&fixture("a.mp3")), 0)
        .unwrap();
    renderer.play();
    until(&mut renderer, |segment| segment.frames > 0);
    let first = renderer.seek(400).unwrap();
    assert!(first.frames > 0 && !first.ended);
    until(&mut renderer, |segment| segment.ended);
    // The last 100 ms at 48 kHz.
    assert_eq!(renderer.unload().frames, 4_800);
    // A paused output hands nothing over.
    renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0)
        .unwrap();
    let paused = renderer.poll().frames;
    assert_eq!(paused, 0);
    assert_eq!(renderer.unload().frames, 0);
}

#[test]
fn a_seek_that_cannot_open_the_file_leaves_the_segment_in_hand_playing_and_counted() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("a.mp3");
    std::fs::copy(fixture("a.mp3"), &path).unwrap();
    let mut renderer = AudioRenderer::open(Output::Null { speed: 5 }).unwrap();
    renderer.load(&path, facts(&path), 0).unwrap();
    renderer.play();
    until(&mut renderer, |segment| segment.frames > 0);
    // The file is emptied where it is: nothing can be opened at 400 ms.
    std::fs::File::create(&path).unwrap();
    assert!(renderer.seek(400).is_err());
    // What the segment in hand played is still counted, and it plays on
    // to the end of what it had buffered.
    let heard = renderer.poll().frames;
    assert!(heard > 0, "the segment's count was lost");
    until(&mut renderer, |segment| {
        segment.ended || segment.failed.is_some()
    });
    assert!(renderer.unload().frames >= heard);
}

#[test]
fn a_file_is_played_only_while_it_is_the_one_the_library_read_and_a_seek_reads_that_file() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("01.mp3");
    std::fs::copy(fixture("a.mp3"), &path).unwrap();
    let scanned = facts(&path);
    let mut renderer = AudioRenderer::open(Output::Null { speed: 5 }).unwrap();

    // Another file in its place, of another size: refused, as changed.
    let other = scratch.path().join("other.flac");
    std::fs::copy(fixture("b.flac"), &other).unwrap();
    let before = scratch.path().join("before.mp3");
    std::fs::rename(&path, &before).unwrap();
    std::fs::rename(&other, &path).unwrap();
    let refused = renderer.load(&path, scanned, 0).unwrap_err();
    assert_eq!(refused.reason, "file_changed");
    std::fs::rename(&before, &path).unwrap();

    // Loaded, and then something else takes the path: a seek still reads
    // the file that was loaded, to its end.
    renderer.load(&path, scanned, 0).unwrap();
    renderer.play();
    until(&mut renderer, |segment| segment.frames > 0);
    std::fs::rename(&path, &before).unwrap();
    std::fs::copy(fixture("not-audio.flac"), &path).unwrap();
    assert!(!renderer.seek(400).unwrap().ended);
    until(&mut renderer, |segment| segment.ended);
    assert_eq!(renderer.unload().frames, 4_800);

    // Loaded, and then written to where it is: a seek is refused, and the
    // segment in hand is still counted.
    renderer.load(&before, facts(&before), 0).unwrap();
    renderer.play();
    until(&mut renderer, |segment| segment.frames > 0);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&before)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"more"))
        .unwrap();
    assert_eq!(renderer.seek(400).unwrap_err().reason, "file_changed");
    assert!(renderer.poll().frames > 0);
}
