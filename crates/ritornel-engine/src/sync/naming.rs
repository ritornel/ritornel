//! Where a track's file goes on a target: one fixed layout for now,
//! `<music root>/<album artist>/<album>/<disc>-<track> <title>.<ext>`, the
//! disc only where the album has several. See "Names, tags and what is
//! safe" in docs/internals/device-sync.md.
//!
//! Names are made safe for FAT, the least any player's storage allows:
//! what it forbids becomes `_`, trailing dots and spaces go, and names are
//! composed (NFC). The whole path under the storage root is capped in UTF-8
//! bytes by shortening the title, then the album, then the artist. A path
//! is worked out once: the one a file was written at is stored, and never
//! worked out again.

use ritornel_device::RelPath;
use unicode_normalization::UnicodeNormalization;

/// What a track's path is made from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Named<'a> {
    pub(crate) album_artist: &'a str,
    pub(crate) album: &'a str,
    pub(crate) title: &'a str,
    pub(crate) disc: Option<u32>,
    pub(crate) track: Option<u32>,
    /// Whether its album has more than one disc.
    pub(crate) discs: bool,
    /// The file's extension, without the dot.
    pub(crate) extension: &'a str,
}

/// The names Windows keeps for devices, which no file on FAT may have,
/// with or without an extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One part of a path, safe on FAT: composed, with what FAT forbids
/// replaced, no leading or trailing space or trailing dot, and never empty.
pub(crate) fn part(text: &str, or: &str) -> String {
    let replaced: String = text
        .nfc()
        .map(|c| match c {
            '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let mut safe = replaced.trim().trim_end_matches(['.', ' ']).to_owned();
    if safe.is_empty() {
        return or.to_owned();
    }
    let stem = safe.split('.').next().unwrap_or_default();
    if RESERVED.iter().any(|name| name.eq_ignore_ascii_case(stem)) {
        safe.insert(0, '_');
    }
    safe
}

/// `text` cut to at most `bytes` UTF-8 bytes at a character boundary, with
/// no trailing space or dot left by the cut.
fn cut(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end)
        .unwrap_or_default()
        .trim_end_matches(['.', ' '])
}

/// The least each part is cut to before the next is shortened.
const LEAST: usize = 24;

/// The path of one track under the storage root, at most `limit` UTF-8
/// bytes. `suffix`, where given, is put before the extension to tell apart
/// two tracks whose paths would otherwise be the same.
pub(crate) fn path(
    music_root: &RelPath,
    named: Named<'_>,
    suffix: Option<&str>,
    limit: usize,
) -> Option<RelPath> {
    let artist = part(named.album_artist, "Unknown Artist");
    let album = part(named.album, "Unknown Album");
    let title = part(named.title, "Untitled");
    let number = match (named.disc.filter(|_| named.discs), named.track) {
        (Some(disc), Some(track)) => format!("{disc}-{track:02} "),
        (None, Some(track)) => format!("{track:02} "),
        (Some(disc), None) => format!("{disc}- "),
        (None, None) => String::new(),
    };
    let tail = format!(
        "{}.{}",
        suffix
            .map(|suffix| format!(" [{suffix}]"))
            .unwrap_or_default(),
        named.extension
    );
    // What is fixed: the music root, the separators, the number and the tail.
    let fixed = music_root.as_str().len() + 3 + number.len() + tail.len();
    let room = limit.checked_sub(fixed)?;
    // Shorten the title, then the album, then the artist, each down to a
    // floor before the next is touched, and then past it if it must be.
    let mut names = [title.as_str(), album.as_str(), artist.as_str()];
    for floor in [LEAST, 1] {
        for at in 0..names.len() {
            let over = names
                .iter()
                .map(|name| name.len())
                .sum::<usize>()
                .saturating_sub(room);
            if over == 0 {
                break;
            }
            if let Some(name) = names.get_mut(at) {
                let kept = name.len().saturating_sub(over).max(floor.min(name.len()));
                *name = cut(name, kept);
            }
        }
    }
    let [title, album, artist] = names;
    if artist.len() + album.len() + title.len() > room
        || artist.is_empty()
        || album.is_empty()
        || title.is_empty()
    {
        return None;
    }
    RelPath::new(format!(
        "{}/{artist}/{album}/{number}{title}{tail}",
        music_root.as_str()
    ))
    .ok()
}

/// What two paths are the same by on a file system that ignores case.
pub(crate) fn folded(path: &RelPath) -> String {
    path.as_str().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music() -> RelPath {
        RelPath::new("Music").unwrap()
    }

    fn named<'a>(artist: &'a str, album: &'a str, title: &'a str) -> Named<'a> {
        Named {
            album_artist: artist,
            album,
            title,
            disc: Some(1),
            track: Some(3),
            discs: false,
            extension: "flac",
        }
    }

    #[test]
    fn a_path_is_artist_album_and_numbered_title_with_the_disc_only_where_there_are_several() {
        let one = path(
            &music(),
            named("Ada Vale", "Low Tide", "Shoreline"),
            None,
            250,
        )
        .unwrap();
        assert_eq!(one.as_str(), "Music/Ada Vale/Low Tide/03 Shoreline.flac");
        let mut two = named("Ada Vale", "Low Tide", "Shoreline");
        two.discs = true;
        two.disc = Some(2);
        assert_eq!(
            path(&music(), two, None, 250).unwrap().as_str(),
            "Music/Ada Vale/Low Tide/2-03 Shoreline.flac"
        );
    }

    #[test]
    fn what_fat_forbids_is_replaced_and_trailing_dots_and_spaces_go() {
        let made = path(
            &music(),
            named("AC/DC: Live?", "What... ", " <Title>*|\"x\" "),
            None,
            250,
        )
        .unwrap();
        assert_eq!(
            made.as_str(),
            "Music/AC_DC_ Live_/What/03 _Title____x_.flac"
        );
        assert_eq!(part("...", "Untitled"), "Untitled");
        assert_eq!(part("con", "x"), "_con");
        assert_eq!(part("Cafe\u{301}", "x"), "Caf\u{e9}");
    }

    #[test]
    fn a_long_path_is_capped_by_shortening_the_title_then_the_album_then_the_artist() {
        let long = "Ω".repeat(200);
        let made = path(&music(), named("Ada Vale", "Low Tide", &long), None, 250).unwrap();
        assert!(made.as_str().len() <= 250, "{}", made.as_str().len());
        assert!(made.as_str().starts_with("Music/Ada Vale/Low Tide/03 ΩΩ"));
        assert!(made.as_str().ends_with(".flac"));
        let made = path(&music(), named(&long, &long, &long), Some("a1b2"), 250).unwrap();
        assert!(made.as_str().len() <= 250);
        assert!(made.as_str().ends_with(" [a1b2].flac"));
        // Every part keeps something.
        assert_eq!(made.parts().count(), 4);
        assert!(path(&music(), named("a", "b", "c"), None, 12).is_none());
    }
}
