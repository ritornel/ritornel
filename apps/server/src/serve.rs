//! `ritornel-server serve`: one run of the server, from taking the lock to
//! the clean stop. See "Starting, finding and stopping" in
//! docs/design/server.md.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use ritornel_engine::playback::Output;
use ritornel_engine::{Clock, Entropy, OsEntropy, ScanSettings, SystemClock, new_id};
use tokio::runtime::Runtime;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};

use crate::cli::ServeArgs;
use crate::config::{self, Flags, Settings};
use crate::data_dir::{self, HeldDir};
use crate::{Bounds, Env, Error, PlaybackRun, Run, Server, logs};

/// Why a run is stopping.
#[derive(Debug, Clone, Copy)]
enum Stop {
    Interrupt,
    Terminate,
    OwnerGone,
}

impl Stop {
    fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::OwnerGone => "standard input closed",
        }
    }
}

/// Runs the server until it is asked to stop. It writes one line to `out`
/// when it is ready, and nothing else.
///
/// It must be called on the main thread and it blocks that thread. Async
/// work runs on the runtime's own threads.
pub(crate) fn serve(args: &ServeArgs, env: Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let root = data_dir::resolve(args.data_dir.as_deref(), env)?;
    // From here to the end of the run, this is the only server on the
    // directory.
    let held = HeldDir::take(root)?;

    let flags = Flags {
        log_level: args.log_level.clone(),
        listen: args.listen.clone(),
    };
    let settings = config::load(&held.dir().config_toml(), &flags, env)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entropy: Arc<dyn Entropy> = Arc::new(OsEntropy);
    let boot_id = new_id(clock.now_ms(), &*entropy)?;
    logs::init(
        &held.dir().logs(),
        boot_id,
        settings.log_level,
        Arc::clone(&clock),
    )?;
    info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        data_dir = %held.dir().root().display(),
        owned = args.owned,
        "starting"
    );

    let jobs = Run {
        boot_id: Some(boot_id),
        scan: test_hooks(env),
        playback: playback(args.output.as_deref(), env),
        lastfm: lastfm(env),
    };
    let run = run(held, args, settings, clock, entropy, jobs, out);
    match &run {
        Ok(()) => info!("stopped"),
        Err(error) => error!(%error, "stopped on an error"),
    }
    run
}

/// The part of a run that is logged: everything after the lock is held and
/// the log is open.
fn run(
    held: HeldDir,
    args: &ServeArgs,
    settings: Settings,
    clock: Arc<dyn Clock>,
    entropy: Arc<dyn Entropy>,
    jobs: Run,
    out: &mut dyn Write,
) -> Result<(), Error> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("ritornel-async")
        .enable_all()
        .build()
        .map_err(Error::Runtime)?;
    let (stop, stopped) = mpsc::channel();
    watch_signals(&runtime, &stop).map_err(Error::Runtime)?;
    if args.owned {
        watch_stdin(stop.clone()).map_err(Error::Runtime)?;
    }

    #[cfg(all(feature = "media-session", target_os = "macos"))]
    let media_session = jobs.playback.media_session;
    // Everything that can stop the server is being watched, so it may
    // start, and tell a client or a launcher where it is.
    let server = runtime.block_on(Server::start_run(
        held,
        settings.listen,
        clock,
        entropy,
        Bounds::default(),
        jobs,
    ))?;
    if let Some(per_second) = args.invent_churn {
        server.invent_churn(per_second)?;
    }
    #[cfg(all(feature = "media-session", target_os = "macos"))]
    if media_session {
        server.start_media_session()?;
    }
    info!(server_id = %server.server_id(), name = %server.name(), "ready");
    let ready = writeln!(
        out,
        "{} pid={} server_id={}",
        ritornel_contract::local::READY,
        std::process::id(),
        server.server_id()
    )
    .and_then(|()| out.flush())
    .map_err(Error::Output);

    // The main thread waits here. On macOS it blocks in the platform run
    // loop, which the media keys need, and stopping stops that loop. `stop`
    // is still in scope, so the channel cannot close under the wait.
    if ready.is_ok() {
        #[cfg(all(feature = "media-session", target_os = "macos"))]
        let reason = crate::media_session::wait(stopped);
        #[cfg(not(all(feature = "media-session", target_os = "macos")))]
        let reason = stopped.recv().ok();
        info!(
            reason = reason.map_or("nothing left to wait for", Stop::as_str),
            "stopping"
        );
    }

    let stopped = runtime.block_on(server.stop());
    runtime.shutdown_background();
    ready.and(stopped)
}

/// How a scan runs, with what the crash test asks of it. Two environment
/// variables are read, and they are for that test alone: neither is a
/// setting, and neither is in the help.
///
/// - `RITORNEL_TEST_SCAN_BATCH_FILES`: files to a step, so that a scan of
///   a few fixtures has steps to be cut off between.
/// - `RITORNEL_TEST_FAIL_AFTER_SCAN_STEP`: the fail point. The server
///   aborts straight after the library commit of that step of a scan,
///   before the step's checkpoint is saved, as a crash there would.
fn test_hooks(env: Env<'_>) -> ScanSettings {
    let number = |name: &str| env(name).and_then(|value| value.to_str()?.parse::<i64>().ok());
    let mut scan = ScanSettings::default();
    if let Some(files) =
        number("RITORNEL_TEST_SCAN_BATCH_FILES").and_then(|files| usize::try_from(files).ok())
    {
        scan.batch_files = files;
    }
    scan.fail_after_step = number("RITORNEL_TEST_FAIL_AFTER_SCAN_STEP");
    scan
}

/// How this run plays: the output `--output` names, and what the crash tests
/// ask of the player. Four environment variables are read, and they are
/// for the tests alone: none is a setting, and none is in the help.
///
/// - `RITORNEL_TEST_NULL_OUTPUT_SPEED`: how many times faster than real
///   time the null output plays.
/// - `RITORNEL_TEST_FAIL_AT_POSITION_MS`: the server aborts once the track
///   playing passes this position, with its listen open and its last
///   checkpoint saved, as a crash there would.
/// - `RITORNEL_TEST_FAIL_AFTER_INGEST`: the server aborts once the library
///   has taken a listen in and before `server.db` lets it go.
/// - `RITORNEL_TEST_NO_MEDIA_SESSION`: the player is not shown in the
///   system's media session, so a test that plays leaves the media keys
///   where they were.
fn playback(output: Option<&str>, env: Env<'_>) -> PlaybackRun {
    let number = |name: &str| env(name).and_then(|value| value.to_str()?.parse::<i64>().ok());
    let speed = number("RITORNEL_TEST_NULL_OUTPUT_SPEED")
        .and_then(|speed| u32::try_from(speed).ok())
        .unwrap_or(1);
    let output = match output {
        Some("default") => Some(Output::Default),
        Some("null") => Some(Output::Null { speed }),
        Some(_) => None,
        None => Output::has_device().then_some(Output::Default),
    };
    PlaybackRun {
        output,
        fail_at_position_ms: number("RITORNEL_TEST_FAIL_AT_POSITION_MS"),
        fail_after_ingest: env("RITORNEL_TEST_FAIL_AFTER_INGEST").is_some(),
        media_session: env("RITORNEL_TEST_NO_MEDIA_SESSION").is_none(),
    }
}

/// How this run reaches Last.fm. The API key and secret are read from the
/// environment, as `scrobl`'s own tool reads its key, and failing that from
/// a file in the data directory. Four more variables are for the tests
/// alone: none is a setting, and none is in the help.
///
/// - `RITORNEL_TEST_LASTFM_URL`: a loopback address to send every request
///   to, with no pause between them. `scrobl` refuses any other.
/// - `RITORNEL_TEST_LASTFM_CHUNK` and `RITORNEL_TEST_LASTFM_PAGE`: the most
///   scrobbles to a span, and rows to a page, so that a small history is
///   read in several batches.
/// - `RITORNEL_TEST_FAIL_AFTER_IMPORT_BATCH`: the server aborts straight
///   after the library commits that many batches of a sync, as a crash
///   there would.
fn lastfm(env: Env<'_>) -> crate::LastfmRun {
    let text = |name: &str| env(name).and_then(|value| value.into_string().ok());
    let number = |name: &str| text(name).and_then(|value| value.parse::<u64>().ok());
    crate::LastfmRun {
        #[cfg(feature = "lastfm")]
        credentials: match (
            text(ritornel_lastfm::KEY_VARIABLE),
            text(ritornel_lastfm::SECRET_VARIABLE),
        ) {
            (Some(key), Some(secret)) if !key.trim().is_empty() && !secret.trim().is_empty() => {
                Some(ritornel_lastfm::ApiCredentials::new(
                    key.trim(),
                    secret.trim(),
                ))
            }
            _ => None,
        },
        base_url: text("RITORNEL_TEST_LASTFM_URL"),
        chunk: number("RITORNEL_TEST_LASTFM_CHUNK"),
        page_size: number("RITORNEL_TEST_LASTFM_PAGE").and_then(|page| u32::try_from(page).ok()),
        fail_after_batch: number("RITORNEL_TEST_FAIL_AFTER_IMPORT_BATCH"),
    }
}

/// Sends a stop when the process gets SIGINT or SIGTERM. The handlers are in
/// place when this returns.
fn watch_signals(runtime: &Runtime, stop: &Sender<Stop>) -> std::io::Result<()> {
    let _in_runtime = runtime.enter();
    for (kind, reason) in [
        (SignalKind::interrupt(), Stop::Interrupt),
        (SignalKind::terminate(), Stop::Terminate),
    ] {
        let mut signals = signal(kind)?;
        let stop = stop.clone();
        runtime.spawn(async move {
            if signals.recv().await.is_some() {
                // The main thread has gone only if it is stopping already.
                let _ = stop.send(reason);
            }
        });
    }
    Ok(())
}

/// Sends a stop when standard input reaches its end: the program that
/// started this server held the other end of that pipe and has let go of
/// it, or died. See decision 4 in docs/design/server.md.
///
/// A blocking read cannot be cancelled, so it has a thread of its own, which
/// ends with the process.
fn watch_stdin(stop: Sender<Stop>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("owner pipe".to_owned())
        .spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let mut ignored = [0; 64];
            loop {
                match stdin.read(&mut ignored) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = stop.send(Stop::OwnerGone);
        })
        .map(|_| ())
}
