//! A small invented library made by hand, beside the drawn one, for judging
//! an artist's page and an album's: an artist with one record, with three,
//! and with twenty and more; records of every kind and of no known kind,
//! which is how most of a real library is tagged; live records,
//! collections, and records of others an artist appears on; records on
//! several discs, in several versions, with no year, with very long titles
//! and with no cover; songs on no record; and names in several scripts.
//!
//! Everything here is invented for the purpose, as the word lists are:
//! nothing is taken from a real catalogue, and a name that happens to be
//! real is chance. It is written as [`seed`](super::seed) writes, through
//! [`LibraryStore::write`], and its covers are painted the same way.

use std::path::Path;

use super::{
    Album, Artist, InventError, Invented, Kind, Rng, Rows, Statements, Track, add_root, album_dir,
    paint, request, title,
};
use crate::{LibraryStore, ServerStore};

/// One record, as written here.
struct Record {
    /// The artist, by index in [`ARTISTS`]; `None` for one of several.
    by: Option<usize>,
    title: &'static str,
    /// The year it first came out; `None` for a record whose files say not.
    year: Option<i64>,
    /// What kind it is, where its files say; `None` where they do not.
    kind: Option<Kind>,
    marks: &'static [&'static str],
    tracks: usize,
    discs: i64,
    /// Later versions: the edition, its year, and how many songs it adds.
    versions: &'static [(&'static str, i64, usize)],
    /// Whether it has a cover.
    cover: bool,
    /// Songs on it credited to another artist here: the track's number
    /// from 1, and the artist. A record of several artists has every track
    /// credited, round the artists named.
    guests: &'static [(usize, usize)],
}

const fn record(by: usize, title: &'static str, year: i64, tracks: usize) -> Record {
    Record {
        by: Some(by),
        title,
        year: Some(year),
        kind: None,
        marks: &[],
        tracks,
        discs: 1,
        versions: &[],
        cover: true,
        guests: &[],
    }
}

/// The artists: their names, and the names they are filed under.
const ARTISTS: &[(&str, Option<&str>)] = &[
    // 0: one record.
    ("Ilse Marchetti", Some("Marchetti, Ilse")),
    // 1: three albums of no known kind, and two singles that say so.
    ("Odile Castellan", Some("Castellan, Odile")),
    // 2: twenty-two records, none of which says what kind it is.
    ("Søren Dunmore Ensemble", None),
    // 3: every kind there is, each said in the files.
    ("Calla Brandt", Some("Brandt, Calla")),
    // 4: no covers at all.
    ("Bram Kowal Trio", None),
    // 5 to 8: names in other scripts.
    ("Мира Кесслер", None),
    ("ナカムラ・ジュノ", None),
    ("Θέα Λίντκβιστ", None),
    ("카스텔란 사중주", None),
    // 9: a very long name.
    (
        "The Underhill & Granger Society for Lost Afternoons Orchestra",
        None,
    ),
];

const ALBUM: Option<Kind> = Some(Kind::Album);
const EP: Option<Kind> = Some(Kind::Ep);
const SINGLE: Option<Kind> = Some(Kind::Single);

const RECORDS: &[Record] = &[
    record(0, "Harbour Lights at Dusk", 2019, 11),
    // Odile Castellan.
    record(1, "Small Hours", 2008, 10),
    Record {
        guests: &[(4, 3)],
        ..record(1, "The Glass Orchard", 2012, 12)
    },
    record(1, "Weather for Two", 2017, 9),
    Record {
        kind: SINGLE,
        ..record(1, "Lantern", 2016, 2)
    },
    Record {
        kind: SINGLE,
        ..record(1, "Paper Moons", 2018, 1)
    },
    // Søren Dunmore Ensemble: twenty-two records, no kinds.
    record(2, "First Light on the Fjord", 1979, 9),
    record(2, "Halvlys", 1981, 8),
    Record {
        versions: &[("Remastered", 2009, 0), ("Deluxe Edition", 2014, 5)],
        ..record(2, "Northern Window", 1984, 11)
    },
    record(2, "Salt and Iron", 1986, 10),
    record(2, "Tidewater", 1988, 4),
    record(2, "The Quiet Engine", 1990, 12),
    Record {
        discs: 3,
        ..record(2, "Collected Rooms", 1993, 34)
    },
    record(2, "Lamplighter", 1995, 10),
    record(2, "Winter Ferries", 1997, 5),
    record(2, "Maps of Nowhere", 1999, 13),
    record(
        2,
        "Everything We Carried Down From the Mountain in the Winter of the Long Rain, and What We Left Behind",
        2002,
        14,
    ),
    record(2, "Glass Harbour", 2004, 9),
    record(2, "Thirteen Lanterns", 2006, 13),
    record(2, "Slow Weather", 2009, 6),
    record(2, "Ashore", 2011, 10),
    Record {
        discs: 2,
        ..record(2, "The Long Table", 2014, 22)
    },
    record(2, "Undertow", 2016, 11),
    record(2, "Small Mercies", 2019, 4),
    record(2, "Late Harvest", 2021, 10),
    record(2, "Afterglow, Again", 2024, 9),
    Record {
        year: None,
        ..record(2, "Rooms Without Doors", 0, 7)
    },
    Record {
        year: None,
        cover: false,
        ..record(2, "Tapes from the Boathouse", 0, 12)
    },
    // Calla Brandt: every kind, each said.
    Record {
        kind: ALBUM,
        ..record(3, "Copper Sky", 2010, 11)
    },
    Record {
        kind: EP,
        ..record(3, "Four Rooms", 2011, 4)
    },
    Record {
        kind: ALBUM,
        guests: &[(6, 2)],
        ..record(3, "The Orchard Keeper", 2013, 12)
    },
    Record {
        kind: EP,
        ..record(3, "Morning Static", 2015, 5)
    },
    Record {
        kind: ALBUM,
        versions: &[("Anniversary Edition", 2022, 4)],
        ..record(3, "Low Country", 2017, 10)
    },
    Record {
        kind: ALBUM,
        ..record(3, "Weathervane", 2020, 11)
    },
    Record {
        kind: SINGLE,
        ..record(3, "Copper Sky", 2010, 2)
    },
    Record {
        kind: SINGLE,
        ..record(3, "Halfway Bright", 2017, 1)
    },
    Record {
        kind: SINGLE,
        ..record(3, "Static", 2023, 3)
    },
    Record {
        kind: ALBUM,
        marks: &["live"],
        ..record(3, "Live at the Granary", 2016, 14)
    },
    Record {
        kind: EP,
        marks: &["live"],
        ..record(3, "Four Rooms, Live", 2021, 4)
    },
    Record {
        kind: ALBUM,
        marks: &["compilation"],
        ..record(3, "B-Sides and Lost Rooms", 2019, 16)
    },
    Record {
        kind: ALBUM,
        marks: &["remix"],
        ..record(3, "Weathervane, Remade", 2021, 9)
    },
    // Bram Kowal Trio: no covers.
    Record {
        cover: false,
        ..record(4, "Second Set", 2005, 8)
    },
    Record {
        cover: false,
        ..record(4, "Brushes", 2009, 9)
    },
    // Other scripts.
    record(5, "Тихая вода", 2013, 10),
    record(5, "Северный ветер", 2018, 9),
    record(6, "夜の灯台", 2016, 11),
    Record {
        kind: SINGLE,
        ..record(6, "青い傘", 2017, 2)
    },
    record(7, "Σκιές του Μαΐου", 2012, 10),
    record(8, "겨울 정원", 2020, 10),
    // A long name.
    record(9, "Afternoon Society Minutes", 2014, 12),
    record(9, "A Second Afternoon", 2017, 11),
    // Records of several artists, which some here appear on.
    Record {
        by: None,
        kind: ALBUM,
        marks: &["compilation"],
        guests: &[(1, 0), (2, 1), (3, 2), (4, 3), (5, 5), (6, 6), (7, 7)],
        ..record(0, "Songs for the Harbour", 2015, 14)
    },
    Record {
        by: None,
        guests: &[(1, 3), (2, 2), (3, 9), (4, 8)],
        ..record(0, "Lights Out, Volume II", 2021, 12)
    },
];

/// Songs on no record: the artist, and how many.
const LOOSE: &[(usize, usize)] = &[(3, 3), (2, 2)];

/// The records here, as [`Rows::album`] takes them.
fn albums(rng: &mut Rng, now_ms: i64) -> Vec<Album> {
    let mut albums = Vec::new();
    for (index, record) in RECORDS.iter().enumerate() {
        let year = record.year.unwrap_or(2000);
        let mut recordings = Vec::new();
        for number in 0..record.tracks {
            // A single is named for its first song.
            let name = if number == 0 && record.kind == SINGLE {
                record.title.to_owned()
            } else {
                title(rng)
            };
            let duration = i64::try_from(rng.between(95_000, 420_000)).unwrap_or(200_000);
            recordings.push((name, duration, None));
        }
        // Tracks credited to another artist: round the guests for a record
        // of several, or the guests named.
        if record.by.is_none() && !record.guests.is_empty() {
            for (number, recording) in recordings.iter_mut().enumerate() {
                let guest = record.guests.get(number % record.guests.len());
                recording.2 = guest.map(|(_, artist)| *artist);
            }
        } else {
            for (number, artist) in record.guests {
                if let Some(recording) = recordings.get_mut(number.saturating_sub(1)) {
                    recording.2 = Some(*artist);
                }
            }
        }
        let mut versions = vec![super::Version {
            title: record.title.to_owned(),
            year,
            tracks: recordings.len(),
        }];
        for (edition, edition_year, more) in record.versions {
            for _ in 0..*more {
                let duration = i64::try_from(rng.between(120_000, 360_000)).unwrap_or(200_000);
                recordings.push((title(rng), duration, None));
            }
            versions.push(super::Version {
                title: format!("{} ({edition})", record.title),
                year: *edition_year,
                tracks: recordings.len(),
            });
        }
        let side = u32::try_from(rng.between(600, 900)).unwrap_or(600);
        albums.push(Album {
            artist: record.by,
            title: record.title.to_owned(),
            kind: record.kind.unwrap_or(Kind::Album),
            first_year: year,
            versions,
            recordings,
            discs: record.discs,
            format: index % 3,
            mixed: false,
            added_ms: now_ms - i64::try_from(index).unwrap_or_default() * 86_400_000,
            cover: record.cover.then(|| (rng.next(), side)),
            typed: record.kind.is_some(),
            marks: record.marks,
            dated: record.year.is_some(),
        });
    }
    albums
}

/// Adds the hand-made library to `library`, its covers under `root`, which
/// `servers` records as the invented root, as [`seed`](super::seed) does.
pub fn seed_showcase(
    library: &LibraryStore,
    servers: &ServerStore,
    root: &Path,
    now_ms: i64,
) -> Result<Invented, InventError> {
    std::fs::create_dir_all(root)?;
    let root = std::path::absolute(root)?;
    add_root(library, servers, &root)?;
    let artists: Vec<Artist> = ARTISTS
        .iter()
        .map(|(name, sort_name)| Artist {
            name: (*name).to_owned(),
            sort_name: sort_name.map(str::to_owned),
        })
        .collect();
    let mut rng = Rng::new(0x5ca1_ab1e);
    let albums = albums(&mut rng, now_ms);
    let batch: Vec<(usize, Album, String)> = albums
        .into_iter()
        .enumerate()
        .map(|(index, album)| {
            let dir = format!("showcase/{}", album_dir(&artists, &album, index));
            (index, album, dir)
        })
        .collect();
    let painted = paint(&root, &batch)?;
    let loose: Vec<(usize, String, i64)> = LOOSE
        .iter()
        .flat_map(|(artist, count)| (0..*count).map(move |_| *artist))
        .map(|artist| {
            let duration = i64::try_from(rng.between(120_000, 300_000)).unwrap_or(200_000);
            (artist, title(&mut rng), duration)
        })
        .collect();
    let mut invented = Invented {
        artists: artists.len(),
        groups: batch.len(),
        releases: batch.iter().map(|(_, album, _)| album.versions.len()).sum(),
        ..Invented::default()
    };
    for painted in painted.iter().flatten() {
        invented.covers += 1;
        invented.cover_bytes += painted.bytes;
    }
    let written = library.write(request("invent.showcase"), move |write| {
        // Keys and IDs of their own, apart from any drawn library's.
        let mut rows = Rows::start(write, 0x5ca1_ab1e, now_ms)?;
        let mut statements = Statements::new(write.connection())?;
        let mut tracks = 0;
        for ((_, album, dir), painted) in batch.iter().zip(&painted) {
            tracks += rows.album(
                write,
                &mut statements,
                &artists,
                album,
                dir,
                painted.as_ref(),
            )?;
        }
        for (number, (artist, title, duration)) in loose.iter().enumerate() {
            let Some(by) = artists.get(*artist) else {
                continue;
            };
            let (_, credit) =
                rows.artist(write, &mut statements, &by.name, by.sort_name.as_deref())?;
            let recording = rows.key("recording");
            let public_id = rows.public_id();
            statements
                .recording
                .execute((recording, public_id, title, credit, write.rev()))?;
            let path = format!("showcase/loose/{} - {number:02} {title}.mp3", by.name);
            rows.track(
                write,
                &mut statements,
                Track {
                    release: None,
                    recording,
                    credit,
                    title,
                    disc: 1,
                    track: i64::try_from(number).unwrap_or_default() + 1,
                    duration: *duration,
                    added: now_ms,
                    path: &path,
                    format: super::FORMATS.get(2).copied().unwrap_or(super::CD),
                    picture: None,
                    artist: &by.name,
                    album: "",
                },
            )?;
            tracks += 1;
        }
        Ok(tracks.to_string())
    })?;
    invented.tracks = written.result_json.parse::<usize>().unwrap_or_default();
    Ok(invented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_showcase_has_what_the_pages_are_judged_by() {
        let mut rng = Rng::new(1);
        let albums = albums(&mut rng, 1_794_744_000_000);
        let by = |artist: usize| {
            albums
                .iter()
                .filter(|album| album.artist == Some(artist))
                .count()
        };
        assert_eq!(by(0), 1);
        assert_eq!(by(1), 5);
        assert!(by(2) >= 20);
        assert!(albums.iter().any(|album| album.discs > 1));
        assert!(albums.iter().any(|album| album.versions.len() > 1));
        assert!(albums.iter().any(|album| !album.dated));
        assert!(albums.iter().any(|album| !album.typed));
        assert!(albums.iter().any(|album| album.marks.contains(&"live")));
        assert!(albums.iter().any(|album| album.artist.is_none()));
        assert!(albums.iter().any(|album| album.title.chars().count() > 80));
        assert!(
            albums
                .iter()
                .filter(|album| album.artist == Some(4))
                .all(|album| album.cover.is_none())
        );
    }
}
