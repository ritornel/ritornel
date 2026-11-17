//! `ritornelctl status`: what the running server reports about itself. See
//! "Configuration and logs" in docs/design/server.md.

use std::io::Write;

use ritornel_client::contract::{ServerStatus, WatchStatus, WatchedFolder, WatchedKind};
use ritornel_client::local;

use crate::cli::{Error, ServerArgs};
use crate::server;

pub(crate) fn status(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let status = server::ask(args, env, async |client| client.status().await)?;
    server::print(args, out, &status, serde_json::to_string_pretty, render)
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
    server::facts(&lines)
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
    use ritornel_client::contract::{FreeDisk, Id, LibraryStatus, ServerRole, Versions};

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
