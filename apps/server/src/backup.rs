//! Backups: a copy of this server's databases in a folder of its own, taken
//! on demand and before a database is migrated. See "The data directory"
//! in docs/design/server.md.
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
//! while it is taken. The folder is written under a name that begins with
//! a dot and is renamed once everything in it is whole, so a folder with
//! the plain name is a backup that finished.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ritornel_contract::{BackupReason, Id};
use ritornel_engine::backup::{Database, Pending, copy_database, pending_migrations};
use serde_json::json;
use tracing::{info, warn};

use crate::data_dir::DataDir;
use crate::platform::{
    close_to_others, create_new_private_dir, create_private_dir, create_private_file, sync_dir,
};

/// The version of `backup.json`.
const MANIFEST_VERSION: u32 = 1;

/// What a backup's folder says about restoring it.
const README: &str = "\
A backup of a Ritornel server, taken by the server itself.

server.db and library.db are whole SQLite databases, each a consistent copy
of the database as it was when the backup began. backup.json says when it
was taken, why, and what each file is.

To restore it, stop the server, and put server.db, library.db and, if it is
here, config.toml into an empty data directory: on a Mac
~/Library/Application Support/Ritornel unless you chose another. Then start
the server on that directory. Keep server.db and library.db together: the
library names the server that may write it, and that server is the one in
server.db.

To look inside without changing anything: sqlite3 -readonly library.db
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
        outside(dir, place)?;
        write(dir, place, at_ms, reason, about)
    }
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
/// the server does not start.
pub(crate) fn before_migrations(
    dir: &DataDir,
    place: Option<&Path>,
    at_ms: i64,
    pending: &IsPending,
) -> Result<BeforeMigrations, crate::Error> {
    let server = pending(&dir.server_db(), Database::Server).map_err(crate::Error::Store)?;
    let library = pending(&dir.library_db(), Database::Library).map_err(crate::Error::Library)?;
    if server.is_none() && library.is_none() {
        return Ok(BeforeMigrations::default());
    }
    let about = About {
        server_schema: server.map(|pending| pending.from),
        library_schema: library.map(|pending| pending.from),
        ..About::default()
    };
    info!(
        ?server,
        ?library,
        "a database will be migrated, so it is copied first"
    );
    let reason = BackupReason::BeforeMigration;
    let mut failed = None;
    if let Some(place) = place {
        match outside(dir, place).and_then(|()| write(dir, place, at_ms, reason, about)) {
            Ok(taken) => {
                info!(path = %taken.path.display(), "backed up before migrating");
                return Ok(BeforeMigrations {
                    taken: Some(taken),
                    failed: None,
                });
            }
            Err(error) => {
                warn!(%error, "the backup before migrating failed; a copy is kept in the data directory instead");
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
    Ok(BeforeMigrations {
        taken: None,
        failed,
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
) -> Result<BeforeMigrations, crate::Error> {
    before_migrations(dir, place, at_ms, &pending_migrations)
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
/// so that the new name is. Each file in it was synced as it was written.
/// On an error nothing of it is left, and it is not to be called whole.
pub(crate) fn publish(partial: &Path, place: &Path, name: &str) -> Result<PathBuf, BackupError> {
    let finished = place.join(name);
    let renamed = sync_dir(partial)
        .map_err(io("sync", partial))
        .and_then(|()| std::fs::rename(partial, &finished).map_err(io("finish", &finished)));
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
    write_synced(&into.join("README.txt"), README.as_bytes())
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
    fn two_servers_backing_up_to_one_place_in_the_same_second_write_two_backups() {
        let scratch = Scratch::new();
        let place = scratch.path().join("shared");
        std::fs::create_dir(&place).unwrap();
        let taken: Vec<_> = (0..2)
            .map(|n| {
                let other = Scratch::new();
                let dir = data(&other);
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
                    drop(other);
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
            before_migrations(&dir, Some(&scratch.path().join("unplugged")), AT, pending).is_err()
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
    fn a_migration_is_copied_for_first_to_the_place_or_else_into_the_data_directory() {
        let pending: &dyn Fn(&Path, Database) -> _ =
            &|_, database| Ok((database == Database::Server).then_some(Pending { from: 1, to: 2 }));
        let none: &IsPending = &|_, _| Ok(None);

        // Nothing to migrate: nothing is copied.
        let scratch = Scratch::new();
        let dir = data(&scratch);
        let place = scratch.path().join("elsewhere");
        std::fs::create_dir(&place).unwrap();
        let done = before_migrations(&dir, Some(&place), AT, none).unwrap();
        assert!(done.taken.is_none() && done.failed.is_none());
        assert_eq!(std::fs::read_dir(&place).unwrap().count(), 0);

        // A place named: the backup goes there, and nothing beside the data.
        let done = before_migrations(&dir, Some(&place), AT, pending).unwrap();
        assert_eq!(
            done.taken.unwrap().path,
            place.join("ritornel-backup-2025-11-21-170102-before-migration")
        );
        assert!(!dir.backups().exists());

        // None named: the way back is kept in the data directory, and it
        // is not a backup.
        let done = before_migrations(&dir, None, AT, pending).unwrap();
        assert!(done.taken.is_none() && done.failed.is_none());
        assert!(
            dir.backups()
                .join("ritornel-backup-2025-11-21-170102-before-migration/server.db")
                .is_file()
        );

        // A place that cannot be written: the way back is kept all the
        // same, and the failure is said.
        let unplugged = scratch.path().join("unplugged");
        let done = before_migrations(&dir, Some(&unplugged), AT, pending).unwrap();
        assert!(done.taken.is_none());
        assert!(done.failed.unwrap().contains("is not there"));
        assert!(
            dir.backups()
                .join("ritornel-backup-2025-11-21-170102-before-migration-2/server.db")
                .is_file()
        );
    }
}
