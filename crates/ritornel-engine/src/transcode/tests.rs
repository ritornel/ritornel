use super::*;
use ritornel_domain::selection::Candidate;

pub(super) fn pcm() -> Pcm {
    Pcm::open(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/a.flac"),
        None,
    )
    .unwrap()
}
fn profile(target: Target) -> Profile {
    Profile {
        target,
        dsp: Dsp::default(),
    }
}
pub(super) fn mp3() -> Profile {
    profile(Target::Mp3 { mode: Mp3::V2 })
}
pub(super) fn opus() -> Profile {
    profile(Target::Opus {
        bitrate_kbps: 160,
        complexity: 9,
        vbr: Vbr::Variable,
    })
}

#[test]
fn canonical_profiles_resolve_defaults_and_reject_unknown_or_invalid_settings() {
    let minimal: Profile =
        serde_json::from_str(r#"{"target":{"mode":"v2","codec":"mp3"}}"#).unwrap();
    assert_eq!(
        minimal.canonical_json().unwrap(),
        r#"{"target":{"codec":"mp3","mode":"v2"},"dsp":{"replay_gain":"off","max_rate_hz":null}}"#
    );
    for target in [
        Target::Mp3 {
            mode: Mp3::Cbr(196),
        },
        Target::Opus {
            bitrate_kbps: 0,
            complexity: 8,
            vbr: Vbr::Constrained,
        },
        Target::Opus {
            bitrate_kbps: 128,
            complexity: 11,
            vbr: Vbr::HardCbr,
        },
    ] {
        assert!(profile(target).canonical_json().is_err());
    }
    for json in [
        r#"{"target":{"codec":"mp3","mode":"v2","speed":0}}"#,
        r#"{"target":{"codec":"flac"},"extra":1}"#,
        r#"{"target":{"codec":"flac","bitrate_kbps":128}}"#,
    ] {
        assert!(serde_json::from_str::<Profile>(json).is_err());
    }
    let dsp = Dsp {
        max_rate_hz: Some(48000),
        ..Dsp::default()
    };
    assert!(
        Profile {
            target: Target::Flac {},
            dsp
        }
        .validate()
        .is_err()
    );
    assert!(
        Dsp {
            max_rate_hz: Some(0),
            ..dsp
        }
        .validate()
        .is_err()
    );
}

#[test]
fn fake_encoding_and_key_track_the_inputs_and_preserve_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encoded");
    let encode = |p| {
        FakeEncoder
            .encode(
                pcm(),
                p,
                blake3::hash(b"test"),
                &path,
                &AtomicBool::new(false),
            )
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        bytes
    };
    let first = encode(mp3());
    assert_eq!(first, encode(mp3()));
    assert_ne!(first, encode(opus()));
    let mut id = FakeEncoder.probe().unwrap().unwrap();
    let asset = AudioAsset {
        scheme: 1,
        hash: blake3::hash(b"audio"),
    };
    let key = id.derivative_key(asset, mp3(), pcm().gain_tags()).unwrap();
    assert_ne!(
        key,
        id.derivative_key(asset, opus(), pcm().gain_tags()).unwrap()
    );
    id.version.push('2');
    assert_ne!(
        key,
        id.derivative_key(asset, mp3(), pcm().gain_tags()).unwrap()
    );
    assert_eq!(
        FakeEncoder
            .encode(
                pcm(),
                mp3(),
                blake3::hash(b"test"),
                &path,
                &AtomicBool::new(true)
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
    assert!(!path.exists());
    std::fs::write(&path, b"master").unwrap();
    assert!(
        FakeEncoder
            .encode(
                pcm(),
                mp3(),
                blake3::hash(b"test"),
                &path,
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(std::fs::read(path).unwrap(), b"master");
    assert_eq!(dir.path().read_dir().unwrap().count(), 1);
}

#[test]
fn baked_keys_use_the_selected_gain_tags_even_for_identical_audio() {
    let tagged = Pcm::open(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/audio/rg.flac"),
        None,
    )
    .unwrap()
    .gain_tags();
    let plain = pcm().gain_tags();
    let id = FakeEncoder.probe().unwrap().unwrap();
    let key = |tags, mode| {
        let mut profile = mp3();
        profile.dsp.replay_gain = mode;
        id.derivative_key(
            AudioAsset {
                scheme: 1,
                hash: blake3::hash(b"same audio"),
            },
            profile,
            tags,
        )
        .unwrap()
    };
    assert_eq!(key(tagged, ReplayGain::Off), key(plain, ReplayGain::Off));
    assert_ne!(
        key(tagged, ReplayGain::Track),
        key(plain, ReplayGain::Track)
    );
    assert_ne!(
        key(tagged, ReplayGain::Track),
        key(tagged, ReplayGain::Album)
    );
    let new_peak = GainTags {
        track: GainTag {
            peak: Some(0.75),
            ..tagged.track
        },
        ..tagged
    };
    assert_ne!(
        key(tagged, ReplayGain::Track),
        key(new_peak, ReplayGain::Track)
    );
    assert_eq!(
        key(tagged, ReplayGain::Album),
        key(new_peak, ReplayGain::Album)
    );
}

#[test]
fn source_policy_uses_the_best_eligible_master() {
    let copy = |asset, lossless, missing| Candidate {
        asset,
        source_item: asset,
        lossless,
        sample_rate_hz: 44100,
        bit_depth: None,
        bitrate_kbps: None,
        missing,
    };
    let copies = [
        (copy(1, false, false), "mp3"),
        (copy(2, false, false), "aac"),
        (copy(3, true, false), "alac"),
        (copy(4, true, false), "flac"),
        (copy(0, true, true), "flac"),
    ];
    let choose = |count, p, accepted, allow| {
        choose_source(&copies[..count], p, |codec| codec == accepted, allow).unwrap()
    };
    let invalid = profile(Target::Mp3 { mode: Mp3::Cbr(17) });
    assert!(choose_source(&copies, invalid, |_| true, true).is_err());
    use SourceChoice::*;
    assert_eq!(choose(0, mp3(), "mp3", true), Blocked);
    assert_eq!(choose(5, mp3(), "mp3", true), Encode(&copies[2].0));
    assert_eq!(
        choose(5, profile(Target::Flac {}), "flac", false),
        PassThrough(&copies[3].0)
    );
    assert_eq!(choose(3, profile(Target::Flac {}), "flac", true), Blocked);
    assert_eq!(choose(2, mp3(), "aac", true), PassThrough(&copies[1].0));
    assert_eq!(choose(2, opus(), "opus", false), Blocked);
    assert_eq!(choose(2, opus(), "opus", true), Encode(&copies[0].0));
    assert_eq!(choose(2, profile(Target::Flac {}), "flac", true), Blocked);
}

#[test]
fn installed_encoders_write_gapless_audio() {
    let dir = tempfile::tempdir().unwrap();
    let stereo = dir.path().join("stereo.wav");
    std::fs::write(&stereo, crate::testing::cd_wav(75, 8)).unwrap();
    for (backend, profile, suffix) in [
        (&LameEncoder as &dyn Encoder, mp3(), "mp3"),
        (
            &LameEncoder,
            profile(Target::Mp3 {
                mode: Mp3::Cbr(128),
            }),
            "mp3",
        ),
        (&OpusEncoder, opus(), "opus"),
    ] {
        if backend.probe().unwrap().is_none() {
            continue;
        }
        for (input, channels, ms) in [
            (pcm(), 1, 500),
            (Pcm::open(&stereo, None).unwrap(), 2, 1000),
        ] {
            let path = dir.path().join(format!("encoded.{suffix}"));
            backend
                .encode(
                    input,
                    profile,
                    blake3::hash(b"test"),
                    &path,
                    &AtomicBool::new(false),
                )
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            if suffix == "mp3" {
                assert!(bytes.windows(4).any(|b| b == b"Xing" || b == b"Info"));
                let output = Pcm::open(&path, None).unwrap();
                let spec = output.spec(profile.dsp).unwrap();
                assert_eq!(spec.channels, channels);
                let mut frames = 0;
                output
                    .stream(profile.dsp, &AtomicBool::new(false), &mut |block| {
                        frames += block.len() / channels;
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(frames as u32, spec.rate * ms / 1000);
            } else {
                let header = bytes.windows(8).position(|s| s == b"OpusHead").unwrap();
                let skip = u16::from_le_bytes(bytes[header + 10..header + 12].try_into().unwrap());
                let last = bytes.windows(4).rposition(|s| s == b"OggS").unwrap();
                let granule = u64::from_le_bytes(bytes[last + 6..last + 14].try_into().unwrap());
                assert_eq!(granule - u64::from(skip), u64::from(ms) * 48);
            }
            std::fs::remove_file(&path).unwrap();
            if suffix == "opus" {
                let input = if channels == 1 {
                    pcm()
                } else {
                    Pcm::open(&stereo, None).unwrap()
                };
                backend
                    .encode(
                        input,
                        profile,
                        blake3::hash(b"test"),
                        &path,
                        &AtomicBool::new(false),
                    )
                    .unwrap();
                assert_eq!(bytes, std::fs::read(&path).unwrap());
                std::fs::remove_file(path).unwrap();
            }
        }
    }
}
