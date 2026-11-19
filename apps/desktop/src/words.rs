//! What the pages say and how they take their room, with the real client's
//! numbers: `pages/words.ts` (counts, lengths, the line of facts under a
//! page's name), `pages/years.ts` (the picture of your years and the
//! banner's sentence) and `pages/layout.ts` (how an artist's page and an
//! album's take the room they have). Their tests come with them.

use ritornel_contract::{
    AlbumPage, ArtColor, HistoryOf, Phrase, RecordType, Shelf, YearPlays, YearSpan,
};

/// What a page says of your history with what it shows, before there is
/// any: the prototype's words for a library with no history yet.
pub(crate) const NO_HISTORY: &str = "Nothing to remember yet. It starts with your first play.";

/// A count with its thousands set apart: "1,200".
pub(crate) fn thousands(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A count and its noun: "1 album", "12 albums".
pub(crate) fn plural(count: u64, one: &str) -> String {
    format!(
        "{} {}{}",
        thousands(count),
        one,
        if count == 1 { "" } else { "s" }
    )
}

/// A track's length, as a clock: "4:05".
pub(crate) fn clock(ms: u64) -> String {
    let seconds = (ms + 500) / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// A record's length: "48 min", "1 h 12 min", "2 h".
pub(crate) fn length(ms: u64) -> String {
    let minutes = (ms + 30_000) / 60_000;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let hours = minutes / 60;
    if minutes.is_multiple_of(60) {
        format!("{hours} h")
    } else {
        format!("{hours} h {} min", minutes % 60)
    }
}

/// What kind of record an album's page says it is, where that is known.
pub(crate) fn kind_of(record_type: Option<RecordType>, shelf: Shelf) -> Option<&'static str> {
    if shelf == Shelf::Live {
        return Some("Live");
    }
    match record_type? {
        RecordType::Album => Some("Album"),
        RecordType::Ep => Some("EP"),
        RecordType::Single => Some("Single"),
        RecordType::Broadcast => Some("Broadcast"),
        RecordType::Other => None,
    }
}

/// The facts under an album's name: its kind where known, its year, its
/// tracks and its length.
pub(crate) fn album_facts(page: &AlbumPage) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(kind) = kind_of(page.record_type, page.shelf) {
        parts.push(kind.to_owned());
    }
    if let Some(year) = page.tile.year {
        parts.push(year.to_string());
    }
    parts.push(plural(page.tile.track_count, "track"));
    if page.tile.duration_ms > 0 {
        parts.push(length(page.tile.duration_ms));
    }
    parts.join(" · ")
}

/// One year of the picture of your years.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bar {
    pub year: i32,
    /// How tall, as a share of the tallest, from 3 to 100; 0 for a year with no plays.
    pub height: f32,
    /// The year played most: the first of those that tie.
    pub peak: bool,
}

/// A bar for every year of `span`, each as tall as the square root of its
/// share of the most, so that the quiet years still show.
pub(crate) fn bars(years: &[YearPlays], span: YearSpan) -> Vec<Bar> {
    let most = years
        .iter()
        .map(|year| year.plays)
        .max()
        .unwrap_or(0)
        .max(1);
    let peak = years
        .iter()
        .find(|year| year.plays == most)
        .map(|year| year.year);
    (span.from..=span.to)
        .map(|year| {
            let played = years
                .iter()
                .find(|each| each.year == year)
                .map_or(0, |each| each.plays);
            Bar {
                year,
                height: if played == 0 {
                    0.
                } else {
                    ((played as f32 / most as f32).sqrt() * 100.).max(3.)
                },
                peak: played > 0 && Some(year) == peak,
            }
        })
        .collect()
}

/// What the axis under the picture says: its first year, the one between and its last.
pub(crate) fn axis(span: YearSpan) -> [String; 3] {
    let middle = (f64::from(span.from) + f64::from(span.to)) / 2.;
    [
        span.from.to_string(),
        (middle.round() as i32).to_string(),
        span.to.to_string(),
    ]
}

/// The hue a picture borrows from the artwork beside it, its strength held
/// down so that it tints and does not shout. `None` keeps the accent.
pub(crate) fn tint(color: Option<ArtColor>) -> Option<(u16, u8)> {
    color.map(|color| (color.hue, color.saturation.clamp(18, 58)))
}

/// Whether a page has a picture of its years to show: dated plays, of a
/// history that has begun.
pub(crate) fn pictured(history: Option<&HistoryOf>) -> Option<(&HistoryOf, YearSpan)> {
    let history = history?;
    let span = history.span?;
    (!history.years.is_empty()).then_some((history, span))
}

/// The banner's sentence: the server's. Before the history is read,
/// nothing; before there is any history at all, the prototype's words for
/// that, said quietly; with history but none of this, nothing.
pub(crate) fn said(history: Option<&HistoryOf>) -> (Vec<Phrase>, bool) {
    let Some(history) = history else {
        return (Vec::new(), false);
    };
    if history.span.is_none() && history.plays == 0 {
        return (
            vec![Phrase {
                text: NO_HISTORY.to_owned(),
                strong: false,
            }],
            true,
        );
    }
    (history.sentence.clone(), false)
}

/// Where an album's cover goes, and how large it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AlbumColumns {
    /// Whether the cover heads a column of its own at the right of the
    /// tracks, hanging out of the banner.
    pub beside: bool,
    pub cover: u32,
}

/// The tracks take up to 900 points and the cover has the rest, from 280
/// to 440; too narrow for two columns, the cover goes under the banner.
pub(crate) fn album_columns(content: f32) -> AlbumColumns {
    if content < 820. {
        return AlbumColumns {
            beside: false,
            cover: 240,
        };
    }
    AlbumColumns {
        beside: true,
        cover: (content * 0.27)
            .max(content - 964.)
            .clamp(280., 440.)
            .round() as u32,
    }
}

/// The gap between the tracks and the cover's column.
pub(crate) const COLUMN_GAP: f32 = 64.;

/// An artist's page: its words and how it takes its room.
pub(crate) mod artist {
    use ritornel_contract::Shelf;

    use super::plural;

    /// The facts under an artist's name: their albums and their other
    /// records, or their records, and their songs. The records of others they
    /// appear on are not theirs to count.
    pub(crate) fn artist_facts(shelves: &[(Shelf, u64)], songs: u64) -> String {
        let theirs = shelves
            .iter()
            .filter(|(shelf, _)| *shelf != Shelf::AppearsOn);
        let records: u64 = theirs.clone().map(|(_, count)| count).sum();
        let albums = theirs
            .clone()
            .find(|(shelf, _)| *shelf == Shelf::Albums)
            .map_or(0, |(_, count)| *count);
        let said = if albums > 0 {
            let mut said = plural(albums, "album");
            if records > albums {
                said.push_str(&format!(
                    " and {}",
                    plural(records - albums, "other record")
                ));
            }
            said
        } else {
            plural(records, "record")
        };
        if songs > 0 {
            format!("{said} · {}", plural(songs, "song"))
        } else {
            said
        }
    }

    /// A shelf's heading.
    pub(crate) fn shelf_heading(shelf: Shelf) -> &'static str {
        match shelf {
            Shelf::Albums => "Albums",
            Shelf::Singles => "Singles",
            Shelf::Live => "Live",
            Shelf::Collections => "Collections and rarities",
            Shelf::AppearsOn => "Appears on",
        }
    }

    /// The first kind of record has larger covers than the rest.
    pub(crate) const FIRST_CARD: f32 = 172.;
    pub(crate) const CARD: f32 = 132.;
    const FIRST_STEP: f32 = FIRST_CARD + 20.;
    const STEP: f32 = CARD + 20.;
    pub(crate) const SHELF_GAP: f32 = 56.;
    const HISTORY: f32 = 340.;
    pub(crate) const HISTORY_GAP: f32 = 52.;
    const WIDE: f32 = 1500.;
    /// Under the records, from this width the picture and the records played
    /// most sit side by side.
    pub(crate) const HISTORY_ACROSS: f32 = 1000.;

    /// How wide the records of each kind need to be laid side by side, each
    /// kind taking only the room it needs.
    pub(crate) fn records_width(counts: &[usize]) -> f32 {
        let kinds: Vec<usize> = counts.iter().copied().filter(|count| *count > 0).collect();
        if kinds.is_empty() {
            return 0.;
        }
        let shelves: f32 = kinds
            .iter()
            .enumerate()
            .map(|(at, count)| *count as f32 * if at == 0 { FIRST_STEP } else { STEP } - 20.)
            .sum();
        shelves + (kinds.len() - 1) as f32 * SHELF_GAP
    }

    /// Whether your history with an artist stands in a column beside their
    /// records: when there is history to show, and the window is wide or the
    /// records leave the room.
    pub(crate) fn history_beside(content: f32, counts: &[usize], history: bool) -> bool {
        history
            && (content + 80. >= WIDE || records_width(counts) <= content - HISTORY_GAP - HISTORY)
    }

    /// How wide that column is: about a quarter of the page, from 340 to 430 points.
    pub(crate) fn history_column(content: f32) -> f32 {
        (content * 0.23).clamp(HISTORY, 430.).round()
    }

    /// How many columns a list of songs takes across `content` points: a
    /// second from 1,040, and more as they fit at 480, 56 apart.
    pub(crate) fn song_columns(content: f32) -> usize {
        if content < 1040. {
            return 1;
        }
        (((content + 56.) / (480. + 56.)).floor() as usize).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::artist::*;
    use super::*;

    #[test]
    fn counts_and_times_as_the_prototype_words_them() {
        assert_eq!(plural(1, "album"), "1 album");
        assert_eq!(plural(12, "album"), "12 albums");
        assert_eq!(plural(1200, "song"), "1,200 songs");
        assert_eq!(clock(245_000), "4:05");
        assert_eq!(clock(59_600), "1:00");
        assert_eq!(length(48 * 60_000), "48 min");
        assert_eq!(length(72 * 60_000), "1 h 12 min");
        assert_eq!(length(120 * 60_000), "2 h");
    }

    #[test]
    fn says_an_artists_albums_and_other_records_and_not_the_records_of_others() {
        let shelves = [
            (Shelf::Albums, 6),
            (Shelf::Singles, 3),
            (Shelf::Live, 2),
            (Shelf::AppearsOn, 3),
        ];
        assert_eq!(
            artist_facts(&shelves, 143),
            "6 albums and 5 other records · 143 songs"
        );
        assert_eq!(
            artist_facts(&[(Shelf::Albums, 1)], 11),
            "1 album · 11 songs"
        );
        assert_eq!(
            artist_facts(&[(Shelf::Singles, 2)], 3),
            "2 records · 3 songs"
        );
    }

    #[test]
    fn says_an_albums_kind_only_where_it_is_known() {
        assert_eq!(kind_of(None, Shelf::Albums), None);
        assert_eq!(kind_of(Some(RecordType::Ep), Shelf::Albums), Some("EP"));
        assert_eq!(kind_of(Some(RecordType::Album), Shelf::Live), Some("Live"));
        assert_eq!(
            kind_of(Some(RecordType::Single), Shelf::Singles),
            Some("Single")
        );
    }

    #[test]
    fn has_a_bar_for_every_year_eased_so_the_quiet_years_show() {
        let years = [
            YearPlays {
                year: 2010,
                plays: 100,
            },
            YearPlays {
                year: 2012,
                plays: 25,
            },
            YearPlays {
                year: 2013,
                plays: 100,
            },
            YearPlays {
                year: 2014,
                plays: 0,
            },
            YearPlays {
                year: 2015,
                plays: 1,
            },
        ];
        let shown = bars(
            &years,
            YearSpan {
                from: 2009,
                to: 2016,
            },
        );
        let heights: Vec<i32> = shown.iter().map(|bar| bar.height.round() as i32).collect();
        assert_eq!(heights, [0, 100, 0, 50, 100, 0, 10, 0]);
        let peaks: Vec<i32> = shown
            .iter()
            .filter(|bar| bar.peak)
            .map(|bar| bar.year)
            .collect();
        assert_eq!(peaks, [2010]);
        let quiet = bars(
            &[
                YearPlays {
                    year: 2020,
                    plays: 10_000,
                },
                YearPlays {
                    year: 2021,
                    plays: 1,
                },
            ],
            YearSpan {
                from: 2020,
                to: 2021,
            },
        );
        assert_eq!(quiet.get(1).map(|bar| bar.height), Some(3.));
    }

    #[test]
    fn says_the_first_year_the_one_between_and_the_last() {
        assert_eq!(
            axis(YearSpan {
                from: 2007,
                to: 2026
            }),
            ["2007", "2017", "2026"]
        );
    }

    #[test]
    fn borrows_the_artworks_hue_at_a_strength_that_tints() {
        assert_eq!(
            tint(Some(ArtColor {
                hue: 212,
                saturation: 90
            })),
            Some((212, 58))
        );
        assert_eq!(
            tint(Some(ArtColor {
                hue: 30,
                saturation: 4
            })),
            Some((30, 18))
        );
        assert_eq!(tint(None), None);
    }

    #[test]
    fn has_the_cover_beside_the_tracks_from_820_points() {
        let columns = |content| album_columns(content);
        assert_eq!(
            columns(780.),
            AlbumColumns {
                beside: false,
                cover: 240
            }
        );
        assert_eq!(
            columns(1020.),
            AlbumColumns {
                beside: true,
                cover: 280
            }
        );
        assert_eq!(
            columns(1200.),
            AlbumColumns {
                beside: true,
                cover: 324
            }
        );
        assert_eq!(
            columns(1360.),
            AlbumColumns {
                beside: true,
                cover: 396
            }
        );
        assert_eq!(
            columns(1400.),
            AlbumColumns {
                beside: true,
                cover: 436
            }
        );
    }

    #[test]
    fn reckons_the_room_each_kind_of_record_needs() {
        assert_eq!(records_width(&[]), 0.);
        assert_eq!(records_width(&[1]), 172.);
        assert_eq!(
            records_width(&[3, 2]),
            3. * 192. - 20. + 56. + 2. * 152. - 20.
        );
        assert_eq!(records_width(&[3, 0, 2]), records_width(&[3, 2]));
        assert!(history_beside(1420., &[22, 3, 2, 2, 3], true));
        assert!(history_beside(1020., &[3], true));
        assert!(!history_beside(1020., &[6, 3], true));
        assert!(!history_beside(1920., &[1], false));
        assert_eq!(song_columns(1020.), 1);
        assert_eq!(song_columns(1040.), 2);
        assert_eq!(song_columns(1920.), 3);
    }
}
