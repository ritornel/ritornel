//! Finding the files of Spotify's extended streaming history in what
//! Spotify sent: the zip file, or the folder it was unpacked to. Each is
//! read where it is, and nothing is unpacked or copied.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::rows::FileError;

/// How deep a folder is looked into. The history sits one folder down in
/// the zip, and a folder it was unpacked into adds one more.
const DEPTH: usize = 4;

/// What a file of an archive is, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A file of the extended streaming history of audio.
    Extended,
    /// A file of the extended streaming history of video, which is not read.
    Video,
    /// A file of the history in the account data, a year to the minute.
    AccountData,
    /// Anything else, which is not read.
    Other,
}

fn kind(name: &str) -> Kind {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let json = base.ends_with(".json");
    if json
        && ["Streaming_History_Audio_", "endsong_"]
            .iter()
            .any(|prefix| base.starts_with(prefix))
    {
        Kind::Extended
    } else if json && base.starts_with("Streaming_History_Video_") {
        Kind::Video
    } else if json && base.starts_with("StreamingHistory") {
        Kind::AccountData
    } else {
        Kind::Other
    }
}

/// Why an archive could not be read. It holds no path and no value from a
/// file: a file is named by its own name in the archive.
#[derive(Debug)]
pub(crate) enum Error {
    /// The archive could not be opened.
    Open(std::io::Error),
    /// It is a file, and not a zip file.
    NotZip,
    /// It holds the account data's history, and no extended history.
    AccountData,
    /// It holds no history at all.
    NoHistory,
    /// A file in it could not be read.
    Unreadable(String),
    /// A file in it is not a list of rows.
    File(String, FileError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => write!(f, "cannot read that archive: {error}"),
            Self::NotZip => f.write_str("that is neither a folder nor a zip file"),
            Self::AccountData => f.write_str(
                "that is the history in Spotify's account data, a year of it, to the minute, \
                 with no track IDs; ask Spotify for the extended streaming history instead",
            ),
            Self::NoHistory => f.write_str(
                "there is no Spotify streaming history there: no Streaming_History_Audio file",
            ),
            Self::Unreadable(name) => write!(f, "{name}: it cannot be read"),
            Self::File(name, error) => write!(f, "{name}: {error}"),
        }
    }
}

/// Calls `each` with each audio history file's own name and its bytes, in
/// the order of their names, and returns how many files of video history
/// it passed over.
pub(crate) fn each_file(
    path: &Path,
    mut each: impl FnMut(&str, &[u8]) -> Result<(), Error>,
) -> Result<u64, Error> {
    let metadata = std::fs::metadata(path).map_err(Error::Open)?;
    if metadata.is_dir() {
        let mut found = Vec::new();
        walk(path, DEPTH, &mut found)?;
        let mut files: Vec<(String, PathBuf)> = found
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_owned();
                Some((name, path))
            })
            .collect();
        files.sort();
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        let video = check(&names)?;
        for (name, path) in &files {
            if kind(name) == Kind::Extended {
                let bytes = std::fs::read(path).map_err(|_| Error::Unreadable(name.clone()))?;
                each(name, &bytes)?;
            }
        }
        return Ok(video);
    }
    let file = File::open(path).map_err(Error::Open)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|_| Error::NotZip)?;
    let mut entries: Vec<(String, usize)> = (0..zip.len())
        .filter_map(|at| {
            let entry = zip.by_index_raw(at).ok()?;
            let name = entry.name().ok()?;
            let base = name.rsplit(['/', '\\']).next().unwrap_or(&name).to_owned();
            entry.is_file().then_some((base, at))
        })
        .collect();
    entries.sort();
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    let video = check(&names)?;
    for (name, at) in &entries {
        if kind(name) != Kind::Extended {
            continue;
        }
        let mut bytes = Vec::new();
        zip.by_index(*at)
            .ok()
            .and_then(|mut entry| entry.read_to_end(&mut bytes).ok())
            .ok_or_else(|| Error::Unreadable(name.clone()))?;
        each(name, &bytes)?;
    }
    Ok(video)
}

/// Refuses an archive with no extended history of audio, and says which
/// export to ask for when it holds the account data's. Returns how many
/// files of video history it holds.
fn check(names: &[&str]) -> Result<u64, Error> {
    let kinds: Vec<Kind> = names.iter().map(|name| kind(name)).collect();
    if kinds.contains(&Kind::Extended) {
        Ok(kinds.iter().filter(|kind| **kind == Kind::Video).count() as u64)
    } else if kinds.contains(&Kind::AccountData) {
        Err(Error::AccountData)
    } else {
        Err(Error::NoHistory)
    }
}

/// The files under a folder, to a depth.
fn walk(folder: &Path, depth: usize, found: &mut Vec<PathBuf>) -> Result<(), Error> {
    for entry in std::fs::read_dir(folder).map_err(Error::Open)? {
        let entry = entry.map_err(Error::Open)?;
        let kind = entry.file_type().map_err(Error::Open)?;
        if kind.is_dir() && depth > 0 {
            walk(&entry.path(), depth - 1, found)?;
        } else if kind.is_file() {
            found.push(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_known_by_its_name() {
        for (name, want) in [
            (
                "Spotify Extended Streaming History/Streaming_History_Audio_2014_1.json",
                Kind::Extended,
            ),
            ("Streaming_History_Video_2020.json", Kind::Video),
            ("MyData/endsong_0.json", Kind::Extended),
            (
                "Spotify Account Data/StreamingHistory_music_0.json",
                Kind::AccountData,
            ),
            ("StreamingHistory0.json", Kind::AccountData),
            ("ReadMeFirst_ExtendedStreamingHistory.pdf", Kind::Other),
            ("Streaming_History_Audio_2014.pdf", Kind::Other),
            ("Playlist1.json", Kind::Other),
        ] {
            assert_eq!(kind(name), want, "{name}");
        }
    }

    #[test]
    fn the_account_data_is_refused_by_name() {
        assert!(matches!(
            check(&["StreamingHistory_music_0.json", "Userdata.json"]),
            Err(Error::AccountData)
        ));
        assert!(matches!(check(&["Userdata.json"]), Err(Error::NoHistory)));
        assert_eq!(
            check(&["StreamingHistory0.json", "endsong_0.json"]).unwrap(),
            0
        );
        assert_eq!(
            check(&[
                "Streaming_History_Audio_2020.json",
                "Streaming_History_Video_2020.json"
            ])
            .unwrap(),
            1
        );
        assert!(matches!(
            check(&["Streaming_History_Video_2020.json"]),
            Err(Error::NoHistory)
        ));
    }
}
