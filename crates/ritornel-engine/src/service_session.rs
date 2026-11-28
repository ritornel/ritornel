//! This server's session at an outside service, in `server.db`: the
//! account it is signed in to, the session key, and whether it scrobbles.
//! The key is a secret that does not expire and can change the account, so
//! it lives in the file that never leaves this machine, and its type cannot
//! be printed.

use std::fmt;

use rusqlite::{Connection, OptionalExtension};

use crate::{Error, Id, ServerStore};

/// A session key at a service. `Debug` prints a fixed redaction and there
/// is no `Display`: it reaches the outside only through [`expose`].
///
/// [`expose`]: ServiceKey::expose
#[derive(Clone, PartialEq, Eq)]
pub struct ServiceKey(String);

impl ServiceKey {
    /// Wraps a key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself. Take care what you do with it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServiceKey(<redacted>)")
    }
}

/// This server's session at one service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSession {
    /// The `service_account` in the library.
    pub account: Id,
    /// The account's name, as the service spells it.
    pub account_name: String,
    /// The session key.
    pub key: ServiceKey,
    /// Whether the account is a subscriber's.
    pub subscriber: bool,
    /// When it was signed in.
    pub signed_in_at_ms: i64,
    /// Plays from this time on are scrobbled; `None` scrobbles none.
    pub scrobble_from_ms: Option<i64>,
    /// The service refused the session: nothing is sent until a new
    /// sign-in.
    pub sign_in_again: bool,
}

/// The `server_setting` row that names the data directory scrobbling to
/// `service` was turned on in: a JSON string, or JSON `null` in a backup's
/// copy, which no data directory is. See "Copied and restored data
/// directories" in docs/internals/history.md.
fn place_key(service: &str) -> String {
    format!("{service}.scrobbling_place")
}

fn forget_place(connection: &Connection, service: &str) -> Result<(), Error> {
    connection.execute(
        "DELETE FROM server_setting WHERE key = ?1",
        [place_key(service)],
    )?;
    Ok(())
}

fn keep_place(connection: &Connection, service: &str, place: Option<&str>) -> Result<(), Error> {
    connection.execute(
        "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
        (place_key(service), serde_json::to_string(&place)?),
    )?;
    Ok(())
}

/// Forgets the session at `service`. Whether there was one.
pub(crate) fn remove_session(connection: &Connection, service: &str) -> Result<bool, Error> {
    forget_place(connection, service)?;
    Ok(connection.execute("DELETE FROM service_session WHERE service = ?1", [service])? > 0)
}

/// Turns scrobbling to `service` on from `now_ms`, unless it is on, or off.
/// Turned on, it is bound to `place`, the data directory this server runs
/// in; turned off, to none. Whether there is a session to turn it on or off
/// for.
pub(crate) fn set_scrobbling(
    connection: &Connection,
    service: &str,
    on: bool,
    now_ms: i64,
    place: &str,
) -> Result<bool, Error> {
    let found = connection.execute(
        "UPDATE service_session
         SET scrobble_from_ms = CASE WHEN ?2 THEN coalesce(scrobble_from_ms, ?3) END
         WHERE service = ?1",
        rusqlite::params![service, on, now_ms],
    )? > 0;
    if found && on {
        keep_place(connection, service, Some(place))?;
    } else {
        forget_place(connection, service)?;
    }
    Ok(found)
}

/// Turns scrobbling off for every session that scrobbles in `connection`,
/// a backup's copy of `server.db`, and marks it as bound to no data
/// directory, so that a server started on it, of this build or an older
/// one, sends nothing until the owner turns scrobbling on again, and this
/// build says why ([`ServerStore::check_scrobbling_place`]). A file with no
/// sessions table is left alone. Returns whether any session was marked.
pub(crate) fn unbind_copy(connection: &Connection) -> Result<bool, Error> {
    let is_server: bool = connection.query_row(
        "SELECT count(*) = 2 FROM sqlite_schema
         WHERE type = 'table' AND name IN ('service_session', 'server_setting')",
        [],
        |row| row.get(0),
    )?;
    if !is_server {
        return Ok(false);
    }
    let services: Vec<String> = connection
        .prepare("SELECT service FROM service_session WHERE scrobble_from_ms IS NOT NULL")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for service in &services {
        keep_place(connection, service, None)?;
    }
    connection.execute("UPDATE service_session SET scrobble_from_ms = NULL", [])?;
    Ok(!services.is_empty())
}

/// What a server starting in a data directory found of where scrobbling to
/// a service was turned on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceCheck {
    /// Scrobbling is off, or on in this data directory.
    Same,
    /// On, with no data directory recorded, as one from a build before
    /// they were, and no restore waiting: this one is recorded now.
    Recorded,
    /// On, turned on in this other data directory, which this one was
    /// copied or moved from: scrobbling is turned off.
    Moved {
        /// The data directory it was turned on in.
        from: String,
    },
    /// Put back from a backup: a backup's copy marked so, or one an older
    /// build took, with no data directory recorded and the library held
    /// until an import. Scrobbling is off.
    Restored,
}

impl ServerStore {
    /// The session at `service`, if this server is signed in there.
    pub fn service_session(&self, service: &str) -> Result<Option<ServiceSession>, Error> {
        let service = service.to_owned();
        self.read(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT account_pid, account_name, session_key, subscriber, signed_in_at_ms,
                            scrobble_from_ms, sign_in_again
                     FROM service_session WHERE service = ?1",
                    [service],
                    |row| {
                        Ok(ServiceSession {
                            account: Id::from_bytes(row.get(0)?),
                            account_name: row.get(1)?,
                            key: ServiceKey(row.get(2)?),
                            subscriber: row.get(3)?,
                            signed_in_at_ms: row.get(4)?,
                            scrobble_from_ms: row.get(5)?,
                            sign_in_again: row.get(6)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    /// Keeps a new session at `service`, in place of any before it. For
    /// another account scrobbling starts off. Signed in again to the same
    /// account, as after the service refused the session, scrobbling stays
    /// as it was, from the time it was turned on, so the plays waiting to
    /// be sent are sent.
    pub fn save_service_session(
        &self,
        service: &str,
        account: Id,
        account_name: &str,
        key: &ServiceKey,
        subscriber: bool,
    ) -> Result<(), Error> {
        let (service, name, key) = (
            service.to_owned(),
            account_name.to_owned(),
            key.expose().to_owned(),
        );
        self.transact(move |transaction, now_ms, _| {
            transaction.execute(
                "INSERT INTO service_session
                   (service, account_pid, account_name, session_key, subscriber, signed_in_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (service) DO UPDATE SET
                   account_pid = excluded.account_pid, account_name = excluded.account_name,
                   session_key = excluded.session_key, subscriber = excluded.subscriber,
                   signed_in_at_ms = excluded.signed_in_at_ms,
                   scrobble_from_ms = CASE WHEN account_pid = excluded.account_pid
                                           THEN scrobble_from_ms END,
                   sign_in_again = 0",
                rusqlite::params![service, account.as_bytes(), name, key, subscriber, now_ms],
            )?;
            Ok(())
        })
    }

    /// Forgets the session at `service`. Whether there was one.
    pub fn remove_service_session(&self, service: &str) -> Result<bool, Error> {
        let service = service.to_owned();
        self.transact(move |transaction, _, _| remove_session(transaction, &service))
    }

    /// Turns scrobbling to `service` on, for plays from now on, bound to
    /// the data directory `place`, or off. Turned on while it is on, it
    /// keeps the time it was turned on. Returns the session as it now is.
    pub fn set_scrobbling(
        &self,
        service: &str,
        on: bool,
        place: &str,
    ) -> Result<Option<ServiceSession>, Error> {
        let (owned, place) = (service.to_owned(), place.to_owned());
        self.transact(move |transaction, now_ms, _| {
            set_scrobbling(transaction, &owned, on, now_ms, &place).map(|_| ())
        })?;
        self.service_session(service)
    }

    /// Checks, as a server starts in the data directory `place`, that
    /// scrobbling to `service` was turned on there. Turned on in another
    /// one, which this one is a copy of or was moved from, or put back from
    /// a backup, it is turned off, so that two servers never send as one
    /// session: the owner turns it on again. A backup's mark is cleared once
    /// it has been found. On with no place recorded, as in a data directory
    /// from an older build, it is bound to `place`, unless `held` says the
    /// library is held until an import, as one an older build's backup put
    /// back is.
    pub fn check_scrobbling_place(
        &self,
        service: &str,
        place: &str,
        held: bool,
    ) -> Result<PlaceCheck, Error> {
        let (service, place) = (service.to_owned(), place.to_owned());
        self.transact(move |transaction, _, _| {
            let stored: Option<String> = transaction
                .query_row(
                    "SELECT value_json FROM server_setting WHERE key = ?1",
                    [place_key(&service)],
                    |row| row.get(0),
                )
                .optional()?;
            // A value that cannot be read is no data directory: off, as for
            // a copy.
            let bound = stored.as_deref().map(|stored| {
                serde_json::from_str::<Option<String>>(stored).map_err(|_| stored.to_owned())
            });
            if let Some(Ok(None)) = bound {
                set_scrobbling(transaction, &service, false, 0, &place)?;
                return Ok(PlaceCheck::Restored);
            }
            let on: bool = transaction
                .query_row(
                    "SELECT scrobble_from_ms IS NOT NULL FROM service_session WHERE service = ?1",
                    [&service],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !on {
                return Ok(PlaceCheck::Same);
            }
            let found = match bound {
                None if held => PlaceCheck::Restored,
                None => {
                    keep_place(transaction, &service, Some(&place))?;
                    return Ok(PlaceCheck::Recorded);
                }
                Some(Ok(Some(from))) if from == place => return Ok(PlaceCheck::Same),
                Some(Ok(Some(from)) | Err(from)) => PlaceCheck::Moved { from },
                Some(Ok(None)) => PlaceCheck::Restored,
            };
            set_scrobbling(transaction, &service, false, 0, &place)?;
            Ok(found)
        })
    }

    /// Records that the service refused the session whose key is `key`,
    /// and when it first did, for the record's sign to say. A refusal of a
    /// session since replaced by a new sign-in changes nothing: the answer
    /// came late, and the new one is not refused.
    pub fn refuse_service_session(&self, service: &str, key: &str) -> Result<(), Error> {
        let (service, key) = (service.to_owned(), key.to_owned());
        self.transact(move |transaction, now_ms, _| {
            let refused = transaction.execute(
                "UPDATE service_session SET sign_in_again = 1
                 WHERE service = ?1 AND session_key = ?2 AND sign_in_again = 0",
                [&service, &key],
            )?;
            if refused > 0 {
                transaction.execute(
                    "INSERT INTO server_setting (key, value_json) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json",
                    (format!("{service}.refused_at_ms"), now_ms.to_string()),
                )?;
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testing::{CountingEntropy, FixedClock};

    const PLACE: &str = "/invented/data";

    #[test]
    fn a_session_is_kept_scrobbling_is_turned_on_from_a_time_and_the_key_never_prints() {
        let clock = Arc::new(FixedClock::at(1_000));
        let store =
            ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default()))
                .unwrap();
        assert_eq!(store.service_session("lastfm").unwrap(), None);
        let key = ServiceKey::new("SENTINEL_SESSION_KEY");
        let account = Id::from_bytes([7; 16]);
        store
            .save_service_session("lastfm", account, "someone", &key, false)
            .unwrap();
        let kept = store.service_session("lastfm").unwrap().unwrap();
        assert_eq!(
            (kept.account, kept.key.expose()),
            (account, "SENTINEL_SESSION_KEY")
        );
        assert_eq!(kept.scrobble_from_ms, None);
        assert!(!format!("{kept:?}").contains("SENTINEL"));

        clock.set(5_000);
        let on = store
            .set_scrobbling("lastfm", true, PLACE)
            .unwrap()
            .unwrap();
        assert_eq!(on.scrobble_from_ms, Some(5_000));
        clock.set(9_000);
        let again = store
            .set_scrobbling("lastfm", true, PLACE)
            .unwrap()
            .unwrap();
        assert_eq!(again.scrobble_from_ms, Some(5_000));
        let off = store
            .set_scrobbling("lastfm", false, PLACE)
            .unwrap()
            .unwrap();
        assert_eq!(off.scrobble_from_ms, None);

        store
            .refuse_service_session("lastfm", "SENTINEL_SESSION_KEY")
            .unwrap();
        assert!(
            store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .sign_in_again
        );
        store
            .save_service_session("lastfm", account, "someone", &key, false)
            .unwrap();
        assert!(
            !store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .sign_in_again
        );
        assert!(store.remove_service_session("lastfm").unwrap());
        assert_eq!(store.service_session("lastfm").unwrap(), None);
    }

    #[test]
    fn scrobbling_turned_on_in_one_data_directory_is_turned_off_in_a_copy_of_it() {
        let clock = Arc::new(FixedClock::at(1_000));
        let store =
            ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default()))
                .unwrap();
        let check = |place| {
            store
                .check_scrobbling_place("lastfm", place, false)
                .unwrap()
        };
        // Signed out, and signed in with scrobbling off, nothing is checked.
        assert_eq!(check("/elsewhere"), PlaceCheck::Same);
        let account = Id::from_bytes([7; 16]);
        store
            .save_service_session("lastfm", account, "someone", &ServiceKey::new("KEY"), false)
            .unwrap();
        assert_eq!(check("/elsewhere"), PlaceCheck::Same);

        // Turned on here, every start here goes on scrobbling.
        clock.set(5_000);
        store.set_scrobbling("lastfm", true, PLACE).unwrap();
        for _ in 0..2 {
            assert_eq!(check(PLACE), PlaceCheck::Same);
        }
        let on = store.service_session("lastfm").unwrap().unwrap();
        assert_eq!(on.scrobble_from_ms, Some(5_000));

        // Started in a copy, or after a move, it is turned off, and stays
        // off there until it is turned on again, from then on.
        assert_eq!(
            check("/copy"),
            PlaceCheck::Moved {
                from: PLACE.to_owned()
            }
        );
        assert_eq!(
            store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .scrobble_from_ms,
            None
        );
        assert_eq!(check("/copy"), PlaceCheck::Same);
        clock.set(9_000);
        store.set_scrobbling("lastfm", true, "/copy").unwrap();
        assert_eq!(check("/copy"), PlaceCheck::Same);
        assert_eq!(
            store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .scrobble_from_ms,
            Some(9_000)
        );

        // On with no place, as a data directory an older build turned it on
        // in, it is bound to where it starts.
        store
            .transact(|transaction, _, _| forget_place(transaction, "lastfm"))
            .unwrap();
        assert_eq!(check(PLACE), PlaceCheck::Recorded);
        assert_eq!(check(PLACE), PlaceCheck::Same);
        // Unless the library is held until an import, as one an older
        // build's backup put back is: then it is a restore, and off.
        store
            .transact(|transaction, _, _| forget_place(transaction, "lastfm"))
            .unwrap();
        assert_eq!(
            store.check_scrobbling_place("lastfm", PLACE, true).unwrap(),
            PlaceCheck::Restored
        );
        assert_eq!(
            store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .scrobble_from_ms,
            None
        );
        store.set_scrobbling("lastfm", true, PLACE).unwrap();
        assert!(
            store
                .service_session("lastfm")
                .unwrap()
                .unwrap()
                .scrobble_from_ms
                .is_some()
        );
    }

    #[test]
    fn signing_in_again_to_the_same_account_keeps_scrobbling_on_and_another_account_does_not() {
        let clock = Arc::new(FixedClock::at(1_000));
        let store =
            ServerStore::open_in_memory(clock.clone(), Arc::new(CountingEntropy::default()))
                .unwrap();
        let (someone, another) = (Id::from_bytes([7; 16]), Id::from_bytes([8; 16]));
        let sign_in = |account, name, key| {
            store
                .save_service_session("lastfm", account, name, &ServiceKey::new(key), false)
                .unwrap();
            store.service_session("lastfm").unwrap().unwrap()
        };
        sign_in(someone, "someone", "KEY_1");
        clock.set(5_000);
        store.set_scrobbling("lastfm", true, PLACE).unwrap();
        store.refuse_service_session("lastfm", "KEY_1").unwrap();

        // The same account, with a new key: on as before, from when it was
        // turned on, and no longer refused.
        clock.set(9_000);
        let again = sign_in(someone, "someone", "KEY_2");
        assert_eq!(
            (
                again.scrobble_from_ms,
                again.sign_in_again,
                again.key.expose()
            ),
            (Some(5_000), false, "KEY_2")
        );
        // Another account starts off.
        let other = sign_in(another, "another", "KEY_3");
        assert_eq!(other.scrobble_from_ms, None);
        // Off by the listener's choice, it stays off through a sign-in to
        // the same account.
        store.set_scrobbling("lastfm", true, PLACE).unwrap();
        store.set_scrobbling("lastfm", false, PLACE).unwrap();
        assert_eq!(sign_in(another, "another", "KEY_4").scrobble_from_ms, None);
    }
}
