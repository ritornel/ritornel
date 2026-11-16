//! `ritornel-server serve`: one run of the server, from taking the lock to
//! the clean stop. See "Starting, finding and stopping" in
//! docs/design/server.md.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};

use ritornel_engine::{Clock, Entropy, OsEntropy, SystemClock, new_id};
use tokio::runtime::Runtime;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};

use crate::cli::ServeArgs;
use crate::config::{self, Flags, Settings};
use crate::data_dir::{self, HeldDir};
use crate::{Env, Error, Server, logs};

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

    let run = run(held, args, settings, clock, entropy, out);
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

    // Everything that can stop the server is being watched, so it may
    // start, and tell a client or a launcher where it is.
    let server = runtime.block_on(Server::start(held, settings.listen, clock, entropy))?;
    info!(server_id = %server.server_id(), name = %server.name(), "ready");
    let ready = writeln!(
        out,
        "ready pid={} server_id={}",
        std::process::id(),
        server.server_id()
    )
    .and_then(|()| out.flush())
    .map_err(Error::Output);

    // The main thread waits here. On macOS this is where it will block in
    // the platform run loop, and stopping will mean stopping that loop.
    // `stop` is still in scope, so the channel cannot close under the wait.
    if ready.is_ok() {
        let reason = stopped
            .recv()
            .map_or("nothing left to wait for", Stop::as_str);
        info!(reason, "stopping");
    }

    let stopped = runtime.block_on(server.stop());
    runtime.shutdown_background();
    ready.and(stopped)
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
