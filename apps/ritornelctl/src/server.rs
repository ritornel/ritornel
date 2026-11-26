//! Talking to the running server: what every command starts with.
//!
//! It does steps 1 and 2 of "Process lifecycle" in
//! docs/internals/server.md and no more: it finds the server through
//! `server.json` and, with no server, says so. It never starts one.

use std::io::Write;
use std::time::Duration;

use ritornel_client::contract::{CONTRACT_VERSION, ClientKind, HelloParams};
use ritornel_client::{Client, local};

use crate::cli::{Error, ServerArgs};

/// How long finding the server and saying hello may take. A server on this
/// machine answers them in milliseconds.
const FIND_WAIT: Duration = Duration::from_secs(10);

/// How long a whole command may take. Most calls are answered in
/// milliseconds, but one that writes, such as creating the library or
/// rebuilding history, does real work, and a slow machine such as a
/// Raspberry Pi, or a busy one, can take many seconds over it. The bound is
/// there so that a server that never answers cannot hold the command for
/// ever, not to give up on one that is working.
pub(crate) const ANSWER_WAIT: Duration = Duration::from_secs(120);

/// `ritornelctl server stop`: asks the server on the data directory to
/// stop, and waits for it to have stopped. One still stopping when the
/// wait ends is a failure, so that what runs next does not find it.
pub(crate) fn stop(
    args: &ServerArgs,
    env: local::Env<'_>,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let data_dir = local::data_dir(args.data_dir.as_deref(), env)?;
    let stopped = local::stop(&data_dir, ritornel_client::launch::STOP_WAIT)?;
    let printed = print(
        args,
        out,
        &stopped,
        |stopped| serde_json::to_string_pretty(&serde_json::json!({ "stopped": stopped })),
        |stopped| {
            if *stopped {
                "The server has stopped.\n".to_owned()
            } else {
                "The server was asked to stop, and is still stopping.\n".to_owned()
            }
        },
    );
    match (printed, stopped) {
        (Ok(()), false) => Err(Error::StillStopping),
        (printed, _) => printed,
    }
}

/// Finds the server the arguments name, says hello, and makes one call.
pub(crate) fn ask<T>(
    args: &ServerArgs,
    env: local::Env<'_>,
    call: impl AsyncFnOnce(&Client) -> Result<T, ritornel_client::Error>,
) -> Result<T, Error> {
    ask_for(args, env, Some(ANSWER_WAIT), call)
}

/// As [`ask`], for at most `wait`. With none, the command takes as long as
/// what it waits for: a scan that is watched to its end.
pub(crate) fn ask_for<T>(
    args: &ServerArgs,
    env: local::Env<'_>,
    wait: Option<Duration>,
    call: impl AsyncFnOnce(&Client) -> Result<T, ritornel_client::Error>,
) -> Result<T, Error> {
    let data_dir = local::data_dir(args.data_dir.as_deref(), env)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    let answer = runtime.block_on(async {
        let asked = async {
            // Finding the server and saying hello never take long.
            let client = tokio::time::timeout(FIND_WAIT, async {
                let client = local::find(&data_dir).await?.connect().await?;
                client
                    .hello(&HelloParams {
                        presence: None,
                        name: env!("CARGO_PKG_NAME").to_owned(),
                        kind: ClientKind::Cli,
                        version: env!("RITORNEL_VERSION_DISPLAY").to_owned(),
                        contract_version: CONTRACT_VERSION,
                    })
                    .await?;
                Ok::<_, ritornel_client::Error>(client)
            })
            .await
            .map_err(|_| None)?
            .map_err(Some)?;
            call(&client).await.map_err(Some)
        };
        match wait {
            Some(wait) => tokio::time::timeout(wait, asked).await.unwrap_or(Err(None)),
            None => asked.await,
        }
    });
    answer.map_err(|failed| failed.map_or(Error::TimedOut, Error::Client))
}

/// Prints an answer: as JSON, in the shape the server sent it in, or as
/// `render` writes it for a person. `json` is `serde_json::to_string_pretty`.
pub(crate) fn print<T>(
    args: &ServerArgs,
    out: &mut dyn Write,
    answer: &T,
    json: impl FnOnce(&T) -> serde_json::Result<String>,
    render: impl FnOnce(&T) -> String,
) -> Result<(), Error> {
    let text = if args.json {
        let mut text = json(answer).map_err(|error| Error::Output(error.into()))?;
        text.push('\n');
        text
    } else {
        render(answer)
    };
    out.write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .map_err(Error::Output)
}

/// Facts as a person reads them: one a line, the values in a column.
pub(crate) fn facts(lines: &[(&str, String)]) -> String {
    lines
        .iter()
        .map(|(label, value)| format!("{label:<18}{value}\n"))
        .collect()
}
