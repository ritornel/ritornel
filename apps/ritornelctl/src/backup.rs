//! `ritornelctl backup`: a copy of the server's databases, now, in the
//! folder its backups go to. See "Backups and pre-migration snapshots" in
//! docs/internals/server.md.

use std::io::Write;

use ritornel_client::contract::methods::ServerBackup;
use ritornel_client::contract::{BackedUp, BackupParams};
use ritornel_client::local;

use crate::cli::{Error, ServerArgs};
use crate::server;

pub(crate) fn backup(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    // A large library on a slow disk takes as long as it takes.
    let backed_up = server::ask_for(args, env, None, async |client| {
        client
            .call::<ServerBackup>(&BackupParams {
                idempotency_key: ritornel_client::idempotency_key()?,
            })
            .await
    })?;
    server::print(args, out, &backed_up, serde_json::to_string_pretty, render)
}

fn render(backed_up: &BackedUp) -> String {
    match &backed_up.backup.path {
        Some(path) => format!("Backed up to {path}\n"),
        None => "Backed up.\n".to_owned(),
    }
}
