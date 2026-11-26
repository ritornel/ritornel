//! What the app remembers on this computer between runs: which wall and
//! which order the Library shows, whether the large letters stand beside the
//! walls, the section Settings was last left on, the place the music was
//! sent to, and how the bar shows volume. It is the app's own,
//! not the server's, and lives in the system's place for an app's settings.
//! Remembering is a convenience: a file that cannot be read or written is
//! the defaults.
//!
//! How the app looks (`Looks`) is also held for the app's life as a global,
//! so that a change made in Settings is followed at once in the main window.

use std::path::PathBuf;

use gpui::{App, Global};

use crate::places::VolumeControl;
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
    /// The place the music was last sent to, other than this computer, by
    /// its ID in `players.list`. A restart goes back to it only if it is
    /// still playing (`places.rs`).
    pub play_on: Option<String>,
    /// When the bar shows a volume slider: "remote", "always" or "never",
    /// as Settings › Appearance chooses.
    pub volume_control: String,
}

impl Default for Remembered {
    fn default() -> Self {
        Self {
            wall: "artists".to_owned(),
            albums_sort: "artist".to_owned(),
            letters: true,
            settings_section: "library".to_owned(),
            play_on: None,
            volume_control: "remote".to_owned(),
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
    path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .map_or_else(Remembered::default, |read| read_from(&read))
}

/// What a file says is remembered: what it does not say, or says wrongly,
/// is the default.
fn read_from(read: &Value) -> Remembered {
    let mut remembered = Remembered::default();
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
    remembered.play_on = read
        .get("playOn")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(control) = read
        .get("volumeControl")
        .and_then(Value::as_str)
        .filter(|control| ["remote", "always", "never"].contains(control))
    {
        remembered.volume_control = control.to_owned();
    }
    remembered
}

pub(crate) fn save(remembered: &Remembered) {
    let Some(path) = path() else { return };
    let text = written(remembered);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, text.to_string());
}

/// What the file says.
fn written(remembered: &Remembered) -> Value {
    json!({
        "wall": remembered.wall,
        "albumsSort": remembered.albums_sort,
        "letters": remembered.letters,
        "settingsSection": remembered.settings_section,
        "playOn": remembered.play_on,
        "volumeControl": remembered.volume_control,
    })
}

/// How the app looks on this computer, as chosen in Settings › Appearance:
/// for the app's life, read from what is remembered when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Looks {
    /// Whether the walls under letters have the large letter beside them.
    pub letters: bool,
    /// When the player bar shows a volume slider.
    pub volume: VolumeControl,
}

impl Looks {
    /// What is remembered, or the defaults.
    pub(crate) fn remembered() -> Self {
        let remembered = load();
        Self {
            letters: remembered.letters,
            volume: VolumeControl::named(&remembered.volume_control),
        }
    }
}

impl Global for Looks {}

/// What the app looks like now.
pub(crate) fn looks(cx: &App) -> Looks {
    cx.try_global::<Looks>().copied().unwrap_or(Looks {
        letters: true,
        volume: VolumeControl::Remote,
    })
}

/// Turns the large letters on or off: remembered, and followed at once by
/// whatever observes the looks.
pub(crate) fn set_letters(on: bool, cx: &mut App) {
    cx.set_global(Looks {
        letters: on,
        ..looks(cx)
    });
    let mut remembered = load();
    remembered.letters = on;
    save(&remembered);
}

/// Sets when the player bar shows a volume slider: remembered, and
/// followed at once by the bar.
pub(crate) fn set_volume_control(volume: VolumeControl, cx: &mut App) {
    cx.set_global(Looks {
        volume,
        ..looks(cx)
    });
    let mut remembered = load();
    remembered.volume_control = volume.name().to_owned();
    save(&remembered);
}

/// Remembers the place the music was sent to, or that it is on this
/// computer again.
pub(crate) fn set_play_on(place: Option<String>) {
    let mut remembered = load();
    if remembered.play_on != place {
        remembered.play_on = place;
        save(&remembered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_control_and_the_place_played_on_are_kept_and_read_back() {
        let remembered = Remembered {
            play_on: Some("0193f2c4-0000-7000-8000-000000000001".to_owned()),
            volume_control: VolumeControl::Always.name().to_owned(),
            ..Remembered::default()
        };
        assert_eq!(read_from(&written(&remembered)), remembered);
        // An unknown control is the default, and none is `remote`.
        let odd = json!({ "volumeControl": "loud" });
        assert_eq!(read_from(&odd).volume_control, "remote");
        assert_eq!(read_from(&json!({})), Remembered::default());
    }
}
