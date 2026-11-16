//! `ritornelctl status`: what the running server reports about itself. See
//! "Configuration and logs" in docs/design/server.md.
//!
//! It does steps 1 and 2 of "Starting, finding and stopping" and no more:
//! it finds the server through `server.json` and, with no server, says so
//! and exits. It never starts one.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ritornel_client::contract::{CONTRACT_VERSION, ClientKind, HelloParams, ServerStatus};
use ritornel_client::local;

use crate::cli::{Error, StatusArgs};

/// How long the whole command may take. A server on this machine answers
/// in milliseconds.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

pub(crate) fn status(
    args: &StatusArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let data_dir = local::data_dir(args.data_dir.as_deref(), env)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    let status = runtime
        .block_on(async { tokio::time::timeout(ANSWER_WAIT, ask(&data_dir)).await })
        .map_err(|_| Error::TimedOut)??;

    let text = if args.json {
        let mut text =
            serde_json::to_string_pretty(&status).map_err(|error| Error::Output(error.into()))?;
        text.push('\n');
        text
    } else {
        render(&status)
    };
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Output)
}

/// Finds the server on `data_dir`, says hello and asks for its status.
async fn ask(data_dir: &Path) -> Result<ServerStatus, ritornel_client::Error> {
    let client = local::find(data_dir).await?.connect().await?;
    client
        .hello(&HelloParams {
            name: env!("CARGO_PKG_NAME").to_owned(),
            kind: ClientKind::Cli,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            contract_version: CONTRACT_VERSION,
        })
        .await?;
    client.status().await
}

/// The status as a person reads it: one fact a line.
fn render(status: &ServerStatus) -> String {
    let free = status
        .free_disk
        .data_bytes
        .map_or_else(|| "unknown".to_owned(), size);
    let lines = [
        ("Server ID", status.server_id.to_string()),
        ("Name", status.name.clone()),
        ("Role", status.role.as_str().to_owned()),
        ("Server version", status.versions.server.clone()),
        ("Contract version", status.versions.contract.to_string()),
        ("SQLite version", status.versions.sqlite.clone()),
        ("Server schema", status.versions.server_schema.to_string()),
        ("Free disk, data", free),
        ("Log path", status.log_path.clone()),
    ];
    lines
        .iter()
        .map(|(label, value)| format!("{label:<18}{value}\n"))
        .collect()
}

/// A number of bytes in the largest decimal unit that keeps it above one.
fn size(bytes: u64) -> String {
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
    use ritornel_client::contract::{FreeDisk, Id, ServerRole, Versions};

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
                server_schema: 1,
            },
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
Server schema     1
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
