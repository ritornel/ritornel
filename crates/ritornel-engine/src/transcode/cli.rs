//! The two raw PCM command lines share sample packing and process lifetime.

use super::PcmSpec;
use super::{Encoder, EncoderId, Mp3, Pcm, Profile, Target, Vbr, check_cancel, publish, staged};
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct LameEncoder;
#[derive(Debug)]
pub struct OpusEncoder;

impl Encoder for LameEncoder {
    fn probe(&self) -> io::Result<Option<EncoderId>> {
        probe("lame")
    }
    fn supports(&self, profile: Profile) -> bool {
        matches!(profile.target, Target::Mp3 { .. }) && profile.validate().is_ok()
    }
    fn encode(
        &self,
        pcm: Pcm,
        profile: Profile,
        _key: blake3::Hash,
        output: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let Target::Mp3 { mode } = profile.target else {
            return Err(io::Error::other("expected MP3 profile"));
        };
        profile.validate()?;
        // Select an MP3 rate within the ceiling and let our shared DSP do
        // the conversion. LAME otherwise chooses its own rate and can
        // silently lower a requested CBR bitrate for MPEG-2 or MPEG-2.5.
        let ceiling = pcm.spec(profile.dsp)?.rate;
        let rate = mp3_rate(ceiling)?;
        let mut profile = profile;
        profile.dsp.max_rate_hz = Some(rate);
        let args = lame_args(pcm.spec(profile.dsp)?, mode)?;
        encode(
            Command::new("lame").args(args),
            pcm,
            profile,
            output,
            cancel,
        )
    }
}

fn mp3_rate(ceiling: u32) -> io::Result<u32> {
    [48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000]
        .into_iter()
        .find(|rate| *rate <= ceiling)
        .ok_or_else(|| io::Error::other("MP3 requires at least 8000 Hz"))
}

fn lame_args(spec: PcmSpec, mode: Mp3) -> io::Result<Vec<String>> {
    let channels = match spec.channels {
        1 => "m",
        2 => "j",
        _ => return Err(io::Error::other("MP3 requires mono or stereo PCM")),
    };
    let mut args = [
        "--silent",
        "-r",
        "--signed",
        "--little-endian",
        "--bitwidth",
        "24",
        "-m",
        channels,
        "-s",
    ]
    .map(String::from)
    .to_vec();
    let rate = (f64::from(spec.rate) / 1000.0).to_string();
    args.extend([rate.clone(), "--resample".into(), rate]);
    // -T also writes a seekable Info header for CBR, including encoder delay.
    args.push("-T".into());
    match mode {
        Mp3::V0 => args.push("-V0".into()),
        Mp3::V2 => args.push("-V2".into()),
        Mp3::Cbr(kbps) if (spec.rate < 16000 && kbps > 64) || (spec.rate < 32000 && kbps > 160) => {
            return Err(io::Error::other(
                "MP3 bitrate exceeds the limit at this sample rate",
            ));
        }
        Mp3::Cbr(kbps) => args.extend(["--cbr".into(), "-b".into(), kbps.to_string()]),
    }
    Ok(args)
}

impl Encoder for OpusEncoder {
    fn probe(&self) -> io::Result<Option<EncoderId>> {
        probe("opusenc")
    }
    fn supports(&self, profile: Profile) -> bool {
        matches!(profile.target, Target::Opus { .. }) && profile.validate().is_ok()
    }
    fn encode(
        &self,
        pcm: Pcm,
        profile: Profile,
        key: blake3::Hash,
        output: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        // opusenc chooses a random stream serial by default. Each derivative
        // is a single Ogg logical stream, with a reproducible serial.
        let [a, b, c, d, ..] = *key.as_bytes();
        let serial = u32::from_le_bytes([a, b, c, d]) & 0x7fff_ffff;
        let args = opus_args(pcm.spec(profile.dsp)?, profile, serial)?;
        encode(
            Command::new("opusenc").args(args),
            pcm,
            profile,
            output,
            cancel,
        )
    }
}

fn opus_args(spec: PcmSpec, profile: Profile, serial: u32) -> io::Result<Vec<String>> {
    profile.validate()?;
    let Target::Opus {
        bitrate_kbps,
        complexity,
        vbr,
    } = profile.target
    else {
        return Err(io::Error::other("expected Opus profile"));
    };
    // Raw multichannel input needs an explicit channel layout conversion.
    if !matches!(spec.channels, 1 | 2) {
        return Err(io::Error::other("Opus requires mono or stereo PCM"));
    }
    let mode = match vbr {
        Vbr::Variable => "--vbr",
        Vbr::Constrained => "--cvbr",
        Vbr::HardCbr => "--hard-cbr",
    };
    Ok(vec![
        "--quiet".into(),
        "--serial".into(),
        serial.to_string(),
        "--raw".into(),
        "--raw-bits".into(),
        "24".into(),
        "--raw-endianness".into(),
        "0".into(),
        "--raw-rate".into(),
        spec.rate.to_string(),
        "--raw-chan".into(),
        spec.channels.to_string(),
        "--bitrate".into(),
        bitrate_kbps.to_string(),
        "--comp".into(),
        complexity.to_string(),
        mode.into(),
    ])
}

fn version(family: &'static str, banner: &str) -> io::Result<EncoderId> {
    let mut words = banner.split_whitespace();
    let value = match family {
        "lame" if words.next() == Some("LAME") => words.skip_while(|w| *w != "version").nth(1),
        "opusenc" if words.next() == Some("opusenc") => {
            // opusenc from opus-tools 0.2 (using libopus 1.5.2)
            let tool = words.find(|w| w.starts_with(|c: char| c.is_ascii_digit()));
            if let Some(tool) = tool
                && let Some(lib) = words.skip_while(|w| *w != "libopus").nth(1)
                && lib.starts_with(|c: char| c.is_ascii_digit())
            {
                return Ok(EncoderId {
                    family,
                    version: format!("{tool}/{}", lib.trim_end_matches(')')),
                });
            }
            None
        }
        _ => None,
    };
    let version = value
        .filter(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .ok_or_else(|| io::Error::other(format!("unrecognized {family} version")))?;
    Ok(EncoderId {
        family,
        version: version.to_owned(),
    })
}

fn probe(family: &'static str) -> io::Result<Option<EncoderId>> {
    let mut banner = tempfile::tempfile()?;
    let child = Command::new(family)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(banner.try_clone()?)
        .stderr(banner.try_clone()?)
        .spawn();
    let mut child = match child {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    let until = Instant::now() + Duration::from_secs(2);
    let status = wait(&mut child, || {
        if Instant::now() >= until {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "encoder probe timed out",
            ))
        } else {
            Ok(())
        }
    })?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{family} probe exited with {status}"
        )));
    }
    let banner = diagnostic(&mut banner)?;
    version(family, &banner).map(Some)
}

fn diagnostic(file: &mut File) -> io::Result<String> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(4096).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

fn wait(child: &mut Child, check: impl Fn() -> io::Result<()>) -> io::Result<ExitStatus> {
    loop {
        match check().and_then(|()| child.try_wait()) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
}

pub(super) fn encode(
    cmd: &mut Command,
    pcm: Pcm,
    profile: Profile,
    output: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    check_cancel(cancel)?;
    let stage = staged(output)?;
    let mut stderr = tempfile::tempfile()?;
    let mut child = cmd
        .arg("-")
        .arg(std::path::absolute(stage.path())?)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(stderr.try_clone()?)
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing encoder stdin"))?;
    // The child can stall with a full pipe. Its waiter must be able to kill
    // it even while the decoding thread is blocked writing that pipe.
    let failed = AtomicBool::new(false);
    let (sent, status) = std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            wait(&mut child, || {
                check_cancel(cancel)?;
                check_cancel(&failed)
            })
        });
        let mut bytes = Vec::new();
        let sent = pcm.stream(profile.dsp, cancel, &mut |samples| {
            bytes.clear();
            for sample in samples {
                // The raw integer input saturates at full scale, including
                // overshoot from resampling a source with no headroom.
                let value = (sample * 8_388_608.0)
                    .round()
                    .clamp(-8_388_608.0, 8_388_607.0) as i32;
                let [a, b, c, _] = value.to_le_bytes();
                bytes.extend_from_slice(&[a, b, c]);
            }
            input.write_all(&bytes)
        });
        drop(input);
        if sent
            .as_ref()
            .is_err_and(|e| e.kind() != io::ErrorKind::BrokenPipe)
        {
            failed.store(true, std::sync::atomic::Ordering::Release);
        }
        (
            sent,
            waiter
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("encoder waiter panicked"))),
        )
    });
    check_cancel(cancel)?;
    if let Ok(status) = status
        && !status.success()
    {
        return Err(io::Error::other(format!(
            "encoder exited with {status}: {}",
            diagnostic(&mut stderr)?
        )));
    }
    sent?;
    status?;
    if stage.as_file().metadata()?.len() == 0 {
        return Err(io::Error::other("encoder produced no audio"));
    }
    publish(stage, output, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcode::tests::{mp3, opus, pcm};
    #[test]
    fn raw_arguments_select_framing_channels_and_quality() {
        let spec = PcmSpec {
            rate: 44100,
            channels: 2,
        };
        for (mode, end) in [
            (Mp3::V0, vec!["-V0"]),
            (Mp3::V2, vec!["-V2"]),
            (Mp3::Cbr(320), vec!["--cbr", "-b", "320"]),
        ] {
            let mut args = vec![
                "--silent",
                "-r",
                "--signed",
                "--little-endian",
                "--bitwidth",
                "24",
                "-m",
                "j",
                "-s",
                "44.1",
                "--resample",
                "44.1",
                "-T",
            ];
            args.extend(end);
            assert_eq!(lame_args(spec, mode).unwrap(), args);
        }
        for (vbr, flag) in [
            (Vbr::HardCbr, "--hard-cbr"),
            (Vbr::Constrained, "--cvbr"),
            (Vbr::Variable, "--vbr"),
        ] {
            let p = Profile {
                target: Target::Opus {
                    bitrate_kbps: 192,
                    complexity: 7,
                    vbr,
                },
                ..opus()
            };
            assert_eq!(
                opus_args(spec, p, 257).unwrap(),
                [
                    "--quiet",
                    "--serial",
                    "257",
                    "--raw",
                    "--raw-bits",
                    "24",
                    "--raw-endianness",
                    "0",
                    "--raw-rate",
                    "44100",
                    "--raw-chan",
                    "2",
                    "--bitrate",
                    "192",
                    "--comp",
                    "7",
                    flag
                ]
            );
        }
        assert!(
            lame_args(
                PcmSpec {
                    rate: 16000,
                    ..spec
                },
                Mp3::Cbr(320)
            )
            .is_err()
        );
        assert!(lame_args(PcmSpec { rate: 8000, ..spec }, Mp3::Cbr(128)).is_err());
        assert_eq!(mp3_rate(96_000).unwrap(), 48_000);
        assert_eq!(mp3_rate(44_100).unwrap(), 44_100);
        assert!(mp3_rate(7999).is_err());
        assert!(!LameEncoder.supports(opus()));
        assert!(!OpusEncoder.supports(mp3()));
        assert!(
            lame_args(
                PcmSpec {
                    channels: 6,
                    ..spec
                },
                Mp3::V2
            )
            .is_err()
        );
    }
    #[test]
    fn probe_names_the_codec_version() {
        assert_eq!(
            version("lame", "LAME 64bits version 3.100 (url)")
                .unwrap()
                .version,
            "3.100"
        );
        assert_eq!(
            version("opusenc", "opusenc opus-tools 0.2 (using libopus 1.5.2)")
                .unwrap()
                .version,
            "0.2/1.5.2"
        );
        assert!(version("lame", "unknown").is_err());
        assert!(version("opusenc", "opusenc opus-tools 0.2").is_err());
    }
    #[test]
    fn child_failures_do_not_publish_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("audio");
        for mut command in [Command::new(dir.path().join("absent")), {
            let mut c = Command::new(std::env::current_exe().unwrap());
            c.arg("--invalid-test-harness-option");
            c
        }] {
            assert!(encode(&mut command, pcm(), mp3(), &output, &AtomicBool::new(false)).is_err());
            assert_eq!(dir.path().read_dir().unwrap().count(), 0);
        }
    }
}
