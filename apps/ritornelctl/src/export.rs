//! `ritornelctl export <folder>`: the record, written by the server into a
//! new folder of its own in `<folder>`. See "Export" in
//! docs/internals/history.md.

use std::io::Write;

use ritornel_client::contract::methods::HistoryExport;
use ritornel_client::contract::{HistoryExportParams, HistoryExported};
use ritornel_client::local;

use crate::cli::{Error, ExportArgs};
use crate::server;

pub(crate) fn export(
    args: &ExportArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    // The server is on this machine, and is given the folder as a whole
    // path, since its own working folder is not this one.
    let folder = std::path::absolute(&args.folder)
        .map_err(|_| Error::Nothing("give a folder to export into"))?;
    let folder = folder
        .to_str()
        .ok_or(Error::Nothing("the folder's path must be text"))?
        .to_owned();
    // A long history on a slow disk takes as long as it takes.
    let exported = server::ask_for(&args.server, env, None, async |client| {
        client
            .call::<HistoryExport>(&HistoryExportParams {
                idempotency_key: ritornel_client::idempotency_key()?,
                folder,
            })
            .await
    })?;
    server::print(
        &args.server,
        out,
        &exported,
        serde_json::to_string_pretty,
        render,
    )
}

fn render(exported: &HistoryExported) -> String {
    format!(
        "Exported {} observations, {} ratings, {} scrobbles and {} plays to {}\n",
        exported.observations, exported.ratings, exported.deliveries, exported.plays, exported.path
    )
}
