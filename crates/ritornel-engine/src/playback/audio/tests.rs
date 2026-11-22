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
use crate::playback::renderer::{End, Event, FileFacts, Progress, Renderer, ReplayGain};

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

/// Gathers what the renderer says until `done` holds of all of it, for at
/// most ten seconds. The null output runs at many times real time, so this
/// is milliseconds.
fn until(renderer: &mut AudioRenderer, said: &mut Vec<Event>, done: impl Fn(&[Event]) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        said.extend(renderer.drain());
        if done(said) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the output did not get there: {said:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// How span `span` ended, with its count, once it has.
fn ended(said: &[Event], span: u64) -> Option<(u64, End)> {
    said.iter().find_map(|event| match event {
        Event::Ended {
            span: ended,
            frames,
            end,
            ..
        } if *ended == span => Some((*frames, end.clone())),
        _ => None,
    })
}

/// Waits until the output has handed over some of what it plays.
fn heard(renderer: &AudioRenderer) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let progress = renderer.progress();
        if progress.frames > 0 {
            return progress.frames;
        }
        assert!(Instant::now() < deadline, "nothing was heard");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn the_null_output_plays_a_track_to_its_end_and_counts_every_frame_it_was_handed() {
    let mut renderer = AudioRenderer::open(Output::Null { speed: 50 }).unwrap();
    assert_eq!((renderer.rate(), renderer.channels()), (48_000, 2));
    let format = renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0, 1)
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
    assert_eq!(renderer.progress().frames, 0);
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    assert!(matches!(said[0], Event::Began { span: 1, .. }));
    assert_eq!(ended(&said, 1), Some((24_000, End::Drained)));
}

#[test]
fn a_seek_ends_the_span_with_its_count_and_the_next_plays_the_rest() {
    let mut renderer = AudioRenderer::open(Output::Null { speed: 5 }).unwrap();
    renderer
        .load(&fixture("a.mp3"), facts(&fixture("a.mp3")), 0, 1)
        .unwrap();
    renderer.play();
    heard(&renderer);
    renderer.seek(1, 400, 2).unwrap();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    let (first, end) = ended(&said, 1).unwrap();
    assert!(first > 0 && end == End::Replaced, "{said:?}");
    // The last 100 ms at 48 kHz.
    assert_eq!(ended(&said, 2), Some((4_800, End::Drained)));
    // A paused output hands nothing over.
    renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0, 3)
        .unwrap();
    assert_eq!(
        renderer.progress(),
        Progress {
            span: 3,
            frames: 0,
            lost: false
        }
    );
    renderer.unload();
    assert_eq!(ended(&renderer.drain(), 3), Some((0, End::Replaced)));
}

#[test]
fn a_seek_to_the_end_of_a_track_or_past_it_reads_nothing_and_ends_it_as_its_audio_ran_out() {
    // Each fixture is 500 ms long: a seek bar dragged to its end asks for
    // 500, and a duration the library has wrong can ask for more.
    for name in ["a.flac", "a.mp3", "a.ogg", "a.wav", "a.aiff", "a.alac.m4a"] {
        for at in [500, 10_000] {
            let mut source = Source::open(&fixture(name), at)
                .unwrap_or_else(|error| panic!("{name} at {at}: {error}"));
            assert!(!source.next().unwrap(), "{name} at {at}");
            // Past the end, the whole track was once read for nothing.
            if at > 500 {
                assert_eq!(source.packets, 0, "{name} at {at}");
            }
        }
    }
    // Cut short, the file says it is longer than it is: a seek past the
    // cut is not to the end it says, and fails as any unreadable file does.
    let scratch = crate::testing::Scratch::new();
    let cut = scratch.path().join("cut.flac");
    let whole = std::fs::read(fixture("a.flac")).unwrap();
    std::fs::write(&cut, &whole[..whole.len() - 800]).unwrap();
    assert_eq!(Source::open(&cut, 450).unwrap_err().reason, "unreadable");
    assert!(!Source::open(&cut, 500).unwrap().next().unwrap());
    let mut renderer = AudioRenderer::open(Output::Null { speed: 5 }).unwrap();
    let path = fixture("a.flac");
    renderer.load(&path, facts(&path), 0, 1).unwrap();
    renderer.play();
    heard(&renderer);
    renderer.seek(1, 500, 2).unwrap();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    assert_eq!(ended(&said, 2), Some((0, End::Drained)));
}

#[test]
fn a_seek_that_cannot_open_the_file_leaves_the_span_in_hand_playing_and_counted() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("a.mp3");
    std::fs::copy(fixture("a.mp3"), &path).unwrap();
    let mut renderer = AudioRenderer::open(Output::Null { speed: 5 }).unwrap();
    renderer.load(&path, facts(&path), 0, 1).unwrap();
    renderer.play();
    heard(&renderer);
    // The file is emptied where it is: nothing can be opened at 400 ms.
    std::fs::File::create(&path).unwrap();
    assert!(renderer.seek(1, 400, 2).is_err());
    // The span in hand plays on, counted, to the end of what it had.
    let progress = renderer.progress();
    assert!(progress.span == 1 && progress.frames > 0, "{progress:?}");
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    let (frames, end) = ended(&said, 1).unwrap();
    assert!(
        frames >= progress.frames && end != End::Replaced,
        "{said:?}"
    );
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
    let refused = renderer.load(&path, scanned, 0, 1).unwrap_err();
    assert_eq!(refused.reason, "file_changed");
    std::fs::rename(&before, &path).unwrap();

    // Loaded, and then something else takes the path: a seek still reads
    // the file that was loaded, to its end.
    renderer.load(&path, scanned, 0, 2).unwrap();
    renderer.play();
    heard(&renderer);
    std::fs::rename(&path, &before).unwrap();
    std::fs::copy(fixture("not-audio.flac"), &path).unwrap();
    renderer.seek(2, 400, 3).unwrap();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 3).is_some());
    assert_eq!(ended(&said, 3), Some((4_800, End::Drained)));

    // Loaded, and then written to where it is: a seek is refused, and the
    // span in hand is still counted.
    renderer.load(&before, facts(&before), 0, 4).unwrap();
    renderer.play();
    heard(&renderer);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&before)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"more"))
        .unwrap();
    assert_eq!(renderer.seek(4, 400, 5).unwrap_err().reason, "file_changed");
    assert!(renderer.progress().frames > 0);
}

/// The tones of every fixture are mono at 16 kHz.
const STEREO_48K: OutputSpec = OutputSpec {
    rate: 48_000,
    channels: 2,
};

/// What a null output of `spec` was handed while the renderer played
/// `tracks` one after the other, each queued to join the one before it,
/// and each track's count.
fn played(spec: OutputSpec, gain: ReplayGain, tracks: &[&str]) -> (Vec<f32>, Vec<u64>) {
    let (mut renderer, tap) = AudioRenderer::tapped(spec, 50).unwrap();
    renderer.set_replay_gain(gain);
    let path = |at: usize| fixture(tracks[at]);
    renderer.load(&path(0), facts(&path(0)), 0, 1).unwrap();
    let offer = |renderer: &mut AudioRenderer, at: usize| {
        if at < tracks.len() {
            let span = at as u64 + 1;
            let format = renderer.queue_next(&path(at), facts(&path(at)), span);
            assert!(format.unwrap().is_some(), "{} was not queued", tracks[at]);
        }
    };
    offer(&mut renderer, 1);
    renderer.play();
    let (mut said, mut counts) = (Vec::new(), Vec::new());
    for at in 0..tracks.len() {
        let span = at as u64 + 1;
        until(&mut renderer, &mut said, |said| ended(said, span).is_some());
        let (frames, end) = ended(&said, span).unwrap();
        counts.push(frames);
        if end != (End::Joined { next: span + 1 }) {
            break;
        }
        offer(&mut renderer, at + 2);
    }
    let samples = tap.lock().unwrap().clone();
    (samples, counts)
}
fn s16(name: &str) -> Vec<f32> {
    decoded_by_ffmpeg(name)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
        .collect()
}

fn f32le(name: &str) -> Vec<f32> {
    decoded_by_ffmpeg(name)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| f32::from_le_bytes(*word))
        .collect()
}

/// The largest step from one sample to the next, in one channel.
fn largest_step(samples: &[f32], channels: usize) -> f32 {
    let one: Vec<f32> = samples.iter().step_by(channels).copied().collect();
    one.windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .fold(0.0, f32::max)
}

#[test]
fn with_gain_and_resampling_left_out_the_output_is_handed_the_decoded_samples_unchanged() {
    // The fixtures' own rate and channels, and a file with no ReplayGain
    // tags asked to play at its track gain: every step is left out, and
    // the null output is handed the reference decode, bit for bit, from
    // its first frame.
    let (samples, counts) = played(AS_IS, ReplayGain::Track, &["a.flac"]);
    assert_eq!(counts, [8000]);
    assert_eq!(samples, s16("a.flac.s16le"));
}

#[test]
fn a_gapless_pair_of_flacs_joins_to_the_frame_into_the_whole_tone() {
    let (samples, counts) = played(AS_IS, ReplayGain::Off, &["g1.flac", "g2.flac"]);
    // Each track counts its own frames, and the join is where the cut was.
    assert_eq!(counts, [7001, 8999]);
    assert_eq!(samples, s16("g.s16le"));
}

#[test]
fn a_gapless_pair_of_mp3s_has_the_encoders_delay_and_padding_trimmed_and_nothing_between() {
    let (samples, counts) = played(AS_IS, ReplayGain::Off, &["g1.mp3", "g2.mp3"]);
    assert_eq!(counts, [7001, 8999]);
    // As ffmpeg decodes each half, one straight after the other.
    let mut reference = f32le("g1.mp3.f32le");
    reference.extend(f32le("g2.mp3.f32le"));
    assert_eq!(samples.len(), reference.len());
    let worst = samples
        .iter()
        .zip(&reference)
        .map(|(sample, expected)| (sample - expected).abs())
        .fold(0.0_f32, f32::max);
    assert!(worst < 1e-4, "the decoders differ by {worst}");
}

#[test]
fn a_gapless_pair_of_vorbis_files_joins_with_no_step_at_the_join() {
    let (samples, counts) = played(AS_IS, ReplayGain::Off, &["g1.ogg", "g2.ogg"]);
    assert_eq!(counts, [7001, 8999]);
    // A 440 Hz tone at 1/8 of full scale moves at most 0.022 from one
    // sample to the next at 16 kHz. A gap, or a sample lost or doubled at
    // the join, would be a step several times that.
    let around = &samples[6900..7100];
    let step = largest_step(around, 1);
    assert!(step < 0.03, "a step of {step} at the join");
}

#[test]
fn a_gapless_pair_resampled_goes_through_one_resampler_and_stays_as_loud_across_the_join() {
    let (samples, counts) = played(STEREO_48K, ReplayGain::Off, &["g1.flac", "g2.flac"]);
    // 7001 frames at 16 kHz are 21,003 at 48 kHz, and the whole is one
    // second.
    assert_eq!(counts, [21_003, 26_997]);
    assert_eq!(samples.len(), 48_000 * 2);
    // Two resamplers, one ending and one starting, would fade out and in
    // at the join; one keeps the tone as smooth there as anywhere.
    let middle = &samples[2 * 2400..2 * 45_600];
    assert!(largest_step(middle, 2) < 0.0075);
    let windows = loudness(&samples, 2, 48_000);
    let steady = &windows[5..95];
    let (quietest, loudest) = steady.iter().fold((f32::MAX, 0.0_f32), |(low, high), rms| {
        (low.min(*rms), high.max(*rms))
    });
    assert!(quietest > 0.98 * loudest, "{quietest} to {loudest}");
}

#[test]
fn a_joined_track_that_cannot_be_read_lets_the_one_before_end_whole_and_then_fails() {
    // Resampled, the join's marker lies past what the shared resampler has
    // handed on: the track before must still be heard to it.
    let scratch = crate::testing::Scratch::new();
    let broken = scratch.path().join("unreadable-g2.flac");
    std::fs::copy(fixture("g2.flac"), &broken).unwrap();
    let (mut renderer, tap) = AudioRenderer::tapped(STEREO_48K, 50).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    let queued = renderer.queue_next(&broken, facts(&broken), 2).unwrap();
    assert!(queued.is_some());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    assert_eq!(ended(&said, 1), Some((21_003, End::Joined { next: 2 })));
    assert!(
        matches!(ended(&said, 2), Some((_, End::Failed(_)))),
        "{said:?}"
    );
    assert_eq!(tap.lock().unwrap().len(), 21_003 * 2);
}

#[test]
fn a_track_offered_once_the_decoder_has_finished_is_refused_and_the_one_in_hand_ends_whole() {
    let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 1).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    renderer.play();
    // The whole first track fits in the ring: its decoder has finished,
    // and the next is loaded once its audio runs out.
    heard(&renderer);
    std::thread::sleep(Duration::from_millis(50));
    let next = fixture("g2.flac");
    assert!(
        renderer
            .queue_next(&next, facts(&next), 2)
            .unwrap()
            .is_none()
    );
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    assert_eq!(ended(&said, 1), Some((7001, End::Drained)));
    assert_eq!(tap.lock().unwrap().len(), 7001);
}

/// Writes a WAV of `frames` frames of a tone, mono at 16 kHz.
fn short_wav(path: &Path, frames: u32) {
    let samples = (0..frames).map(|at| (f64::from(at) * 0.17).sin() * 4096.0);
    wav(
        path,
        16_000,
        1,
        samples
            .map(|sample| sample as i16)
            .flat_map(i16::to_le_bytes),
    );
}

/// Writes a mono WAV said to be at `rate`, of 16-bit samples (`format`
/// 1) or 32-bit floats (`format` 3), from their bytes.
fn wav(path: &Path, rate: u32, format: u16, data: impl Iterator<Item = u8>) {
    let data: Vec<u8> = data.collect();
    let width: u16 = if format == 3 { 4 } else { 2 };
    let length = u32::try_from(data.len()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&format.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&rate.wrapping_mul(u32::from(width)).to_le_bytes());
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&(width * 8).to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&data);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn a_track_shorter_than_the_rings_low_water_still_joins_when_the_next_is_queued_before_play() {
    let scratch = crate::testing::Scratch::new();
    let short = scratch.path().join("short.wav");
    short_wav(&short, 1280);
    let (mut renderer, _) = AudioRenderer::tapped(AS_IS, 50).unwrap();
    renderer.load(&short, facts(&short), 0, 1).unwrap();
    // Loaded and not yet playing: the decoder has all 80 ms of it, and must
    // not give up on a next track before any of it has been heard.
    std::thread::sleep(Duration::from_millis(60));
    let next = fixture("g2.flac");
    assert!(
        renderer
            .queue_next(&next, facts(&next), 2)
            .unwrap()
            .is_some()
    );
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    assert_eq!(ended(&said, 1), Some((1280, End::Joined { next: 2 })));
}

#[test]
fn every_format_the_player_reads_plays_through_the_pipeline_to_its_length() {
    // In their own containers, and ALAC in MP4. AAC in MP4 plays with its
    // encoder's priming and padding, which Symphonia does not trim ("Audio
    // pipeline" in docs/internals/playback.md): 1024 frames before and 192
    // after the 8000 of the tone.
    for (name, frames) in [
        ("a.flac", 8000),
        ("a.mp3", 8000),
        ("a.ogg", 8000),
        ("a.wav", 8000),
        ("a.aiff", 8000),
        ("a.alac.m4a", 8000),
        ("a.m4a", 9216),
    ] {
        let (samples, counts) = played(AS_IS, ReplayGain::Off, &[name]);
        assert_eq!(counts, [frames], "{name}");
        assert_eq!(samples.len(), frames as usize, "{name}");
    }
    // The lossless ones are the FLAC's samples exactly.
    let flac = s16("a.flac.s16le");
    for name in ["a.wav", "a.aiff", "a.alac.m4a"] {
        assert_eq!(played(AS_IS, ReplayGain::Off, &[name]).0, flac, "{name}");
    }
}

#[test]
fn a_file_that_says_it_is_at_a_rate_no_audio_is_recorded_at_is_refused() {
    let scratch = crate::testing::Scratch::new();
    let silence = |frames: usize| std::iter::repeat_n(0_u8, frames * 2);
    // Each would take the resampler an age, or more memory than there is.
    for rate in [1, 7, 900, 1_000_000, 100_000_007] {
        let path = scratch.path().join(format!("{rate}.wav"));
        wav(&path, rate, 1, silence(64));
        let refused = Source::open(&path, 0).unwrap_err();
        assert_eq!(refused.reason, "unsupported_format", "{rate} Hz");
    }
    for rate in [8_000, 11_025, 44_100, 192_000, 768_000] {
        let path = scratch.path().join(format!("{rate}.wav"));
        wav(&path, rate, 1, silence(64));
        assert_eq!(
            Source::open(&path, 0).unwrap().spec(),
            (rate, 1),
            "{rate} Hz"
        );
    }
}

#[test]
fn a_sample_that_is_not_a_number_is_silence_and_one_far_past_full_scale_cannot_stop_the_track() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("float.wav");
    let mut samples: Vec<f32> = (0..8000)
        .map(|at| (at as f32 * 0.17).sin() * 0.25)
        .collect();
    samples[100] = f32::NAN;
    samples[4000] = f32::INFINITY;
    samples[6000] = f32::NEG_INFINITY;
    // As large as a float holds: the resampler's sums would overflow.
    samples[1000..3048].fill(f32::MAX);
    wav(
        &path,
        16_000,
        3,
        samples.iter().flat_map(|sample| sample.to_le_bytes()),
    );
    // As it is, and resampled, which either would stop.
    for (spec, frames) in [(AS_IS, 8000), (STEREO_48K, 24_000)] {
        let (mut renderer, tap) = AudioRenderer::tapped(spec, 50).unwrap();
        renderer.load(&path, facts(&path), 0, 1).unwrap();
        renderer.play();
        let mut said = Vec::new();
        until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
        assert_eq!(ended(&said, 1), Some((frames, End::Drained)), "{spec:?}");
        let heard = tap.lock().unwrap();
        assert!(heard.iter().all(|sample| sample.is_finite()), "{spec:?}");
        if spec == AS_IS {
            assert_eq!(heard[100], 0.0);
            assert_eq!(heard[4000], 0.0);
            assert_eq!(heard[99], samples[99]);
            assert_eq!(heard[1000], 1.0);
        }
    }
}

#[test]
fn replay_gain_tags_are_read_from_vorbis_comments_and_id3() {
    for name in ["rg.flac", "rg.mp3"] {
        let tags = Source::open(&fixture(name), 0).unwrap().tags;
        assert_eq!(tags.track.gain_db, Some(-6.02), "{name}");
        assert_eq!(tags.track.peak, Some(0.125), "{name}");
        assert_eq!(tags.album.gain_db, Some(20.0), "{name}");
        assert_eq!(tags.album.peak, Some(0.125), "{name}");
    }
    assert_eq!(
        Source::open(&fixture("a.flac"), 0).unwrap().tags,
        decode::GainTags::default()
    );
}

/// A copy of a fixture with an APEv2 tag of `items` after its audio, with
/// a header and a footer, as mp3gain and foobar2000 write one.
fn with_ape_tag(name: &str, to: &Path, items: &[(&str, &str)]) {
    let mut body = Vec::new();
    for (key, value) in items {
        body.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&0_u32.to_le_bytes());
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value.as_bytes());
    }
    let size = u32::try_from(body.len() + 32).unwrap();
    let count = u32::try_from(items.len()).unwrap();
    let block = |flags: u32| {
        let mut block = b"APETAGEX".to_vec();
        for word in [2000, size, count, flags] {
            block.extend_from_slice(&u32::to_le_bytes(word));
        }
        block.extend_from_slice(&[0; 8]);
        block
    };
    let mut bytes = std::fs::read(fixture(name)).unwrap();
    // The header says it is one, and both say there is a header.
    bytes.extend(block(0xA000_0000));
    bytes.extend(body);
    bytes.extend(block(0x8000_0000));
    std::fs::write(to, bytes).unwrap();
}

#[test]
fn replay_gain_in_an_ape_tag_after_the_audio_is_applied_and_the_tag_is_not_played() {
    let scratch = crate::testing::Scratch::new();
    let untouched = peak(&s16("a.flac.s16le"));
    for name in ["a.mp3", "a.flac"] {
        let path = scratch.path().join(name);
        let items = [
            ("REPLAYGAIN_TRACK_GAIN", "-6.02 dB"),
            ("REPLAYGAIN_TRACK_PEAK", "0.125"),
        ];
        with_ape_tag(name, &path, &items);
        let tags = Source::open(&path, 0).unwrap().tags;
        assert_eq!(tags.track.gain_db, Some(-6.02), "{name}");
        assert_eq!(tags.track.peak, Some(0.125), "{name}");
        let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 50).unwrap();
        renderer.set_replay_gain(ReplayGain::Track);
        renderer.load(&path, facts(&path), 0, 1).unwrap();
        renderer.play();
        let mut said = Vec::new();
        until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
        assert_eq!(ended(&said, 1), Some((8000, End::Drained)), "{name}");
        let heard = tap.lock().unwrap();
        assert_eq!(heard.len(), 8000, "{name}");
        let level = peak(&heard) / untouched;
        assert!((level - 0.5).abs() < 0.03, "{name}: {level}");
    }
}

#[test]
fn a_gain_tagged_before_the_audio_is_the_one_applied_whatever_a_tag_after_it_says() {
    let scratch = crate::testing::Scratch::new();
    for name in ["rg.flac", "rg.mp3"] {
        let path = scratch.path().join(name);
        with_ape_tag(name, &path, &[("REPLAYGAIN_TRACK_GAIN", "-30 dB")]);
        let tags = Source::open(&path, 0).unwrap().tags;
        assert_eq!(tags.track.gain_db, Some(-6.02), "{name}");
        assert_eq!(tags.album.gain_db, Some(20.0), "{name}");
    }
    // A gain and its peak are measured together: a peak with no gain
    // before the audio is not paired with a gain after it.
    let path = scratch.path().join("peak-only.flac");
    with_ape_tag("rg.flac", &path, &[("REPLAYGAIN_TRACK_GAIN", "+20 dB")]);
    let mut bytes = std::fs::read(&path).unwrap();
    let key = b"REPLAYGAIN_TRACK_GAIN";
    let at = bytes
        .windows(key.len())
        .position(|word| word == key)
        .unwrap();
    bytes[at] = b'X';
    std::fs::write(&path, bytes).unwrap();
    let tags = Source::open(&path, 0).unwrap().tags;
    assert_eq!(
        tags.track,
        decode::GainTag {
            gain_db: Some(20.0),
            peak: None
        }
    );
}

#[test]
fn a_gain_is_held_under_full_scale_by_its_peak_and_never_raised_with_none() {
    let tag = |gain_db, peak| decode::GainTag {
        gain_db: Some(gain_db),
        peak,
    };
    let tags = decode::GainTags {
        track: tag(-6.0, Some(0.5)),
        album: tag(12.0, Some(0.5)),
    };
    assert_eq!(tags.factor(ReplayGain::Off), 1.0);
    assert!((tags.factor(ReplayGain::Track) - 0.501).abs() < 0.001);
    // Twelve decibels would take a peak of one half to two: it is held to a
    // decibel under full scale.
    assert!((tags.factor(ReplayGain::Album) - 1.782).abs() < 0.001);
    let no_peak = decode::GainTags {
        track: tag(6.0, None),
        album: decode::GainTag::default(),
    };
    assert_eq!(no_peak.factor(ReplayGain::Track), 1.0);
    // An album with no album gain plays at its track's.
    assert_eq!(no_peak.factor(ReplayGain::Album), 1.0);
    let quieter = decode::GainTags {
        track: tag(-20.0, None),
        album: decode::GainTag::default(),
    };
    assert!((quieter.factor(ReplayGain::Album) - 0.1).abs() < 1e-6);
    // Tags that would take the gain past what a float holds leave the
    // track as it is.
    let absurd = decode::GainTags {
        track: tag(800.0, Some(1e-40)),
        album: decode::GainTag::default(),
    };
    assert_eq!(absurd.factor(ReplayGain::Track), 1.0);
}

fn peak(samples: &[f32]) -> f32 {
    samples
        .iter()
        .fold(0.0, |peak, sample| peak.max(sample.abs()))
}

#[test]
fn an_album_and_a_mix_both_play_at_the_tagged_level_and_neither_clips() {
    let untouched = peak(&s16("a.flac.s16le"));
    for name in ["rg.flac", "rg.mp3"] {
        // A mix plays each track at its own gain: about half as loud.
        let (track, _) = played(STEREO_48K, ReplayGain::Track, &[name]);
        let level = peak(&track) / untouched;
        assert!((level - 0.5).abs() < 0.03, "{name}: {level}");
        // The album's gain would take it to 1.25, which the peak holds to
        // a decibel under full scale, and no resampled sample goes over.
        let (album, _) = played(STEREO_48K, ReplayGain::Album, &[name]);
        let loudest = peak(&album);
        assert!(loudest <= 0.9, "{name}: {loudest}");
        assert!(loudest > 0.85, "{name}: {loudest}");
    }
}

#[test]
fn a_square_wave_played_at_its_album_gain_and_resampled_never_goes_over_full_scale() {
    // Its tagged peak is its true sample peak, and the gain is held to it,
    // but resampled its corners ring above it.
    let (album, _) = played(STEREO_48K, ReplayGain::Album, &["square.flac"]);
    let loudest = peak(&album);
    assert!(loudest <= 1.0, "{loudest}");
    assert!(loudest > 0.8, "{loudest}");
}

#[test]
fn the_volume_scales_what_is_handed_to_the_output_from_the_first_frame() {
    let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 50).unwrap();
    renderer.set_volume(0.5);
    renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0, 1)
        .unwrap();
    renderer.play();
    until(&mut renderer, &mut Vec::new(), |said| {
        ended(said, 1).is_some()
    });
    let halved: Vec<f32> = s16("a.flac.s16le").iter().map(|s| s * 0.5).collect();
    assert_eq!(*tap.lock().unwrap(), halved);
}

#[test]
fn a_queued_track_let_go_in_time_is_not_played_and_the_first_ends_as_any_track_does() {
    let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 50).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    let next = |name: &str| fixture(name);
    let queued = renderer.queue_next(&next("g2.flac"), facts(&next("g2.flac")), 2);
    assert!(queued.unwrap().is_some());
    // Only one is queued at a time.
    let another = renderer.queue_next(&next("b.flac"), facts(&next("b.flac")), 3);
    assert!(another.unwrap().is_none());
    // Paused, the decoder has not reached the end of the first.
    assert!(renderer.cancel_next());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    // The one let go ends, never heard, and the first ends whole.
    assert_eq!(ended(&said, 2), Some((0, End::Replaced)));
    assert_eq!(ended(&said, 1), Some((7001, End::Drained)));
    assert_eq!(tap.lock().unwrap().len(), 7001);
}

#[test]
fn a_track_the_decoder_has_taken_is_let_go_until_the_output_reaches_it_and_never_heard() {
    let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 1).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    let next = fixture("g2.flac");
    renderer.queue_next(&next, facts(&next), 2).unwrap();
    renderer.play();
    // The first fits in the ring: its decoder has taken the next, and the
    // output is still on the first.
    heard(&renderer);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(renderer.progress().span, 1);
    assert!(renderer.cancel_next());
    // Nothing more joins that ring: what follows loads once it ends.
    let other = fixture("b.flac");
    assert!(
        renderer
            .queue_next(&other, facts(&other), 3)
            .unwrap()
            .is_none()
    );
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    // The first ends whole, where it would have; the second is never heard.
    assert_eq!(ended(&said, 1), Some((7001, End::Drained)));
    assert_eq!(ended(&said, 2), Some((0, End::Replaced)));
    assert!(
        !said
            .iter()
            .any(|event| matches!(event, Event::Began { span: 2, .. }))
    );
    assert_eq!(tap.lock().unwrap().len(), 7001);
    renderer.unload();
    assert!(renderer.drain().is_empty());
}

#[test]
fn a_seek_plays_the_track_it_names_even_once_the_output_has_run_on_into_the_next() {
    let (mut renderer, _) = AudioRenderer::tapped(AS_IS, 50).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    let next = fixture("g2.flac");
    renderer.queue_next(&next, facts(&next), 2).unwrap();
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    // The player decided on the first, which the output has just left.
    renderer.seek(1, 400, 3).unwrap();
    until(&mut renderer, &mut said, |said| ended(said, 3).is_some());
    assert!(matches!(ended(&said, 2), Some((_, End::Replaced))));
    // From 400 ms of the first track to its end: 37 ms at 16 kHz.
    assert_eq!(ended(&said, 3), Some((7001 - 6400, End::Drained)));
}

#[test]
fn a_ten_ms_track_resampled_takes_a_next_track_before_it_plays() {
    let scratch = crate::testing::Scratch::new();
    let short = scratch.path().join("short.wav");
    short_wav(&short, 160);
    let (mut renderer, _) = AudioRenderer::tapped(STEREO_48K, 1).unwrap();
    renderer.load(&short, facts(&short), 0, 1).unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(renderer.progress().frames, 0);
    let next = fixture("g2.flac");
    assert!(
        renderer
            .queue_next(&next, facts(&next), 2)
            .unwrap()
            .is_some(),
        "the unstarted track has already closed its handoff"
    );
}

#[test]
fn a_decoder_waiting_for_a_next_track_lets_go_at_once_when_asked() {
    let scratch = crate::testing::Scratch::new();
    let short = scratch.path().join("short.wav");
    short_wav(&short, 1280);
    let (mut renderer, _) = AudioRenderer::tapped(AS_IS, 1).unwrap();
    renderer.load(&short, facts(&short), 0, 1).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let next = fixture("g2.flac");
    assert!(
        renderer
            .queue_next(&next, facts(&next), 2)
            .unwrap()
            .is_some()
    );
    renderer.cancel_next();
    let before = Instant::now();
    renderer.seek(1, 0, 3).unwrap();
    renderer.unload();
    assert!(before.elapsed() < Duration::from_secs(1));
    renderer.load(&short, facts(&short), 0, 4).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    let before = Instant::now();
    drop(renderer);
    assert!(before.elapsed() < Duration::from_secs(1));
}

#[test]
fn a_short_last_track_paused_then_played_ends_with_nothing_after_it() {
    let scratch = crate::testing::Scratch::new();
    let short = scratch.path().join("short.wav");
    short_wav(&short, 1280);
    let (mut renderer, _) = AudioRenderer::tapped(AS_IS, 1).unwrap();
    renderer.load(&short, facts(&short), 0, 1).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(renderer.drain().is_empty());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    assert_eq!(ended(&said, 1), Some((1280, End::Drained)));
}

#[test]
fn two_ten_ms_tracks_resampled_join_and_end_whole() {
    let scratch = crate::testing::Scratch::new();
    let (a, b) = (scratch.path().join("a.wav"), scratch.path().join("b.wav"));
    short_wav(&a, 160);
    short_wav(&b, 160);
    let (mut renderer, _) = AudioRenderer::tapped(STEREO_48K, 1).unwrap();
    renderer.load(&a, facts(&a), 0, 1).unwrap();
    assert!(renderer.queue_next(&b, facts(&b), 2).unwrap().is_some());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    assert_eq!(ended(&said, 1), Some((480, End::Joined { next: 2 })));
    assert_eq!(ended(&said, 2), Some((480, End::Drained)));
}

#[test]
fn half_a_second_then_ten_ms_resampled_reach_the_join_and_both_end_whole() {
    let scratch = crate::testing::Scratch::new();
    let b = scratch.path().join("b.wav");
    short_wav(&b, 160);
    let (mut renderer, _) = AudioRenderer::tapped(STEREO_48K, 1).unwrap();
    renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0, 1)
        .unwrap();
    assert!(renderer.queue_next(&b, facts(&b), 2).unwrap().is_some());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    assert_eq!(ended(&said, 1), Some((24_000, End::Joined { next: 2 })));
    assert_eq!(ended(&said, 2), Some((480, End::Drained)));
}

#[test]
fn a_next_offered_once_the_decoder_has_read_past_a_short_joined_track_is_refused_and_it_ends_whole()
{
    let scratch = crate::testing::Scratch::new();
    let b = scratch.path().join("b.wav");
    short_wav(&b, 160);
    let (mut renderer, tap) = AudioRenderer::tapped(AS_IS, 1).unwrap();
    renderer
        .load(&fixture("a.flac"), facts(&fixture("a.flac")), 0, 1)
        .unwrap();
    assert!(renderer.queue_next(&b, facts(&b), 2).unwrap().is_some());
    renderer.play();
    let mut said = Vec::new();
    until(&mut renderer, &mut said, |said| ended(said, 1).is_some());
    // No join is promised after a track shorter than the ring: it loads
    // once the short one's audio runs out.
    let next = fixture("g2.flac");
    assert!(
        renderer
            .queue_next(&next, facts(&next), 3)
            .unwrap()
            .is_none()
    );
    until(&mut renderer, &mut said, |said| ended(said, 2).is_some());
    assert_eq!(ended(&said, 2), Some((160, End::Drained)));
    assert_eq!(tap.lock().unwrap().len(), 8000 + 160);
}

#[test]
fn no_mix_of_short_tracks_nexts_pauses_and_seeks_leaves_a_span_without_an_end_or_its_count() {
    use crate::testing::Draws;
    // From one frame to 600 ms, at 16 kHz.
    let lengths = [1_u32, 16, 160, 800, 1600, 4000, 9600];
    let scratch = crate::testing::Scratch::new();
    let paths: Vec<PathBuf> = lengths
        .iter()
        .map(|frames| {
            let path = scratch.path().join(format!("{frames}.wav"));
            short_wav(&path, *frames);
            path
        })
        .collect();
    for seed in 1..=40 {
        let mut draws = Draws::seeded(seed);
        let spec = if draws.below(2) == 0 {
            AS_IS
        } else {
            STEREO_48K
        };
        let per_frame = u64::from(spec.rate / 16_000);
        let (mut renderer, _) = AudioRenderer::tapped(spec, 20).unwrap();
        let queue: Vec<usize> = (0..6)
            .map(|_| usize::try_from(draws.below(7)).unwrap())
            .collect();
        let path = |at: usize| &paths[queue[at]];
        let whole = |at: usize| u64::from(lengths[queue[at]]) * per_frame;
        // What each span is to count when it ends by itself.
        let mut expected = std::collections::HashMap::new();
        // Every span the renderer took, and the last number given.
        let (mut given, mut span) = (vec![1], 1);
        let (mut at, mut current, mut playing) = (0, span, true);
        expected.insert(current, whole(0));
        renderer.load(path(0), facts(path(0)), 0, current).unwrap();
        // Often as the player does: the next as soon as a track is in hand.
        let offer = |renderer: &mut AudioRenderer,
                     at: usize,
                     span: &mut u64,
                     given: &mut Vec<u64>,
                     draws: &mut Draws| {
            if at + 1 < queue.len() && draws.below(3) > 0 {
                *span += 1;
                let next = path(at + 1);
                if renderer
                    .queue_next(next, facts(next), *span)
                    .unwrap()
                    .is_some()
                {
                    given.push(*span);
                    return Some(*span);
                }
            }
            None
        };
        let mut queued = offer(&mut renderer, at, &mut span, &mut given, &mut draws);
        if let Some(queued) = queued {
            expected.insert(queued, whole(1));
        }
        renderer.play();
        let (mut meddling, mut alone) = (draws.below(8), None::<Instant>);
        let mut said = Vec::new();
        loop {
            let seen = said.len();
            said.extend(renderer.drain());
            let mut moved = false;
            for event in said.get(seen..).unwrap_or_default().to_vec() {
                let Event::Ended {
                    span: ended,
                    frames,
                    end,
                    ..
                } = event
                else {
                    continue;
                };
                assert!(!matches!(end, End::Failed(_)), "seed {seed}: {end:?}");
                if ended != current || end == End::Replaced {
                    continue;
                }
                assert_eq!(
                    frames, expected[&ended],
                    "seed {seed}: span {ended} of {queue:?}"
                );
                at += 1;
                moved = true;
                match end {
                    End::Joined { next } => {
                        assert_eq!(
                            Some(next),
                            queued,
                            "seed {seed}: joined what was not queued"
                        );
                        current = next;
                    }
                    _ => {
                        assert_eq!(queued, None, "seed {seed}: ended alone with a next queued");
                        if at == queue.len() {
                            break;
                        }
                        span += 1;
                        (current, playing) = (span, true);
                        given.push(current);
                        expected.insert(current, whole(at));
                        renderer
                            .load(path(at), facts(path(at)), 0, current)
                            .unwrap();
                        renderer.play();
                    }
                }
                queued = offer(&mut renderer, at, &mut span, &mut given, &mut draws);
                if let Some(queued) = queued {
                    expected.insert(queued, whole(at + 1));
                }
            }
            if at == queue.len() {
                break;
            }
            if moved {
                (meddling, alone) = (draws.below(8), None);
                continue;
            }
            if meddling == 0 {
                if !playing {
                    renderer.play();
                    playing = true;
                }
                let since = *alone.get_or_insert_with(Instant::now);
                assert!(
                    since.elapsed() < Duration::from_secs(2),
                    "seed {seed}: span {current} of {queue:?}, left alone, has no end: {said:?}"
                );
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            meddling -= 1;
            match draws.below(10) {
                0 if playing => {
                    renderer.pause();
                    playing = false;
                }
                0 => {
                    renderer.play();
                    playing = true;
                }
                1 => {
                    let to = draws.below(u64::from(lengths[queue[at]]) / 16 + 2);
                    span += 1;
                    renderer.seek(current, to, span).unwrap();
                    current = span;
                    given.push(current);
                    queued = None;
                    let rest = u64::from(lengths[queue[at]]).saturating_sub(to * 16);
                    expected.insert(current, rest * per_frame);
                }
                2 | 3 if queued.is_none() => {
                    queued = offer(&mut renderer, at, &mut span, &mut given, &mut draws);
                    if let Some(queued) = queued {
                        expected.insert(queued, whole(at + 1));
                    }
                }
                4 if queued.is_some() => {
                    if renderer.cancel_next() {
                        queued = None;
                    }
                }
                _ => std::thread::sleep(Duration::from_millis(draws.below(5))),
            }
        }
        // Every span ended once.
        renderer.unload();
        said.extend(renderer.drain());
        let mut ends: Vec<u64> = said
            .iter()
            .filter_map(|event| match event {
                Event::Ended { span, .. } => Some(*span),
                Event::Began { .. } => None,
            })
            .collect();
        ends.sort_unstable();
        assert_eq!(ends, given, "seed {seed}");
    }
}

/// The player over a renderer, with an album of tone WAVs of a few seconds
/// in a library that has read them as they are.
struct Pipeline {
    _scratch: crate::testing::Scratch,
    root: PathBuf,
    album: Vec<crate::playback::tests::Track>,
    server: crate::ServerStore,
    library: crate::LibraryStore,
    player: crate::playback::Player,
    appearances: Vec<crate::Id>,
    commands: u8,
}

impl Pipeline {
    fn new(lengths_ms: &[i64], renderer: Box<dyn Renderer>) -> Self {
        use std::sync::Arc;

        use crate::playback::player::{Player, PlayerSetup};
        use crate::playback::tests::{Track, catalog};
        use crate::{
            LibraryStore, OsEntropy, ServerStore, SystemClock, SystemTimekeeper, WriteRequest,
        };
        const FILES: [&str; 3] = ["Album/01.wav", "Album/02.wav", "Album/03.wav"];
        let album: Vec<Track> = lengths_ms
            .iter()
            .zip(FILES)
            .map(|(length_ms, file)| Track {
                title: file,
                length_ms: *length_ms,
                file,
            })
            .collect();
        let scratch = crate::testing::Scratch::new();
        let root = scratch.path().join("music");
        std::fs::create_dir_all(root.join("Album")).unwrap();
        for track in &album {
            let frames = u32::try_from(track.length_ms * 16).unwrap();
            short_wav(&root.join(track.file), frames);
        }
        let (clock, entropy) = (Arc::new(SystemClock), Arc::new(OsEntropy));
        let server = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
        let (identity, _) = server.ensure_identity("desk").unwrap();
        let request = |method| WriteRequest {
            method,
            command: None,
            client: None,
        };
        let (library, _) = LibraryStore::create_in_memory(
            &identity,
            request("library.create"),
            |_| String::new(),
            clock.clone(),
            entropy.clone(),
        )
        .unwrap();
        server.claim_library(library.library_id()).unwrap();
        server.add_root("music", root.to_str().unwrap()).unwrap();
        let (appearances, items) = catalog(&library, identity.server_id, &album);
        // The library read the files as they are.
        let read: Vec<(i64, FileFacts)> = items
            .iter()
            .zip(&album)
            .map(|(item, track)| (*item, facts(&root.join(track.file))))
            .collect();
        library
            .write(request("test.facts"), move |write| {
                for (item, facts) in &read {
                    write.connection().execute(
                        "UPDATE source_item SET size_bytes = ?1, mtime_ms = ?2 WHERE id = ?3",
                        (
                            i64::try_from(facts.size_bytes).unwrap(),
                            facts.mtime_ms,
                            item,
                        ),
                    )?;
                }
                Ok(String::new())
            })
            .unwrap();
        let setup = PlayerSetup {
            server_id: identity.server_id,
            run: identity.server_id,
            clock,
            timekeeper: Arc::new(SystemTimekeeper),
            entropy,
            checkpoint_every_ms: crate::playback::CHECKPOINT_EVERY_MS,
            fail_at_position_ms: None,
            fail_saves_after_ms: None,
        };
        let (player, _) = Player::restore(setup, renderer, &server, Some(&library)).unwrap();
        Self {
            _scratch: scratch,
            root,
            album,
            server,
            library,
            player,
            appearances,
            commands: 0,
        }
    }

    fn request(&mut self, method: &'static str) -> crate::playback::Request {
        self.commands += 1;
        crate::playback::Request {
            command: crate::Command {
                id: crate::Id::from_bytes([self.commands; 16]),
                fingerprint: crate::Fingerprint::of(method, &[self.commands]),
            },
            method,
            answer: Box::new(|_| String::new()),
        }
    }

    fn play(&mut self) {
        let request = self.request("playback.play");
        let what = crate::playback::PlayTracks {
            tracks: self.appearances.clone(),
            start: Some(0),
            context: (crate::playback::ContextKind::Album, None),
            actor: None,
            shuffle: None,
            known: false,
        };
        let library = Some(&self.library);
        self.player
            .play(request, what, &self.server, library)
            .unwrap();
    }

    /// Ticks every few milliseconds until `done` says so.
    fn until(&mut self, done: impl Fn(&crate::playback::PlayerState) -> bool) {
        self.ticking(5, done);
    }

    /// Ticks every `ms` until `done` says so.
    fn ticking(&mut self, ms: u64, done: impl Fn(&crate::playback::PlayerState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !done(&self.player.state()) {
            assert!(Instant::now() < deadline, "{:?}", self.player.state());
            std::thread::sleep(Duration::from_millis(ms));
            self.player.tick(&self.server, Some(&self.library)).unwrap();
        }
    }

    /// Every listen staged, oldest first.
    fn staged(&self) -> Vec<crate::playback::store::Staged> {
        let ids = self
            .server
            .read(|connection| {
                let mut statement =
                    connection.prepare("SELECT listen_id FROM listen_staging ORDER BY id")?;
                let rows = statement.query_map([], |row| row.get::<_, [u8; 16]>(0))?;
                Ok(rows.collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap();
        ids.into_iter()
            .map(|id| {
                let id = crate::Id::from_bytes(id);
                self.server.staged_listen(id).unwrap().unwrap()
            })
            .collect()
    }
}

#[test]
fn an_album_played_through_the_real_pipeline_is_its_samples_and_each_listen_begins_as_the_last_ended()
 {
    use crate::playback::Status;
    // Three tracks of a few seconds, played at four times real time: each
    // is joined in time, as in use.
    let (renderer, tap) = AudioRenderer::tapped(AS_IS, 4).unwrap();
    let mut rig = Pipeline::new(&[2_000, 1_500, 2_500], Box::new(renderer));
    rig.play();
    rig.until(|state| state.status == Status::Stopped);

    // The output was handed each file's samples, one after the other.
    let samples: Vec<f32> = rig
        .album
        .iter()
        .flat_map(|track| {
            let bytes = std::fs::read(rig.root.join(track.file)).unwrap();
            let data = bytes.get(44..).unwrap().to_vec();
            data.as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(*tap.lock().unwrap(), samples);
    // A listen each, whole, and each begins where the one before ended:
    // the same stamp, read through each clock, to a millisecond.
    let staged = rig.staged();
    assert_eq!(staged.len(), 3);
    for (listen, track) in staged.iter().zip(&rig.album) {
        let ended = listen.ended.as_ref().unwrap();
        assert_eq!(
            (listen.heard.ms_played, ended.end_reason.as_str()),
            (track.length_ms, "finished")
        );
    }
    for pair in staged.windows(2) {
        let (before, after) = (pair[0].ended.as_ref().unwrap(), &pair[1].started);
        assert!(
            (after.started_at_ms - before.ended_at_ms).abs() <= 1,
            "{pair:?}"
        );
        let mono = before.ended_mono_ms.unwrap();
        assert!((after.started_mono_ms - mono).abs() <= 1, "{pair:?}");
        assert_eq!(after.start_reason.as_str(), "auto_advance");
    }
}

#[test]
fn a_song_taken_out_of_up_next_in_the_last_half_second_is_never_heard_through_the_real_pipeline() {
    use crate::playback::{Order, Status};
    // At real time, ticking as the server does. The output holds 300 ms
    // before the first ends, so that however slowly the machine runs, the
    // second is taken out there; and only once the ring is full, so its
    // decoder is 200 ms into the second by then.
    let (renderer, tap, hold) = AudioRenderer::held(AS_IS, 1, 700 * 16).unwrap();
    let full = usize::try_from(AS_IS.rate * super::RING_MS / 1000).unwrap() * AS_IS.channels;
    let mut rig = Pipeline::new(&[1_000, 1_000, 1_000], Box::new(renderer));
    rig.play();
    rig.ticking(50, |_| hold.waiting() == Some(full));
    // Held, the output stands still: one more tick sees where it stands.
    rig.player.tick(&rig.server, Some(&rig.library)).unwrap();
    let [first, second] = [0, 1].map(|at| rig.player.window(Order::Queue, at, 1).entries[0].0);
    let state = rig.player.state();
    assert_eq!(state.current.map(|now| now.entry_id), Some(first));
    assert!(
        (700..800).contains(&state.position_ms),
        "{}",
        state.position_ms
    );
    let request = rig.request("queue.remove");
    let (server, library) = (&rig.server, Some(&rig.library));
    rig.player
        .remove(request, None, &[second], server, library)
        .unwrap();
    hold.let_go();
    rig.ticking(50, |state| state.status == Status::Stopped);
    // The first played whole, the second never, and the third after it as
    // any track does.
    let samples = |at: usize| -> Vec<f32> {
        let bytes = std::fs::read(rig.root.join(rig.album[at].file)).unwrap();
        bytes
            .get(44..)
            .unwrap()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
            .collect()
    };
    let expected: Vec<f32> = samples(0).into_iter().chain(samples(2)).collect();
    assert_eq!(*tap.lock().unwrap(), expected);
    let [one, three] = rig.staged().try_into().unwrap();
    assert_eq!(
        (one.ended.unwrap().end_reason.as_str(), one.heard.ms_played),
        ("finished", 1_000)
    );
    assert_eq!(three.started.start_reason.as_str(), "auto_advance");
    assert_eq!(three.heard.ms_played, 1_000);
}

#[test]
fn an_output_that_goes_away_mid_track_saves_where_it_was_heard_and_play_goes_on_from_there() {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use crate::playback::Status;
    // The output the player opens on demand, already open, so that the
    // test can take its device away.
    let output = Output::Null { speed: 2 };
    let opened = AudioRenderer::open(output).unwrap();
    let device = Arc::clone(&opened.shared);
    let renderer = super::OnDemand {
        output,
        opened: Some(opened),
        gain: ReplayGain::Off,
        volume: 1.0,
        said: Vec::new(),
    };
    let mut rig = Pipeline::new(&[2_000], Box::new(renderer));
    rig.play();
    rig.until(|state| state.position_ms >= 500);
    // The device goes away: the output is let go, and the player waits
    // where the track was heard to.
    device.lost.store(true, Ordering::SeqCst);
    rig.until(|state| state.status == Status::Paused);
    let saved = rig.server.playback_session().unwrap().position_ms;
    let [lost] = rig.staged().try_into().unwrap();
    let ended = lost.ended.clone().unwrap();
    assert_eq!(ended.end_reason.as_str(), "output_lost");
    assert_eq!(
        (ended.end_position_ms, lost.heard.ms_played),
        (saved, saved)
    );
    assert!((500..1_500).contains(&saved), "saved at {saved} of 2000");
    // Play opens an output again and carries on from there, as a listen
    // that continues the one cut off.
    let request = rig.request("playback.resume");
    let (server, library) = (&rig.server, Some(&rig.library));
    rig.player.resume(request, server, library).unwrap();
    rig.until(|state| state.status == Status::Stopped);
    let [_, resumed] = rig.staged().try_into().unwrap();
    assert_eq!(resumed.started.continues, Some(lost.started.listen_id));
    assert_eq!(resumed.started.start_position_ms, saved);
    assert!(
        (saved + resumed.heard.ms_played - 2_000).abs() <= 1,
        "{resumed:?}"
    );
}

#[test]
fn a_next_the_output_has_run_into_cannot_be_let_go_before_the_player_has_heard_of_it() {
    let (mut renderer, _) = AudioRenderer::tapped(AS_IS, 50).unwrap();
    renderer
        .load(&fixture("g1.flac"), facts(&fixture("g1.flac")), 0, 1)
        .unwrap();
    let next = fixture("g2.flac");
    renderer.queue_next(&next, facts(&next), 2).unwrap();
    renderer.play();
    // The output is on the second, and nothing it said has been drained.
    let deadline = Instant::now() + Duration::from_secs(10);
    while renderer.progress().span != 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!renderer.cancel_next());
    // The join is still there for the player to take in.
    let said = renderer.drain();
    assert_eq!(ended(&said, 1), Some((7001, End::Joined { next: 2 })));
}

#[test]
fn a_file_none_of_whose_packets_decodes_is_unreadable_and_not_a_track_that_ended() {
    let scratch = crate::testing::Scratch::new();
    let mut bytes = std::fs::read(fixture("a.m4a")).unwrap();
    // Every byte of the audio itself is garbage; the container is whole.
    let mdat = bytes.windows(4).position(|w| w == b"mdat").unwrap();
    let size = u32::from_be_bytes(bytes[mdat - 4..mdat].try_into().unwrap()) as usize;
    let end = (mdat - 4 + size).min(bytes.len());
    bytes[mdat + 4..end].fill(0x01);
    let path = scratch.path().join("garbled.m4a");
    std::fs::write(&path, &bytes).unwrap();
    let mut source = Source::open(&path, 0).unwrap();
    // Every packet is tried and passed over, and then it is said.
    let failed = source.next().unwrap_err();
    assert_eq!(failed.reason, "unreadable");
    assert!(source.bad_packets > 0);
    // A file with no audio in it at all is no different.
    let mut empty = Source::open(&fixture("no-packets.m4a"), 0).unwrap();
    assert_eq!(empty.next().unwrap_err().reason, "unreadable");
}
