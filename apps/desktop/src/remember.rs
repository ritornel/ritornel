//! What the app remembers on this computer between runs: which wall and
//! which order the Library shows, whether the large letters stand beside the
//! walls, and the section Settings was last left on. It is the app's own,
//! not the server's, and lives in the system's place for an app's settings.
//! Remembering is a convenience: a file that cannot be read or written is
//! the defaults.
//!
//! How the app looks (`Looks`) is also held for the app's life as a global,
//! so that a change made in Settings is followed at once in the main window.

use std::path::PathBuf;

use gpui::{App, Global};
use serde_json::{Value, json};

/// What is remembered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Remembered {
    /// "artists" or "albums".
    pub wall: String,
    /// The Albums wall's order: "artist", "title", "year" or "added".
    pub albums_sort: String,
    /// Whether the large letters stand in their lane beside the walls.
    pub letters: bool,
    /// The section Settings was last left on: "library", "appearance",
    /// "lastfm" or "about".
    pub settings_section: String,
}

impl Default for Remembered {
    fn default() -> Self {
        Self {
            wall: "artists".to_owned(),
            albums_sort: "artist".to_owned(),
            letters: true,
            settings_section: "library".to_owned(),
        }
    }
}

/// Where it is kept; in tests, nowhere, so that a test never reads what
/// this computer remembers.
fn path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    Some(
        crate::platform::settings_dir()?
            .join("dev.ritornel.desktop-gpui")
            .join("remembered.json"),
    )
}

pub(crate) fn load() -> Remembered {
    let read = path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let mut remembered = Remembered::default();
    let Some(read) = read else { return remembered };
    if read.get("wall").and_then(Value::as_str) == Some("albums") {
        remembered.wall = "albums".to_owned();
    }
    if let Some(sort) = read
        .get("albumsSort")
        .and_then(Value::as_str)
        .filter(|sort| ["artist", "title", "year", "added"].contains(sort))
    {
        remembered.albums_sort = sort.to_owned();
    }
    if let Some(letters) = read.get("letters").and_then(Value::as_bool) {
        remembered.letters = letters;
    }
    if let Some(section) = read
        .get("settingsSection")
        .and_then(Value::as_str)
        .filter(|section| ["library", "appearance", "lastfm", "about"].contains(section))
    {
        remembered.settings_section = section.to_owned();
    }
    remembered
}

pub(crate) fn save(remembered: &Remembered) {
    let Some(path) = path() else { return };
    let text = json!({
        "wall": remembered.wall,
        "albumsSort": remembered.albums_sort,
        "letters": remembered.letters,
        "settingsSection": remembered.settings_section,
    });
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, text.to_string());
}

/// How the app looks on this computer, as chosen in Settings › Appearance:
/// for the app's life, read from what is remembered when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Looks {
    /// Whether the walls under letters have the large letter beside them.
    pub letters: bool,
}

impl Global for Looks {}

/// What the app looks like now.
pub(crate) fn looks(cx: &App) -> Looks {
    cx.try_global::<Looks>()
        .copied()
        .unwrap_or(Looks { letters: true })
}

/// Turns the large letters on or off: remembered, and followed at once by
/// whatever observes the looks.
pub(crate) fn set_letters(on: bool, cx: &mut App) {
    cx.set_global(Looks { letters: on });
    let mut remembered = load();
    remembered.letters = on;
    save(&remembered);
}
