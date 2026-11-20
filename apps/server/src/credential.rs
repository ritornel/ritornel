//! The credential file: the local owner's session token, in a file in the
//! data directory that only the OS user can read. A program on this machine
//! that can read it, such as `ritornelctl`, holds a `full` session. See
//! "Pairing a client" in docs/design/linking.md.
//!
//! `server.db` keeps only the token's hash, so the file is the one place the
//! token is. If the file is lost, is not the token the database expects, or
//! can be read by other users, the server revokes that session and issues a
//! new one. That gives away nothing: whoever could remove the file could
//! also have read it.

use std::fmt;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

use ritornel_contract::Id;
use ritornel_engine::{ServerStore, SessionToken};
use subtle::ConstantTimeEq;

use crate::Error;
use crate::error::io_error;

/// Whether nobody but the OS user (and on Windows the system and the
/// administrators, who can take any file) may open the file: on Unix no
/// permission bit for the group or others, on Windows no entry in its
/// access list for anyone else.
fn private(file: &File) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(file.metadata()?.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        ritornel_windows::private::is_private(file)
    }
}

/// Creates a new file at `path` that only the OS user can open, from
/// before anything is written to it. It fails if something is there.
fn create_private(path: &Path) -> std::io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        ritornel_windows::private::create_restricted(path)
    }
}

/// What [`ensure`] found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Credential {
    /// First start: the token that came with the new identity was written.
    Issued,
    /// The file holds the local credential. Nothing was changed.
    Kept,
    /// The file was missing, open to other users, or not the local
    /// credential. The old session was revoked and a new token written.
    Replaced,
}

impl Credential {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Kept => "kept",
            Self::Replaced => "replaced",
        }
    }
}

/// What a running server checks a presented token against: the SHA-256 of
/// the local credential's token, never the token. It is taken once, when
/// the server starts, so checking a credential reads no file and no
/// database. The server is the only writer of sessions, so what it loaded
/// stays true until it issues or revokes one, and that goes through here.
///
/// With it is the public ID of the session the credential holds: the client
/// that a command sent with this credential is recorded as.
#[derive(Clone)]
pub(crate) struct Verifier {
    hash: [u8; 32],
    session: Option<Id>,
}

impl Verifier {
    fn of(token: &SessionToken, store: &ServerStore) -> Result<Self, Error> {
        Ok(Self {
            hash: token.hash(),
            session: store.local_session()?,
        })
    }

    /// Whether `token` is the local credential. The two hashes are compared
    /// in the same time wherever they first differ.
    pub(crate) fn accepts(&self, token: &SessionToken) -> bool {
        token.hash().ct_eq(&self.hash).into()
    }

    /// The public ID of the local credential's session.
    pub(crate) fn session(&self) -> Option<Id> {
        self.session
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Verifier(<redacted>)")
    }
}

/// Makes the file at `path` hold a token that `store` accepts as the local
/// credential, and returns what to check a presented token against.
/// `issued` is the token from a first start, if this is one.
pub(crate) fn ensure(
    store: &ServerStore,
    path: &Path,
    issued: Option<SessionToken>,
) -> Result<(Credential, Verifier), Error> {
    if let Some(token) = issued {
        write(path, &token)?;
        return Ok((Credential::Issued, Verifier::of(&token, store)?));
    }
    if let Some(token) = read(path)?
        && store.is_local_credential(&token)?
    {
        return Ok((Credential::Kept, Verifier::of(&token, store)?));
    }
    let token = store.reissue_local_credential()?;
    write(path, &token)?;
    Ok((Credential::Replaced, Verifier::of(&token, store)?))
}

/// The token in the file, if the file holds one that can still be trusted.
/// A file that is missing, that other users can read, or that is not text
/// holds none, and the caller replaces it. Only a failure to read is an
/// error.
fn read(path: &Path) -> Result<Option<SessionToken>, Error> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", path)(error)),
    };
    if !private(&file).map_err(io_error("read", path))? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(io_error("read", path))?;
    Ok(String::from_utf8(bytes)
        .ok()
        .map(|text| SessionToken::new(text.trim())))
}

/// Writes the token under a temporary name, readable by the OS user alone
/// from the moment it exists, then renames it into place.
fn write(path: &Path, token: &SessionToken) -> Result<(), Error> {
    let temporary = path.with_extension("tmp");
    match std::fs::remove_file(&temporary) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(io_error("remove", &temporary)(error));
        }
        _ => {}
    }
    create_private(&temporary)
        .and_then(|mut file| {
            writeln!(file, "{}", token.expose())?;
            file.sync_all()
        })
        .map_err(io_error("write", &temporary))?;
    std::fs::rename(&temporary, path).map_err(io_error("write", path))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ritornel_engine::{OsEntropy, SystemClock};

    use super::*;
    use crate::testing::{Scratch, open_to_others, private};

    fn store() -> ServerStore {
        ServerStore::open_in_memory(Arc::new(SystemClock), Arc::new(OsEntropy)).unwrap()
    }

    fn token_in(path: &Path) -> SessionToken {
        read(path).unwrap().unwrap()
    }

    #[test]
    fn first_start_writes_the_token_where_only_the_os_user_can_read_it() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        let issued = issued.unwrap();

        let (did, verifier) = ensure(&store, &path, Some(issued.clone())).unwrap();
        assert_eq!(did, Credential::Issued);
        assert!(verifier.accepts(&issued));
        assert!(private(&path));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", issued.expose())
        );
        assert!(store.is_local_credential(&token_in(&path)).unwrap());
        assert!(!scratch.path().join("credential.tmp").exists());
    }

    #[test]
    fn the_verifier_accepts_the_token_in_the_file_and_no_other() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        let first = issued.clone().unwrap();
        ensure(&store, &path, issued).unwrap();

        // A later start keeps the token, and checks against the same one.
        let (did, kept) = ensure(&store, &path, None).unwrap();
        assert_eq!(did, Credential::Kept);
        assert!(kept.accepts(&first));
        assert!(kept.accepts(&token_in(&path)));
        for wrong in [
            "",
            "0",
            &first.expose()[1..],
            &format!("{}0", first.expose()),
        ] {
            assert!(!kept.accepts(&SessionToken::new(wrong)), "{}", wrong.len());
        }

        // A replaced token is the only one accepted from then on.
        std::fs::remove_file(&path).unwrap();
        let (did, replaced) = ensure(&store, &path, None).unwrap();
        assert_eq!(did, Credential::Replaced);
        assert!(replaced.accepts(&token_in(&path)));
        assert!(!replaced.accepts(&first));
        // What it holds is not printed.
        assert_eq!(format!("{replaced:?}"), "Verifier(<redacted>)");
    }

    #[test]
    fn a_later_start_keeps_the_file_as_it_is() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        ensure(&store, &path, issued).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let (_, none) = store.ensure_identity("desk").unwrap();
        assert_eq!(ensure(&store, &path, none).unwrap().0, Credential::Kept);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn a_lost_file_is_replaced_and_the_lost_token_stops_working() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        let lost = issued.unwrap();
        // The file was never written, as after a crash between the commit
        // and the write, or a `server.db` restored from a backup.

        assert_eq!(ensure(&store, &path, None).unwrap().0, Credential::Replaced);
        assert!(private(&path));
        let replacement = token_in(&path);
        assert_ne!(replacement.expose(), lost.expose());
        assert!(store.is_local_credential(&replacement).unwrap());
        assert!(!store.is_local_credential(&lost).unwrap());
    }

    #[test]
    fn a_file_that_is_not_the_local_credential_is_replaced() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        ensure(&store, &path, issued).unwrap();

        for wrong in ["a token from some other server\n", ""] {
            std::fs::write(&path, wrong).unwrap();
            assert_eq!(ensure(&store, &path, None).unwrap().0, Credential::Replaced);
            assert!(store.is_local_credential(&token_in(&path)).unwrap());
        }
    }

    #[test]
    fn a_file_other_users_can_read_is_replaced_and_its_token_stops_working() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        ensure(&store, &path, issued).unwrap();
        let exposed = token_in(&path);
        // As after a restore that did not keep the file's mode.
        open_to_others(&path);

        assert_eq!(ensure(&store, &path, None).unwrap().0, Credential::Replaced);
        assert!(private(&path));
        assert!(store.is_local_credential(&token_in(&path)).unwrap());
        assert!(!store.is_local_credential(&exposed).unwrap());
    }

    #[test]
    fn a_file_that_is_not_text_is_replaced() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();
        ensure(&store, &path, issued).unwrap();
        std::fs::write(&path, [0xff, 0xfe]).unwrap();

        assert_eq!(ensure(&store, &path, None).unwrap().0, Credential::Replaced);
        assert!(store.is_local_credential(&token_in(&path)).unwrap());
    }

    #[test]
    fn a_temporary_file_left_by_a_crash_is_no_obstacle_and_is_not_trusted() {
        let scratch = Scratch::new();
        let path = scratch.path().join("credential");
        let left_behind = scratch.path().join("credential.tmp");
        std::fs::write(&left_behind, "left by a crash, and open to everyone").unwrap();
        let store = store();
        let (_, issued) = store.ensure_identity("desk").unwrap();

        ensure(&store, &path, issued).unwrap();
        assert!(private(&path));
        assert!(store.is_local_credential(&token_in(&path)).unwrap());
        assert!(!left_behind.exists());
    }
}
