//! `ritornelctl library create`: makes the running server a hub with a
//! library of its own. See "Server identity and hub role" in
//! docs/internals/linking.md.
//!
//! `ritornelctl library stats`: what the library holds and what the scans
//! made of it. Until there is something to browse with, this is how to see
//! what a scan produced.

use std::io::Write;

use ritornel_client::contract::{Count, LibraryCreated, LibraryStats};
use ritornel_client::local;

use crate::cli::{Error, ServerArgs};
use crate::server;

pub(crate) fn create(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    // One key for the one thing asked. The command is sent once: this
    // program does not send it again, so a person who runs it twice asks
    // twice, and the second is refused.
    let key = ritornel_client::idempotency_key()?;
    let created = server::ask(args, env, async |client| client.create_library(key).await)?;
    server::print(args, out, &created, serde_json::to_string_pretty, render)
}

fn render(created: &LibraryCreated) -> String {
    let revision = created
        .receipt
        .revision
        .map_or_else(|| "unknown".to_owned(), |revision| revision.to_string());
    server::facts(&[
        ("Library ID", created.library_id.to_string()),
        ("Library revision", revision),
    ])
}

pub(crate) fn stats(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let stats = server::ask(args, env, async |client| client.library_stats().await)?;
    server::print(
        args,
        out,
        &stats,
        serde_json::to_string_pretty,
        render_stats,
    )
}

/// A count, and the share of `whole` that it is.
fn share(count: u64, whole: u64) -> String {
    if whole == 0 {
        return count.to_string();
    }
    // Tenths of a percent, rounded to the nearest.
    let tenths = (count * 1000 + whole / 2) / whole;
    format!("{count} ({}.{}%)", tenths / 10, tenths % 10)
}

/// Counts under a heading, one a line.
fn listed(heading: &str, counts: &[Count], whole: u64) -> String {
    if counts.is_empty() {
        return String::new();
    }
    let mut text = format!("\n{heading}:\n");
    for count in counts {
        text.push_str(&format!(
            "  {:<16}{}\n",
            count.name,
            share(count.count, whole)
        ));
    }
    text
}

/// The stats as a person reads them.
fn render_stats(stats: &LibraryStats) -> String {
    let readable = stats.files.saturating_sub(stats.files_unreadable);
    let bound: u64 = stats.decisions.iter().map(|count| count.count).sum();
    let mut text = server::facts(&[
        ("Library revision", stats.library_revision.to_string()),
        ("Files", stats.files.to_string()),
        ("Size", crate::status::size(stats.file_bytes)),
        ("Missing", stats.files_missing.to_string()),
        ("Retired", stats.files_retired.to_string()),
        ("Unreadable", share(stats.files_unreadable, stats.files)),
        (
            "With an MBID",
            share(stats.files_with_musicbrainz_id, stats.files),
        ),
        ("Unresolved", stats.unresolved.to_string()),
        ("Artists", stats.artists.to_string()),
        ("Release groups", stats.release_groups.to_string()),
        ("Releases", stats.releases.to_string()),
        ("Recordings", stats.recordings.to_string()),
        ("Track appearances", stats.appearances.to_string()),
        ("Loose tracks", stats.loose_tracks.to_string()),
        ("Need review", share(stats.needs_review, bound)),
        (
            "Groups with kind",
            share(stats.groups_with_kind, stats.release_groups),
        ),
        (
            "With first year",
            share(stats.groups_with_first_year, stats.release_groups),
        ),
        (
            "  from a tag",
            share(
                stats.groups_with_first_year_from_tag,
                stats.groups_with_first_year,
            ),
        ),
        ("Pictures", stats.pictures.to_string()),
        (
            "With a cover",
            share(stats.releases_with_cover, stats.releases),
        ),
    ]);
    text.push_str(&listed("Bindings by method", &stats.decisions, bound));
    text.push_str(&listed("Files by codec", &stats.codecs, readable));
    text.push_str(&listed(
        "Release groups by kind",
        &stats.kinds,
        stats.release_groups,
    ));
    if !stats.unreadable_reasons.is_empty() {
        text.push_str("\nWhy files could not be read:\n");
        for reason in &stats.unreadable_reasons {
            text.push_str(&format!("  {:>6}  {}\n", reason.count, reason.name));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use ritornel_client::contract::{Id, Receipt, ReceiptStatus};

    use super::*;

    #[test]
    fn the_stats_say_what_a_scan_produced() {
        let count = |name: &str, count| Count {
            name: name.to_owned(),
            count,
        };
        let stats = LibraryStats {
            library_revision: 31,
            files: 14_000,
            file_bytes: 330_000_000_000,
            files_missing: 2,
            files_unreadable: 7,
            unreadable_reasons: vec![count("unsupported: no suitable format reader found", 7)],
            files_retired: 1,
            codecs: vec![count("flac", 13_300), count("mp3", 693)],
            files_with_musicbrainz_id: 1_400,
            unresolved: 0,
            artists: 900,
            release_groups: 1_150,
            releases: 1_200,
            recordings: 13_500,
            appearances: 13_900,
            loose_tracks: 40,
            decisions: vec![
                count("new", 13_500),
                count("tags_duration", 400),
                count("same_audio", 93),
            ],
            needs_review: 120,
            groups_with_kind: 115,
            kinds: vec![count("album", 100), count("ep", 15)],
            groups_with_first_year: 1_100,
            groups_with_first_year_from_tag: 110,
            pictures: 1_300,
            releases_with_cover: 1_100,
        };
        assert_eq!(
            render_stats(&stats),
            "\
Library revision  31
Files             14000
Size              330.0 GB
Missing           2
Retired           1
Unreadable        7 (0.1%)
With an MBID      1400 (10.0%)
Unresolved        0
Artists           900
Release groups    1150
Releases          1200
Recordings        13500
Track appearances 13900
Loose tracks      40
Need review       120 (0.9%)
Groups with kind  115 (10.0%)
With first year   1100 (95.7%)
  from a tag      110 (10.0%)
Pictures          1300
With a cover      1100 (91.7%)

Bindings by method:
  new             13500 (96.5%)
  tags_duration   400 (2.9%)
  same_audio      93 (0.7%)

Files by codec:
  flac            13300 (95.0%)
  mp3             693 (5.0%)

Release groups by kind:
  album           100 (8.7%)
  ep              15 (1.3%)

Why files could not be read:
       7  unsupported: no suitable format reader found
"
        );
        assert_eq!(share(3, 0), "3");
    }

    #[test]
    fn a_new_library_is_its_id_and_its_first_revision() {
        let created = LibraryCreated {
            receipt: Receipt {
                command_id: Id::from_bytes([0xc0; 16]),
                status: ReceiptStatus::Done,
                revision: Some(1),
            },
            library_id: Id::from_bytes([0x1b; 16]),
        };
        assert_eq!(
            render(&created),
            "\
Library ID        1b1b1b1b-1b1b-1b1b-1b1b-1b1b1b1b1b1b
Library revision  1
"
        );
    }
}
