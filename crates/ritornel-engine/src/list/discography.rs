//! Filing an artist's records as a discography, the way a person who knows
//! the records would ("Album and artist pages" in
//! docs/internals/desktop.md): albums first, with EPs among them, then
//! singles, live recordings, collections and rarities, and last the records
//! of others they appear on. Newest first within each, by the year a record
//! first came out and not a reissue's, records with no year last. Nothing is
//! left out.
//!
//! This is the one place the rule is written. The Albums list orders an
//! artist's records by it (`SortKey::Discography`), with a section for each
//! shelf, and an album's page says which shelf its record is on; a client
//! reads both and works out neither.
//!
//! What kind a record is comes from its release group: the primary type a
//! release-type tag gives (album, EP, single, broadcast, other) and the
//! marks beside it (MusicBrainz's secondary types). In a library tagged by
//! neither Picard nor Beets most records have no type at all, so an
//! unknown kind is the common case and not an exception: it is filed with
//! the albums, which is where such a record nearly always belongs.

use std::cmp::Ordering;

use ritornel_contract::{RecordType, Shelf};

/// The marks that make a record one of the collections and rarities.
const COLLECTED: &[&str] = &[
    "compilation",
    "remix",
    "dj-mix",
    "mixtape/street",
    "demo",
    "interview",
];

/// The primary type as stored, read.
pub(crate) fn record_type(primary: Option<&str>) -> Option<RecordType> {
    match primary? {
        "album" => Some(RecordType::Album),
        "ep" => Some(RecordType::Ep),
        "single" => Some(RecordType::Single),
        "broadcast" => Some(RecordType::Broadcast),
        "other" => Some(RecordType::Other),
        _ => None,
    }
}

/// The marks as stored: a JSON array of strings. Anything else is no marks.
pub(crate) fn marks(json: Option<&str>) -> Vec<String> {
    json.and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .unwrap_or_default()
}

/// The shelf a record goes on in an artist's discography. `theirs` is
/// whether the record's own credit names the artist: one that does not,
/// and is there because the artist is credited on a song of it, is a
/// record they appear on, whatever kind it is.
pub(crate) fn shelf(primary: Option<RecordType>, marks: &[String], theirs: bool) -> Shelf {
    if !theirs {
        return Shelf::AppearsOn;
    }
    let marked = |mark: &str| marks.iter().any(|have| have.eq_ignore_ascii_case(mark));
    // A live record is live first: a live compilation is filed with the
    // live recordings, as a person would look for it.
    if marked("live") {
        return Shelf::Live;
    }
    if COLLECTED.iter().any(|mark| marked(mark)) {
        return Shelf::Collections;
    }
    match primary {
        Some(RecordType::Single) => Shelf::Singles,
        // An EP is an album; so is a broadcast, anything else, and a record
        // whose kind is not known.
        Some(RecordType::Album | RecordType::Ep | RecordType::Broadcast | RecordType::Other)
        | None => Shelf::Albums,
    }
}

/// The order within a shelf: newest first by the year a record first came
/// out, records with no year last, then by title as it is filed.
pub(crate) fn newest_first(
    a_year: Option<i64>,
    a_title: &str,
    b_year: Option<i64>,
    b_title: &str,
) -> Ordering {
    let years = match (a_year, b_year) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    years.then_with(|| super::open::by_name_for_order(a_title, b_title))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(marks: &[&str]) -> Vec<String> {
        marks.iter().map(|mark| (*mark).to_owned()).collect()
    }

    #[test]
    fn eps_and_records_of_no_known_kind_are_albums() {
        for primary in [
            None,
            Some(RecordType::Album),
            Some(RecordType::Ep),
            Some(RecordType::Broadcast),
            Some(RecordType::Other),
        ] {
            assert_eq!(shelf(primary, &[], true), Shelf::Albums, "{primary:?}");
        }
        assert_eq!(shelf(Some(RecordType::Single), &[], true), Shelf::Singles);
    }

    #[test]
    fn live_comes_before_collected_and_both_before_the_type() {
        assert_eq!(
            shelf(Some(RecordType::Album), &marked(&["live"]), true),
            Shelf::Live
        );
        assert_eq!(
            shelf(Some(RecordType::Single), &marked(&["live"]), true),
            Shelf::Live
        );
        assert_eq!(
            shelf(None, &marked(&["compilation", "live"]), true),
            Shelf::Live
        );
        for mark in [
            "compilation",
            "remix",
            "dj-mix",
            "mixtape/street",
            "demo",
            "interview",
        ] {
            assert_eq!(
                shelf(Some(RecordType::Album), &marked(&[mark]), true),
                Shelf::Collections,
                "{mark}"
            );
        }
        // A soundtrack or a spoken record is filed by its type.
        assert_eq!(
            shelf(Some(RecordType::Album), &marked(&["soundtrack"]), true),
            Shelf::Albums
        );
        assert_eq!(
            shelf(Some(RecordType::Single), &marked(&["Spokenword"]), true),
            Shelf::Singles
        );
        // Marks are read whatever their case.
        assert_eq!(shelf(None, &marked(&["Live"]), true), Shelf::Live);
    }

    #[test]
    fn a_record_of_others_is_one_they_appear_on_whatever_its_kind() {
        for primary in [None, Some(RecordType::Album), Some(RecordType::Single)] {
            assert_eq!(shelf(primary, &[], false), Shelf::AppearsOn);
            assert_eq!(shelf(primary, &marked(&["live"]), false), Shelf::AppearsOn);
        }
    }

    #[test]
    fn newest_first_by_first_year_with_no_year_last_then_by_title() {
        let mut records = [
            (Some(1999), "Morning"),
            (None, "Undated"),
            (Some(2014), "Late"),
            (Some(1999), "Evening"),
            (None, "Another undated"),
            (Some(2020), "Newest"),
        ];
        records.sort_by(|a, b| newest_first(a.0, a.1, b.0, b.1));
        let titles: Vec<&str> = records.iter().map(|record| record.1).collect();
        assert_eq!(
            titles,
            [
                "Newest",
                "Late",
                "Evening",
                "Morning",
                "Another undated",
                "Undated"
            ]
        );
    }

    #[test]
    fn types_and_marks_are_read_as_stored() {
        assert_eq!(record_type(Some("ep")), Some(RecordType::Ep));
        assert_eq!(record_type(Some("mixtape")), None);
        assert_eq!(record_type(None), None);
        assert_eq!(
            marks(Some(r#"["compilation","live"]"#)),
            ["compilation", "live"]
        );
        assert!(marks(Some("[]")).is_empty());
        assert!(marks(Some("not json")).is_empty());
        assert!(marks(None).is_empty());
    }
}
