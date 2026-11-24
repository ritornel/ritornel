//! A disc's layout from what was found beside its files: the rip logs, CUE
//! sheets and checksum reports a scan keeps as sidecars. A layout is where
//! each track of a CD starts and where the disc ends, in sectors, and it
//! gives the disc's MusicBrainz disc ID. See "Disc layouts" in
//! docs/internals/corrections.md.
//!
//! Everything here is pure: the engine hands over the sidecars' bytes and
//! what it knows of each audio file, and gets the layouts back. A change to
//! how a layout is read is a new [`LAYOUT_VERSION`], and the layouts are
//! derived again from the stored sidecars.

use std::collections::{BTreeMap, HashMap, HashSet};

use sha1::{Digest, Sha1};
use unicode_normalization::UnicodeNormalization;

/// The version of how sidecars are read into layouts.
pub const LAYOUT_VERSION: u32 = 1;

/// The largest sidecar a scan keeps, in bytes.
pub const SIDECAR_MAX_BYTES: u64 = 1 << 20;

/// The sectors a disc's lead-in takes before its first track.
const LEAD_IN: u64 = 150;

/// The sectors between the audio session and a data session on an enhanced
/// CD, as a drive's log shows them.
const SESSION_GAP: u64 = 11_400;

/// Sectors a second: a sector is 588 stereo samples at 44.1 kHz.
const SECTORS_PER_SECOND: u64 = 75;

/// The furthest sector a layout may name: a disc ID holds each offset,
/// lead-in included, in 32 bits. A number past it in a sidecar is not a
/// CD's, and is read as nothing.
const MAX_SECTOR: u64 = u32::MAX as u64 - LEAD_IN;

/// What kind of sidecar a file is, by its extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A CUE sheet.
    Cue,
    /// A rip log.
    Log,
    /// An AccurateRip or CUETools report.
    AccurateRip,
    /// A text file, which is a sidecar only if it reads as a rip log.
    Text,
}

/// The kind of sidecar a file of this name would be, if any.
pub fn kind(name: &str) -> Option<Kind> {
    let (_, extension) = name.rsplit_once('.')?;
    match extension.to_ascii_lowercase().as_str() {
        "cue" => Some(Kind::Cue),
        "log" => Some(Kind::Log),
        "accurip" => Some(Kind::AccurateRip),
        "txt" | "nfo" => Some(Kind::Text),
        _ => None,
    }
}

/// Whether a scan keeps a file of this name and these bytes as a sidecar.
pub fn keeps(name: &str, bytes: &[u8]) -> bool {
    match kind(name) {
        None => false,
        Some(_) if bytes.len() as u64 > SIDECAR_MAX_BYTES => false,
        Some(Kind::Text) => reads_as_rip_log(&decode(bytes)),
        Some(_) => true,
    }
}

/// Whether text is a rip log by what it says it is.
fn reads_as_rip_log(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "eac extraction logfile",
        "exact audio copy",
        "x lossless decoder",
        "cdda2wav",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

/// The bytes of a sidecar as text. Rip logs come as UTF-16 with a byte
/// order mark, as UTF-8, and as Windows-1252.
pub fn decode(bytes: &[u8]) -> String {
    let units = |big: bool| {
        let (pairs, _) = bytes.get(2..).unwrap_or_default().as_chunks::<2>();
        pairs.iter().map(move |pair| {
            if big {
                u16::from_be_bytes(*pair)
            } else {
                u16::from_le_bytes(*pair)
            }
        })
    };
    match bytes {
        [0xFF, 0xFE, ..] => char::decode_utf16(units(false))
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        [0xFE, 0xFF, ..] => char::decode_utf16(units(true))
            .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect(),
        _ => {
            let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
            match std::str::from_utf8(bytes) {
                Ok(text) => text.to_owned(),
                Err(_) => bytes.iter().map(|byte| windows_1252(*byte)).collect(),
            }
        }
    }
}

/// One byte of Windows-1252.
fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{FFFD}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{FFFD}', 'Ž',
        '\u{FFFD}', '\u{FFFD}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ',
        '\u{FFFD}', 'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH
            .get(usize::from(byte - 0x80))
            .copied()
            .unwrap_or(char::REPLACEMENT_CHARACTER),
        _ => char::from(byte),
    }
}

/// Where each track of a disc starts and where its audio ends, in sectors
/// from the start of the first track's audio, without the lead-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toc {
    /// The sector each track starts at, in order.
    pub starts: Vec<u64>,
    /// The sector after the last track's audio: the lead-out.
    pub lead_out: u64,
}

impl Toc {
    /// The disc's MusicBrainz disc ID, if this is a layout a CD can have:
    /// 1 to 99 tracks, each starting after the one before, the last before
    /// the lead-out, and every offset within 32 bits. See MusicBrainz's
    /// "Disc ID Calculation".
    pub fn disc_id(&self) -> Option<String> {
        let count = self.starts.len();
        let increasing = self
            .starts
            .iter()
            .chain(std::iter::once(&self.lead_out))
            .collect::<Vec<_>>()
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a < b));
        if !(1..=99).contains(&count) || !increasing || self.lead_out > MAX_SECTOR {
            return None;
        }
        let mut text = format!("01{count:02X}{:08X}", self.lead_out + LEAD_IN);
        for slot in 0..99 {
            let offset = self.starts.get(slot).map_or(0, |start| start + LEAD_IN);
            text.push_str(&format!("{offset:08X}"));
        }
        Some(disc_id_text(&Sha1::digest(text.as_bytes()).into()))
    }

    /// The disc's CDDB ID as a CUE sheet prints it, which is only as
    /// precise as a second.
    fn cddb(&self) -> String {
        let seconds: Vec<u64> = self
            .starts
            .iter()
            .map(|start| start.saturating_add(LEAD_IN) / SECTORS_PER_SECOND)
            .collect();
        let digits: u64 = seconds
            .iter()
            .map(|second| {
                second
                    .to_string()
                    .chars()
                    .filter_map(|digit| digit.to_digit(10))
                    .map(u64::from)
                    .sum::<u64>()
            })
            .sum();
        let first = seconds.first().copied().unwrap_or(0);
        let length =
            (self.lead_out.saturating_add(LEAD_IN) / SECTORS_PER_SECOND).saturating_sub(first);
        format!("{:02x}{length:04x}{:02x}", digits % 255, self.starts.len())
    }
}

/// A disc ID's text: base64 of the SHA-1, in MusicBrainz's alphabet.
fn disc_id_text(digest: &[u8; 20]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._";
    let symbol = |index: u32| {
        ALPHABET
            .get(usize::try_from(index & 63).unwrap_or(0))
            .map_or('?', |byte| char::from(*byte))
    };
    let mut text = String::with_capacity(28);
    for chunk in digest.chunks(3) {
        let bytes = [0, 1, 2].map(|i| chunk.get(i).copied().map_or(0, u32::from));
        let group = (bytes[0] << 16) | (bytes[1] << 8) | bytes[2];
        text.push(symbol(group >> 18));
        text.push(symbol(group >> 12));
        text.push(if chunk.len() > 1 {
            symbol(group >> 6)
        } else {
            '-'
        });
        text.push(if chunk.len() > 2 { symbol(group) } else { '-' });
    }
    text
}

/// How many CD sectors a file's audio holds, from what the scan stored of
/// it: only for lossless stereo at 44.1 kHz. The stored length is cut to
/// the millisecond, which still fixes the count of a file that is a whole
/// number of sectors, since a sector is 13⅓ ms. `None` if no whole number
/// of sectors gives that length.
pub fn sectors(duration_ms: u64, sample_rate: u32, channels: u32, lossless: bool) -> Option<u64> {
    if !lossless || sample_rate != 44_100 || channels != 2 || duration_ms == 0 {
        return None;
    }
    // A file of n sectors is 40n/3 ms long, cut to the millisecond:
    // 3 × ms ≤ 40n < 3 × ms + 3.
    let thrice = duration_ms.checked_mul(3)?;
    let sectors = thrice.div_ceil(40);
    (sectors <= MAX_SECTOR && 40 * sectors < thrice + 3).then_some(sectors)
}

/// One audio file of a folder, as the layouts need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFile {
    /// The caller's key for it.
    pub item: i64,
    /// Its name, without its folder.
    pub name: String,
    /// Its track number, as its tags give it.
    pub track: Option<u64>,
    /// Its title, as its tags give it.
    pub title: Option<String>,
    /// Its length in CD sectors, from [`sectors`].
    pub sectors: Option<u64>,
}

/// One folder of audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    /// The folder, relative to the root, with `/` between its parts: empty
    /// for the root.
    pub path: String,
    /// Its audio files, in the order of their names.
    pub files: Vec<AudioFile>,
}

/// One sidecar as it was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    /// The caller's key for the revision it was read from.
    pub revision: i64,
    /// Its name, without its folder.
    pub name: String,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

/// How a layout was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Method {
    /// A drive's table of sectors in a rip log.
    Log,
    /// A CUE sheet, with the exact lengths of the files it names.
    Cue,
    /// A CUE sheet whose files were renamed since, matched by their track
    /// numbers and checked by the CDDB ID it prints or by their titles.
    RenamedCue,
    /// The files' lengths in order, checked against an AccurateRip disc ID
    /// printed beside them.
    AccurateRip,
    /// The files' lengths in order, checked against a CDDB ID a CUE sheet
    /// prints.
    Cddb,
}

impl Method {
    /// The method as it is stored.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Cue => "cue",
            Self::RenamedCue => "renamed_cue",
            Self::AccurateRip => "accuraterip",
            Self::Cddb => "cddb",
        }
    }
}

/// One layout of the files of a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The disc ID it gives.
    pub disc_id: String,
    /// The layout.
    pub toc: Toc,
    /// Each way it was found, with the sidecar revision it was read from,
    /// in the order found.
    pub found: Vec<(Method, i64)>,
    /// A CUE sheet that disagrees with the CDDB ID printed in it: the
    /// layout may not be the disc's.
    pub provisional: bool,
    /// The file at each position, if every position has exactly one file
    /// of its length and track number. Empty if not.
    pub tracks: Vec<i64>,
    /// The one file that holds the whole disc, if the folder holds one
    /// file and it is the disc's length.
    pub whole_file: Option<i64>,
}

/// The layouts of one folder's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderLayouts {
    /// The folder.
    pub folder: String,
    /// Its layouts, in the order found.
    pub layouts: Vec<Layout>,
}

/// The layouts of every folder of audio, from the sidecars kept, by the
/// folder they are in. A folder's own sidecars count, and those in a folder
/// inside it that holds no audio, such as `Misc/`. The sidecars of a folder
/// above it that holds no audio count only for a layout that fits its files
/// exactly, as one log for the two discs of a set does.
pub fn layouts(
    folders: &[Folder],
    sidecars: &BTreeMap<String, Vec<Sidecar>>,
) -> Vec<FolderLayouts> {
    let music: HashSet<&str> = folders.iter().map(|folder| folder.path.as_str()).collect();
    let mut found = Vec::new();
    for folder in folders {
        let mut local: Vec<&Sidecar> = sidecars.get(&folder.path).into_iter().flatten().collect();
        for (sub, items) in sidecars {
            if parent(sub) == Some(folder.path.as_str()) && !music.contains(sub.as_str()) {
                local.extend(items);
            }
        }
        let mut layouts = describe(&folder.files, &local);
        if let Some(above) = parent(&folder.path)
            && !music.contains(above)
            && let Some(items) = sidecars.get(above)
        {
            let items: Vec<&Sidecar> = items.iter().collect();
            for layout in describe(&folder.files, &items) {
                let fits = !layout.tracks.is_empty() || layout.whole_file.is_some();
                if fits
                    && !layout.provisional
                    && !layouts.iter().any(|had| had.disc_id == layout.disc_id)
                {
                    layouts.push(layout);
                }
            }
        }
        if !layouts.is_empty() {
            found.push(FolderLayouts {
                folder: folder.path.clone(),
                layouts,
            });
        }
    }
    found
}

/// The folder a folder is in: `None` for the root.
fn parent(path: &str) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    Some(path.rsplit_once('/').map_or("", |(above, _)| above))
}

/// The layouts one set of sidecars gives one folder's files.
fn describe(files: &[AudioFile], sidecars: &[&Sidecar]) -> Vec<Layout> {
    let mut logs = Vec::new();
    let mut text_logs = Vec::new();
    let mut cues = Vec::new();
    let mut reports = Vec::new();
    for sidecar in sidecars {
        let text = decode(&sidecar.bytes);
        match kind(&sidecar.name) {
            Some(Kind::Cue) => cues.push((sidecar.revision, text)),
            Some(Kind::Log) => logs.push((sidecar.revision, text)),
            Some(Kind::AccurateRip) => reports.push((sidecar.revision, text)),
            Some(Kind::Text) if reads_as_rip_log(&text) => text_logs.push((sidecar.revision, text)),
            _ => {}
        }
    }
    let mut found = Found::default();
    for (revision, text) in logs.iter().chain(&text_logs) {
        for toc in log_tocs(text) {
            found.record(toc, Method::Log, *revision);
        }
    }
    let cddb: Vec<(String, i64)> = cues
        .iter()
        .flat_map(|(revision, text)| printed_cddb(text).into_iter().map(|id| (id, *revision)))
        .collect();
    let printed = |id: &str| {
        cddb.iter()
            .find(|(had, _)| had == id)
            .map(|(_, revision)| *revision)
    };
    let lengths: Vec<(&str, Option<u64>)> = files
        .iter()
        .map(|file| (file.name.as_str(), file.sectors))
        .collect();
    let extra_session = reports
        .iter()
        .any(|(_, text)| text.contains("CD-Extra data track length"));
    // Each CUE sheet is checked against the CDDB ID it prints itself.
    for (revision, text) in &cues {
        let own = printed_cddb(text);
        if let Some(toc) = cue_toc(text, &lengths) {
            let audio = toc.starts.len() as u64;
            let extra = extra_session || has_data_track(text);
            let agrees = own.is_empty()
                || own.contains(&toc.cddb())
                || (extra
                    && own.iter().any(|id| {
                        id.get(6..)
                            .and_then(|count| u64::from_str_radix(count, 16).ok())
                            == Some(audio + 1)
                    }));
            if agrees {
                found.record(toc, Method::Cue, *revision);
            } else {
                found.provisional(toc, *revision);
            }
        } else if let Some(toc) = renamed_cue(text, files, &own) {
            found.record(toc, Method::RenamedCue, *revision);
        }
    }
    // Files renamed since, or an older log with no table of sectors: their
    // lengths in order, if a checksum printed beside them agrees.
    let mut numbered: Vec<(u64, u64)> = files
        .iter()
        .filter_map(|file| Some((number(file)?, file.sectors.filter(|n| *n > 0)?)))
        .collect();
    numbered.sort_unstable();
    let counted = (1..=numbered.len() as u64).eq(numbered.iter().map(|(n, _)| *n));
    if !numbered.is_empty() && numbered.len() == files.len() && counted {
        let mut bounds = vec![0u64];
        for (_, length) in &numbered {
            let end = bounds.last().copied().unwrap_or(0).saturating_add(*length);
            bounds.push(end);
        }
        for (revision, text) in &reports {
            let shift = text
                .find("Pregap length ")
                .and_then(|at| text.get(at + "Pregap length ".len()..))
                .and_then(|rest| {
                    let time: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_digit() || *c == ':')
                        .collect();
                    msf(&time)
                })
                .unwrap_or(0);
            let shifted: Vec<u64> = bounds.iter().map(|n| n.saturating_add(shift)).collect();
            let first = shifted.iter().fold(0u64, |sum, n| sum.saturating_add(*n));
            let second = shifted.iter().zip(1u64..).fold(0u64, |sum, (n, i)| {
                sum.saturating_add(i.saturating_mul((*n).max(1)))
            });
            if text
                .to_lowercase()
                .contains(&format!("{first:08x}-{second:08x}"))
                && let Some((lead_out, starts)) = shifted.split_last()
            {
                let toc = Toc {
                    starts: starts.to_vec(),
                    lead_out: *lead_out,
                };
                found.record(toc, Method::AccurateRip, *revision);
            }
        }
        if let Some((lead_out, starts)) = bounds.split_last() {
            let toc = Toc {
                starts: starts.to_vec(),
                lead_out: *lead_out,
            };
            let only_renamed = found.layouts.iter().all(|layout| {
                layout
                    .found
                    .iter()
                    .all(|(method, _)| *method == Method::RenamedCue)
            });
            if only_renamed && let Some(revision) = printed(&toc.cddb()) {
                found.record(toc, Method::Cddb, revision);
            }
        }
    }
    let mut layouts = found.layouts;
    for (toc, revision) in found.provisional {
        let Some(disc_id) = toc.disc_id() else {
            continue;
        };
        if layouts.iter().any(|layout| layout.disc_id == disc_id) {
            continue;
        }
        layouts.push(Layout {
            disc_id,
            toc,
            found: vec![(Method::Cue, revision)],
            provisional: true,
            tracks: Vec::new(),
            whole_file: None,
        });
    }
    for layout in &mut layouts {
        layout.tracks = sector_files(files, &layout.toc);
        // One file the length of a disc of several tracks is an image of
        // it. A disc of one track that file fills is that track.
        layout.whole_file = match files {
            [file] if layout.tracks.is_empty() && file.sectors == Some(layout.toc.lead_out) => {
                Some(file.item)
            }
            _ => None,
        };
    }
    layouts
}

/// The layouts found so far, each once by its disc ID.
#[derive(Default)]
struct Found {
    layouts: Vec<Layout>,
    provisional: Vec<(Toc, i64)>,
}

impl Found {
    fn record(&mut self, toc: Toc, method: Method, revision: i64) {
        let Some(disc_id) = toc.disc_id() else {
            return;
        };
        if let Some(layout) = self
            .layouts
            .iter_mut()
            .find(|layout| layout.disc_id == disc_id)
        {
            if !layout.found.iter().any(|(had, _)| *had == method) {
                layout.found.push((method, revision));
            }
            return;
        }
        self.layouts.push(Layout {
            disc_id,
            toc,
            found: vec![(method, revision)],
            provisional: false,
            tracks: Vec::new(),
            whole_file: None,
        });
    }

    fn provisional(&mut self, toc: Toc, revision: i64) {
        if toc.disc_id().is_some() && !self.provisional.iter().any(|(had, _)| *had == toc) {
            self.provisional.push((toc, revision));
        }
    }
}

/// A file's track number: its tag's, or the digits its name starts with.
fn number(file: &AudioFile) -> Option<u64> {
    file.track.or_else(|| leading_digits(&file.name))
}

fn leading_digits(text: &str) -> Option<u64> {
    let digits: String = text.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The files at each position of a layout, if every position has exactly
/// one file of its length, and of its number when the file has one.
fn sector_files(files: &[AudioFile], toc: &Toc) -> Vec<i64> {
    let mut aligned: Vec<i64> = Vec::with_capacity(toc.starts.len());
    let ends = toc
        .starts
        .iter()
        .skip(1)
        .chain(std::iter::once(&toc.lead_out));
    for (position, (start, stop)) in (1..).zip(toc.starts.iter().zip(ends)) {
        let length = stop.saturating_sub(*start);
        let mut fitting = files.iter().filter(|file| {
            file.sectors == Some(length) && number(file).is_none_or(|n| n == position)
        });
        match (fitting.next(), fitting.next()) {
            (Some(file), None) if !aligned.contains(&file.item) => aligned.push(file.item),
            _ => return Vec::new(),
        }
    }
    aligned
}

/// Text split into lines at any of `\r\n`, `\n` and `\r`.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\r', '\n'])
}

/// A time as `MM:SS:FF` in sectors.
fn msf(text: &str) -> Option<u64> {
    let mut parts = text.split(':');
    let mut next = || -> Option<u64> {
        let part = parts.next()?;
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    };
    let (minutes, seconds, frames) = (next()?, next()?, next()?);
    let sectors = minutes
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(SECTORS_PER_SECOND)?
        .checked_add(frames)?;
    (parts.next().is_none() && sectors <= MAX_SECTOR).then_some(sectors)
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// One row of a drive's table of sectors, as EAC and XLD print it: the
/// track, its start and length as times, and its first and last sectors.
fn log_row(line: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = line.split('|').map(str::trim).collect();
    let [track, start, length, first, last] = parts.as_slice() else {
        return None;
    };
    let time = |text: &str| {
        !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_digit() || c == ':' || c == '.')
    };
    if !(is_number(track) && time(start) && time(length) && is_number(first) && is_number(last)) {
        return None;
    }
    let sector = |text: &str| text.parse::<u64>().ok().filter(|n| *n < MAX_SECTOR);
    Some((track.parse().ok()?, sector(first)?, sector(last)?))
}

/// Every table of sectors in a rip log. The end sector a log prints is
/// the track's last. On an enhanced CD the audio ends where the gap before
/// the data session begins.
fn log_tocs(text: &str) -> Vec<Toc> {
    let rows: Vec<(u64, u64, u64)> = lines(text).filter_map(log_row).collect();
    let mut tocs = Vec::new();
    let mut at = 0;
    while let Some(first) = rows.get(at) {
        // A run of rows whose track numbers count up by one is one table.
        let key = |index: usize, row: &(u64, u64, u64)| row.0 as i128 - index as i128;
        let run = rows
            .iter()
            .enumerate()
            .skip(at)
            .take_while(|(index, row)| key(*index, row) == key(at, first))
            .count();
        let table = rows.get(at..at + run).unwrap_or_default();
        at += run.max(1);
        if first.0 != 1 {
            continue;
        }
        let mut starts = Vec::new();
        let mut end: Option<u64> = None;
        for (_, start, last) in table {
            if let Some(end) = end
                && *start != end
            {
                if start.checked_sub(end) != Some(SESSION_GAP) {
                    starts.clear();
                }
                break;
            }
            starts.push(*start);
            end = Some(last + 1);
        }
        if let Some(lead_out) = end.filter(|_| !starts.is_empty()) {
            tocs.push(Toc { starts, lead_out });
        }
    }
    if let Some(toc) = cdda2wav_toc(text) {
        tocs.push(toc);
    }
    tocs
}

/// The table cdda2wav prints: each track's first sector, then the
/// lead-out's.
fn cdda2wav_toc(text: &str) -> Option<Toc> {
    let (_, table) = text.split_once("Table of Contents: starting sectors")?;
    let (rows, lead_out) = table.match_indices("lead-out(").find_map(|(at, marker)| {
        let rest = table.get(at + marker.len()..)?.trim_start();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        rest.get(digits.len()..)?.starts_with(')').then_some(())?;
        let lead_out = digits.parse::<u64>().ok().filter(|n| *n <= MAX_SECTOR)?;
        Some((table.get(..at)?, lead_out))
    })?;
    let mut starts = Vec::new();
    let mut rest = rows;
    while let Some(at) = rest.find(|c: char| c.is_ascii_digit()) {
        let from = rest.get(at..)?;
        let digits: String = from.chars().take_while(char::is_ascii_digit).collect();
        let after = from.get(digits.len()..)?;
        rest = after;
        let Some(inside) = after.strip_prefix(".(") else {
            continue;
        };
        let inside = inside.trim_start();
        let sector: String = inside.chars().take_while(char::is_ascii_digit).collect();
        let Some(tail) = inside.get(sector.len()..).and_then(|t| t.strip_prefix(')')) else {
            continue;
        };
        if sector.is_empty() {
            continue;
        }
        let sector = sector.parse::<u64>().ok().filter(|n| *n <= MAX_SECTOR)?;
        starts.push((digits.parse::<u64>().ok()?, sector));
        rest = tail;
    }
    let counted = (1..=starts.len() as u64).eq(starts.iter().map(|(n, _)| *n));
    (counted && !starts.is_empty()).then(|| Toc {
        starts: starts.into_iter().map(|(_, sector)| sector).collect(),
        lead_out,
    })
}

/// The CDDB IDs a CUE sheet prints, in lower case.
fn printed_cddb(text: &str) -> Vec<String> {
    lines(text)
        .filter_map(|line| {
            let head = line.get(..11)?;
            let id = line.get(11..)?.trim_end();
            (head.eq_ignore_ascii_case("REM DISCID ")
                && id.len() == 8
                && id.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| id.to_ascii_lowercase())
        })
        .collect()
}

fn has_data_track(text: &str) -> bool {
    lines(text).any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.windows(3).any(|w| match w {
            [track, number, mode] => {
                track.eq_ignore_ascii_case("TRACK")
                    && is_number(number)
                    && mode
                        .get(..4)
                        .is_some_and(|m| m.eq_ignore_ascii_case("MODE"))
            }
            _ => false,
        })
    })
}

/// A name folded for matching a CUE sheet's file to a file: case folded,
/// accents and punctuation taken off.
fn fold(text: &str) -> String {
    text.to_lowercase()
        .replace('ß', "ss")
        .nfkd()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// A file name without its folder and its last extension.
fn stem(name: &str) -> &str {
    let name = name.rsplit('/').next().unwrap_or(name);
    match name.rfind('.') {
        Some(at) if at > 0 => name.get(..at).unwrap_or(name),
        _ => name,
    }
}

/// The file a CUE sheet's `FILE` line names, if the line is one.
fn cue_file(line: &str) -> Option<&str> {
    let keyword = line.get(..4)?;
    let rest = line.get(4..)?;
    if !keyword.eq_ignore_ascii_case("FILE") || !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let word =
        |text: &str| !text.is_empty() && text.chars().all(|c| c.is_alphanumeric() || c == '_');
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.rfind('"')?;
        let (name, after) = (quoted.get(..end)?, quoted.get(end + 1..)?);
        (!name.is_empty() && after.starts_with(char::is_whitespace) && word(after.trim_start()))
            .then_some(name)
    } else {
        let mut words = rest.split_whitespace();
        let (name, kind) = (words.next()?, words.next()?);
        (words.next().is_none() && word(kind)).then_some(name)
    }
}

/// A CUE sheet's `TRACK` line: its number and type.
fn cue_track(line: &str) -> Option<(u64, &str)> {
    let mut words = line.split_whitespace();
    let (keyword, number, kind) = (words.next()?, words.next()?, words.next()?);
    (words.next().is_none()
        && keyword.eq_ignore_ascii_case("TRACK")
        && line
            .get(..5)
            .is_some_and(|k| k.eq_ignore_ascii_case("TRACK"))
        && is_number(number))
    .then(|| Some((number.parse().ok()?, kind)))
    .flatten()
}

/// A line that starts with `keyword` and is followed by one time.
fn cue_time(line: &str, keyword: &str) -> Option<u64> {
    let head = line.get(..keyword.len())?;
    let rest = line.get(keyword.len()..)?;
    if !head.eq_ignore_ascii_case(keyword) || !rest.starts_with(char::is_whitespace) {
        return None;
    }
    msf(rest.trim())
}

/// A CUE sheet's layout, given the exact length of each file in sectors.
/// An `INDEX 01` is relative to its `FILE`, and a pregap a sheet declares
/// rather than stores is added. A data track after the audio ends the
/// audio session where its `INDEX 00` says.
fn cue_toc(text: &str, files: &[(&str, Option<u64>)]) -> Option<Toc> {
    let lengths: HashMap<String, Option<u64>> = files
        .iter()
        .map(|(name, sectors)| (fold(stem(name)), *sectors))
        .collect();
    if lengths.len() != files.len() {
        return None;
    }
    let (mut base, mut length, mut gap) = (0u64, 0u64, 0u64);
    let mut data_track = false;
    let mut starts: Vec<u64> = Vec::new();
    let mut track = None;
    for line in lines(text) {
        let line = line.trim();
        if let Some(name) = cue_file(line) {
            base = base.checked_add(length)?;
            length = (*lengths.get(&fold(stem(&name.replace('\\', "/"))))?)?;
        } else if let Some((number, kind)) = cue_track(line) {
            let next = starts.len() as u64 + 1;
            if kind
                .get(..4)
                .is_some_and(|m| m.eq_ignore_ascii_case("MODE"))
                && number == next
            {
                data_track = true;
                continue;
            }
            if data_track || !kind.eq_ignore_ascii_case("AUDIO") {
                return None;
            }
            track = Some(number);
        } else if data_track {
            if cue_time(line, "INDEX 00") == Some(length) {
                return Some(Toc {
                    starts,
                    lead_out: base.checked_add(gap)?.checked_add(length)?,
                });
            }
        } else if let Some(frames) = cue_time(line, "INDEX 01") {
            if track != Some(starts.len() as u64 + 1) || frames >= length {
                return None;
            }
            starts.push(base.checked_add(gap)?.checked_add(frames)?);
        } else if let Some(frames) = cue_time(line, "PREGAP").or_else(|| cue_time(line, "POSTGAP"))
        {
            gap = gap.checked_add(frames)?;
        }
    }
    let lead_out = base.checked_add(length)?.checked_add(gap)?;
    (!starts.is_empty() && !data_track).then_some(Toc { starts, lead_out })
}

/// The number at the start of a file's name, or after a ` - ` in it, as
/// `03 - Title.flac` and `Artist - 03 - Title.flac` have it.
fn number_in_name(name: &str) -> Option<u64> {
    let starts = std::iter::once(0).chain(name.match_indices(" - ").map(|(at, _)| at + 3));
    for start in starts {
        let Some(rest) = name.get(start..) else {
            continue;
        };
        for width in [2, 1] {
            let digits = rest.get(..width).filter(|d| is_number(d));
            let after = rest.get(width..).unwrap_or_default();
            if let Some(digits) = digits
                && (after.starts_with(" - ") || after.starts_with(' ') || after.starts_with('.'))
            {
                return digits.parse().ok();
            }
        }
    }
    None
}

/// A CUE sheet whose files were renamed since it was written: its files
/// taken in the order of the folder's track numbers. It counts only if a
/// CDDB ID it prints agrees, or, with none printed, if every file's title
/// ends the name the sheet gave it.
fn renamed_cue(text: &str, files: &[AudioFile], cddb: &[String]) -> Option<Toc> {
    let refs: Vec<String> = lines(text)
        .filter_map(|line| {
            let name = cue_file(line.trim())?.replace('\\', "/");
            Some(name.rsplit('/').next().unwrap_or(&name).to_owned())
        })
        .collect();
    let distinct: HashSet<&String> = refs.iter().collect();
    if refs.len() != files.len() || distinct.len() != refs.len() {
        return None;
    }
    let mut by_number: Vec<(u64, &AudioFile)> = files
        .iter()
        .map(|file| Some((file.track.or_else(|| number_in_name(&file.name))?, file)))
        .collect::<Option<_>>()?;
    by_number.sort_by_key(|(number, _)| *number);
    if !(1..=refs.len() as u64).eq(by_number.iter().map(|(n, _)| *n)) {
        return None;
    }
    let renamed: Vec<(&str, Option<u64>)> = refs
        .iter()
        .zip(&by_number)
        .map(|(name, (_, file))| (name.as_str(), file.sectors))
        .collect();
    let toc = cue_toc(text, &renamed)?;
    let titles_agree = refs.iter().zip(&by_number).all(|(name, (_, file))| {
        let title = fold(file.title.as_deref().unwrap_or_default());
        !title.is_empty() && fold(stem(name)).ends_with(&title)
    });
    let printed = cddb.contains(&toc.cddb());
    (printed || (cddb.is_empty() && titles_agree)).then_some(toc)
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
