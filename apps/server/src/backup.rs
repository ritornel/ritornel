//! Backups: a copy of this server's databases in a folder of its own, taken
//! on demand and before a database is migrated. See "Backups and
//! pre-migration snapshots" in docs/internals/server.md.
//!
//! A backup is written to a place named outside the data directory, since
//! a copy that lives beside what it copies is lost with it. With no place
//! named there is no backup. A migration then still keeps a copy in the
//! data directory's `backups/`, which is a way back from that migration
//! and nothing more, and is never called a backup.
//!
//! ```text
//! <place>/ritornel-backup-2026-11-21-170102/
//!   server.db     this server: its identity, its queue, listens not yet delivered
//!   library.db    the library, with the listening history, on its hub
//!   config.toml   the settings, if there were any
//!   backup.json   what was copied, when and why
//!   README.txt    how to restore it
//! ```
//!
//! Each database is copied whole with `VACUUM INTO` on a connection of its
//! own, beside the writer, so the server goes on playing and recording
//! while it is taken. The two are copied one after the other, not at one
//! moment: a listen open in the copy of `server.db` may already be in the
//! copy of `library.db`, which keeps its own and records the other as a
//! conflict when the listen is delivered after a restore. The folder is
//! written under a name that begins with a dot and is renamed once
//! everything in it is whole, so a folder with the plain name is a backup
//! that finished.
//!
//! A place holds the backups of one data directory, which
//! `ritornel-data-dir.txt` in it names on its first line, written after the
//! first backup there. A copy of a data directory carries its settings, and
//! with them where its backups go, and its databases, and with them the
//! same server and library IDs: only the data directory's path tells the
//! copy from what it copied. So a server whose data directory is not the
//! one named writes no backup there. A place that older versions wrote to
//! without the file belongs to the data directory of the earliest backup in
//! it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ritornel_contract::local::Preparing;
use ritornel_contract::{BackupReason, Id};
use ritornel_engine::backup::{
    Database, Pending, copy_database, hold_sending, pending_migrations, recorded_id,
};
use serde_json::json;
use tracing::{info, warn};

use crate::data_dir::DataDir;
use crate::platform::{
    close_to_others, create_new_private_dir, create_private_dir, create_private_file, rename_dir,
    sync_dir,
};

/// The version of `backup.json`.
const MANIFEST_VERSION: u32 = 1;

/// The file in a place for backups that names, on its first line, the data
/// directory whose backups it holds.
pub(crate) const OWNER_FILE: &str = "ritornel-data-dir.txt";

/// What follows the first line of [`OWNER_FILE`].
const OWNER_NOTE: &str = "
This folder holds the backups of the Ritornel data directory named on the
first line. A server started on any other data directory, such as a copy of
this one, writes no backup here. If that data directory has moved, or a
backup of it was restored to a new place that replaces it, put the new path
on the first line.
";

/// What a backup's folder says about restoring it, taken from the data
/// directory `from`.
fn readme(from: &Path, about: About) -> String {
    let mut ids = String::new();
    if let Some(server) = about.server_id {
        ids.push_str(&format!("Server ID:  {server}\n"));
    }
    if let Some(library) = about.library_id {
        ids.push_str(&format!("Library ID: {library}\n"));
    }
    if !ids.is_empty() {
        ids.insert(0, '\n');
    }
    format!(
        "\
A backup of a Ritornel server, taken by the server itself, of the data
directory

  {from}
{ids}
server.db and library.db are whole SQLite databases, each a consistent copy
of that database as it was when its own copy began: server.db first, then
library.db. The two can differ by what happened in between: a listen still
being heard in server.db may have ended, and reached your history, by the
time library.db was copied. Restored, the history in library.db is kept as
it is, and the copy of that listen in server.db is set aside. backup.json
says when the backup was taken, why, and what each file is.

To restore it, stop the server, and put server.db, library.db and, if it is
here, config.toml into an empty data directory, such as the one above. Then
start the server on that directory. Keep server.db and library.db together:
the library names the server that may write it, and that server is the one
in server.db.

Restored to the data directory above, the server writes its backups where it
did before. Restored anywhere else, it writes none into a folder that holds
the backups of the data directory above, as the folder this backup is in
does when {OWNER_FILE} beside it names it. Give the new data directory a
backup folder of its own with --backup-dir or RITORNEL_BACKUP_DIR, or, if it
replaces the one above, put its path on the first line of that file.

To look inside without changing anything: sqlite3 -readonly library.db
",
        from = from.display(),
    )
}

/// What the README adds when the library copied has a Last.fm account.
const HELD: &str = "
This library has a Last.fm account. A server started on it scrobbles nothing
to Last.fm until that account has imported from there, which it does by
itself: Last.fm may already have taken plays after the backup was taken.
";

/// A backup that could not be taken.
#[derive(Debug)]
pub(crate) enum BackupError {
    /// No place is named for backups.
    NoPlace,
    /// The place named is not a folder that is there.
    Missing(PathBuf),
    /// The place named is, as it resolves now, in the data directory.
    Inside(PathBuf),
    /// The place named holds the backups of another data directory.
    Foreign {
        place: PathBuf,
        /// The data directory whose backups it holds.
        owner: PathBuf,
        /// The data directory of this server.
        ours: PathBuf,
    },
    /// Another backup is being taken.
    Busy,
    /// A file could not be written.
    Io {
        doing: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    /// A database could not be copied.
    Copy(&'static str, ritornel_engine::Error),
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlace => f.write_str(
                "no backup folder is named: give `ritornel-server serve --backup-dir`, set \
                 RITORNEL_BACKUP_DIR, or put backup_dir in config.toml",
            ),
            Self::Missing(path) => write!(
                f,
                "the backup folder {} is not there: is its disk connected?",
                path.display()
            ),
            Self::Inside(path) => write!(
                f,
                "the backup folder {} is inside the data directory, where a backup is lost with \
                 what it copies",
                path.display()
            ),
            Self::Foreign { place, owner, ours } => write!(
                f,
                "the backup folder {place} holds the backups of the data directory {owner}, not of \
                 this one, {ours}: give this data directory a backup folder of its own with \
                 `ritornel-server serve --backup-dir` or RITORNEL_BACKUP_DIR, or, if this is that \
                 data directory moved or restored to a new place, put {ours} on the first line \
                 of {marker}",
                place = place.display(),
                owner = owner.display(),
                ours = ours.display(),
                marker = place.join(OWNER_FILE).display(),
            ),
            Self::Busy => f.write_str("a backup is already being taken"),
            Self::Io {
                doing,
                path,
                source,
            } => write!(f, "cannot {doing} {}: {source}", path.display()),
            Self::Copy(file, error) => write!(f, "cannot copy {file}: {error}"),
        }
    }
}

fn io(doing: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> BackupError {
    let path = path.to_owned();
    move |source| BackupError::Io {
        doing,
        path,
        source,
    }
}

/// A backup that was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Taken {
    /// Its folder.
    pub(crate) path: PathBuf,
    /// When it began, as Unix milliseconds.
    pub(crate) at_ms: i64,
    pub(crate) reason: BackupReason,
}

/// What a backup records about the server, when it is known.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct About {
    pub(crate) server_id: Option<Id>,
    pub(crate) library_id: Option<Id>,
    pub(crate) server_schema: Option<i64>,
    pub(crate) library_schema: Option<i64>,
}

/// The backups a running server takes: where they go, and one at a time.
#[derive(Debug, Default)]
pub(crate) struct Backups {
    place: Option<PathBuf>,
    taking: Mutex<()>,
}

impl Backups {
    pub(crate) fn new(place: Option<PathBuf>) -> Self {
        Self {
            place,
            taking: Mutex::new(()),
        }
    }

    /// The folder backups go to, if one is named.
    pub(crate) fn place(&self) -> Option<&Path> {
        self.place.as_deref()
    }

    /// Takes a backup now, of the databases in `dir` as they are.
    pub(crate) fn take(
        &self,
        dir: &DataDir,
        at_ms: i64,
        reason: BackupReason,
        about: About,
    ) -> Result<Taken, BackupError> {
        let place = self.place.as_deref().ok_or(BackupError::NoPlace)?;
        let _taking = self.taking.try_lock().map_err(|_| BackupError::Busy)?;
        to_place(dir, place, at_ms, reason, about)
    }
}

/// Writes a backup into a place named for backups: one that is there,
/// outside the data directory, and that holds no other data directory's
/// backups. After it, the place names this data directory as the one whose
/// backups it holds, if it did not already.
fn to_place(
    dir: &DataDir,
    place: &Path,
    at_ms: i64,
    reason: BackupReason,
    about: About,
) -> Result<Taken, BackupError> {
    outside(dir, place)?;
    if !place.is_dir() {
        return Err(BackupError::Missing(place.to_owned()));
    }
    let ours = resolved(dir.root());
    let named = match owner(place)? {
        Owner::Named(owner) | Owner::Inferred(owner) if !same_dir(&owner, &ours) => {
            return Err(BackupError::Foreign {
                place: place.to_owned(),
                owner,
                ours,
            });
        }
        Owner::Named(_) => true,
        Owner::Inferred(_) | Owner::Unknown => false,
    };
    let taken = write(dir, place, at_ms, reason, about)?;
    if !named && let Err(error) = name_owner(place, &ours) {
        // The backup is whole. Without the file, the place is still known
        // as this data directory's by the backup just written.
        warn!(%error, "the backup folder could not be marked as this data directory's");
    }
    Ok(taken)
}

/// Whose backups a place holds.
#[derive(Debug)]
enum Owner {
    /// The data directory [`OWNER_FILE`] names.
    Named(PathBuf),
    /// With no [`OWNER_FILE`], as older versions left a place: the data
    /// directory of the earliest backup in it.
    Inferred(PathBuf),
    /// No one's yet.
    Unknown,
}

fn owner(place: &Path) -> Result<Owner, BackupError> {
    let file = place.join(OWNER_FILE);
    match std::fs::read(&file) {
        Ok(text) => {
            let text = String::from_utf8_lossy(&text);
            // As an editor may leave it: a byte-order mark, or `\r\n`.
            let first = text
                .trim_start_matches('\u{feff}')
                .lines()
                .next()
                .unwrap_or_default();
            if first.is_empty() {
                return Err(BackupError::Io {
                    doing: "read a data directory from",
                    path: file,
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "its first line is empty",
                    ),
                });
            }
            return Ok(Owner::Named(PathBuf::from(first)));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("read", &file)(error)),
    }
    // Each backup's name begins with when it was taken, so the first name
    // is the earliest.
    let mut names: Vec<String> = std::fs::read_dir(place)
        .map_err(io("read", place))?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("ritornel-backup-"))
        .collect();
    names.sort();
    for name in names {
        let Ok(text) = std::fs::read(place.join(&name).join("backup.json")) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(from) = manifest.get("from").and_then(serde_json::Value::as_str) {
            return Ok(Owner::Inferred(PathBuf::from(from)));
        }
    }
    Ok(Owner::Unknown)
}

/// Whether `path`, as the system resolves it now, is the data directory
/// `ours`, already resolved.
fn same_dir(path: &Path, ours: &Path) -> bool {
    resolved(path).to_string_lossy() == ours.to_string_lossy()
}

/// Writes [`OWNER_FILE`] in `place`, naming `ours`, whole or not at all.
fn name_owner(place: &Path, ours: &Path) -> Result<(), BackupError> {
    let file = place.join(OWNER_FILE);
    let text = format!("{}\n{OWNER_NOTE}", ours.display());
    let mut written = None;
    for n in 1..=SAME_SECOND {
        let partial = place.join(format!(".{OWNER_FILE}.{n}.partial"));
        match write_synced(&partial, text.as_bytes()) {
            Ok(()) => {
                written = Some(partial);
                break;
            }
            Err(BackupError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                return Err(error);
            }
        }
    }
    let partial = written.ok_or_else(|| BackupError::Io {
        doing: "name a new file in",
        path: place.to_owned(),
        source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
    })?;
    if let Err(error) = rename_dir(&partial, &file) {
        let _ = std::fs::remove_file(&partial);
        return Err(io("write", &file)(error));
    }
    sync_dir(place).map_err(io("sync", place))
}

/// Whether opening a database would migrate it: [`pending_migrations`],
/// or what a test says.
type IsPending = dyn Fn(&Path, Database) -> Result<Option<Pending>, ritornel_engine::Error>;

/// Before either database is migrated: whether one would be, and if so a
/// copy of both, in the place named or, with none or when it cannot be
/// written, in the data directory's `backups/`. It answers what was
/// written and what failed, for `server.db` to record once it is open.
///
/// Nothing is migrated without a copy: if neither place can be written,
/// the server does not start. `say` is told when the copy begins, and when
/// it is done and the migrations are next.
pub(crate) fn before_migrations(
    dir: &DataDir,
    place: Option<&Path>,
    at_ms: i64,
    pending: &IsPending,
    say: &dyn Fn(Preparing),
) -> Result<BeforeMigrations, crate::Error> {
    let server = pending(&dir.server_db(), Database::Server).map_err(crate::Error::Store)?;
    let library = pending(&dir.library_db(), Database::Library).map_err(crate::Error::Library)?;
    if server.is_none() && library.is_none() {
        return Ok(BeforeMigrations::default());
    }
    let about = About {
        server_id: recorded(&dir.server_db(), Database::Server),
        library_id: recorded(&dir.library_db(), Database::Library),
        server_schema: server.map(|pending| pending.from),
        library_schema: library.map(|pending| pending.from),
    };
    info!(
        ?server,
        ?library,
        "a database will be migrated, so it is copied first"
    );
    say(Preparing::Backup);
    let reason = BackupReason::BeforeMigration;
    let mut failed = None;
    if let Some(place) = place {
        match to_place(dir, place, at_ms, reason, about) {
            Ok(taken) => {
                info!(path = %taken.path.display(), "backed up before migrating");
                say(Preparing::Migration);
                return Ok(BeforeMigrations {
                    taken: Some(taken),
                    failed: None,
                });
            }
            Err(error) => {
                warn!(
                    %error,
                    kept_in = %dir.backups().display(),
                    "the backup before migrating failed; a copy is kept in the data directory instead"
                );
                failed = Some(error.to_string());
            }
        }
    }
    let rollback = dir.backups();
    create_private_dir(&rollback)?;
    let kept = write(dir, &rollback, at_ms, reason, about).map_err(|error| crate::Error::Io {
        doing: "copy the databases before migrating them into",
        path: rollback.clone(),
        source: std::io::Error::other(error.to_string()),
    })?;
    info!(path = %kept.path.display(), "kept a copy in the data directory before migrating");
    say(Preparing::Migration);
    Ok(BeforeMigrations {
        taken: None,
        failed,
    })
}

/// The ID a database records for itself, as a backup taken before it is
/// migrated says it: one that cannot be read is left out of the backup and
/// does not stop the migration.
fn recorded(path: &Path, database: Database) -> Option<Id> {
    recorded_id(path, database).unwrap_or_else(|error| {
        warn!(%error, path = %path.display(), "the ID this database records could not be read for its backup");
        None
    })
}

/// Refuses a place that resolves, now, into the data directory.
fn outside(dir: &DataDir, place: &Path) -> Result<(), BackupError> {
    if is_inside(place, dir.root()) {
        Err(BackupError::Inside(place.to_owned()))
    } else {
        Ok(())
    }
}

/// [`before_migrations`] as the server runs it: each database is asked
/// whether opening it would migrate it.
pub(crate) fn before_migrating(
    dir: &DataDir,
    place: Option<&Path>,
    at_ms: i64,
    say: &dyn Fn(Preparing),
) -> Result<BeforeMigrations, crate::Error> {
    before_migrations(dir, place, at_ms, &pending_migrations, say)
}

/// What [`before_migrations`] did, for `server.db` to record.
#[derive(Debug, Default)]
pub(crate) struct BeforeMigrations {
    /// The backup written to the place named.
    pub(crate) taken: Option<Taken>,
    /// Why the place named could not be written to.
    pub(crate) failed: Option<String>,
}

/// Whether `path` is `root` or inside it, as the system resolves each: a
/// link is followed as far as the path exists, and `.` and `..` are taken
/// out of the rest. A folder named for backups is asked this when it is
/// named and again before each backup, when what it resolves to may have
/// changed.
pub(crate) fn is_inside(path: &Path, root: &Path) -> bool {
    resolved(path).starts_with(resolved(root))
}

fn resolved(path: &Path) -> PathBuf {
    use std::path::Component;
    // The longest part of the path that is there, as the system resolves
    // it, and then the rest.
    let mut existing = path;
    let mut rest = Vec::new();
    let base = loop {
        if let Ok(real) = existing.canonicalize() {
            break real;
        }
        match (existing.parent(), existing.components().next_back()) {
            (Some(parent), Some(last)) => {
                rest.push(last);
                existing = parent;
            }
            _ => break PathBuf::new(),
        }
    };
    let mut resolved = base;
    for part in rest.into_iter().rev() {
        match part {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other),
        }
    }
    resolved
}

/// Writes a backup of the databases in `dir` into a new folder in `place`.
fn write(
    dir: &DataDir,
    place: &Path,
    at_ms: i64,
    reason: BackupReason,
    about: About,
) -> Result<Taken, BackupError> {
    // The place is never made: a folder on a disk that is not connected
    // would be made on the disk that holds the data.
    if !place.is_dir() {
        return Err(BackupError::Missing(place.to_owned()));
    }
    let (name, partial) = reserve(place, at_ms, reason)?;
    if let Err(error) = fill(dir, &partial, at_ms, reason, about) {
        // What was written of it is no backup.
        let _ = std::fs::remove_dir_all(&partial);
        return Err(error);
    }
    let finished = publish(&partial, place, &name)?;
    Ok(Taken {
        path: finished,
        at_ms,
        reason,
    })
}

/// A new backup's folder name, and the folder it is written in first.
fn reserve(
    place: &Path,
    at_ms: i64,
    reason: BackupReason,
) -> Result<(String, PathBuf), BackupError> {
    let suffix = match reason {
        BackupReason::OnDemand => "",
        BackupReason::BeforeMigration => "-before-migration",
    };
    reserve_named(place, &format!("ritornel-backup-{}{suffix}", stamp(at_ms)))
}

/// A time as it is put in a folder's name: `2026-11-21-170102`, in UTC,
/// with no colon, which Windows does not allow in a name.
pub(crate) fn stamp(at_ms: i64) -> String {
    let utc = crate::logs::utc(at_ms);
    let (date, time) = utc.split_once('T').unwrap_or((&utc, ""));
    let time: String = time.chars().filter(char::is_ascii_digit).take(6).collect();
    format!("{date}-{time}")
}

/// How many folders of one name and second a place may already hold.
const SAME_SECOND: u32 = 1_000;

/// A new folder's name in `place`, `base` or `base-2` and on, and the
/// folder it is written in first, under a dotted name, which this call
/// makes and no other can: two writers in the same second, in this server
/// or another writing to the same place, take two names.
pub(crate) fn reserve_named(place: &Path, base: &str) -> Result<(String, PathBuf), BackupError> {
    for n in 1..=SAME_SECOND {
        let name = if n == 1 {
            base.to_owned()
        } else {
            format!("{base}-{n}")
        };
        if place.join(&name).exists() {
            continue;
        }
        let partial = place.join(format!(".{name}.partial"));
        match create_new_private_dir(&partial) {
            // Another writer may have finished a folder of this name since
            // it was looked for; then the next name is taken.
            Ok(()) if place.join(&name).exists() => {
                let _ = std::fs::remove_dir(&partial);
            }
            Ok(()) => return Ok((name, partial)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io("create", &partial)(error)),
        }
    }
    Err(BackupError::Io {
        doing: "name a new folder in",
        path: place.to_owned(),
        source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
    })
}

/// Makes the folder written under `partial` whole, as `name` in `place`:
/// it is synced, so that its names are kept, renamed, and the place synced,
/// so that the new name is. On Windows, where a folder is not synced, the
/// rename asks to be written through to the disk instead. Each file in it
/// was synced as it was written. On an error nothing of it is left, and it
/// is not to be called whole.
pub(crate) fn publish(partial: &Path, place: &Path, name: &str) -> Result<PathBuf, BackupError> {
    let finished = place.join(name);
    let renamed = sync_dir(partial)
        .map_err(io("sync", partial))
        .and_then(|()| rename_dir(partial, &finished).map_err(io("finish", &finished)));
    if let Err(error) = renamed {
        let _ = std::fs::remove_dir_all(partial);
        return Err(error);
    }
    if let Err(error) = sync_dir(place) {
        // Renamed, it is this writer's alone; unsynced, it is not whole.
        let _ = std::fs::remove_dir_all(&finished);
        return Err(io("sync", place)(error));
    }
    Ok(finished)
}

fn fill(
    dir: &DataDir,
    into: &Path,
    at_ms: i64,
    reason: BackupReason,
    about: About,
) -> Result<(), BackupError> {
    let mut files = Vec::new();
    let mut held = false;
    for (name, source) in [
        ("server.db", dir.server_db()),
        ("library.db", dir.library_db()),
    ] {
        // A server that holds no library has an empty file, or none. A
        // backup of a server that knows itself is none without server.db,
        // which holds who it is, and of one that holds a library none
        // without library.db.
        if !present(&source)? {
            let required = match name {
                "server.db" => about.server_id.is_some(),
                _ => about.library_id.is_some(),
            };
            if required {
                return Err(BackupError::Io {
                    doing: "copy",
                    path: source.clone(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                });
            }
            continue;
        }
        let copy = into.join(name);
        copy_database(&source, &copy).map_err(|error| BackupError::Copy(name, error))?;
        if name == "library.db" {
            // Put back, it holds plays Last.fm may have taken since.
            held = hold_sending(&copy, at_ms).map_err(|error| BackupError::Copy(name, error))?;
        }
        // SQLite makes a file anyone may read, and server.db holds this
        // server's signing key.
        close_to_others(&copy).map_err(io("close", &copy))?;
        files.push(name);
    }
    let config = dir.config_toml();
    if present(&config)? {
        // Read and written again, so that the copy is the owner's alone and
        // can be synced whatever the file's own mode.
        let text = std::fs::read(&config).map_err(io("read", &config))?;
        write_synced(&into.join("config.toml"), &text)?;
        files.push("config.toml");
    }
    let manifest = json!({
        "format": "ritornel-backup",
        "version": MANIFEST_VERSION,
        "takenAt": crate::logs::utc(at_ms),
        "takenAtMs": at_ms,
        "reason": reason.as_str(),
        "serverVersion": env!("CARGO_PKG_VERSION"),
        "serverId": about.server_id,
        "libraryId": about.library_id,
        "schemas": { "server": about.server_schema, "library": about.library_schema },
        "files": files,
        "from": dir.root().to_string_lossy(),
    });
    let text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    write_synced(&into.join("backup.json"), text.as_bytes())?;
    let mut readme = readme(dir.root(), about);
    if held {
        readme.push_str(HELD);
    }
    write_synced(&into.join("README.txt"), readme.as_bytes())
}

/// Whether there is a file with something in it at `path`. One whose
/// details cannot be read is an error, never taken for one that is not
/// there.
fn present(path: &Path) -> Result<bool, BackupError> {
    match std::fs::metadata(path) {
        Ok(file) => Ok(file.len() > 0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io("read", path)(error)),
    }
}

/// Writes a new file that only its owner may open, and syncs it.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    use std::io::Write;
    create_private_file(path)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .map_err(io("write", path))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ritornel_engine::{OsEntropy, ServerStore, SystemClock};

    use super::*;
    use crate::testing::Scratch;
    use ritornel_contract::Id;

    const AT: i64 = 1_763_744_462_123;

    fn data(scratch: &Scratch) -> DataDir {
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let store = ServerStore::open(&dir.server_db(), Arc::new(SystemClock), Arc::new(OsEntropy))
            .unwrap();
        store.ensure_identity("desk").unwrap();
        dir
    }

    #[test]
    fn with_no_place_named_nothing_is_written_anywhere() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let backups = Backups::new(None);
        assert!(matches!(
            backups.take(&dir, AT, BackupReason::OnDemand, About::default()),
            Err(BackupError::NoPlace)
        ));
        assert!(!dir.backups().exists());
    }

    #[test]
    fn a_backup_is_a_whole_folder_of_its_own_in_the_place_named() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("elsewhere");
        std::fs::create_dir(&place).unwrap();
        std::fs::write(dir.config_toml(), "log_level = \"warn\"\n").unwrap();
        let backups = Backups::new(Some(place.clone()));

        let first = backups
            .take(&dir, AT, BackupReason::OnDemand, About::default())
            .unwrap();
        assert_eq!(first.path, place.join("ritornel-backup-2025-11-21-170102"));
        let mut names: Vec<_> = std::fs::read_dir(&first.path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        // No library was made, so there is none to copy.
        assert_eq!(
            names,
            ["README.txt", "backup.json", "config.toml", "server.db"]
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(first.path.join("backup.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["reason"], "on_demand");
        // The two databases are copied one after the other, and the README
        // says what that means for a listen heard meanwhile.
        let readme = std::fs::read_to_string(first.path.join("README.txt")).unwrap();
        assert!(readme.contains("server.db first, then"), "{readme}");
        // It names the data directory it was taken from, whatever the
        // system.
        assert!(
            readme.contains(&format!("\n  {}\n", dir.root().display())),
            "{readme}"
        );
        assert!(!readme.contains("Application Support"), "{readme}");
        assert_eq!(manifest["takenAt"], "2025-11-21T17:01:02.123Z");

        // A second in the same second is told apart, and nothing is left
        // half written.
        let second = backups
            .take(&dir, AT, BackupReason::OnDemand, About::default())
            .unwrap();
        assert_eq!(
            second.path,
            place.join("ritornel-backup-2025-11-21-170102-2")
        );
        assert!(std::fs::read_dir(&place).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        }));
        // And nothing was written in the data directory.
        assert!(!dir.backups().exists());
    }

    #[test]
    fn a_backup_of_a_library_with_nothing_waiting_to_be_sent_holds_nothing_back() {
        use ritornel_engine::{LibraryStore, WriteRequest};
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let store = ServerStore::open(&dir.server_db(), Arc::new(SystemClock), Arc::new(OsEntropy))
            .unwrap();
        let (identity, _) = store.ensure_identity("desk").unwrap();
        let open = |path: &Path| {
            LibraryStore::open(
                path,
                identity.server_id,
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
            )
            .unwrap()
        };
        drop(
            LibraryStore::create(
                &dir.library_db(),
                &identity,
                WriteRequest {
                    method: "library.create",
                    command: None,
                    client: None,
                },
                |_| String::new(),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
            )
            .unwrap(),
        );
        let place = elsewhere(&scratch);
        let taken = backup(&dir, &place, About::default()).unwrap();
        // With no service accounts there is nothing to hold back.
        assert_eq!(open(&dir.library_db()).any_held_for_import().unwrap(), None);
        assert_eq!(
            open(&taken.path.join("library.db"))
                .any_held_for_import()
                .unwrap(),
            None
        );
        let readme = std::fs::read_to_string(taken.path.join("README.txt")).unwrap();
        // Nor does its README say scrobbling waits.
        assert!(!readme.contains("scrobbles nothing"), "{readme}");
    }

    #[test]
    fn a_backup_of_a_library_with_accounts_holds_each_even_without_pending_deliveries() {
        use ritornel_engine::{LibraryStore, WriteRequest};
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let store = ServerStore::open(&dir.server_db(), Arc::new(SystemClock), Arc::new(OsEntropy))
            .unwrap();
        let (identity, _) = store.ensure_identity("desk").unwrap();
        let open = |path: &Path| {
            LibraryStore::open(
                path,
                identity.server_id,
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
            )
            .unwrap()
        };
        let (library, _) = LibraryStore::create(
            &dir.library_db(),
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        let first = library.ensure_service_account("lastfm", "first").unwrap();
        let second = library.ensure_service_account("lastfm", "second").unwrap();
        drop(library);
        let place = elsewhere(&scratch);
        let taken = backup(&dir, &place, About::default()).unwrap();
        // The live library is untouched; the backup holds every account.
        assert_eq!(open(&dir.library_db()).any_held_for_import().unwrap(), None);
        let copy = open(&taken.path.join("library.db"));
        assert!(copy.held_for_import(first).unwrap().is_some());
        assert!(copy.held_for_import(second).unwrap().is_some());
        let readme = std::fs::read_to_string(taken.path.join("README.txt")).unwrap();
        assert!(readme.contains("scrobbles nothing"), "{readme}");
    }

    #[test]
    fn two_writers_backing_up_to_one_place_in_the_same_second_write_two_backups() {
        let scratch = Scratch::new();
        let place = scratch.path().join("shared");
        std::fs::create_dir(&place).unwrap();
        // Two servers on one data directory, one after the other, write
        // as two writers in one server do.
        let dir = data(&scratch);
        let taken: Vec<_> = (0..2)
            .map(|n| {
                let dir = dir.clone();
                let place = place.clone();
                std::thread::spawn(move || {
                    let backups = Backups::new(Some(place));
                    let taken: Vec<_> = (0..10)
                        .map(|_| {
                            backups
                                .take(&dir, AT, BackupReason::OnDemand, About::default())
                                .unwrap()
                                .path
                        })
                        .collect();
                    (n, taken)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|thread| thread.join().unwrap().1)
            .collect();
        let mut names: Vec<_> = taken.iter().collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 20);
        for backup in &taken {
            assert!(backup.join("server.db").is_file(), "{backup:?}");
        }
    }

    // A link is made differently on each system, and on Windows only with
    // a privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_place_that_has_come_to_lead_into_the_data_directory_is_refused() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("alias");
        std::os::unix::fs::symlink(dir.root(), &place).unwrap();
        let backups = Backups::new(Some(place.join("later")));
        std::fs::create_dir(dir.root().join("later")).unwrap();
        let refused = backups
            .take(&dir, AT, BackupReason::OnDemand, About::default())
            .unwrap_err();
        assert!(matches!(refused, BackupError::Inside(_)), "{refused}");
        assert_eq!(
            std::fs::read_dir(dir.root().join("later")).unwrap().count(),
            0
        );
    }

    fn backup(dir: &DataDir, place: &Path, about: About) -> Result<Taken, BackupError> {
        Backups::new(Some(place.to_owned())).take(dir, AT, BackupReason::OnDemand, about)
    }

    fn elsewhere(scratch: &Scratch) -> PathBuf {
        let place = scratch.path().join("elsewhere");
        std::fs::create_dir_all(&place).unwrap();
        place
    }

    #[test]
    fn a_server_has_no_backup_without_its_own_database() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        std::fs::remove_file(dir.server_db()).unwrap();
        let about = About {
            server_id: Some(Id::from_bytes([0x11; 16])),
            ..About::default()
        };
        assert!(backup(&dir, &place, about).is_err());
        assert_eq!(std::fs::read_dir(&place).unwrap().count(), 0);
    }

    #[test]
    fn a_server_that_holds_a_library_has_no_backup_without_it() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        let about = About {
            library_id: Some(Id::from_bytes([0x1b; 16])),
            ..About::default()
        };
        assert!(backup(&dir, &place, about).is_err());
        assert_eq!(std::fs::read_dir(&place).unwrap().count(), 0);
    }

    // A link to itself is a file whose details cannot be read, as one on a
    // failing disk or in a folder closed to the server is; the link is
    // made differently on each system, and on Windows only with a
    // privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_database_whose_details_cannot_be_read_is_no_database_that_is_not_there() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        std::os::unix::fs::symlink(dir.library_db(), dir.library_db()).unwrap();
        assert!(backup(&dir, &place, About::default()).is_err());
        assert_eq!(std::fs::read_dir(&place).unwrap().count(), 0);
    }

    /// Gives the server in `dir` a library, and answers the server's ID and
    /// the library's.
    fn with_library(dir: &DataDir) -> (Id, Id) {
        use ritornel_engine::{LibraryStore, WriteRequest};
        let store = ServerStore::open(&dir.server_db(), Arc::new(SystemClock), Arc::new(OsEntropy))
            .unwrap();
        let (identity, _) = store.ensure_identity("desk").unwrap();
        let (library, _) = LibraryStore::create(
            &dir.library_db(),
            &identity,
            WriteRequest {
                method: "library.create",
                command: None,
                client: None,
            },
            |_| String::new(),
            Arc::new(SystemClock),
            Arc::new(OsEntropy),
        )
        .unwrap();
        (identity.server_id, library.library_id())
    }

    fn manifest_of(backup: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(backup.join("backup.json")).unwrap()).unwrap()
    }

    fn readme_of(backup: &Path) -> String {
        std::fs::read_to_string(backup.join("README.txt")).unwrap()
    }

    /// Every finished backup in `place`.
    fn finished(place: &Path) -> Vec<PathBuf> {
        let mut found: Vec<_> = std::fs::read_dir(place)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("ritornel-backup-")
            })
            .collect();
        found.sort();
        found
    }

    /// Copies the folder `from` to `to`, everything in it, as `cp -R` does.
    fn copy_all(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_all(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn server_pending() -> &'static IsPending {
        &|_, database| Ok((database == Database::Server).then_some(Pending { from: 1, to: 2 }))
    }

    #[test]
    fn a_backup_before_a_migration_names_the_server_and_library_it_copied() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let (server, library) = with_library(&dir);
        let place = elsewhere(&scratch);
        let done = before_migrations(&dir, Some(&place), AT, server_pending(), &|_| {}).unwrap();
        let taken = done.taken.unwrap();
        let manifest = manifest_of(&taken.path);
        assert_eq!(manifest["serverId"], serde_json::to_value(server).unwrap());
        assert_eq!(
            manifest["libraryId"],
            serde_json::to_value(library).unwrap()
        );
        assert_eq!(manifest["schemas"]["server"], 1);
        let readme = readme_of(&taken.path);
        assert!(
            readme.contains(&format!("Server ID:  {server}\n")),
            "{readme}"
        );
        assert!(
            readme.contains(&format!("Library ID: {library}\n")),
            "{readme}"
        );
        assert!(
            readme.contains(&format!("\n  {}\n", dir.root().display())),
            "{readme}"
        );
    }

    #[test]
    fn a_backup_before_a_migration_of_a_server_without_a_library_names_only_the_server() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        let done = before_migrations(&dir, Some(&place), AT, server_pending(), &|_| {}).unwrap();
        let taken = done.taken.unwrap();
        let manifest = manifest_of(&taken.path);
        assert!(manifest["serverId"].is_string(), "{manifest}");
        assert!(manifest["libraryId"].is_null(), "{manifest}");
        let readme = readme_of(&taken.path);
        assert!(readme.contains("Server ID:  "), "{readme}");
        assert!(!readme.contains("Library ID"), "{readme}");

        // With neither database there to read, it names neither, and the
        // migration is not stopped.
        let bare = Scratch::new();
        let dir = DataDir::prepare(bare.path().join("data")).unwrap();
        std::fs::remove_file(dir.server_db()).unwrap();
        std::fs::write(dir.library_db(), b"").unwrap();
        let done = before_migrations(&dir, Some(&place), AT, server_pending(), &|_| {}).unwrap();
        // That one is another data directory's, so it is kept beside the
        // data.
        assert!(done.taken.is_none());
        let kept = finished(&dir.backups());
        assert_eq!(kept.len(), 1);
        let manifest = manifest_of(&kept[0]);
        assert!(manifest["serverId"].is_null(), "{manifest}");
        assert!(manifest["libraryId"].is_null(), "{manifest}");
        assert!(!readme_of(&kept[0]).contains(" ID: "));
    }

    #[test]
    fn a_copy_of_a_data_directory_writes_no_backup_into_the_backup_folder_of_what_it_copied() {
        let scratch = Scratch::new();
        let original = data(&scratch);
        let (server, library) = with_library(&original);
        let place = elsewhere(&scratch);
        std::fs::write(
            original.config_toml(),
            format!("backup_dir = {:?}\n", place.to_str().unwrap()),
        )
        .unwrap();
        let first = backup(&original, &place, About::default()).unwrap();
        let marker = std::fs::read_to_string(place.join(OWNER_FILE)).unwrap();
        assert_eq!(
            marker.lines().next().unwrap(),
            resolved(original.root()).to_string_lossy()
        );
        assert!(crate::platform::path_is_private(&place.join(OWNER_FILE)));

        // `cp -R`: the same settings, and so the same backup folder, and
        // the same server and library.
        let trial = scratch.path().join("trial");
        copy_all(original.root(), &trial);
        let copy = DataDir::prepare(trial).unwrap();
        assert_eq!(recorded(&copy.server_db(), Database::Server), Some(server));
        assert_eq!(
            recorded(&copy.library_db(), Database::Library),
            Some(library)
        );

        // Before a migration, its copy is kept in its own data directory,
        // and the backup folder is not written.
        let done = before_migrations(&copy, Some(&place), AT, server_pending(), &|_| {}).unwrap();
        assert!(done.taken.is_none());
        let failed = done.failed.unwrap();
        assert!(
            failed.contains("holds the backups of the data directory"),
            "{failed}"
        );
        assert!(failed.contains("--backup-dir"), "{failed}");
        assert!(failed.contains(OWNER_FILE), "{failed}");
        assert_eq!(finished(&place), std::slice::from_ref(&first.path));
        let kept = finished(&copy.backups());
        assert_eq!(kept.len(), 1);
        assert_eq!(
            manifest_of(&kept[0])["libraryId"],
            serde_json::to_value(library).unwrap()
        );

        // Asked for, a backup is refused, and it says what to do.
        let refused = backup(&copy, &place, About::default()).unwrap_err();
        let BackupError::Foreign { owner, ours, .. } = &refused else {
            panic!("{refused}");
        };
        assert!(same_dir(owner, &resolved(original.root())), "{refused}");
        assert_eq!(ours, &resolved(copy.root()));
        assert_eq!(finished(&place), std::slice::from_ref(&first.path));

        // The original goes on backing up there.
        let second = backup(&original, &place, About::default()).unwrap();
        assert_eq!(finished(&place), [first.path, second.path]);
    }

    #[test]
    fn a_backup_folder_older_versions_wrote_to_belongs_to_its_earliest_backup() {
        let scratch = Scratch::new();
        let original = data(&scratch);
        let place = elsewhere(&scratch);
        let other = Scratch::new();
        let copy = data(&other);
        // As older versions left it: no file naming its data directory, a
        // backup of the original, and a later one a copy wrote there.
        for (name, from) in [
            ("ritornel-backup-2025-01-01-000000", original.root()),
            (
                "ritornel-backup-2025-06-01-000000-before-migration",
                copy.root(),
            ),
        ] {
            std::fs::create_dir(place.join(name)).unwrap();
            std::fs::write(
                place.join(name).join("backup.json"),
                serde_json::to_vec(&json!({ "from": from.to_string_lossy() })).unwrap(),
            )
            .unwrap();
        }
        let refused = backup(&copy, &place, About::default()).unwrap_err();
        assert!(matches!(refused, BackupError::Foreign { .. }), "{refused}");
        assert!(!place.join(OWNER_FILE).exists());

        let taken = backup(&original, &place, About::default()).unwrap();
        assert!(taken.path.join("server.db").is_file());
        // From then on the file names it.
        let marker = std::fs::read_to_string(place.join(OWNER_FILE)).unwrap();
        assert!(same_dir(
            Path::new(marker.lines().next().unwrap()),
            &resolved(original.root())
        ));
        assert!(matches!(
            backup(&copy, &place, About::default()),
            Err(BackupError::Foreign { .. })
        ));
    }

    #[test]
    fn a_data_directory_restored_where_it_was_backs_up_where_it_did() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        with_library(&dir);
        let place = elsewhere(&scratch);
        let taken = backup(&dir, &place, About::default()).unwrap();

        // Lost, and put back from the backup into an empty data directory
        // at the same path.
        std::fs::remove_dir_all(dir.root()).unwrap();
        std::fs::create_dir(dir.root()).unwrap();
        for name in ["server.db", "library.db"] {
            std::fs::copy(taken.path.join(name), dir.root().join(name)).unwrap();
        }
        let restored = DataDir::prepare(dir.root().to_owned()).unwrap();
        let again = backup(&restored, &place, About::default()).unwrap();
        assert_eq!(finished(&place), [taken.path.clone(), again.path]);

        // Put back somewhere else, it is a data directory of its own
        // until the file says it replaces the one before.
        let moved = scratch.path().join("moved");
        std::fs::create_dir(&moved).unwrap();
        for name in ["server.db", "library.db"] {
            std::fs::copy(taken.path.join(name), moved.join(name)).unwrap();
        }
        let moved = DataDir::prepare(moved).unwrap();
        let refused = backup(&moved, &place, About::default()).unwrap_err();
        assert!(matches!(refused, BackupError::Foreign { .. }), "{refused}");
        let readme = readme_of(&taken.path);
        assert!(readme.contains(OWNER_FILE), "{readme}");
        // As an editor may save it.
        std::fs::write(
            place.join(OWNER_FILE),
            format!("\u{feff}{}\r\n", moved.root().display()),
        )
        .unwrap();
        backup(&moved, &place, About::default()).unwrap();
        assert!(matches!(
            backup(&restored, &place, About::default()),
            Err(BackupError::Foreign { .. })
        ));
    }

    #[test]
    fn a_backup_folder_whose_file_names_no_data_directory_is_written_to_by_none() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        std::fs::write(place.join(OWNER_FILE), b"\nsomething\n").unwrap();
        let refused = backup(&dir, &place, About::default()).unwrap_err();
        assert!(refused.to_string().contains(OWNER_FILE), "{refused}");
        assert!(finished(&place).is_empty());
    }

    // A link is made differently on each system, and on Windows only with
    // a privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_data_directory_and_a_backup_folder_are_the_same_by_any_link_to_them() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        let by_link = scratch.path().join("data-link");
        std::os::unix::fs::symlink(dir.root(), &by_link).unwrap();
        let place_by_link = scratch.path().join("elsewhere-link");
        std::os::unix::fs::symlink(&place, &place_by_link).unwrap();

        // Marked by a server on the link.
        let linked = DataDir::prepare(by_link.clone()).unwrap();
        backup(&linked, &place_by_link, About::default()).unwrap();
        // Known by the server on the folder itself, by either path.
        backup(&dir, &place, About::default()).unwrap();
        backup(&dir, &place_by_link, About::default()).unwrap();
        // And with a trailing separator.
        let trailing = PathBuf::from(format!("{}/", dir.root().display()));
        backup(
            &DataDir::prepare(trailing).unwrap(),
            &place,
            About::default(),
        )
        .unwrap();
        assert_eq!(finished(&place).len(), 4);

        // A backup older versions wrote from the link is the folder's too.
        let legacy = Scratch::new();
        let place = elsewhere(&legacy);
        std::fs::create_dir(place.join("ritornel-backup-2025-01-01-000000")).unwrap();
        std::fs::write(
            place.join("ritornel-backup-2025-01-01-000000/backup.json"),
            serde_json::to_vec(&json!({ "from": by_link.to_string_lossy() })).unwrap(),
        )
        .unwrap();
        backup(&dir, &place, About::default()).unwrap();
    }

    #[test]
    fn a_settings_file_only_its_owner_may_change_is_copied_all_the_same() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        std::fs::write(dir.config_toml(), "log_level = \"warn\"\n").unwrap();
        let mut read_only = std::fs::metadata(dir.config_toml()).unwrap().permissions();
        read_only.set_readonly(true);
        std::fs::set_permissions(dir.config_toml(), read_only).unwrap();
        let taken = backup(&dir, &place, About::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(taken.path.join("config.toml")).unwrap(),
            "log_level = \"warn\"\n"
        );
    }

    #[test]
    fn every_file_of_a_backup_is_its_owners_alone() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        std::fs::write(dir.config_toml(), "log_level = \"warn\"\n").unwrap();
        let taken = backup(&dir, &place, About::default()).unwrap();
        assert!(crate::platform::path_is_private(&taken.path));
        for name in ["server.db", "config.toml", "backup.json", "README.txt"] {
            assert!(
                crate::platform::path_is_private(&taken.path.join(name)),
                "{name}"
            );
        }
    }

    #[test]
    fn with_neither_place_to_copy_to_no_migration_is_run() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        // The named folder is not there, and where the way back would go
        // is a file.
        std::fs::write(dir.backups(), b"").unwrap();
        let pending: &IsPending =
            &|_, database| Ok((database == Database::Server).then_some(Pending { from: 1, to: 2 }));
        assert!(
            before_migrations(
                &dir,
                Some(&scratch.path().join("unplugged")),
                AT,
                pending,
                &|_| {}
            )
            .is_err()
        );
    }

    #[test]
    fn a_place_that_is_not_there_is_never_made() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("Volumes/Unplugged/Backups");
        let backups = Backups::new(Some(place.clone()));
        let refused = backups
            .take(&dir, AT, BackupReason::OnDemand, About::default())
            .unwrap_err();
        assert!(matches!(&refused, BackupError::Missing(path) if *path == place));
        assert!(!scratch.path().join("Volumes").exists());
    }

    #[test]
    fn the_copy_and_then_the_migration_are_said_as_they_come() {
        let pending: &IsPending =
            &|_, database| Ok((database == Database::Server).then_some(Pending { from: 1, to: 2 }));
        let none: &IsPending = &|_, _| Ok(None);
        let said = |place: Option<&Path>, pending: &IsPending| {
            let scratch = Scratch::new();
            let dir = data(&scratch);
            let steps = std::cell::RefCell::new(Vec::new());
            let done = before_migrations(&dir, place, AT, pending, &|step| {
                steps.borrow_mut().push(step);
            });
            (done.is_ok(), steps.into_inner())
        };
        let scratch = Scratch::new();
        let place = scratch.path().join("elsewhere");
        std::fs::create_dir(&place).unwrap();
        assert_eq!(said(Some(&place), none), (true, vec![]));
        let both = vec![Preparing::Backup, Preparing::Migration];
        assert_eq!(said(Some(&place), pending), (true, both.clone()));
        assert_eq!(said(None, pending), (true, both));
    }

    #[test]
    fn a_migration_is_copied_for_first_to_the_place_or_else_into_the_data_directory() {
        let pending: &dyn Fn(&Path, Database) -> _ =
            &|_, database| Ok((database == Database::Server).then_some(Pending { from: 1, to: 2 }));
        let none: &IsPending = &|_, _| Ok(None);

        // Nothing to migrate: nothing is copied.
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("elsewhere");
        std::fs::create_dir(&place).unwrap();
        let done = before_migrations(&dir, Some(&place), AT, none, &|_| {}).unwrap();
        assert!(done.taken.is_none() && done.failed.is_none());
        assert_eq!(std::fs::read_dir(&place).unwrap().count(), 0);

        // A place named: the backup goes there, and nothing beside the data.
        let done = before_migrations(&dir, Some(&place), AT, pending, &|_| {}).unwrap();
        assert_eq!(
            done.taken.unwrap().path,
            place.join("ritornel-backup-2025-11-21-170102-before-migration")
        );
        assert!(!dir.backups().exists());

        // None named: the way back is kept in the data directory, and it
        // is not a backup.
        let done = before_migrations(&dir, None, AT, pending, &|_| {}).unwrap();
        assert!(done.taken.is_none() && done.failed.is_none());
        assert!(
            dir.backups()
                .join("ritornel-backup-2025-11-21-170102-before-migration/server.db")
                .is_file()
        );

        // A place that cannot be written: the way back is kept all the
        // same, and the failure is said.
        let unplugged = scratch.path().join("unplugged");
        let done = before_migrations(&dir, Some(&unplugged), AT, pending, &|_| {}).unwrap();
        assert!(done.taken.is_none());
        assert!(done.failed.unwrap().contains("is not there"));
        assert!(
            dir.backups()
                .join("ritornel-backup-2025-11-21-170102-before-migration-2/server.db")
                .is_file()
        );
    }
}
