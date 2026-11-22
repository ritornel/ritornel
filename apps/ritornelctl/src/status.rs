//! `ritornelctl status`: what the running server reports about itself. See
//! "Configuration and logs" in docs/design/server.md.

use std::io::Write;

use ritornel_client::contract::methods::RecordDismiss;
use ritornel_client::contract::{
    BackupStatus, DismissParams, RecordProblem, RecordStatus, ServerStatus, WatchStatus,
    WatchedFolder, WatchedKind,
};
use ritornel_client::{local, record};

use crate::cli::{Error, StatusArgs};
use crate::server;

pub(crate) fn status(
    args: &StatusArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let dismiss = args.dismiss;
    let status = server::ask(&args.server, env, async |client| {
        if dismiss {
            client
                .call::<RecordDismiss>(&DismissParams {
                    idempotency_key: ritornel_client::idempotency_key()?,
                })
                .await?;
        }
        client.status().await
    })?;
    server::print(
        &args.server,
        out,
        &status,
        serde_json::to_string_pretty,
        render,
    )
}

/// The status as a person reads it: one fact a line.
fn render(status: &ServerStatus) -> String {
    let free = status
        .free_disk
        .data_bytes
        .map_or_else(|| "unknown".to_owned(), size);
    let mut lines = vec![
        ("Server ID", status.server_id.to_string()),
        ("Name", status.name.clone()),
        ("Role", status.role.as_str().to_owned()),
        ("Server version", status.versions.server.clone()),
        ("Contract version", status.versions.contract.to_string()),
        ("SQLite version", status.versions.sqlite.clone()),
        ("Server schema", status.versions.server_schema.to_string()),
    ];
    if let Some(schema) = status.versions.library_schema {
        lines.push(("Library schema", schema.to_string()));
    }
    match &status.library {
        Some(library) => {
            lines.push(("Library ID", library.library_id.to_string()));
            lines.push(("Library revision", library.revision.to_string()));
        }
        None => lines.push(("Library", "none".to_owned())),
    }
    lines.push(("Free disk, data", free));
    lines.push(("Log path", status.log_path.clone()));
    if let Some(watch) = &status.watch {
        watching(watch, &mut lines);
    }
    if let Some(backup) = &status.backup {
        backups(backup, &mut lines);
    }
    if let Some(record) = &status.record {
        recording(record, &mut lines);
    }
    server::facts(&lines)
}

/// Whether the record advances, and if not, a line for each thing that
/// stops it, with since when.
fn recording(record: &RecordStatus, lines: &mut Vec<(&str, String)>) {
    if record.problems.is_empty() {
        lines.push(("Record", "advancing".to_owned()));
    }
    for (n, problem) in record.problems.iter().enumerate() {
        lines.push((if n == 0 { "Record" } else { "" }, stopped(problem)));
        lines.push(("", record::then(problem, WORDS)));
    }
    // What happened that no one can act on: told, and quieter.
    for (n, note) in record.notes.iter().enumerate() {
        lines.push((if n == 0 { "Note" } else { "" }, stopped(note)));
        lines.push(("", record::then(note, WORDS)));
    }
}

/// How this program says what the person does.
const WORDS: record::Words<'static> = record::Words {
    dismiss: "`ritornelctl status --dismiss`",
    restart: "restart the server",
    sign_in: "Sign in again with `ritornelctl lastfm signin`.",
};

/// One thing that stops the record, or a note: what, since when and why.
fn stopped(problem: &RecordProblem) -> String {
    let mut line = format!(
        "{}, since {}",
        record::what(problem),
        crate::lastfm::utc(problem.since)
    );
    if let Some(detail) = &problem.detail {
        line.push_str(&format!(": {detail}"));
    }
    line
}

/// Where backups go, the last one and the last failure. With no folder
/// named, it says there is no backup, whatever was taken before.
fn backups(backup: &BackupStatus, lines: &mut Vec<(&str, String)>) {
    let Some(place) = &backup.place else {
        lines.push((
            "Backup",
            "none: no backup folder is named (backup_dir in config.toml)".to_owned(),
        ));
        return;
    };
    lines.push((
        "Backup",
        match &backup.last {
            Some(last) if last.present => format!("{}, {}", crate::lastfm::utc(last.at), last.path),
            Some(last) => format!(
                "none: the last, {}, is no longer at {}",
                crate::lastfm::utc(last.at),
                last.path
            ),
            None => "none yet: run `ritornelctl backup`".to_owned(),
        },
    ));
    lines.push(("Backup folder", place.clone()));
    if let Some(failed) = &backup.failed {
        lines.push((
            "Backup failed",
            format!("{}: {}", crate::lastfm::utc(failed.at), failed.message),
        ));
    }
}

/// Whether the server notices changes to its folders, and each folder's
/// line.
fn watching(watch: &WatchStatus, lines: &mut Vec<(&str, String)>) {
    lines.push((
        "Watching",
        match &watch.off {
            Some(off) => format!("off: {off}"),
            None => "on".to_owned(),
        },
    ));
    for (n, folder) in watch.folders.iter().enumerate() {
        lines.push((if n == 0 { "Watched folders" } else { "" }, watched(folder)));
    }
}

/// A folder: how it is watched, what is waiting, and what was last done on
/// its own.
fn watched(folder: &WatchedFolder) -> String {
    let mut line = format!(
        "{} ({}): {}",
        folder.name,
        folder.kind.as_str(),
        folder.state.as_str()
    );
    if let Some(detail) = &folder.detail {
        line.push_str(&format!(" ({detail})"));
    }
    if let Some(since) = folder.pending_since {
        line.push_str(&format!(
            ", changes waiting since {}",
            crate::lastfm::utc(since)
        ));
    }
    let what = match folder.kind {
        WatchedKind::Music => "rescan",
        WatchedKind::Pictures => "import",
    };
    match &folder.last_auto {
        Some(last) => line.push_str(&format!(
            ", last {what} on its own {} after {}, {}",
            crate::lastfm::utc(last.started_at),
            last.trigger.as_str(),
            last.state.as_str()
        )),
        None => line.push_str(&format!(", no {what} on its own yet")),
    }
    line
}

/// A number of bytes in the largest decimal unit that keeps it above one.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000_000, "TB"),
        (1_000_000_000, "GB"),
        (1_000_000, "MB"),
        (1_000, "kB"),
    ];
    for (unit, name) in UNITS {
        if bytes >= unit {
            // Tenths, cut and not rounded: free space is not overstated.
            let tenths = bytes / (unit / 10);
            return format!("{}.{} {name}", tenths / 10, tenths % 10);
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use ritornel_client::contract::{
        FreeDisk, Id, LibraryStatus, RecordProblemKind, ServerRole, Versions,
    };

    use super::*;

    fn example() -> ServerStatus {
        ServerStatus {
            server_id: Id::from_bytes([0x11; 16]),
            name: "desk".to_owned(),
            role: ServerRole::Unassigned,
            versions: Versions {
                server: "0.1.0".to_owned(),
                contract: 0,
                sqlite: "3.54.1".to_owned(),
                server_schema: 2,
                library_schema: None,
            },
            library: None,
            free_disk: FreeDisk {
                data_bytes: Some(412_345_678_901),
            },
            log_path: "/srv/ritornel/logs".to_owned(),
            watch: None,
            backup: None,
            record: None,
        }
    }

    #[test]
    fn the_status_is_one_fact_a_line() {
        assert_eq!(
            render(&example()),
            "\
Server ID         11111111-1111-1111-1111-111111111111
Name              desk
Role              unassigned
Server version    0.1.0
Contract version  0
SQLite version    3.54.1
Server schema     2
Library           none
Free disk, data   412.3 GB
Log path          /srv/ritornel/logs
"
        );
    }

    #[test]
    fn backups_say_where_they_go_and_when_there_is_none() {
        use ritornel_client::contract::{BackupFailure, BackupMade, BackupReason};

        let tail = |backup: BackupStatus| {
            let mut status = example();
            status.backup = Some(backup);
            render(&status)
                .split_once("Log path          /srv/ritornel/logs\n")
                .unwrap()
                .1
                .to_owned()
        };
        let none = BackupStatus {
            place: None,
            last: None,
            failed: None,
        };
        assert_eq!(
            tail(none.clone()),
            "Backup            none: no backup folder is named (backup_dir in config.toml)\n"
        );
        let place = "/Volumes/Backups/Ritornel".to_owned();
        let at = 1_763_744_462_000;
        assert_eq!(
            tail(BackupStatus {
                place: Some(place.clone()),
                ..none.clone()
            }),
            "\
Backup            none yet: run `ritornelctl backup`
Backup folder     /Volumes/Backups/Ritornel
"
        );
        let last = BackupMade {
            at,
            path: format!("{place}/ritornel-backup-2025-11-21-170102"),
            reason: BackupReason::OnDemand,
            present: true,
        };
        assert_eq!(
            tail(BackupStatus {
                place: Some(place.clone()),
                last: Some(last.clone()),
                failed: Some(BackupFailure {
                    at: at + 86_400_000,
                    message: "the backup folder /Volumes/Backups/Ritornel is not there: is its \
                              disk connected?"
                        .to_owned(),
                }),
            }),
            "\
Backup            2025-11-21 17:01 UTC, /Volumes/Backups/Ritornel/ritornel-backup-2025-11-21-170102
Backup folder     /Volumes/Backups/Ritornel
Backup failed     2025-11-22 17:01 UTC: the backup folder /Volumes/Backups/Ritornel is not there: is its disk connected?
"
        );
        // A backup that has since gone is no backup.
        assert!(
            tail(BackupStatus {
                place: Some(place),
                last: Some(BackupMade {
                    present: false,
                    ..last
                }),
                failed: None,
            })
            .starts_with(
                "Backup            none: the last, 2025-11-21 17:01 UTC, is no longer at "
            )
        );
    }

    #[test]
    fn the_record_says_what_stops_it_and_since_when() {
        let tail = |record: RecordStatus| {
            let mut status = example();
            status.record = Some(record);
            render(&status)
                .split_once("Log path          /srv/ritornel/logs\n")
                .unwrap()
                .1
                .to_owned()
        };
        assert_eq!(
            tail(RecordStatus {
                problems: vec![],
                notes: vec![]
            }),
            "Record            advancing\n"
        );
        let at = 1_763_744_462_000;
        let problem = |kind, count, detail: Option<&str>, dismissable| RecordProblem {
            kind,
            since: at,
            count,
            detail: detail.map(str::to_owned),
            dismissable,
        };
        assert_eq!(
            tail(RecordStatus {
                problems: vec![
                    problem(
                        RecordProblemKind::ListenUnsaved,
                        1,
                        Some("disk I/O error"),
                        false
                    ),
                    problem(RecordProblemKind::ListensUndelivered, 3, None, false),
                    problem(
                        RecordProblemKind::ScrobblesRefused,
                        4,
                        Some("Invalid API key"),
                        true
                    ),
                    problem(RecordProblemKind::SignInNeeded, 1, None, false),
                    problem(
                        RecordProblemKind::ImportFailed,
                        1,
                        Some("Last.fm did not answer"),
                        false
                    ),
                ],
                notes: vec![
                    problem(RecordProblemKind::ScrobblesUnknown, 1, None, true),
                    problem(
                        RecordProblemKind::ScrobblesWithdrawn,
                        2,
                        Some("its play is excluded"),
                        true
                    ),
                ],
            }),
            "\
Record            The listen playing can’t be saved, since 2025-11-21 17:01 UTC: disk I/O error
                  Check that the disk Ritornel’s data is on has free space. Ritornel keeps trying, and saves the listen as soon as it can.
                  3 listens haven’t reached your history, since 2025-11-21 17:01 UTC
                  Ritornel keeps trying. If this lasts, check that the disk Ritornel’s data is on has free space, and look in the server’s log.
                  Last.fm is refusing scrobbles, since 2025-11-21 17:01 UTC: Invalid API key
                  Check the API key and secret Ritornel uses, in lastfm.env in its data folder, then restart the server. It clears by itself once Last.fm takes a scrobble again. Refused scrobbles aren’t sent again; `ritornelctl status --dismiss` sets every scrobble note and warning aside.
                  Last.fm needs you to sign in again, since 2025-11-21 17:01 UTC
                  Last.fm no longer accepts Ritornel’s sign-in, so nothing is scrobbled. Sign in again with `ritornelctl lastfm signin`. If you sign in to the same account, Ritornel sends the plays waiting since then; they stay in your history here either way.
                  Importing from Last.fm failed, since 2025-11-21 17:01 UTC: Last.fm did not answer
                  Ritornel tries again at the next import. If it keeps failing, check the internet connection, and the API key and secret in lastfm.env in Ritornel’s data folder, then restart the server.
Note              1 scrobble may not have reached Last.fm, since 2025-11-21 17:01 UTC
                  The play is still in your history here. If the next import from Last.fm finds it there, this note clears; Ritornel doesn’t send it again. `ritornelctl status --dismiss` sets every scrobble note and warning aside.
                  2 plays weren’t sent to Last.fm, since 2025-11-21 17:01 UTC: its play is excluded
                  They’re still in your history here. Ritornel chose not to send them, for the reason given for the latest. `ritornelctl status --dismiss` sets every scrobble note and warning aside.
"
        );
        // Notes alone: the record advances, and they are said all the same.
        assert!(
            tail(RecordStatus {
                problems: vec![],
                notes: vec![problem(RecordProblemKind::ScrobblesUnknown, 1, None, true)],
            })
            .starts_with("Record            advancing\nNote              1 scrobble")
        );
    }

    #[test]
    fn the_record_says_when_it_cannot_tell_and_how_often_an_import_failed() {
        let mut status = example();
        status.record = Some(RecordStatus {
            problems: vec![
                RecordProblem {
                    kind: RecordProblemKind::ImportFailed,
                    since: 1_763_744_462_000,
                    count: 3,
                    detail: None,
                    dismissable: false,
                },
                RecordProblem {
                    kind: RecordProblemKind::Unreadable,
                    since: 1_763_744_462_000,
                    count: 1,
                    detail: Some("disk I/O error".to_owned()),
                    dismissable: false,
                },
            ],
            notes: vec![],
        });
        let shown = render(&status);
        let record = shown
            .split_once("Log path          /srv/ritornel/logs\n")
            .unwrap()
            .1;
        assert_eq!(
            record,
            "\
Record            Importing from Last.fm failed 3 times in a row, since 2025-11-21 17:01 UTC
                  Ritornel tries again at the next import. If it keeps failing, check the internet connection, and the API key and secret in lastfm.env in Ritornel’s data folder, then restart the server.
                  Ritornel can’t tell whether every listen is being recorded, since 2025-11-21 17:01 UTC: disk I/O error
                  Check that the disk Ritornel’s data is on is connected and has free space, then restart the server.
"
        );
    }

    #[test]
    fn a_hub_shows_the_library_it_holds() {
        let mut status = example();
        status.role = ServerRole::Hub;
        status.versions.library_schema = Some(1);
        status.library = Some(LibraryStatus {
            library_id: Id::from_bytes([0x1b; 16]),
            revision: 58,
        });
        assert_eq!(
            render(&status),
            "\
Server ID         11111111-1111-1111-1111-111111111111
Name              desk
Role              hub
Server version    0.1.0
Contract version  0
SQLite version    3.54.1
Server schema     2
Library schema    1
Library ID        1b1b1b1b-1b1b-1b1b-1b1b-1b1b1b1b1b1b
Library revision  58
Free disk, data   412.3 GB
Log path          /srv/ritornel/logs
"
        );
    }

    #[test]
    fn watching_shows_each_folder_and_what_was_last_done_on_its_own() {
        use ritornel_client::contract::{
            AutoRun, AutoTrigger, JobState, WatchState, WatchedFolder,
        };
        let mut status = example();
        status.watch = Some(WatchStatus {
            on: true,
            enabled: true,
            off: None,
            folders: vec![
                WatchedFolder {
                    name: "music".to_owned(),
                    kind: WatchedKind::Music,
                    state: WatchState::Watching,
                    detail: None,
                    pending_since: Some(1_763_294_410_000),
                    last_auto: Some(AutoRun {
                        trigger: AutoTrigger::Change,
                        started_at: 1_763_294_400_000,
                        finished_at: Some(1_763_294_401_000),
                        state: JobState::Succeeded,
                        job_id: None,
                    }),
                },
                WatchedFolder {
                    name: "share".to_owned(),
                    kind: WatchedKind::Music,
                    state: WatchState::Polling,
                    detail: Some("no events".to_owned()),
                    pending_since: None,
                    last_auto: None,
                },
            ],
        });
        assert!(render(&status).ends_with(
            "\
Watching          on
Watched folders   music (music): watching, changes waiting since 2025-11-16 12:00 UTC, last rescan on its own 2025-11-16 12:00 UTC after change, succeeded
                  share (music): polling (no events), no rescan on its own yet
"
        ));
        status.watch = Some(WatchStatus {
            on: false,
            enabled: true,
            off: Some("this server is not the library's hub".to_owned()),
            folders: Vec::new(),
        });
        assert!(
            render(&status)
                .ends_with("Watching          off: this server is not the library's hub\n")
        );
    }

    #[test]
    fn free_space_the_system_would_not_report_is_unknown() {
        let mut status = example();
        status.free_disk.data_bytes = None;
        assert!(render(&status).contains("Free disk, data   unknown\n"));
    }

    #[test]
    fn a_size_is_cut_to_a_tenth_of_its_unit() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_000), "1.0 kB");
        assert_eq!(size(1_999_999), "1.9 MB");
        assert_eq!(size(412_345_678_901), "412.3 GB");
        assert_eq!(size(8_000_000_000_000), "8.0 TB");
        assert_eq!(size(u64::MAX), "18446744.0 TB");
    }
}
