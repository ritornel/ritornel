//! Disc numbers that a release's files imply and do not claim. Two switches
//! of the policy, both off in policy 1, which the dry run turns on to ask
//! what they would change:
//!
//! - [`Policy::disc_from_album_title`]: a trailing "Disc 2" or "CD 2" in an
//!   album title is the disc number, and is left out of the title, so that
//!   "Night Drives (Disc 1)" and "Night Drives (Disc 2)" are one release.
//! - [`Policy::discs_from_folders`]: sibling folders of one album, by one
//!   album artist, whose files collide on track numbers and share no title
//!   are discs of one release, when each folder's name gives a disc of its
//!   own ("CD1", "Disc 2") and that disc is the folder's. Two copies of one
//!   album share their titles, so they are never joined. Two different
//!   albums of one title, such as two self-titled ones, look like discs to
//!   the rest of the rule; the names guard against them ("Ninefold (1994)"
//!   names no disc), and so do their dates: folders whose files claim
//!   different years are never joined.
//!
//! Both rewrite what the files' claims say before anything is resolved, so
//! the release key, the collision rule and the disc shown all see the same
//! numbers. A disc the tags give, other than 1, is never overridden.

use std::collections::{BTreeMap, BTreeSet};

use crate::extract::{Facts, parse_date};
use crate::library::Library;
use crate::normalize::normalize;
use crate::resolve::Policy;

/// The disc a marker names: "cd2", "disc 2", "disk-2", "cd #2". `None` for
/// anything else.
fn marker(text: &str) -> Option<i64> {
    let text = text.trim().to_lowercase();
    let rest = ["disc", "disk", "cd"]
        .iter()
        .find_map(|word| text.strip_prefix(word))?;
    let digits = rest.trim_start_matches([' ', '.', '-', '_', '#']);
    if digits.is_empty() || digits.len() > 2 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|disc| *disc >= 1)
}

/// An album title with a disc marker at its end, or before the bracketed
/// notes at its end, split into the title without it and the disc. The
/// notes are kept: "White Field Disk 1 [Half-Speed]" is disc 1 of "White Field
/// [Half-Speed]". A title that is nothing but the marker is left alone.
pub(crate) fn disc_in_title(album: &str) -> Option<(String, i64)> {
    // The notes taken off the end, last first.
    let mut notes: Vec<&str> = Vec::new();
    let mut rest = album.trim_end();
    loop {
        if let Some((mut title, disc)) = trailing_disc(rest) {
            // A bracket left open in the title is a note the marker sits
            // inside.
            if !closed(&title) {
                return None;
            }
            for note in notes.iter().rev() {
                title.push(' ');
                title.push_str(note);
            }
            return Some((title, disc));
        }
        let at = last_note(rest)?;
        let note = rest.get(at..)?;
        // "(of 2)" is the marker's, and goes with it.
        let counted = normalize(note)
            .strip_prefix("of ")
            .is_some_and(|count| !count.is_empty() && count.chars().all(|c| c.is_ascii_digit()));
        if !counted {
            notes.push(note);
        }
        rest = rest.get(..at)?.trim_end();
    }
}

/// Whether no bracket in `text` is left open. A closing one with nothing
/// of its kind open, as in a smiley, is no bracket.
fn closed(text: &str) -> bool {
    let mut open: Vec<char> = Vec::new();
    for c in text.chars() {
        match c {
            '(' | '[' => open.push(c),
            ')' | ']' => {
                let opens = if c == ')' { '(' } else { '[' };
                if open.last() == Some(&opens) {
                    open.pop();
                }
            }
            _ => {}
        }
    }
    open.is_empty()
}

/// Where the bracketed note that ends `text` opens: the bracket that
/// matches the last one, round brackets with round and square with square.
/// `None` when the text does not end in a note, or its brackets do not pair.
fn last_note(text: &str) -> Option<usize> {
    let mut open: Vec<char> = Vec::new();
    for (at, c) in text.char_indices().rev() {
        match c {
            ')' | ']' => open.push(c),
            '(' | '[' => {
                let close = open.pop()?;
                if (c == '(') != (close == ')') {
                    return None;
                }
                if open.is_empty() {
                    return Some(at);
                }
            }
            _ if open.is_empty() => return None,
            _ => {}
        }
    }
    None
}

/// An album title whose last words or last bracketed note are a disc
/// marker, split into the title without it and the disc.
fn trailing_disc(album: &str) -> Option<(String, i64)> {
    let album = album.trim_end();
    let (before, disc) = if album.ends_with([')', ']']) {
        let at = last_note(album)?;
        let inner = album.get(at + 1..album.len() - 1)?;
        (album.get(..at)?, marker(inner)?)
    } else {
        // The marker is the last word, or the last two: "CD2", "Disc 2".
        let mut starts: Vec<usize> = Vec::new();
        let mut in_word = false;
        for (at, c) in album.char_indices() {
            if c.is_whitespace() {
                in_word = false;
            } else if !in_word {
                starts.push(at);
                in_word = true;
            }
        }
        let (start, disc) = [1, 2].into_iter().find_map(|count| {
            let start = *starts.get(starts.len().checked_sub(count)?)?;
            Some((start, marker(album.get(start..)?)?))
        })?;
        (album.get(..start)?, disc)
    };
    let title = before.trim_end_matches([' ', '-', ',', ':', '–', '—', '/']);
    if !title.chars().any(char::is_alphanumeric) {
        return None;
    }
    Some((title.to_owned(), disc))
}

/// The disc a folder's own name gives: "CD1", `Disc 2 [FLAC]`, "Night
/// Drives (Disc 2)".
fn disc_of_folder(name: &str) -> Option<i64> {
    let mut rest = name.trim_end();
    while let Some(at) = last_note(rest) {
        rest = rest.get(..at)?.trim_end();
    }
    marker(rest).or_else(|| disc_in_title(name).map(|(_, disc)| disc))
}

/// Whether a value holds this field of the item, which the disc rules
/// then leave as it is.
fn holds(item: &crate::library::Item, field: &str) -> bool {
    item.held.contains(&field)
}

/// The library with the disc numbers the switches imply, or `None` when
/// neither is on.
pub(crate) fn apply(library: &Library, policy: &Policy) -> Option<Library> {
    apply_with_probes(library, policy, &mut [], &[])
}

/// Return a library with implied discs, or `None` when both disc rules are off.
/// Apply the title rule to each replacement in place. Replacements have no
/// release assignment or folder position of their own.
pub(crate) fn apply_with_probes(
    library: &Library,
    policy: &Policy,
    probes: &mut [Facts],
    keeps: &[(bool, bool)],
) -> Option<Library> {
    if !policy.disc_from_album_title && !policy.discs_from_folders {
        return None;
    }
    let mut implied = library.clone();
    if policy.disc_from_album_title {
        let conflicts = |claim: Option<i64>, disc| claim.is_some_and(|n| n != 1 && n != disc);
        // A conflicting disc tag anywhere in the library keeps the album
        // title intact for every file that shares it.
        let contradicted: BTreeSet<&str> = library
            .items
            .iter()
            .filter_map(|item| {
                let album = item.facts.album.as_deref()?;
                let (_, disc) = disc_in_title(album)?;
                conflicts(item.facts.disc, disc).then_some(album)
            })
            .collect();
        let apply_title = |facts: &mut Facts, keep_title: bool, keep_disc: bool| {
            if let Some(album) = facts.album.as_deref()
                && !contradicted.contains(album)
                && let Some((title, disc)) = disc_in_title(album)
                && !conflicts(facts.disc, disc)
            {
                if !keep_title {
                    facts.album = Some(title);
                }
                if !keep_disc {
                    facts.disc = Some(disc);
                }
            }
        };
        // A held title or disc number is left as it is.
        for item in &mut implied.items {
            let (title, disc) = (holds(item, "album"), holds(item, "disc_number"));
            if !(title && disc) {
                apply_title(&mut item.facts, title, disc);
            }
        }
        // A replacement's disc tag affects only its own title. It cannot
        // veto a different replacement being judged in the same scan. Its
        // item's held title or disc number is left as it is, as above.
        for (at, facts) in probes.iter_mut().enumerate() {
            let (title, disc) = keeps.get(at).copied().unwrap_or_default();
            apply_title(facts, title, disc);
        }
    }
    if policy.discs_from_folders {
        folder_discs(&mut implied, policy);
    }
    Some(implied)
}

/// One folder of an album: its items, the track numbers they take and the
/// titles they carry.
#[derive(Default)]
struct Folder {
    items: Vec<usize>,
    tracks: BTreeSet<i64>,
    titles: BTreeSet<String>,
    numbered: bool,
    /// The years the files' date tags claim.
    years: BTreeSet<i64>,
}

fn folder_discs(library: &mut Library, policy: &Policy) {
    // Sibling folders of one album by one album artist.
    type Key = (String, String, String);
    let mut albums: BTreeMap<Key, BTreeMap<String, Folder>> = BTreeMap::new();
    for (index, item) in library.items.iter().enumerate() {
        let facts = &item.facts;
        let Some(album) = &facts.album else { continue };
        let parent = item
            .folder
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        let key = (
            parent.to_owned(),
            normalize(album),
            facts
                .album_artist_text()
                .as_deref()
                .map(normalize)
                .unwrap_or_default(),
        );
        let folder = albums
            .entry(key)
            .or_default()
            .entry(item.folder.clone())
            .or_default();
        folder.items.push(index);
        folder.tracks.extend(facts.track);
        folder.titles.extend(
            facts
                .title
                .as_deref()
                .or(facts.title_inferred.as_deref())
                .map(|text| policy.title_core(text)),
        );
        folder.numbered |= facts.disc.is_some_and(|disc| disc != 1);
        folder.years.extend(
            facts
                .date
                .as_deref()
                .and_then(parse_date)
                .map(|(_, year)| year),
        );
    }
    for folders in albums.values() {
        if folders.len() < 2 || folders.values().any(|folder| folder.numbered) {
            continue;
        }
        let list: Vec<(&String, &Folder)> = folders.iter().collect();
        let mut collide = false;
        let mut share = false;
        let mut dated_apart = false;
        for (at, (_, a)) in list.iter().enumerate() {
            for (_, b) in list.iter().skip(at + 1) {
                collide |= !a.tracks.is_disjoint(&b.tracks);
                share |= !a.titles.is_disjoint(&b.titles);
                dated_apart |=
                    !a.years.is_empty() && !b.years.is_empty() && a.years.is_disjoint(&b.years);
            }
        }
        if !collide || share || dated_apart {
            continue;
        }
        // Each folder's name gives its disc, and no two the same.
        let named: Vec<Option<i64>> = list
            .iter()
            .map(|(folder, _)| disc_of_folder(folder.rsplit('/').next().unwrap_or(folder)))
            .collect();
        let distinct: BTreeSet<i64> = named.iter().flatten().copied().collect();
        if distinct.len() != list.len() {
            continue;
        }
        for ((_, folder), named) in list.iter().zip(&named) {
            let Some(disc) = named else { continue };
            // A held disc number is left as it is; its file still counted
            // as evidence above.
            for index in &folder.items {
                if let Some(item) = library.items.get_mut(*index)
                    && !holds(item, "disc_number")
                {
                    item.facts.disc = Some(*disc);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_disc_marker_is_split_from_the_title() {
        let split = |title: &str| disc_in_title(title);
        let some = |title: &str, disc| Some((title.to_owned(), disc));
        assert_eq!(split("Night Drives (Disc 2)"), some("Night Drives", 2));
        assert_eq!(split("Night Drives [CD1]"), some("Night Drives", 1));
        assert_eq!(split("Night Drives - Disc 2"), some("Night Drives", 2));
        assert_eq!(split("Night Drives CD2"), some("Night Drives", 2));
        assert_eq!(split("Night Drives, Disk 3"), some("Night Drives", 3));
        assert_eq!(split("Night  Drives cd 10"), some("Night  Drives", 10));
        for title in [
            "Disc 2",
            "(CD 1)",
            "Night Drives",
            "Night Drives (Deluxe)",
            "The CD",
            "Discovery 2",
            "Night Drives (Disc 0)",
            "Night Drives CD 123",
        ] {
            assert_eq!(split(title), None, "{title}");
        }
    }

    #[test]
    fn a_disc_marker_before_an_edition_note_is_split_and_the_note_kept() {
        let split = |title: &str| disc_in_title(title);
        let some = |title: &str, disc| Some((title.to_owned(), disc));
        assert_eq!(
            split("White Field Disk 1 [Half-Speed]"),
            some("White Field [Half-Speed]", 1)
        );
        assert_eq!(
            split("White Field Disk 2 [Half-Speed]"),
            some("White Field [Half-Speed]", 2)
        );
        assert_eq!(
            split("Night Drives (Disc 2) [Remastered]"),
            some("Night Drives [Remastered]", 2)
        );
        assert_eq!(
            split("Night Drives - CD2 (Deluxe Edition) [24-96]"),
            some("Night Drives (Deluxe Edition) [24-96]", 2)
        );
        // Titles that only hold the word, or a marker that is not last
        // before the notes, are left alone.
        for title in [
            "Disk 1 [Half-Speed]",
            "Slipped Disc (Remastered)",
            "The Disk Drive [Deluxe]",
            "Disco 2 [Live]",
            "Discs 3 (Live)",
            "Disc Jockey (Disco Mix)",
            "Night Drives (Disc 2 Bonus) [Live]",
            "Disc 2 Sessions (Live)",
            "Night Drives [Disc]",
            // A marker inside a note is not before it, and a bracket that
            // closes nothing is not a note.
            "Night Drives [Edition CD2 [Live]]",
            "Night Drives Disc 2 ]",
            "Night Drives Disc 2 (Live]",
            "Night Drives [Sessions Disc 2 (Gold)",
            "Night Drives [Sessions Disc 2",
        ] {
            assert_eq!(split(title), None, "{title}");
        }
        // A stray closing bracket leaves nothing open.
        assert_eq!(
            split("Smile :) Disc 1 [Remastered]"),
            some("Smile :) [Remastered]", 1)
        );
        // "(of 2)" belongs to the marker.
        assert_eq!(split("White Field Disc 1 (of 2)"), some("White Field", 1));
        assert_eq!(
            split("White Field Disc 2 [of 2] [Gold]"),
            some("White Field [Gold]", 2)
        );
        assert_eq!(
            split("Night Drives Disc 1 [Remastered [Gold]]"),
            some("Night Drives [Remastered [Gold]]", 1)
        );
        // Many notes are read without a frame for each.
        let long = format!("Night Drives Disc 1{}", " []".repeat(200_000));
        assert_eq!(split(&long).map(|(_, disc)| disc), Some(1));
    }

    #[test]
    fn a_volume_or_a_part_is_a_record_of_its_own_and_no_disc() {
        // Volumes and parts come out as records of their own, so neither
        // word is a marker, in a title or a folder's name.
        for title in [
            "Night Drives Vol. 1",
            "Night Drives Vol. 2",
            "Night Drives, Vol. 2 [Remastered]",
            "Night Drives Volume 3",
            "Night Drives (Vol. II)",
            "Night Drives Part 2",
            "Night Drives, Pt. 1",
            "Night Drives (Part II) [Deluxe]",
            "Night Drives (Vol. 2)",
            "Night Drives [Part 2]",
            "Night Drives (Vol. 2) [Deluxe]",
        ] {
            assert_eq!(disc_in_title(title), None, "{title}");
        }
        // A disc of a volume is a disc, and the volume stays in the title.
        assert_eq!(
            disc_in_title("Night Drives, Vol. 2 (Disc 1)"),
            Some(("Night Drives, Vol. 2".to_owned(), 1))
        );
        assert_eq!(
            disc_in_title("Night Drives, Vol. 2 (Disc 1) [Deluxe]"),
            Some(("Night Drives, Vol. 2 [Deluxe]".to_owned(), 1))
        );
        assert_eq!(disc_of_folder("Vol. 2 (Disc 1)"), Some(1));
        for folder in [
            "Vol. 1",
            "Vol 2",
            "Volume 2",
            "Part 1",
            "Pt. 2",
            "Vol. 1 [FLAC]",
        ] {
            assert_eq!(disc_of_folder(folder), None, "{folder}");
        }
    }

    #[test]
    fn a_folder_names_its_disc_by_a_marker() {
        assert_eq!(disc_of_folder("CD1"), Some(1));
        assert_eq!(disc_of_folder("Disc 2"), Some(2));
        assert_eq!(disc_of_folder("Night Drives (Disc 2)"), Some(2));
        assert_eq!(disc_of_folder("Night Drives"), None);
        assert_eq!(disc_of_folder("Bonus"), None);
        assert_eq!(disc_of_folder("CD1 [FLAC]"), Some(1));
        assert_eq!(disc_of_folder("Disc 2 (24-96)"), Some(2));
        assert_eq!(disc_of_folder("[FLAC]"), None);
        // Many notes are read in linear time.
        let long = format!("CD1{}", " []".repeat(200_000));
        assert_eq!(disc_of_folder(&long), Some(1));
    }
}
