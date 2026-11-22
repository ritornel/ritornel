//! `ritornelctl identity re-resolve --dry-run`: the resolver run again over
//! what the library has stored, by the policy scans use, by another named
//! policy or by other values, and what that would change. It writes nothing.
//! See "Recording resolution" in docs/internals/identity.md.
//!
//! `ritornelctl identity review`: the bindings marked for review, with the
//! candidate each nearly matched and why it did not bind.

use std::io::Write;
use std::time::Duration;

use ritornel_client::contract::methods::{IdentityDryRun, IdentityReview};
use ritornel_client::contract::{
    Binding, DryRun, DryRunParams, FileFacts, ItemOutcome, Nearest, NoParams, PolicyOverrides,
    ReviewList,
};
use ritornel_client::local;

use crate::cli::{Error, ReResolveArgs, ServerArgs};
use crate::server;

/// How long a dry run may take. It resolves the whole library: seconds for
/// a large one.
const DRY_RUN_WAIT: Duration = Duration::from_secs(300);

/// A fraction from 0 to 1 given on the command line.
fn fraction(text: Option<&str>, message: &'static str) -> Result<Option<f64>, Error> {
    text.map(|text| {
        text.parse::<f64>()
            .ok()
            .filter(|value| (0.0..=1.0).contains(value))
            .ok_or(Error::Usage(message))
    })
    .transpose()
}

/// A switch given on the command line: `on` or `off`.
fn switch(text: Option<&str>, message: &'static str) -> Result<Option<bool>, Error> {
    text.map(|text| match text {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(Error::Usage(message)),
    })
    .transpose()
}

fn overrides(args: &ReResolveArgs) -> Result<PolicyOverrides, Error> {
    Ok(PolicyOverrides {
        version: args
            .policy
            .as_deref()
            .map(|text| match text {
                "1" => Ok(1),
                "2" => Ok(2),
                "3" => Ok(3),
                _ => Err(Error::Usage("--policy needs 1, 2 or 3")),
            })
            .transpose()?,
        bind_max: fraction(
            args.bind_max.as_deref(),
            "--bind-max needs a number from 0 to 1",
        )?,
        min_gap: fraction(
            args.min_gap.as_deref(),
            "--min-gap needs a number from 0 to 1",
        )?,
        review_below: fraction(
            args.review_below.as_deref(),
            "--review-below needs a number from 0 to 1",
        )?,
        duration_grace_ms: args
            .duration_grace_ms
            .as_deref()
            .map(|text| {
                text.parse::<u64>()
                    .ok()
                    .filter(|ms| *ms <= 60_000)
                    .ok_or(Error::Usage(
                        "--duration-grace-ms needs a whole number of milliseconds, at most 60000",
                    ))
            })
            .transpose()?,
        across_albums_within_ms: args
            .across_albums_within_ms
            .as_deref()
            .map(|text| {
                text.parse::<u64>()
                    .ok()
                    .filter(|ms| *ms <= 60_000)
                    .ok_or(Error::Usage(
                        "--across-albums-within-ms needs a whole number of milliseconds, at most 60000",
                    ))
            })
            .transpose()?,
        tracks_of_one_release_differ: switch(
            args.tracks_of_one_release_differ.as_deref(),
            "--tracks-of-one-release-differ is on or off",
        )?,
        marker_list: args
            .marker_list
            .as_deref()
            .map(|text| match text {
                "1" => Ok(1),
                "2" => Ok(2),
                _ => Err(Error::Usage("--marker-list needs 1 or 2")),
            })
            .transpose()?,
        fold_accents: switch(args.fold_accents.as_deref(), "--fold-accents is on or off")?,
        discs_from_folders: switch(
            args.discs_from_folders.as_deref(),
            "--discs-from-folders is on or off",
        )?,
        disc_from_album_title: switch(
            args.disc_from_album_title.as_deref(),
            "--disc-from-album-title is on or off",
        )?,
        tags_bind_across_albums: switch(
            args.tags_across_albums.as_deref(),
            "--tags-across-albums is on or off",
        )?,
    })
}

pub(crate) fn re_resolve(
    args: &ReResolveArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    if !args.dry_run {
        return Err(Error::Usage(
            "re-resolve only runs with --dry-run: a scan resolves the library at its end",
        ));
    }
    let params = DryRunParams {
        policy: overrides(args)?,
        detail: args.detail,
    };
    let run = server::ask_for(&args.server, env, Some(DRY_RUN_WAIT), async |client| {
        client.call::<IdentityDryRun>(&params).await
    })?;
    server::print(
        &args.server,
        out,
        &run,
        serde_json::to_string_pretty,
        render_run,
    )
}

pub(crate) fn review(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let list = server::ask_for(args, env, Some(DRY_RUN_WAIT), async |client| {
        client.call::<IdentityReview>(&NoParams {}).await
    })?;
    server::print(
        args,
        out,
        &list,
        serde_json::to_string_pretty,
        render_review,
    )
}

/// A length as minutes and seconds.
fn length(ms: u64) -> String {
    let seconds = (ms + 500) / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// A file's tags on one line.
fn tags(file: &FileFacts) -> String {
    let or = |text: &Option<String>| text.clone().unwrap_or_else(|| "?".to_owned());
    format!(
        "{} — {} — {} — {}",
        or(&file.title),
        or(&file.artist),
        or(&file.album),
        file.duration_ms.map_or_else(|| "?".to_owned(), length)
    )
}

fn nearest(nearest: &Nearest) -> String {
    let mut text = format!("distance {:.3}", nearest.distance);
    if let Some(penalties) = &nearest.penalties {
        let mut parts = Vec::new();
        for (name, value) in [
            ("title", penalties.title),
            ("artist", penalties.artist),
            ("length", penalties.duration),
            ("album", penalties.album),
        ] {
            if value > 0.0 {
                parts.push(format!("{name} {value:.2}"));
            }
        }
        if penalties.shared_identifier {
            parts.push("shared identifier, halved".to_owned());
        }
        if !parts.is_empty() {
            text.push_str(&format!(": {}", parts.join(", ")));
        }
    }
    text
}

fn binding(binding: &Binding) -> String {
    let mut text = binding.method.clone().unwrap_or_else(|| "held".to_owned());
    if let Some(score) = binding.score.filter(|score| *score > 0.0) {
        text.push_str(&format!(" {score:.3}"));
    }
    match binding.recording_id {
        Some(id) => text.push_str(&format!(", recording {id}")),
        None => text.push_str(", a new recording"),
    }
    if binding.needs_review {
        text.push_str(", for review");
    }
    if let Some(title) = &binding.release_title {
        text.push_str(&format!(", on \"{title}\""));
        if let Some(disc) = binding.disc {
            text.push_str(&format!(" disc {disc}"));
        }
    }
    text
}

fn outcome(outcome: ItemOutcome) -> &'static str {
    match outcome {
        ItemOutcome::Unchanged => "unchanged",
        ItemOutcome::WouldChange => "would change",
        ItemOutcome::Pinned => "pinned",
        ItemOutcome::Blocked => "blocked",
    }
}

/// What the policy was, in words.
fn policy(run: &DryRun) -> String {
    let values = &run.policy;
    if values.exact {
        return format!("policy {}", values.resolver_version);
    }
    let mut changed = vec![
        format!("bind at {}", values.bind_max),
        format!("gap {}", values.min_gap),
        format!("review below {}", values.review_below),
        format!("free length band {} ms", values.duration_grace_ms),
    ];
    changed.push(format!("marker list {}", values.marker_list));
    if !values.tags_bind_across_albums {
        changed.push(format!(
            "across albums only within {} ms",
            values.across_albums_within_ms
        ));
    }
    for (on, name) in [
        (values.fold_accents, "accents folded"),
        (values.discs_from_folders, "discs from folders"),
        (values.disc_from_album_title, "discs from album titles"),
        (values.tags_bind_across_albums, "tags bind across albums"),
        (
            values.tracks_of_one_release_differ,
            "tracks of one release differ",
        ),
    ] {
        if on {
            changed.push(name.to_owned());
        }
    }
    format!(
        "a variant of policy {}: {}",
        values.resolver_version,
        changed.join(", ")
    )
}

/// The dry run as a person reads it.
fn render_run(run: &DryRun) -> String {
    let yes = |flag: bool| if flag { "yes" } else { "no" }.to_owned();
    let mut lines = vec![
        ("Policy", policy(run)),
        ("Library revision", run.library_revision.to_string()),
        ("Files", run.items.to_string()),
        ("Unchanged", run.unchanged.to_string()),
        (
            "Would change",
            format!(
                "{} ({} to another recording)",
                run.would_change, run.recordings_changed
            ),
        ),
        ("Pinned", run.pinned.to_string()),
        ("Blocked", run.blocked.to_string()),
        ("Merges", run.merges.to_string()),
        ("Splits", run.splits.to_string()),
        ("Reviews now", run.reviews_now.to_string()),
        (
            "Reviews after",
            format!(
                "{} ({} cleared, {} added)",
                run.reviews_after, run.reviews_cleared, run.reviews_added
            ),
        ),
        ("Releases now", run.releases_now.to_string()),
        ("Releases after", run.releases_after.to_string()),
        ("Would write", yes(run.would_write)),
    ];
    if run.claims_stale {
        lines.push(("Claims stale", yes(true)));
    }
    let mut text = server::facts(&lines);
    for change in &run.changes {
        text.push_str(&format!(
            "\n{}  {}\n  file     {}\n",
            outcome(change.outcome),
            change.file.path,
            tags(&change.file)
        ));
        if let Some(now) = &change.now {
            text.push_str(&format!("  now      {}\n", binding(now)));
        }
        if let Some(after) = &change.after {
            text.push_str(&format!("  after    {}\n", binding(after)));
            if let Some(near) = &after.nearest {
                text.push_str(&format!(
                    "  nearest  {}\n           {}\n           {}\n",
                    near.file.path,
                    tags(&near.file),
                    nearest(near)
                ));
            }
        }
    }
    text
}

/// The review listing as a person reads it.
fn render_review(list: &ReviewList) -> String {
    let total: u64 = list.causes.iter().map(|count| count.count).sum();
    let mut text = format!("{total} marked for review.\n");
    for count in &list.causes {
        text.push_str(&format!("\n{}: {}\n", count.name, count.count));
        for review in list
            .reviews
            .iter()
            .filter(|review| review.cause.as_str() == count.name)
        {
            text.push_str(&format!(
                "\n  {}\n    this     {}\n",
                review.file.path,
                tags(&review.file)
            ));
            if let Some(near) = &review.nearest {
                text.push_str(&format!(
                    "    nearest  {}\n             {}\n             {}",
                    near.file.path,
                    tags(&near.file),
                    nearest(near)
                ));
                if let Some(gap) = review.runner_up_gap {
                    text.push_str(&format!(", runner-up {gap:.3} behind"));
                }
                text.push('\n');
                if let Some(id) = near.recording_id {
                    text.push_str(&format!("             recording {id}\n"));
                }
            }
            for contradiction in &review.contradictions {
                text.push_str(&format!("    {contradiction}\n"));
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use ritornel_client::contract::{
        Count, Id, ItemChange, Penalties, PolicyValues, Review, ReviewCause,
    };

    use super::*;

    fn file(byte: u8, path: &str, album: &str, ms: u64) -> FileFacts {
        FileFacts {
            item_id: Id::from_bytes([byte; 16]),
            path: path.to_owned(),
            title: Some("Glass Pier".to_owned()),
            artist: Some("Ninefold".to_owned()),
            album: Some(album.to_owned()),
            disc: None,
            track: Some(1),
            duration_ms: Some(ms),
        }
    }

    fn nearest_to(byte: u8) -> Nearest {
        Nearest {
            file: file(byte, "Ninefold/Long Field/01.flac", "Long Field", 200_000),
            distance: 0.0645,
            penalties: Some(Penalties {
                title: 0.0,
                artist: 0.0,
                duration: 0.2,
                album: 0.0,
                shared_identifier: false,
            }),
            recording_id: None,
        }
    }

    #[test]
    fn a_dry_run_reads_as_counts_and_then_each_file_it_would_change() {
        let mut run = DryRun {
            policy: PolicyValues {
                resolver_version: 2,
                exact: false,
                bind_max: 0.04,
                min_gap: 0.25,
                review_below: 0.25,
                duration_grace_ms: 6000,
                fold_accents: true,
                discs_from_folders: false,
                disc_from_album_title: false,
                tags_bind_across_albums: false,
                across_albums_within_ms: 20,
                tracks_of_one_release_differ: true,
                marker_list: 2,
            },
            library_revision: 9,
            claims_stale: false,
            would_write: true,
            items: 3,
            unchanged: 2,
            would_change: 1,
            recordings_changed: 1,
            pinned: 0,
            blocked: 0,
            merges: 1,
            splits: 0,
            reviews_now: 1,
            reviews_after: 0,
            reviews_cleared: 1,
            reviews_added: 0,
            releases_now: 2,
            releases_after: 2,
            changes: Vec::new(),
        };
        run.changes.push(ItemChange {
            outcome: ItemOutcome::WouldChange,
            file: file(2, "Ninefold/Other/01.flac", "Other", 206_000),
            now: Some(Binding {
                recording_id: Some(Id::from_bytes([0x22; 16])),
                method: Some("new".to_owned()),
                score: Some(0.0),
                needs_review: true,
                release_id: Some(Id::from_bytes([0x33; 16])),
                release_title: Some("Other".to_owned()),
                disc: None,
                nearest: None,
            }),
            after: Some(Binding {
                recording_id: Some(Id::from_bytes([0x11; 16])),
                method: Some("tags_duration".to_owned()),
                score: Some(0.0129),
                needs_review: false,
                release_id: Some(Id::from_bytes([0x33; 16])),
                release_title: Some("Other".to_owned()),
                disc: Some(1),
                nearest: Some(nearest_to(1)),
            }),
        });
        assert_eq!(
            render_run(&run),
            "\
Policy            a variant of policy 2: bind at 0.04, gap 0.25, review below 0.25, free length band 6000 ms, marker list 2, across albums only within 20 ms, accents folded, tracks of one release differ
Library revision  9
Files             3
Unchanged         2
Would change      1 (1 to another recording)
Pinned            0
Blocked           0
Merges            1
Splits            0
Reviews now       1
Reviews after     0 (1 cleared, 0 added)
Releases now      2
Releases after    2
Would write       yes

would change  Ninefold/Other/01.flac
  file     Glass Pier — Ninefold — Other — 3:26
  now      new, recording 22222222-2222-2222-2222-222222222222, for review, on \"Other\"
  after    tags_duration 0.013, recording 11111111-1111-1111-1111-111111111111, on \"Other\" disc 1
  nearest  Ninefold/Long Field/01.flac
           Glass Pier — Ninefold — Long Field — 3:20
           distance 0.065: length 0.20
"
        );
    }

    #[test]
    fn the_review_listing_groups_by_cause_with_counts() {
        let list = ReviewList {
            library_revision: 9,
            causes: vec![
                Count {
                    name: "other_album".to_owned(),
                    count: 1,
                },
                Count {
                    name: "length_same_album".to_owned(),
                    count: 1,
                },
            ],
            reviews: vec![
                Review {
                    cause: ReviewCause::OtherAlbum,
                    file: file(3, "Various/Night Drives/07.flac", "Night Drives", 201_000),
                    nearest: Some(Nearest {
                        distance: 0.0323,
                        penalties: Some(Penalties {
                            title: 0.0,
                            artist: 0.0,
                            duration: 0.0,
                            album: 1.0,
                            shared_identifier: false,
                        }),
                        recording_id: Some(Id::from_bytes([0x44; 16])),
                        ..nearest_to(1)
                    }),
                    runner_up_gap: None,
                    candidates: 1,
                    contradictions: Vec::new(),
                },
                Review {
                    cause: ReviewCause::LengthSameAlbum,
                    file: file(2, "Ninefold/Long Field (2)/01.flac", "Long Field", 206_000),
                    nearest: Some(nearest_to(1)),
                    runner_up_gap: None,
                    candidates: 1,
                    contradictions: Vec::new(),
                },
            ],
        };
        assert_eq!(
            render_review(&list),
            "\
2 marked for review.

other_album: 1

  Various/Night Drives/07.flac
    this     Glass Pier — Ninefold — Night Drives — 3:21
    nearest  Ninefold/Long Field/01.flac
             Glass Pier — Ninefold — Long Field — 3:20
             distance 0.032: album 1.00
             recording 44444444-4444-4444-4444-444444444444

length_same_album: 1

  Ninefold/Long Field (2)/01.flac
    this     Glass Pier — Ninefold — Long Field — 3:26
    nearest  Ninefold/Long Field/01.flac
             Glass Pier — Ninefold — Long Field — 3:20
             distance 0.065: length 0.20
"
        );
    }

    #[test]
    fn a_threshold_out_of_range_is_told_with_a_fixed_message() {
        let args = |bind_max: &str, grace: &str| ReResolveArgs {
            dry_run: true,
            bind_max: Some(bind_max.to_owned()),
            duration_grace_ms: Some(grace.to_owned()),
            ..ReResolveArgs::default()
        };
        let given = overrides(&args("0.1", "3000")).unwrap();
        assert_eq!(
            (given.bind_max, given.duration_grace_ms, given.fold_accents),
            (Some(0.1), Some(3000), None)
        );
        for (bind_max, grace, message) in [
            ("1.5", "0", "--bind-max needs a number from 0 to 1"),
            ("SENTINEL", "0", "--bind-max needs a number from 0 to 1"),
            (
                "0",
                "60001",
                "--duration-grace-ms needs a whole number of milliseconds, at most 60000",
            ),
        ] {
            match overrides(&args(bind_max, grace)) {
                Err(Error::Usage(said)) => assert_eq!(said, message),
                other => panic!("{other:?}"),
            }
        }
    }
}
