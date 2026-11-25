//! A tagged, playable fixture of a chosen length.

use std::io::Write;
use std::path::Path;

/// One ID3v2.3 text frame: its id, its size, no flags, and the text in
/// ISO-8859-1.
fn frame(id: &[u8; 4], text: &str) -> Vec<u8> {
    let mut frame = id.to_vec();
    frame.extend_from_slice(&u32::try_from(text.len() + 1).unwrap().to_be_bytes());
    frame.extend_from_slice(&[0, 0, 0]);
    frame.extend_from_slice(text.as_bytes());
    frame
}

/// An ID3v2.3 tag of these frames.
fn id3(frames: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = frames.concat();
    let size = u32::try_from(body.len()).unwrap();
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    // The size is synchsafe: seven bits a byte.
    tag.extend_from_slice(&[
        u8::try_from((size >> 21) & 0x7f).unwrap(),
        u8::try_from((size >> 14) & 0x7f).unwrap(),
        u8::try_from((size >> 7) & 0x7f).unwrap(),
        u8::try_from(size & 0x7f).unwrap(),
    ]);
    tag.extend_from_slice(&body);
    tag
}

/// An AIFF file of `seconds` of tone, 16-bit at 8 kHz, with an `ID3 `
/// chunk naming the track, its artist, album and place. AIFF because
/// symphonia reads an ID3 tag there and not in a WAV file.
pub(crate) fn write_aiff(path: &Path, title: &str, number: &str, hz: f64, seconds: u32) {
    let rate: u32 = 8000;
    let frames = rate * seconds;
    let data = frames * 2;
    let mut tag = id3(&[
        frame(b"TIT2", title),
        frame(b"TPE1", "Invented Artist"),
        frame(b"TALB", "Invented Album"),
        frame(b"TRCK", number),
    ]);
    if tag.len() % 2 == 1 {
        tag.push(0);
    }
    let tag_len = u32::try_from(tag.len()).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FORM");
    bytes.extend_from_slice(&(4 + 26 + 8 + tag_len + 16 + data).to_be_bytes());
    bytes.extend_from_slice(b"AIFFCOMM");
    bytes.extend_from_slice(&18_u32.to_be_bytes());
    bytes.extend_from_slice(&1_u16.to_be_bytes());
    bytes.extend_from_slice(&frames.to_be_bytes());
    bytes.extend_from_slice(&16_u16.to_be_bytes());
    // 8000 as an 80-bit extended float: 1.953125 times two to the 12th.
    bytes.extend_from_slice(&[0x40, 0x0b, 0xfa, 0, 0, 0, 0, 0, 0, 0]);
    bytes.extend_from_slice(b"ID3 ");
    bytes.extend_from_slice(&tag_len.to_be_bytes());
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(b"SSND");
    bytes.extend_from_slice(&(8 + data).to_be_bytes());
    bytes.extend_from_slice(&[0; 8]);
    for frame in 0..frames {
        let t = f64::from(frame) / f64::from(rate);
        let sample = (0.25 * (2.0 * std::f64::consts::PI * hz * t).sin() * 32767.0) as i16;
        bytes.extend_from_slice(&sample.to_be_bytes());
    }
    std::fs::File::create(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}
