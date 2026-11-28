//! What the library has that a target could take, and which of it each
//! source selects. See "The subset" in docs/internals/device-sync.md.
//!
//! Nothing here opens a file: every row is read from `library.db`, so a
//! plan of a whole library is a few queries.

use std::collections::{HashMap, HashSet};

use ritornel_contract::{SyncSelector, SyncSourceRole, SyncUnit};
use ritornel_domain::selection::Candidate;
use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Id};

/// One copy of a track on the hub: a master file, as the last scan left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRow {
    pub source_item: i64,
    pub source_item_pid: Id,
    pub revision_no: i64,
    /// The row of the revision the scan last read, whose stored payload
    /// holds the file's tags.
    pub revision: i64,
    pub asset: i64,
    pub codec: String,
    pub lossless: bool,
    pub sample_rate_hz: i64,
    pub bit_depth: Option<i64>,
    pub bitrate_kbps: Option<i64>,
    pub missing: bool,
    /// The root's name, which the hub's `server.db` maps to a folder.
    pub root: String,
    pub rel_path: String,
    pub size_bytes: u64,
    pub mtime_ms: i64,
    pub hash_scheme: u32,
    pub audio_hash: [u8; 32],
}

impl CopyRow {
    /// The master this copy is, on a server whose roots are `roots` by
    /// name: `None` for a root that server has no folder for.
    pub fn master(
        &self,
        roots: &HashMap<String, std::path::PathBuf>,
    ) -> Option<crate::transcode::Master> {
        Some(crate::transcode::Master {
            path: crate::walk::under(roots.get(&self.root)?, &self.rel_path),
            asset: self.asset(),
            facts: crate::playback::FileFacts {
                size_bytes: self.size_bytes,
                mtime_ms: self.mtime_ms,
            },
        })
    }

    /// The audio it holds, as the encode cache keys it.
    pub fn asset(&self) -> crate::transcode::AudioAsset {
        crate::transcode::AudioAsset {
            scheme: self.hash_scheme,
            hash: self.audio_hash.into(),
        }
    }

    pub(crate) fn candidate(&self) -> Candidate {
        Candidate {
            source_item: self.source_item,
            asset: self.asset,
            lossless: self.lossless,
            sample_rate_hz: self.sample_rate_hz,
            bit_depth: self.bit_depth,
            bitrate_kbps: self.bitrate_kbps,
            missing: self.missing,
        }
    }
}

/// A picture a track's album shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Art {
    pub id: Id,
    pub hash: [u8; 32],
}

/// One track the library can play from the hub, with what its file on a
/// target is named and tagged by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackRow {
    pub appearance: i64,
    pub appearance_pid: Id,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: Option<u16>,
    pub disc: Option<u32>,
    pub track: Option<u32>,
    pub duration_ms: u64,
    pub added_at_ms: Option<i64>,
    /// Its release, or none for a loose file.
    pub release: Option<i64>,
    pub release_pid: Option<Id>,
    pub art: Option<Art>,
    pub copies: Vec<CopyRow>,
    /// Whether its album has more than one disc.
    pub discs: bool,
    /// How many tracks its album has in the library.
    pub album_tracks: u32,
}

impl TrackRow {
    /// What its album is listed under: its release, or itself when loose.
    pub fn group_key(&self) -> String {
        match self.release {
            Some(release) => format!("r{release}"),
            None => format!("t{}", self.appearance),
        }
    }
}

fn narrow<T: TryFrom<i64>>(value: Option<i64>) -> Option<T> {
    value.and_then(|value| T::try_from(value).ok())
}

/// Every track with a master in one of the hub's roots, in the order a
/// person files records: by album artist, year, album, disc and track.
/// That order is total, so a source cut short takes the same records every
/// time.
pub(crate) fn tracks(connection: &Connection, hub: Id) -> Result<Vec<TrackRow>, Error> {
    let mut statement = connection.prepare(
        "SELECT ta.id, ta.public_id, ta.display_title, tac.display_text,
                rel.id, rel.public_id, rel.display_title, relc.display_text,
                coalesce(rel.display_year, rg.display_first_year),
                ta.display_disc_number, ta.display_track_number, ta.display_duration_ms,
                ta.display_added_at_ms, pic.public_id, pic.content_hash,
                si.id, si.public_id, sr.revision_no, sr.id, aa.id, aa.codec, aa.lossless,
                aa.sample_rate_hz, aa.bit_depth, aa.bitrate_kbps, aa.duration_ms,
                si.missing_since_ms IS NOT NULL, lr.name, si.provider_key, si.size_bytes,
                si.mtime_ms, aa.hash_scheme, aa.audio_hash
         FROM assignment a
         JOIN source_item si ON si.id = a.source_item_id
         JOIN source_item_revision sr ON sr.id = si.current_revision_id
         JOIN audio_asset aa ON aa.id = sr.audio_asset_id
         JOIN source_instance src ON src.id = si.source_instance_id
         JOIN library_root lr ON lr.id = src.root_id
         JOIN server s ON s.id = lr.server_id
         JOIN track_appearance named ON named.id = a.appearance_id
         JOIN track_appearance ta ON ta.id = named.canonical_id
         LEFT JOIN artist_credit tac ON tac.id = ta.display_artist_credit_id
         LEFT JOIN release own ON own.id = ta.release_id
         LEFT JOIN release rel ON rel.id = own.canonical_id
         LEFT JOIN release_group rg ON rg.id = rel.release_group_id
         LEFT JOIN artist_credit relc ON relc.id = rel.display_artist_credit_id
         LEFT JOIN picture pic ON pic.id = rel.display_picture_id
         WHERE a.superseded_at_ms IS NULL
           AND si.retired_at_ms IS NULL
           AND si.item_kind = 'file'
           AND lr.kind = 'master'
           AND s.public_id = ?1",
    )?;
    let mut rows = statement.query([hub.as_bytes()])?;
    let mut found: HashMap<i64, TrackRow> = HashMap::new();
    while let Some(row) = rows.next()? {
        let appearance: i64 = row.get(0)?;
        let copy = CopyRow {
            source_item: row.get(15)?,
            source_item_pid: Id::from_bytes(row.get(16)?),
            revision_no: row.get(17)?,
            revision: row.get(18)?,
            asset: row.get(19)?,
            codec: row.get(20)?,
            lossless: row.get(21)?,
            sample_rate_hz: row.get(22)?,
            bit_depth: row.get(23)?,
            bitrate_kbps: row.get(24)?,
            missing: row.get(26)?,
            root: row.get(27)?,
            rel_path: row.get(28)?,
            size_bytes: narrow(row.get(29)?).unwrap_or(0),
            mtime_ms: row.get::<_, Option<i64>>(30)?.unwrap_or(i64::MIN),
            hash_scheme: row.get(31)?,
            audio_hash: row.get(32)?,
        };
        if let Some(track) = found.get_mut(&appearance) {
            track.copies.push(copy);
            continue;
        }
        let artist: Option<String> = row.get(3)?;
        let album_artist: Option<String> = row.get(7)?;
        let art = match (
            row.get::<_, Option<[u8; 16]>>(13)?,
            row.get::<_, Option<Vec<u8>>>(14)?,
        ) {
            (Some(id), Some(hash)) => <[u8; 32]>::try_from(hash).ok().map(|hash| Art {
                id: Id::from_bytes(id),
                hash,
            }),
            _ => None,
        };
        found.insert(
            appearance,
            TrackRow {
                appearance,
                appearance_pid: Id::from_bytes(row.get(1)?),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                album_artist: album_artist
                    .clone()
                    .or_else(|| artist.clone())
                    .unwrap_or_default(),
                artist: artist.or(album_artist).unwrap_or_default(),
                release: row.get(4)?,
                release_pid: row.get::<_, Option<[u8; 16]>>(5)?.map(Id::from_bytes),
                album: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                year: narrow(row.get(8)?),
                disc: narrow(row.get(9)?),
                track: narrow(row.get(10)?),
                duration_ms: narrow(row.get::<_, Option<i64>>(11)?.or(row.get(25)?)).unwrap_or(0),
                added_at_ms: row.get(12)?,
                art,
                copies: vec![copy],
                discs: false,
                album_tracks: 1,
            },
        );
    }
    let mut tracks: Vec<TrackRow> = found.into_values().collect();
    // What each album comes to: how many tracks, and whether several discs.
    let mut albums: HashMap<i64, (u32, Option<u32>, bool)> = HashMap::new();
    for track in &tracks {
        if let Some(release) = track.release {
            let album = albums.entry(release).or_insert((0, track.disc, false));
            album.0 += 1;
            album.2 |= track.disc != album.1;
        }
    }
    for track in &mut tracks {
        if let Some((count, _, discs)) = track.release.and_then(|release| albums.get(&release)) {
            track.album_tracks = *count;
            track.discs = *discs;
        }
        track.copies.sort_by_key(|copy| copy.source_item);
    }
    tracks.sort_by(|a, b| {
        let key = |track: &TrackRow| {
            (
                track.album_artist.to_lowercase(),
                track.year.is_none(),
                track.year,
                track.album.to_lowercase(),
                track.release,
                track.disc.is_none(),
                track.disc,
                track.track.is_none(),
                track.track,
                track.appearance,
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(tracks)
}

/// One source of a target as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    pub id: Id,
    pub role: SyncSourceRole,
    pub selector: SyncSelector,
    pub unit: SyncUnit,
    /// What it selects, in words, and a second line and a cover for it.
    pub label: String,
    pub detail: Option<String>,
    pub art: Option<Id>,
}

/// What a selector selects, worked out once for a plan.
#[derive(Debug, Clone, Default)]
pub enum Matcher {
    #[default]
    Everything,
    /// The releases named, and the loose tracks named.
    Records {
        releases: HashSet<i64>,
        loose: HashSet<i64>,
    },
    AddedSince(i64),
}

impl Matcher {
    pub(crate) fn takes(&self, track: &TrackRow) -> bool {
        match self {
            Self::Everything => true,
            Self::Records { releases, loose } => match track.release {
                Some(release) => releases.contains(&release),
                None => loose.contains(&track.appearance),
            },
            Self::AddedSince(since) => track.added_at_ms.is_some_and(|added| added >= *since),
        }
    }
}

fn keys(connection: &Connection, sql: &str, id: Id) -> Result<HashSet<i64>, Error> {
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map([id.as_bytes()], |row| row.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Resolves a selector against the library as it is now. A selector that
/// names something the library no longer has selects nothing.
pub(crate) fn matcher(
    connection: &Connection,
    selector: SyncSelector,
    now_ms: i64,
) -> Result<Matcher, Error> {
    Ok(match selector {
        SyncSelector::Everything => Matcher::Everything,
        SyncSelector::AddedWithin { days } => {
            Matcher::AddedSince(now_ms.saturating_sub(i64::from(days).saturating_mul(86_400_000)))
        }
        // Every record whose credit names the artist, and their loose
        // tracks.
        SyncSelector::Artist { artist_id } => Matcher::Records {
            releases: keys(
                connection,
                "SELECT r.canonical_id FROM release r
                 JOIN artist_credit_name acn ON acn.artist_credit_id = r.display_artist_credit_id
                 JOIN artist a ON a.id = acn.artist_id
                 WHERE a.canonical_id = (SELECT canonical_id FROM artist WHERE public_id = ?1)",
                artist_id,
            )?,
            loose: keys(
                connection,
                "SELECT ta.canonical_id FROM track_appearance ta
                 JOIN artist_credit_name acn ON acn.artist_credit_id = ta.display_artist_credit_id
                 JOIN artist a ON a.id = acn.artist_id
                 WHERE ta.release_id IS NULL
                   AND a.canonical_id = (SELECT canonical_id FROM artist WHERE public_id = ?1)",
                artist_id,
            )?,
        },
        // A record is a release group, every version of it, or one release.
        SyncSelector::Album { album_id } => Matcher::Records {
            releases: keys(
                connection,
                "SELECT r.canonical_id FROM release r
                 JOIN release_group g ON g.id = r.release_group_id
                 WHERE g.public_id = ?1
                 UNION
                 SELECT canonical_id FROM release WHERE public_id = ?1",
                album_id,
            )?,
            loose: HashSet::new(),
        },
    })
}

/// A selector in words: an artist's name, or a record's title with its
/// artist and cover. Empty where the selector says it all, and where what
/// it names is gone.
pub(crate) fn label(
    connection: &Connection,
    selector: SyncSelector,
) -> Result<(String, Option<String>, Option<Id>), Error> {
    Ok(match selector {
        SyncSelector::Everything | SyncSelector::AddedWithin { .. } => (String::new(), None, None),
        SyncSelector::Artist { artist_id } => (
            connection
                .query_row(
                    "SELECT c.display_name FROM artist a JOIN artist c ON c.id = a.canonical_id
                     WHERE a.public_id = ?1",
                    [artist_id.as_bytes()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten()
                .unwrap_or_default(),
            None,
            None,
        ),
        SyncSelector::Album { album_id } => connection
            .query_row(
                "SELECT r.display_title, ac.display_text, p.public_id
                 FROM release r
                 LEFT JOIN artist_credit ac ON ac.id = r.display_artist_credit_id
                 LEFT JOIN picture p ON p.id = r.display_picture_id
                 WHERE r.id = coalesce(
                   (SELECT coalesce(g.display_release_id,
                                    (SELECT min(id) FROM release WHERE release_group_id = g.id))
                    FROM release_group g WHERE g.public_id = ?1),
                   (SELECT canonical_id FROM release WHERE public_id = ?1))",
                [album_id.as_bytes()],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<[u8; 16]>>(2)?.map(Id::from_bytes),
                    ))
                },
            )
            .optional()?
            .unwrap_or_default(),
    })
}
