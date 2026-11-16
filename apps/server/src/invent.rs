//! `ritornel-server invent`: developer-only, and not in the help. Fills the
//! library on a data directory with an invented one, for developing and
//! measuring before there is a scan of real music.
//!
//! ```text
//! ritornel-server invent --data-dir <dir> --root <dir> --albums <n>
//!                        [--seed <n>] [--cover-min <px>] [--cover-max <px>]
//!                        [--showcase]
//! ```
//!
//! `--showcase` adds, after the drawn records, a small library made by hand
//! for judging an artist's page and an album's.
//!
//! It takes the data directory's lock, as `serve` does, so it runs while no
//! server does. A server with no library is given one first, as
//! `library.create` would. The covers are written under `--root`, which is
//! recorded as the root named `invented`; nothing else is written there.
//! The same seed makes the same library. See `ritornel_engine::invent`.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::Args;
use ritornel_engine::invent::{self, Plan};
use ritornel_engine::{
    Clock, Entropy, LibraryStore, OsEntropy, ServerStore, SystemClock, WriteRequest,
};

use crate::data_dir::{self, HeldDir};
use crate::{Env, Error, credential, library};

/// The arguments of `ritornel-server invent`.
#[derive(Debug, PartialEq, Eq, Args)]
pub(crate) struct InventArgs {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Where the covers are written.
    #[arg(long)]
    root: PathBuf,
    /// How many records.
    #[arg(long)]
    albums: usize,
    /// What the library is made from.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// The smallest side of a cover, in pixels.
    #[arg(long, default_value_t = 600)]
    cover_min: u32,
    /// The largest side of a cover, in pixels.
    #[arg(long, default_value_t = 1500)]
    cover_max: u32,
    /// Adds a small library made by hand, for judging an artist's page and
    /// an album's: see `ritornel_engine::invent::seed_showcase`.
    #[arg(long)]
    showcase: bool,
}

fn failed(doing: &'static str, path: &std::path::Path, why: impl std::fmt::Display) -> Error {
    Error::Io {
        doing,
        path: path.to_owned(),
        source: std::io::Error::other(why.to_string()),
    }
}

/// Invents a library on the data directory, and says how it went on `out`.
pub(crate) fn run(args: &InventArgs, env: Env<'_>, out: &mut dyn Write) -> Result<(), Error> {
    let held = HeldDir::take(data_dir::resolve(args.data_dir.as_deref(), env)?)?;
    let dir = held.dir();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entropy: Arc<dyn Entropy> = Arc::new(OsEntropy);
    let servers = ServerStore::open(&dir.server_db(), Arc::clone(&clock), Arc::clone(&entropy))?;
    let (identity, issued) = servers.ensure_identity(crate::server::DEFAULT_NAME)?;
    credential::ensure(&servers, &dir.credential(), issued)?;
    let (identity, library) = library::open(dir, &servers, identity, &clock, &entropy)?;
    let library = match library {
        Some(library) => library,
        None => {
            let request = WriteRequest {
                method: "invent.create",
                command: None,
                client: None,
            };
            let (library, _) = LibraryStore::create(
                &dir.private_library_db()?,
                &identity,
                request,
                |_| String::new(),
                Arc::clone(&clock),
                Arc::clone(&entropy),
            )
            .map_err(Error::Library)?;
            servers.claim_library(library.library_id())?;
            library
        }
    };
    let plan = Plan {
        albums: args.albums,
        seed: args.seed,
        root: args.root.clone(),
        now_ms: clock.now_ms(),
        cover_sides: (args.cover_min, args.cover_max.max(args.cover_min)),
    };
    let started = Instant::now();
    let invented = invent::seed(&library, &servers, &plan, |done, of| {
        // Progress is for a person watching; there may be none.
        let _ = writeln!(out, "invented {done} of {of} records");
    })
    .map_err(|error| failed("invent", &args.root, error))?;
    if args.showcase {
        let shown = invent::seed_showcase(&library, &servers, &args.root, plan.now_ms)
            .map_err(|error| failed("invent", &args.root, error))?;
        writeln!(
            out,
            "added a showcase of {} artists, {} records and {} tracks",
            shown.artists, shown.groups, shown.tracks
        )
        .map_err(Error::Output)?;
    }
    writeln!(
        out,
        "invented {} artists, {} records ({} releases), {} tracks and {} covers ({} MB) in {:.1} s",
        invented.artists,
        invented.groups,
        invented.releases,
        invented.tracks,
        invented.covers,
        invented.cover_bytes / 1_000_000,
        started.elapsed().as_secs_f64()
    )
    .map_err(Error::Output)?;
    library.close().map_err(Error::Library)?;
    servers.close()?;
    Ok(())
}
