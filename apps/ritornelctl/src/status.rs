//! `ritornelctl status`: what the running server reports about itself. See
//! "Configuration and logs" in docs/design/server.md.

use std::io::Write;

use ritornel_client::contract::ServerStatus;
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
    server::facts(&lines)
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
