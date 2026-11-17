//! What the player reads from the library: a track's copies on this server,
//! what a track shows, and the tracks of an album or of a folder in the
//! order they are played. See "Choosing a copy" and "The queue" in
//! docs/design/playback.md.

use ritornel_domain::selection::{self, Candidate};
use rusqlite::{Connection, OptionalExtension, Row};

use crate::playback::renderer::FileFacts;
use crate::{Error, Id, LibraryStore};

/// What a track shows: the displayed fields of an appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDisplay {
    /// The appearance.
    pub appearance: Id,
    /// Its recording.
    pub recording: Option<Id>,
    /// Its release, unless it is a loose file.
    pub release: Option<Id>,
    /// The title.
    pub title: Option<String>,
    /// The artist credit, as one string.
    pub artist: Option<String>,
    /// The album's title.
    pub album: Option<String>,
    /// The disc number.
    pub disc: Option<i64>,
    /// The track number.
    pub track: Option<i64>,
    /// The length.
    pub duration_ms: Option<i64>,
    /// The cover of its release.
    pub art: Option<Id>,
    /// The listener's stars for its recording, 0 to 100.
    pub rating: Option<i64>,
    /// Whether its recording is loved.
    pub loved: bool,
    /// Its recording's canonical key, which what is said of it is read by.
    recording_key: Option<i64>,
}

/// One copy of a track on this server: a master file, where it is and
/// what the listen names when it plays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Copy {
    pub(crate) candidate: Candidate,
    pub(crate) source_item: Id,
    pub(crate) revision_no: i64,
    pub(crate) asset: Id,
    pub(crate) duration_ms: i64,
    /// The root's name, which this server's `server.db` maps to a folder.
    pub(crate) root: String,
    /// The path under the root.
    pub(crate) rel_path: String,
    /// What the scan that read this revision recorded of the file.
    pub(crate) facts: FileFacts,
}

const DISPLAY_COLUMNS: &str = "ta.public_id, r.public_id, rel.public_id, ta.display_title,
    ac.display_text, rel.display_title, ta.display_disc_number, ta.display_track_number,
    ta.display_duration_ms, pic.public_id, r.id";

const DISPLAY_JOINS: &str = "FROM track_appearance ta
    JOIN recording r ON r.id = (SELECT canonical_id FROM recording WHERE id = ta.recording_id)
    LEFT JOIN release rel ON rel.id = ta.release_id
    LEFT JOIN picture pic ON pic.id = rel.display_picture_id
    LEFT JOIN artist_credit ac ON ac.id = ta.display_artist_credit_id";

/// A track as [`DISPLAY_COLUMNS`] reads it. What is said of it is read
/// after, by [`with_said`].
fn display(row: &Row<'_>) -> rusqlite::Result<TrackDisplay> {
    let id = |column: usize| -> rusqlite::Result<Option<Id>> {
        Ok(row.get::<_, Option<[u8; 16]>>(column)?.map(Id::from_bytes))
    };
    Ok(TrackDisplay {
        appearance: Id::from_bytes(row.get(0)?),
        recording: id(1)?,
        release: id(2)?,
        title: row.get(3)?,
        artist: row.get(4)?,
        album: row.get(5)?,
        disc: row.get(6)?,
        track: row.get(7)?,
        duration_ms: row.get(8)?,
        art: id(9)?,
        rating: None,
        loved: false,
        recording_key: row.get(10)?,
    })
}

/// Fills in what the listener has said of each track's recording.
fn with_said<'a>(
    connection: &Connection,
    tracks: impl IntoIterator<Item = &'a mut TrackDisplay>,
) -> Result<(), Error> {
    let tracks: Vec<&mut TrackDisplay> = tracks.into_iter().collect();
    let keys: Vec<i64> = tracks
        .iter()
        .filter_map(|track| track.recording_key)
        .collect();
    let said = crate::ratings::said(connection, &keys)?;
    for track in tracks {
        if let Some(found) = track.recording_key.and_then(|key| said.get(&key)) {
            track.rating = found.rating;
            track.loved = found.loved;
        }
    }
    Ok(())
}

/// What an appearance shows, followed through its redirect.
fn track(connection: &Connection, appearance: Id) -> Result<Option<TrackDisplay>, Error> {
    let mut found = connection
        .query_row(
            &format!(
                "SELECT {DISPLAY_COLUMNS} {DISPLAY_JOINS}
                 WHERE ta.id = (SELECT canonical_id FROM track_appearance WHERE public_id = ?1)"
            ),
            [appearance.as_bytes()],
            display,
        )
        .optional()?;
    with_said(connection, found.iter_mut())?;
    Ok(found)
}

/// Sorts tracks the way a person reads an album: by disc and track, with
/// what has no number after what has one, then by the order given.
fn album_order(tracks: &mut [(TrackDisplay, String)]) {
    tracks.sort_by(|(a, a_path), (b, b_path)| {
        let key = |track: &TrackDisplay| {
            (
                track.disc.is_none(),
                track.disc,
                track.track.is_none(),
                track.track,
            )
        };
        key(a).cmp(&key(b)).then_with(|| a_path.cmp(b_path))
    });
}

impl LibraryStore {
    /// What an appearance shows, followed through its redirect. `None` when
    /// there is no such appearance.
    pub fn track(&self, appearance: Id) -> Result<Option<TrackDisplay>, Error> {
        self.read(|connection| track(connection, appearance))
    }

    /// The picture an appearance's release shows, by its art ID, followed
    /// through the appearance's redirect.
    pub fn track_art(&self, appearance: Id) -> Result<Option<Id>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT p.public_id FROM track_appearance ta
                     JOIN release r ON r.id = ta.release_id
                     JOIN picture p ON p.id = r.display_picture_id
                     WHERE ta.id = (SELECT canonical_id FROM track_appearance WHERE public_id = ?1)",
                    [appearance.as_bytes()],
                    |row| row.get::<_, [u8; 16]>(0),
                )
                .optional()?
                .map(Id::from_bytes))
        })
    }

    /// What each appearance shows, in the order given: `None` for one the
    /// library does not have.
    pub fn tracks(&self, appearances: &[Id]) -> Result<Vec<Option<TrackDisplay>>, Error> {
        self.read(|connection| {
            let mut statement = connection.prepare_cached(&format!(
                "SELECT {DISPLAY_COLUMNS} {DISPLAY_JOINS}
                 WHERE ta.id = (SELECT canonical_id FROM track_appearance WHERE public_id = ?1)"
            ))?;
            let mut found = appearances
                .iter()
                .map(|appearance| {
                    statement
                        .query_row([appearance.as_bytes()], display)
                        .optional()
                })
                .collect::<Result<Vec<_>, _>>()?;
            with_said(connection, found.iter_mut().flatten())?;
            Ok(found)
        })
    }

    /// Which of `appearances` the library does not have, if any.
    pub(crate) fn unknown_appearance(&self, appearances: &[Id]) -> Result<Option<Id>, Error> {
        self.read(|connection| {
            let mut statement =
                connection.prepare_cached("SELECT 1 FROM track_appearance WHERE public_id = ?1")?;
            for appearance in appearances {
                if !statement.exists([appearance.as_bytes()])? {
                    return Ok(Some(*appearance));
                }
            }
            Ok(None)
        })
    }

    /// Reads again what is said of a track: after a rating is set.
    pub fn said_again(&self, track: &mut TrackDisplay) -> Result<(), Error> {
        self.read(|connection| with_said(connection, std::iter::once(track)))
    }

    /// The listener a new player plays for: the library's first.
    pub(crate) fn first_listener(&self) -> Result<Option<Id>, Error> {
        self.read(|connection| {
            Ok(connection
                .query_row(
                    "SELECT public_id FROM listener ORDER BY id LIMIT 1",
                    [],
                    |row| row.get::<_, [u8; 16]>(0),
                )
                .optional()?
                .map(Id::from_bytes))
        })
    }

    /// The copies of an appearance on the server `server`, best first:
    /// every file bound to it, or to an appearance merged into it, that is
    /// in one of that server's roots and was found by the last scan.
    pub(crate) fn copies(&self, appearance: Id, server: Id) -> Result<Vec<Copy>, Error> {
        let found: Vec<Copy> = self.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT si.id, si.public_id, sr.revision_no, aa.id, aa.public_id, aa.lossless,
                        aa.sample_rate_hz, aa.bit_depth, aa.bitrate_kbps, aa.duration_ms,
                        si.missing_since_ms IS NOT NULL, lr.name, si.provider_key,
                        si.size_bytes, si.mtime_ms
                 FROM assignment a
                 JOIN source_item si ON si.id = a.source_item_id
                 JOIN source_item_revision sr ON sr.id = si.current_revision_id
                 JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                 JOIN source_instance src ON src.id = si.source_instance_id
                 JOIN library_root lr ON lr.id = src.root_id
                 JOIN server s ON s.id = lr.server_id
                 WHERE a.superseded_at_ms IS NULL
                   AND si.retired_at_ms IS NULL
                   AND si.item_kind = 'file'
                   AND lr.kind = 'master'
                   AND s.public_id = ?2
                   AND a.appearance_id IN (
                     SELECT id FROM track_appearance WHERE canonical_id =
                       (SELECT canonical_id FROM track_appearance WHERE public_id = ?1))",
            )?;
            let rows = statement.query_map((appearance.as_bytes(), server.as_bytes()), |row| {
                Ok(Copy {
                    candidate: Candidate {
                        source_item: row.get(0)?,
                        asset: row.get(3)?,
                        lossless: row.get(5)?,
                        sample_rate_hz: row.get(6)?,
                        bit_depth: row.get(7)?,
                        bitrate_kbps: row.get(8)?,
                        missing: row.get(10)?,
                    },
                    source_item: Id::from_bytes(row.get(1)?),
                    revision_no: row.get(2)?,
                    asset: Id::from_bytes(row.get(4)?),
                    duration_ms: row.get(9)?,
                    root: row.get(11)?,
                    rel_path: row.get(12)?,
                    // An item the scan left without them matches no file.
                    facts: FileFacts {
                        size_bytes: row
                            .get::<_, Option<i64>>(13)?
                            .and_then(|size| u64::try_from(size).ok())
                            .unwrap_or(u64::MAX),
                        mtime_ms: row.get::<_, Option<i64>>(14)?.unwrap_or(i64::MIN),
                    },
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let candidates: Vec<Candidate> = found.iter().map(|copy| copy.candidate.clone()).collect();
        Ok(selection::select(&candidates)
            .into_iter()
            .filter_map(|chosen| {
                found
                    .iter()
                    .find(|copy| copy.candidate.source_item == chosen.source_item)
                    .cloned()
            })
            .collect())
    }

    /// The tracks of a release, in disc and track order. `None` when there
    /// is no such release.
    pub fn release_tracks(&self, release: Id) -> Result<Option<Vec<TrackDisplay>>, Error> {
        self.read(|connection| {
            let canonical: Option<i64> = connection
                .query_row(
                    "SELECT canonical_id FROM release WHERE public_id = ?1",
                    [release.as_bytes()],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(canonical) = canonical else {
                return Ok(None);
            };
            let mut statement = connection.prepare(&format!(
                "SELECT {DISPLAY_COLUMNS} {DISPLAY_JOINS}
                 WHERE ta.redirect_to_id IS NULL
                   AND ta.release_id IN (SELECT id FROM release WHERE canonical_id = ?1)"
            ))?;
            let mut tracks: Vec<(TrackDisplay, String)> = statement
                .query_map([canonical], display)?
                .map(|row| row.map(|track| (track, String::new())))
                .collect::<Result<_, _>>()?;
            with_said(connection, tracks.iter_mut().map(|(track, _)| track))?;
            album_order(&mut tracks);
            Ok(Some(tracks.into_iter().map(|(track, _)| track).collect()))
        })
    }

    /// The tracks of the files under `rel_prefix` in the root `root` of the
    /// server `server`, in the order they are played: album by album, each
    /// in disc and track order, the albums in the order of their files'
    /// paths, and a file of no album where its path puts it. A file with no
    /// binding yet is left out, and so is a second file of one track.
    pub fn folder_tracks(
        &self,
        root: &str,
        rel_prefix: &str,
        server: Id,
    ) -> Result<Vec<TrackDisplay>, Error> {
        let (root, prefix) = (root.to_owned(), rel_prefix.trim_matches('/').to_owned());
        self.read(|connection| {
            // The files under the prefix are a range of the live-path
            // index: the prefix and a slash, up to the prefix and the
            // character after the slash.
            let (from, to) = if prefix.is_empty() {
                (String::new(), "\u{10ffff}".to_owned())
            } else {
                (format!("{prefix}/"), format!("{prefix}0"))
            };
            let mut statement = connection.prepare(
                "SELECT ta.public_id, r.public_id, rel.public_id, ta.display_title,
                        ac.display_text, rel.display_title, ta.display_disc_number,
                        ta.display_track_number, ta.display_duration_ms, pic.public_id, r.id,
                        si.provider_key
                 FROM server s
                 JOIN library_root lr ON lr.server_id = s.id AND lr.name = ?1
                 JOIN source_instance src ON src.root_id = lr.id
                 JOIN source_item si ON si.source_instance_id = src.id
                                    AND si.item_kind = 'file'
                                    AND si.retired_at_ms IS NULL
                 JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 JOIN track_appearance queued ON queued.id = a.appearance_id
                 JOIN track_appearance ta ON ta.id = queued.canonical_id
                 JOIN recording r ON r.id = (SELECT canonical_id FROM recording WHERE id = ta.recording_id)
                 LEFT JOIN release rel ON rel.id = ta.release_id
                 LEFT JOIN picture pic ON pic.id = rel.display_picture_id
                 LEFT JOIN artist_credit ac ON ac.id = ta.display_artist_credit_id
                 WHERE s.public_id = ?2
                   AND (si.provider_key = ?3 OR (si.provider_key >= ?4 AND si.provider_key < ?5))",
            )?;
            let mut rows: Vec<(TrackDisplay, String)> = statement
                .query_map((&root, server.as_bytes(), &prefix, &from, &to), |row| {
                    Ok((display(row)?, row.get(11)?))
                })?
                .collect::<Result<_, _>>()?;
            with_said(connection, rows.iter_mut().map(|(track, _)| track))?;
            // Each release is placed where its first file is.
            let mut first_path: std::collections::HashMap<Option<Id>, String> =
                std::collections::HashMap::new();
            for (track, path) in &rows {
                let slot = first_path.entry(track.release).or_insert_with(|| path.clone());
                if path < slot {
                    slot.clone_from(path);
                }
            }
            let mut seen = std::collections::HashSet::new();
            let mut tracks: Vec<(TrackDisplay, String)> = rows
                .into_iter()
                .filter(|(track, _)| seen.insert(track.appearance))
                .collect();
            tracks.sort_by(|(a, a_path), (b, b_path)| {
                let place = |track: &TrackDisplay, path: &String| match track.release {
                    Some(_) => first_path.get(&track.release).cloned().unwrap_or_default(),
                    None => path.clone(),
                };
                place(a, a_path).cmp(&place(b, b_path))
            });
            // Within each release, disc and track order.
            let mut ordered = Vec::with_capacity(tracks.len());
            let mut at = 0;
            while at < tracks.len() {
                let release = tracks.get(at).and_then(|(track, _)| track.release);
                let mut end = at + 1;
                while release.is_some()
                    && tracks.get(end).is_some_and(|(track, _)| track.release == release)
                {
                    end += 1;
                }
                let mut group: Vec<(TrackDisplay, String)> =
                    tracks.get(at..end).map(<[_]>::to_vec).unwrap_or_default();
                album_order(&mut group);
                ordered.extend(group.into_iter().map(|(track, _)| track));
                at = end;
            }
            Ok(ordered)
        })
    }
}
