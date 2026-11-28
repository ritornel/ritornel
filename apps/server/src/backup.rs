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
//! `ritornel-data-dir.txt` in it names on its first line. The first backup
//! there makes the file, only if no other writer has, and removes it again
//! if the backup fails. A copy of a data directory carries its settings,
//! and with them where its backups go, and its databases, and with them the
//! same server and library IDs: only the data directory's path tells the
//! copy from what it copied. So a server whose data directory is not the
//! one named writes no backup there. A place that older versions wrote to
//! without the file belongs to the data directory of the earliest backup in
//! it, and a place whose owner is in doubt is written to by no one.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ritornel_contract::local::Preparing;
use ritornel_contract::{BackupReason, Id};
use ritornel_engine::backup::{
    Database, MigrationState, Pending, copy_database, hold_sending, pending_migrations,
    recorded_id, unbind_scrobbling,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::data_dir::DataDir;
use crate::error::io_error;
use crate::platform::{
    close_to_others, create_new_private_dir, create_private_dir, create_private_file,
    no_hard_links, plain_path, rename_dir, rename_new_file, sync_dir,
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

Restored to the data directory above, and started with the same backup
folder setting (--backup-dir, RITORNEL_BACKUP_DIR or backup_dir in
config.toml), the server writes its backups where it did before.

A server started on any other data directory writes no backups into a folder
that holds the backups of the data directory above. When {OWNER_FILE} beside
this backup names that data directory, this folder is one of those. To back
up a data directory restored to a new place, give it a backup folder of its
own. If it replaces the data directory above, put its new path on the first
line of {OWNER_FILE} instead.

To look inside without changing anything: sqlite3 -readonly library.db
",
        from = from.display(),
    )
}

/// What the README adds when the server copied scrobbles to Last.fm.
const UNBOUND: &str = "
This server scrobbles to Last.fm. A server started on this backup, wherever
it is put back, has scrobbling off until you turn it on again with
`ritornelctl lastfm scrobbling on`: the server it was taken from may still be
scrobbling as the same account.
";

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
    /// Whose backups the place named holds cannot be told.
    Unclear { place: PathBuf, why: String },
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
                 this one, {ours}. Give this data directory a backup folder of its own with \
                 `ritornel-server serve --backup-dir`, RITORNEL_BACKUP_DIR or backup_dir in \
                 config.toml. If this data directory replaces that one, moved or restored to a \
                 new place, put {ours} on the first line of {marker} instead",
                place = plain_path(place).display(),
                owner = plain_path(owner).display(),
                ours = plain_path(ours).display(),
                marker = plain_path(&place.join(OWNER_FILE)).display(),
            ),
            Self::Unclear { place, why } => write!(
                f,
                "cannot tell whose backups the backup folder {place} holds: {why}. Put the path of \
                 the data directory whose backups it holds on the first line of {marker}, or give \
                 this data directory a backup folder of its own with `ritornel-server serve \
                 --backup-dir`, RITORNEL_BACKUP_DIR or backup_dir in config.toml",
                place = plain_path(place).display(),
                marker = plain_path(&place.join(OWNER_FILE)).display(),
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
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
/// backups. A place that is no one's yet is claimed first, and given up
/// again if the backup fails.
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
    let claimed = claim(place, &resolved(dir.root()))?;
    write(dir, place, at_ms, reason, about).inspect_err(|_| {
        if claimed {
            // The place is left as it was found, with no file naming
            // whose it is.
            let _ = std::fs::remove_file(place.join(OWNER_FILE));
            let _ = sync_dir(place);
        }
    })
}

/// Makes sure `place` holds the backups of the data directory `ours`, as
/// resolved, and answers whether this call is what made it so. A place
/// that holds another's is refused, before anything is written in it.
fn claim(place: &Path, ours: &Path) -> Result<bool, BackupError> {
    match owner(place)? {
        Owner::Named(owner) | Owner::Inferred(owner) if !same_dir(&owner, ours) => {
            return Err(foreign(place, owner, ours));
        }
        Owner::Named(_) => return Ok(false),
        Owner::Inferred(_) | Owner::Unknown => {}
    }
    match name_owner(place, ours) {
        Ok(()) => Ok(true),
        // Another writer named its owner since: the place is this data
        // directory's only if it named this one.
        Err(BackupError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::AlreadyExists =>
        {
            match owner(place)? {
                Owner::Named(owner) if same_dir(&owner, ours) => Ok(false),
                Owner::Named(owner) | Owner::Inferred(owner) => Err(foreign(place, owner, ours)),
                Owner::Unknown => Err(unclear(place, format!("{OWNER_FILE} came and went"))),
            }
        }
        Err(error) => Err(error),
    }
}

fn foreign(place: &Path, owner: PathBuf, ours: &Path) -> BackupError {
    BackupError::Foreign {
        place: place.to_owned(),
        owner,
        ours: ours.to_owned(),
    }
}

fn unclear(place: &Path, why: String) -> BackupError {
    BackupError::Unclear {
        place: place.to_owned(),
        why,
    }
}

/// Whose backups a place holds.
#[derive(Debug)]
enum Owner {
    /// The data directory [`OWNER_FILE`] names.
    Named(PathBuf),
    /// With no [`OWNER_FILE`], as older versions left a place: the data
    /// directory of the earliest backup in it.
    Inferred(PathBuf),
    /// No one's yet: no [`OWNER_FILE`] and no backup.
    Unknown,
}

/// Whose backups `place` holds. Whatever leaves that in doubt is an error,
/// never taken for a place that is no one's.
fn owner(place: &Path) -> Result<Owner, BackupError> {
    let file = place.join(OWNER_FILE);
    match std::fs::symlink_metadata(&file) {
        Ok(found) if found.is_file() => return named(place, &file).map(Owner::Named),
        // A link, even one that leads nowhere, or a folder.
        Ok(_) => return Err(unclear(place, format!("{OWNER_FILE} is not a plain file"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unclear(place, format!("cannot read {OWNER_FILE}: {error}"))),
    }
    inferred(place)
}

/// The data directory on the first line of [`OWNER_FILE`], as an editor
/// may leave it: after a byte-order mark, and ending in `\r\n`.
fn named(place: &Path, file: &Path) -> Result<PathBuf, BackupError> {
    let bytes = std::fs::read(file)
        .map_err(|error| unclear(place, format!("cannot read {OWNER_FILE}: {error}")))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| unclear(place, format!("{OWNER_FILE} is not UTF-8 text")))?;
    let first = text
        .trim_start_matches('\u{feff}')
        .lines()
        .next()
        .unwrap_or_default();
    if first.trim().is_empty() {
        return Err(unclear(
            place,
            format!("{OWNER_FILE} names no data directory on its first line"),
        ));
    }
    let path = PathBuf::from(first);
    if !path.is_absolute() {
        return Err(unclear(
            place,
            format!("{OWNER_FILE} names {first}, which is not an absolute path"),
        ));
    }
    Ok(path)
}

/// The data directory of the earliest backup in `place`, by when each was
/// taken, for a place older versions wrote to without [`OWNER_FILE`].
fn inferred(place: &Path) -> Result<Owner, BackupError> {
    let cannot_list = |error: std::io::Error| unclear(place, format!("cannot list it: {error}"));
    let mut found = Vec::new();
    for entry in std::fs::read_dir(place).map_err(cannot_list)? {
        let path = entry.map_err(cannot_list)?.path();
        // Its name as it is, so one that is not UTF-8 is looked at too.
        let is_backup = path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"ritornel-backup-"));
        if !is_backup {
            continue;
        }
        // What a link leads to, and one that leads nowhere is in doubt.
        let is_dir = std::fs::metadata(&path)
            .map(|found| found.is_dir())
            .map_err(|error| unclear(place, format!("{}: {error}", plain_path(&path).display())))?;
        if !is_dir {
            continue;
        }
        let (at_ms, from) = taken_from(&path)
            .map_err(|why| unclear(place, format!("{}: {why}", plain_path(&path).display())))?;
        found.push((at_ms, path, from));
    }
    found.sort();
    let Some((earliest, _, from)) = found.first() else {
        return Ok(Owner::Unknown);
    };
    let from_one = found
        .iter()
        .take_while(|(at_ms, ..)| at_ms == earliest)
        .all(|(.., other)| resolved(other) == resolved(from));
    if !from_one {
        return Err(unclear(
            place,
            "its earliest backups were taken at the same moment from different data directories"
                .to_owned(),
        ));
    }
    Ok(Owner::Inferred(from.clone()))
}

/// When the backup in `folder` was taken, and from which data directory,
/// as its `backup.json` says.
fn taken_from(folder: &Path) -> Result<(i64, PathBuf), String> {
    let text = std::fs::read(folder.join("backup.json"))
        .map_err(|error| format!("cannot read backup.json: {error}"))?;
    let manifest: serde_json::Value = serde_json::from_slice(&text)
        .map_err(|error| format!("backup.json is not JSON: {error}"))?;
    let at_ms = manifest
        .get("takenAtMs")
        .and_then(serde_json::Value::as_i64)
        .ok_or("backup.json does not say when it was taken")?;
    let from = manifest
        .get("from")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .filter(|from| from.is_absolute())
        .ok_or("backup.json does not name the data directory it was taken from")?;
    Ok((at_ms, from))
}

/// Whether `path`, as the system resolves it now, is the data directory
/// `ours`, already resolved.
fn same_dir(path: &Path, ours: &Path) -> bool {
    resolved(path) == ours
}

/// Writes [`OWNER_FILE`] in `place`, naming `ours`, if there is none. It
/// is written whole under a dotted name first, then given its own name
/// only if nothing has it, so no reader sees it half written and none is
/// written over. On an error nothing of it is left.
fn name_owner(place: &Path, ours: &Path) -> Result<(), BackupError> {
    // Checked before it is made plain, which on Windows reads it lossily.
    let one_line = ours
        .to_str()
        .is_some_and(|line| !line.contains(['\n', '\r']));
    let shown = plain_path(ours);
    let line = shown.to_str().filter(|_| one_line).ok_or_else(|| {
        unclear(
            place,
            format!(
                "this data directory's path, {}, cannot be written on one line of {OWNER_FILE}",
                shown.display()
            ),
        )
    })?;
    let text = format!("{line}\n{OWNER_NOTE}");
    let file = place.join(OWNER_FILE);
    let partial = reserve_file(place, OWNER_FILE, text.as_bytes())?;
    match rename_new_file(&partial, &file) {
        Ok(()) => {}
        // A disk without hard links, such as FAT or exFAT: the file is
        // made in place, still only if it is not there, and a reader may
        // find it empty for as long as it takes to write.
        Err(error) if no_hard_links(&error) => {
            let _ = std::fs::remove_file(&partial);
            write_synced(&file, text.as_bytes()).inspect_err(|error| {
                if !matches!(error, BackupError::Io { source, .. } if source.kind() == std::io::ErrorKind::AlreadyExists)
                {
                    let _ = std::fs::remove_file(&file);
                }
            })?;
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            return Err(io("create", &file)(error));
        }
    }
    sync_dir(place).map_err(|error| {
        let _ = std::fs::remove_file(&file);
        io("sync", place)(error)
    })
}

/// Writes `bytes` to a new file in `place` under a dotted name made from
/// `name` that no other writer has, and answers it.
fn reserve_file(place: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, BackupError> {
    for n in 1..=SAME_SECOND {
        let partial = place.join(format!(".{name}.{n}.partial"));
        match write_synced(&partial, bytes) {
            Ok(()) => return Ok(partial),
            Err(BackupError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                let _ = std::fs::remove_file(&partial);
                return Err(error);
            }
        }
    }
    Err(BackupError::Io {
        doing: "name a new file in",
        path: place.to_owned(),
        source: std::io::Error::from(std::io::ErrorKind::AlreadyExists),
    })
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
    let marker = dir.root().join(MIGRATION_BACKUP);
    let previous = MigrationBackup::read(&marker).filter(|backup| {
        backup.reusable(dir, place, about, at_ms)
            && backup.state.as_ref().is_some_and(|state| {
                state
                    .unchanged(&dir.server_db(), &dir.library_db())
                    .unwrap_or(false)
            })
    });
    let backup = match previous {
        Some(previous) if previous.in_place || place.is_none() => {
            info!(path = %previous.path.display(), "reusing the snapshot from the previous migration attempt");
            previous
        }
        fallback => {
            say(Preparing::Backup);
            let (taken, failed) = snapshot_before(dir, place, at_ms, about, fallback.as_ref())?;
            let backup = MigrationBackup {
                about,
                state: fallback.and_then(|previous| previous.state).or_else(|| {
                    MigrationState::read(&dir.server_db(), &dir.library_db()).inspect_err(|error| {
                        warn!(%error, "could not record database state for migration snapshot reuse");
                    }).ok()
                }),
                server_target: ritornel_engine::SERVER_SCHEMA_VERSION,
                library_target: ritornel_engine::LIBRARY_SCHEMA_VERSION,
                path: taken.path,
                at_ms: taken.at_ms,
                in_place: place.is_some() && failed.is_none(),
                failed,
            };
            if let Err(error) = backup.save(dir, &marker) {
                warn!(%error, path = %marker.display(), "could not record the completed migration snapshot for reuse");
            }
            backup
        }
    };
    say(Preparing::Migration);
    Ok(BeforeMigrations {
        taken: backup.in_place.then_some(Taken {
            path: backup.path,
            at_ms: backup.at_ms,
            reason: BackupReason::BeforeMigration,
        }),
        failed: backup.failed,
    })
}

const REUSE_MS: i64 = 24 * 60 * 60 * 1_000;
const MIGRATION_BACKUP: &str = "migration-backup.json";

#[derive(Serialize, Deserialize)]
struct MigrationBackup {
    #[serde(flatten)]
    about: About,
    state: Option<MigrationState>,
    server_target: i64,
    library_target: i64,
    path: PathBuf,
    at_ms: i64,
    in_place: bool,
    failed: Option<String>,
}

impl MigrationBackup {
    fn read(marker: &Path) -> Option<Self> {
        if !marker.symlink_metadata().ok()?.is_file() {
            return None;
        }
        serde_json::from_reader(std::fs::File::open(marker).ok()?).ok()
    }

    fn reusable(&self, dir: &DataDir, place: Option<&Path>, about: About, at_ms: i64) -> bool {
        if !(self.at_ms..=self.at_ms.saturating_add(REUSE_MS)).contains(&at_ms)
            || self.about != about
            || self.server_target != ritornel_engine::SERVER_SCHEMA_VERSION
            || self.library_target != ritornel_engine::LIBRARY_SCHEMA_VERSION
        {
            return false;
        }
        let Some(parent) = self.path.parent() else {
            return false;
        };
        let destination_matches = if self.in_place {
            place.is_some_and(|place| {
                same_dir(parent, &resolved(place)) && outside(dir, place).is_ok()
            })
        } else {
            same_dir(parent, &resolved(&dir.backups())) && is_inside(parent, dir.root())
        };
        destination_matches
            && self.path.symlink_metadata().is_ok_and(|m| m.is_dir())
            && self
                .path
                .join("backup.json")
                .symlink_metadata()
                .is_ok_and(|m| m.is_file())
            && taken_from(&self.path)
                .is_ok_and(|(at, from)| at == self.at_ms && same_dir(&from, &resolved(dir.root())))
            && ["server.db", "library.db"].into_iter().all(|name| {
                let live = present(&dir.root().join(name));
                let copy = self.path.join(name).symlink_metadata();
                match (live, copy) {
                    (Ok(true), Ok(file)) => file.is_file() && file.len() > 0,
                    (Ok(false), Err(error)) => error.kind() == std::io::ErrorKind::NotFound,
                    _ => false,
                }
            })
    }

    fn save(&self, dir: &DataDir, marker: &Path) -> std::io::Result<()> {
        let temp = tempfile::NamedTempFile::new_in(dir.root())?;
        close_to_others(temp.path())?;
        serde_json::to_writer(temp.as_file(), self)?;
        temp.as_file().sync_all()?;
        temp.persist(marker)?;
        sync_dir(dir.root())
    }
}

pub(crate) fn migrations_finished(dir: &DataDir) {
    let marker = dir.root().join(MIGRATION_BACKUP);
    let cleared = match std::fs::remove_file(&marker) {
        Ok(()) => sync_dir(dir.root()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => Err(error),
    };
    if let Err(error) = cleared {
        warn!(%error, path = %marker.display(), "could not clear the migration snapshot marker");
    }
}

fn snapshot_before(
    dir: &DataDir,
    place: Option<&Path>,
    at_ms: i64,
    about: About,
    fallback: Option<&MigrationBackup>,
) -> Result<(Taken, Option<String>), crate::Error> {
    let reason = BackupReason::BeforeMigration;
    let mut failed = None;
    if let Some(place) = place {
        match to_place(dir, place, at_ms, reason, about) {
            Ok(taken) => {
                info!(path = %taken.path.display(), "backed up before migrating");
                return Ok((taken, None));
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
    if let Some(previous) = fallback {
        info!(path = %previous.path.display(), "reusing the local migration snapshot");
        return Ok((
            Taken {
                path: previous.path.clone(),
                at_ms: previous.at_ms,
                reason,
            },
            failed,
        ));
    }
    let rollback = dir.backups();
    // A `backups/` that leads out of the data directory, as a link to
    // the folder just refused would, is not written through.
    let leads_out = || crate::Error::Io {
        doing: "copy the databases before migrating them into",
        path: rollback.clone(),
        source: std::io::Error::other("it is a link, or leads out of the data directory"),
    };
    match std::fs::symlink_metadata(&rollback) {
        Ok(found) if found.file_type().is_symlink() => return Err(leads_out()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("read", &rollback)(error)),
    }
    create_private_dir(&rollback)?;
    if !is_inside(&rollback, dir.root()) {
        return Err(leads_out());
    }
    let kept = write(dir, &rollback, at_ms, reason, about).map_err(|error| crate::Error::Io {
        doing: "copy the databases before migrating them into",
        path: rollback.clone(),
        source: std::io::Error::other(error.to_string()),
    })?;
    info!(path = %kept.path.display(), "kept a copy in the data directory before migrating");
    Ok((kept, failed))
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

pub(crate) fn resolved(path: &Path) -> PathBuf {
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
            // A root that is not there, such as a drive not connected,
            // is kept as it is.
            _ => break existing.to_path_buf(),
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
    let (mut held, mut unbound) = (false, false);
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
        } else {
            // Put back, it scrobbles nothing until the owner says so: the
            // server it was taken from may still be scrobbling.
            unbound = unbind_scrobbling(&copy).map_err(|error| BackupError::Copy(name, error))?;
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
        "serverVersion": env!("RITORNEL_VERSION_DISPLAY"),
        "serverId": about.server_id,
        "libraryId": about.library_id,
        "schemas": { "server": about.server_schema, "library": about.library_schema },
        "files": files,
        "from": dir.root().to_string_lossy(),
    });
    let text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    write_synced(&into.join("backup.json"), text.as_bytes())?;
    let mut readme = readme(dir.root(), about);
    if unbound {
        readme.push_str(UNBOUND);
    }
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

    #[tokio::test]
    async fn a_failed_upgrade_takes_one_snapshot_until_repaired() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let start = || async {
            crate::Server::start(
                crate::HeldDir::take(dir.root().to_owned())?,
                crate::Listen::on_port(0),
                Arc::new(SystemClock),
                Arc::new(OsEntropy),
            )
            .await
        };
        // Build a database at the shipped first schema, with its real ledger
        // row. Restoring it later exercises a separate upgrade to the same target.
        let old = scratch.path().join("first-schema.db");
        let fixture = rusqlite::Connection::open(&old).unwrap();
        fixture
            .execute_batch(include_str!(
                "../../../crates/ritornel-engine/migrations/server/0001_server.sql"
            ))
            .unwrap();
        fixture
            .execute(
                "ATTACH DATABASE ?1 AS current",
                [dir.server_db().to_str().unwrap()],
            )
            .unwrap();
        fixture.execute_batch(
            "INSERT INTO main.schema_migration SELECT * FROM current.schema_migration WHERE version = 1"
        ).unwrap();
        drop(fixture);
        std::fs::copy(&old, dir.server_db()).unwrap();
        rusqlite::Connection::open(dir.server_db())
            .unwrap()
            .execute_batch(
                "INSERT INTO server_setting (key, value_json) VALUES ('test.setting', '1');
                 CREATE TRIGGER fail_upgrade BEFORE INSERT ON schema_migration BEGIN
                 SELECT raise(ABORT, 'migration failed'); END",
            )
            .unwrap();
        assert!(matches!(
            start().await,
            Err(crate::Error::Store(
                ritornel_engine::Error::MigrationFailed { .. }
            ))
        ));
        let original = finished(&dir.backups());
        assert_eq!(original.len(), 1);
        for _ in 0..3 {
            assert!(matches!(
                start().await,
                Err(crate::Error::Store(
                    ritornel_engine::Error::MigrationFailed { .. }
                ))
            ));
        }
        assert_eq!(finished(&dir.backups()), original);

        // A previous build changes an existing setting while the failed
        // upgrade's marker remains. The row count, ID and schema all match.
        let sqlite = rusqlite::Connection::open(dir.server_db()).unwrap();
        assert_eq!(
            sqlite
                .execute(
                    "UPDATE server_setting SET value_json = '2' WHERE key = 'test.setting'",
                    []
                )
                .unwrap(),
            1
        );
        drop(sqlite);
        assert!(matches!(
            start().await,
            Err(crate::Error::Store(
                ritornel_engine::Error::MigrationFailed { .. }
            ))
        ));
        let updated = finished(&dir.backups());
        assert_eq!(updated.len(), 2);
        let marker = MigrationBackup::read(&dir.root().join(MIGRATION_BACKUP)).unwrap();
        let value: String = rusqlite::Connection::open(marker.path.join("server.db"))
            .unwrap()
            .query_row(
                "SELECT value_json FROM server_setting WHERE key = 'test.setting'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(value, "2");

        rusqlite::Connection::open(dir.server_db())
            .unwrap()
            .execute_batch("DROP TRIGGER fail_upgrade")
            .unwrap();
        start().await.unwrap().stop().await.unwrap();
        assert_eq!(finished(&dir.backups()), updated);
        assert!(!dir.root().join(MIGRATION_BACKUP).exists());

        std::fs::copy(&old, dir.server_db()).unwrap();
        start().await.unwrap().stop().await.unwrap();
        assert_eq!(finished(&dir.backups()).len(), 3);
    }

    #[tokio::test]
    async fn a_failed_library_migration_keeps_one_snapshot_as_receipts_expire() {
        const THIRTY_DAYS_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
        let scratch = Scratch::new();
        let dir = data(&scratch);
        with_library(&dir);
        let clock = Arc::new(crate::testing::SetClock::at(AT));
        let start = || async {
            crate::Server::start(
                crate::HeldDir::take(dir.root().to_owned())?,
                crate::Listen::on_port(0),
                clock.clone(),
                Arc::new(OsEntropy),
            )
            .await
        };
        start().await.unwrap().stop().await.unwrap();

        // Make library migration 2 fail when it records its ledger entry.
        let library = rusqlite::Connection::open(dir.library_db()).unwrap();
        library
            .execute_batch(
                "CREATE TEMP TABLE later_migrations AS SELECT * FROM schema_migration WHERE version > 1;
                 DELETE FROM schema_migration WHERE version > 1;
                 CREATE TRIGGER fail_migration BEFORE INSERT ON schema_migration BEGIN
                   SELECT raise(ABORT, 'migration failed');
                 END",
            )
            .unwrap();
        let server = rusqlite::Connection::open(dir.server_db()).unwrap();
        for (key, created_at) in [(1u8, AT - THIRTY_DAYS_MS - 1), (2, AT - THIRTY_DAYS_MS + 1)] {
            server
                .execute(
                    "INSERT INTO command_receipt
                     (command_id, request_fingerprint, method, result_json, created_at_ms)
                     VALUES (?1, zeroblob(32), 'queue.clear', 'null', ?2)",
                    ([key; 16], created_at),
                )
                .unwrap();
        }
        let receipts = || -> i64 {
            server
                .query_row("SELECT count(*) FROM command_receipt", [], |row| row.get(0))
                .unwrap()
        };
        for elapsed in [0, 10_000, 20_000] {
            clock.set(AT + elapsed);
            assert!(matches!(
                start().await,
                Err(crate::Error::Library(
                    ritornel_engine::Error::MigrationFailed { version: 2, .. }
                ))
            ));
            assert_eq!(finished(&dir.backups()).len(), 1);
            assert_eq!(receipts(), 2);
        }
        library
            .execute_batch(
                "DROP TRIGGER fail_migration;
                 INSERT INTO schema_migration SELECT * FROM later_migrations",
            )
            .unwrap();
        drop(library);
        start().await.unwrap().stop().await.unwrap();
        assert_eq!(receipts(), 0);
        assert_eq!(finished(&dir.backups()).len(), 1);
    }

    #[test]
    fn retries_replace_missing_or_incomplete_snapshots_and_back_up_a_replaced_database() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        let take = |at_ms| {
            before_migrations(&dir, Some(&place), at_ms, server_pending(), &|_| {})
                .unwrap()
                .taken
                .unwrap()
        };

        let first = take(AT);
        let steps = std::cell::RefCell::new(Vec::new());
        let again = before_migrations(&dir, Some(&place), AT + 1, server_pending(), &|step| {
            steps.borrow_mut().push(step)
        })
        .unwrap();
        assert_eq!(again.taken.as_ref(), Some(&first));
        assert_eq!(steps.into_inner(), [Preparing::Migration]);
        assert_eq!(finished(&place).len(), 1);

        std::fs::remove_dir_all(&first.path).unwrap();
        let replacement = take(AT + 1_000);
        assert_ne!(replacement.path, first.path);
        std::fs::remove_file(replacement.path.join("server.db")).unwrap();
        let complete = take(AT + 2_000);
        assert_ne!(complete.path, replacement.path);

        let other = Scratch::new();
        let other_dir = data(&other);
        std::fs::copy(other_dir.server_db(), dir.server_db()).unwrap();
        let restored = take(AT + 3_000);
        assert_ne!(restored.path, complete.path);
        assert_ne!(
            manifest_of(&complete.path)["serverId"],
            manifest_of(&restored.path)["serverId"]
        );
        assert_eq!(take(AT + 4_000), restored);
    }

    #[test]
    fn a_partial_migration_takes_one_snapshot_of_the_new_starting_schemas() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        with_library(&dir);
        let both: &IsPending = &|_, _| Ok(Some(Pending { from: 1, to: 2 }));
        let library_only: &IsPending =
            &|_, db| Ok((db == Database::Library).then_some(Pending { from: 1, to: 2 }));
        let unplugged = scratch.path().join("unplugged");
        let run =
            |pending| before_migrations(&dir, Some(&unplugged), AT, pending, &|_| {}).unwrap();
        let first = run(both);
        assert!(first.failed.is_some());
        let partial = run(library_only);
        assert_eq!(finished(&dir.backups()).len(), 2);
        let again = run(library_only);
        assert!(again.taken.is_none());
        assert_eq!(again.failed, partial.failed);
        assert_eq!(finished(&dir.backups()).len(), 2);

        std::fs::write(dir.root().join(MIGRATION_BACKUP), b"{").unwrap();
        run(library_only);
        run(library_only);
        assert_eq!(finished(&dir.backups()).len(), 3);
    }

    #[test]
    fn migration_reuse_expires_and_a_library_write_requires_a_new_copy() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        with_library(&dir);
        let place = elsewhere(&scratch);
        let take = |at| {
            before_migrations(&dir, Some(&place), at, server_pending(), &|_| {})
                .unwrap()
                .taken
                .unwrap()
        };
        let first = take(AT);
        assert_eq!(take(AT + REUSE_MS), first);
        let expired = take(AT + REUSE_MS + 1);
        assert_ne!(expired, first);
        let earlier = take(AT);
        assert_ne!(earlier, expired);
        rusqlite::Connection::open(dir.library_db())
            .unwrap()
            .execute("UPDATE library_meta SET rev = rev + 1", [])
            .unwrap();
        assert_ne!(take(AT), earlier);
    }

    #[test]
    fn a_retry_promotes_a_local_snapshot_when_the_backup_disk_returns() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("backup-disk");
        let take =
            |at| before_migrations(&dir, Some(&place), at, server_pending(), &|_| {}).unwrap();
        assert!(take(AT).failed.is_some());
        assert!(take(AT + 1).failed.is_some());
        let local = finished(&dir.backups());
        assert_eq!(local.len(), 1);
        std::fs::create_dir(&place).unwrap();
        let promoted = take(AT + 2).taken.unwrap();
        assert_eq!(take(AT + 3).taken, Some(promoted.clone()));
        assert_eq!(finished(&place), [promoted.path]);
        assert_eq!(finished(&dir.backups()), local);
    }

    #[test]
    fn a_marker_that_cannot_be_replaced_does_not_block_migration_after_the_copy() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        std::fs::create_dir(dir.root().join(MIGRATION_BACKUP)).unwrap();
        let steps = std::cell::RefCell::new(Vec::new());
        before_migrations(&dir, None, AT, server_pending(), &|step| {
            steps.borrow_mut().push(step)
        })
        .unwrap();
        assert_eq!(
            steps.into_inner(),
            [Preparing::Backup, Preparing::Migration]
        );
        assert_eq!(finished(&dir.backups()).len(), 1);
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
        assert_eq!(manifest["serverVersion"], env!("RITORNEL_VERSION_DISPLAY"));
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
        // Two writers backing up one data directory into a place that is
        // no one's yet: both claim it, and both have it.
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
            plain_path(&resolved(original.root())).to_string_lossy()
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
        // The copy's sorts first by name, as an on-demand backup sorts
        // before one taken before a migration in the same second, but it
        // was taken later.
        legacy(
            &place,
            "ritornel-backup-2025-01-01-000000",
            json!({ "takenAtMs": 2_000, "from": copy.root() }),
        );
        legacy(
            &place,
            "ritornel-backup-2025-01-01-000000-before-migration",
            json!({ "takenAtMs": 1_000, "from": original.root() }),
        );
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

    /// A backup as an older version left it, with only `manifest` in it.
    fn legacy(place: &Path, name: &str, manifest: serde_json::Value) {
        std::fs::create_dir(place.join(name)).unwrap();
        std::fs::write(
            place.join(name).join("backup.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn two_data_directories_racing_for_an_empty_backup_folder_leave_it_one_owner() {
        for _ in 0..20 {
            let scratch = Scratch::new();
            let place = elsewhere(&scratch);
            let dirs = [
                DataDir::prepare(scratch.path().join("one")).unwrap(),
                DataDir::prepare(scratch.path().join("two")).unwrap(),
            ];
            for dir in &dirs {
                ServerStore::open(&dir.server_db(), Arc::new(SystemClock), Arc::new(OsEntropy))
                    .unwrap()
                    .ensure_identity("desk")
                    .unwrap();
            }
            let start = Arc::new(std::sync::Barrier::new(2));
            let done: Vec<_> = dirs
                .iter()
                .cloned()
                .map(|dir| {
                    let (place, start) = (place.clone(), start.clone());
                    std::thread::spawn(move || {
                        start.wait();
                        let done = backup(&dir, &place, About::default());
                        (dir.root().to_owned(), done)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect();
            let won: Vec<_> = done.iter().filter(|(_, done)| done.is_ok()).collect();
            assert_eq!(won.len(), 1, "{done:?}");
            let (winner, _) = won[0];
            for (_, lost) in done.iter().filter(|(_, done)| done.is_err()) {
                assert!(matches!(lost, Err(BackupError::Foreign { .. })), "{lost:?}");
            }
            // Only the winner's backup is there, and the folder is its.
            let backups = finished(&place);
            assert_eq!(backups.len(), 1);
            assert_eq!(manifest_of(&backups[0])["from"], winner.to_str().unwrap());
            assert!(matches!(
                owner(&place).unwrap(),
                Owner::Named(owner) if same_dir(&owner, &resolved(winner))
            ));
        }
    }

    #[test]
    fn a_first_backup_that_fails_leaves_the_folder_no_ones() {
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
        // So another data directory may still have it.
        let other = Scratch::new();
        backup(&data(&other), &place, About::default()).unwrap();
    }

    #[test]
    fn a_backup_folder_older_versions_wrote_to_is_written_to_by_none_while_its_owner_is_in_doubt() {
        let scratch = Scratch::new();
        let original = data(&scratch);
        let other = Scratch::new();
        let copy = data(&other);
        let earliest = "ritornel-backup-2025-01-01-000000";
        type Make<'a> = Box<dyn Fn(&Path) + 'a>;
        let cases: Vec<(&str, Make<'_>)> = vec![
            (
                "cannot read backup.json",
                Box::new(|place| std::fs::create_dir(place.join(earliest)).unwrap()),
            ),
            (
                "is not JSON",
                Box::new(|place| {
                    std::fs::create_dir(place.join(earliest)).unwrap();
                    std::fs::write(place.join(earliest).join("backup.json"), b"{ not").unwrap();
                }),
            ),
            (
                "does not say when",
                Box::new(|place| legacy(place, earliest, json!({ "from": original.root() }))),
            ),
            (
                "does not name the data directory",
                Box::new(|place| {
                    legacy(place, earliest, json!({ "takenAtMs": 1_000, "from": "" }));
                }),
            ),
            (
                "does not name the data directory",
                Box::new(|place| {
                    legacy(
                        place,
                        earliest,
                        json!({ "takenAtMs": 1_000, "from": "relative/data" }),
                    );
                }),
            ),
            (
                "same moment from different data directories",
                Box::new(|place| {
                    legacy(
                        place,
                        earliest,
                        json!({ "takenAtMs": 1_000, "from": original.root() }),
                    );
                    legacy(
                        place,
                        "ritornel-backup-2025-01-01-000000-2",
                        json!({ "takenAtMs": 1_000, "from": copy.root() }),
                    );
                }),
            ),
        ];
        for (why, make) in cases {
            let folder = Scratch::new();
            let place = elsewhere(&folder);
            // A backup of the original, after the one in doubt.
            legacy(
                &place,
                "ritornel-backup-2025-06-01-000000",
                json!({ "takenAtMs": 9_000, "from": original.root() }),
            );
            make(&place);
            let before = std::fs::read_dir(&place).unwrap().count();
            for dir in [&original, &copy] {
                let refused = backup(dir, &place, About::default()).unwrap_err();
                assert!(
                    matches!(refused, BackupError::Unclear { .. }),
                    "{why}: {refused}"
                );
                assert!(refused.to_string().contains(why), "{refused}");
                assert!(refused.to_string().contains(OWNER_FILE), "{refused}");
            }
            assert_eq!(std::fs::read_dir(&place).unwrap().count(), before, "{why}");
        }
    }

    #[test]
    fn a_file_that_names_no_absolute_data_directory_is_written_past_by_none() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let cases: [(&[u8], &str); 4] = [
            (b"\nsomething\n", "names no data directory"),
            (b"   \r\n", "names no data directory"),
            (b"relative/data\n", "not an absolute path"),
            (b"/data/\xff\xfe\n", "not UTF-8"),
        ];
        for (text, why) in cases {
            let folder = Scratch::new();
            let place = elsewhere(&folder);
            std::fs::write(place.join(OWNER_FILE), text).unwrap();
            let refused = backup(&dir, &place, About::default()).unwrap_err();
            assert!(matches!(refused, BackupError::Unclear { .. }), "{refused}");
            assert!(refused.to_string().contains(why), "{refused}");
            assert!(finished(&place).is_empty());
            assert_eq!(std::fs::read(place.join(OWNER_FILE)).unwrap(), text);
        }

        // Nor one that is a folder.
        let folder = Scratch::new();
        let place = elsewhere(&folder);
        std::fs::create_dir(place.join(OWNER_FILE)).unwrap();
        let refused = backup(&dir, &place, About::default()).unwrap_err();
        assert!(
            refused.to_string().contains("not a plain file"),
            "{refused}"
        );
    }

    // A link is made differently on each system, and on Windows only with
    // a privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_file_naming_the_owner_that_is_a_link_is_written_past_by_none() {
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = elsewhere(&scratch);
        // Even one that leads nowhere, which reading would take for no
        // file at all.
        std::os::unix::fs::symlink(scratch.path().join("nowhere"), place.join(OWNER_FILE)).unwrap();
        let refused = backup(&dir, &place, About::default()).unwrap_err();
        assert!(
            refused.to_string().contains("not a plain file"),
            "{refused}"
        );
        assert!(finished(&place).is_empty());
        assert!(!scratch.path().join("nowhere").exists());

        // Nor, with no such file, one whose backup is a link that leads
        // nowhere.
        let older = Scratch::new();
        let place = elsewhere(&older);
        std::os::unix::fs::symlink(
            older.path().join("nowhere"),
            place.join("ritornel-backup-2025-01-01-000000"),
        )
        .unwrap();
        let refused = backup(&dir, &place, About::default()).unwrap_err();
        assert!(matches!(refused, BackupError::Unclear { .. }), "{refused}");
        assert!(!place.join(OWNER_FILE).exists());
    }

    // A link is made differently on each system, and on Windows only with
    // a privilege a test does not have.
    #[cfg(unix)]
    #[test]
    fn a_way_back_that_leads_out_of_the_data_directory_is_not_written_through() {
        let scratch = Scratch::new();
        let original = data(&scratch);
        let place = elsewhere(&scratch);
        backup(&original, &place, About::default()).unwrap();
        let other = Scratch::new();
        let copy = data(&other);
        // The copy's `backups/` leads to the folder it is refused.
        std::os::unix::fs::symlink(&place, copy.backups()).unwrap();
        let Err(error) = before_migrations(&copy, Some(&place), AT, server_pending(), &|_| {})
        else {
            panic!("migrated through a link out of the data directory");
        };
        assert!(
            error
                .to_string()
                .contains("leads out of the data directory"),
            "{error}"
        );
        assert_eq!(finished(&place).len(), 1);
        assert!(before_migrations(&copy, None, AT, server_pending(), &|_| {}).is_err());
        assert_eq!(finished(&place).len(), 1);

        // Nor one that leads nowhere, which would be made where it leads.
        let nowhere = other.path().join("nowhere");
        std::fs::remove_file(copy.backups()).unwrap();
        std::os::unix::fs::symlink(&nowhere, copy.backups()).unwrap();
        assert!(before_migrations(&copy, None, AT, server_pending(), &|_| {}).is_err());
        assert!(!nowhere.exists());
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
        let older = Scratch::new();
        let place = elsewhere(&older);
        legacy(
            &place,
            "ritornel-backup-2025-01-01-000000",
            json!({ "takenAtMs": 1_000, "from": by_link }),
        );
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

        migrations_finished(&dir);
        // None named: the way back is kept in the data directory, and it
        // is not a backup.
        let done = before_migrations(&dir, None, AT, pending, &|_| {}).unwrap();
        assert!(done.taken.is_none() && done.failed.is_none());
        assert!(
            dir.backups()
                .join("ritornel-backup-2025-11-21-170102-before-migration/server.db")
                .is_file()
        );

        migrations_finished(&dir);
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
