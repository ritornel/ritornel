//! The Last.fm calls: status, sign-in and sign-out, the sync, scrobbling
//! and the report. On a server built without Last.fm, `lastfm.status` says
//! so and every other call answers `unavailable`.

use ritornel_contract::{
    ErrorCode, KeyParams, LastfmReport, LastfmReportParams, LastfmStatus, LastfmSyncParams,
    LastfmSyncStarted, RpcError, ScrobblingParams, ScrobblingSet, SignInFinishParams,
    SignInFinished, SignInStarted, SignedOut,
};

use super::{Core, Stores};
use crate::lastfm::Lastfm;

impl Core {
    /// Sets up Last.fm for this run. Called once, at start.
    pub(crate) fn set_lastfm(&self, lastfm: Lastfm) {
        // A second call is a mistake of the caller's, and keeps the first.
        let _ = self.lastfm.set(lastfm);
    }

    /// Last.fm, once the server has started.
    pub(crate) fn lastfm(&self) -> Option<&Lastfm> {
        self.lastfm.get()
    }

    /// Wakes the scrobbler: a play that counts may be due.
    pub(crate) fn wake_scrobbler(&self) {
        if let Some(lastfm) = self.lastfm() {
            lastfm.wake();
        }
    }

    /// Says a track began to be heard, or was heard again after a pause,
    /// for Last.fm's "now playing". It only puts it where the scrobbler
    /// takes it from, so the player never waits on Last.fm.
    pub(crate) fn now_playing(&self, appearance: ritornel_contract::Id, duration_ms: i64) {
        if let Some(lastfm) = self.lastfm() {
            lastfm.began(appearance, duration_ms);
        }
    }

    /// Says the player paused or stopped: a track not yet shown as playing
    /// is not shown.
    pub(crate) fn nothing_playing(&self) {
        if let Some(lastfm) = self.lastfm() {
            lastfm.stopped();
        }
    }
}

#[cfg(not(feature = "lastfm"))]
fn not_built() -> RpcError {
    RpcError::new(
        ErrorCode::Unavailable,
        "this server was built without Last.fm",
    )
    .with_detail("reason", "not_built")
}

#[cfg(not(feature = "lastfm"))]
impl Core {
    pub(super) fn lastfm_status(&self, _: &Stores) -> Result<LastfmStatus, RpcError> {
        Ok(LastfmStatus {
            available: false,
            configured: false,
            account: None,
            sign_in_again: false,
            scrobbling_since: None,
            waiting_for_import_since: None,
            deliveries: Vec::new(),
            imported: 0,
            zones_set: false,
            import: ritornel_contract::ImportSchedule::default(),
        })
    }

    pub(super) fn lastfm_sign_in_start(&self) -> Result<SignInStarted, RpcError> {
        Err(not_built())
    }

    pub(super) fn lastfm_sign_in_finish(
        &self,
        _: &Stores,
        _: &SignInFinishParams,
    ) -> Result<SignInFinished, RpcError> {
        Err(not_built())
    }

    pub(super) fn lastfm_sign_out(&self, _: &Stores, _: &KeyParams) -> Result<SignedOut, RpcError> {
        Err(not_built())
    }

    pub(super) fn lastfm_sync(
        &self,
        _: &Stores,
        _: &LastfmSyncParams,
    ) -> Result<LastfmSyncStarted, RpcError> {
        Err(not_built())
    }

    pub(super) fn lastfm_set_scrobbling(
        &self,
        _: &Stores,
        _: &ScrobblingParams,
    ) -> Result<ScrobblingSet, RpcError> {
        Err(not_built())
    }

    pub(super) fn lastfm_report(
        &self,
        _: &Stores,
        _: &LastfmReportParams,
    ) -> Result<LastfmReport, RpcError> {
        Err(not_built())
    }
}

#[cfg(feature = "lastfm")]
mod enabled {
    use ritornel_contract::methods::{LastfmSetScrobbling, LastfmSignOut, LastfmSync, Method};
    use ritornel_contract::{
        DeliveryCount, DoubtfulTrack, ImportSchedule, LastImport, NearlyMatched, Receipt,
        ReceiptStatus,
    };
    use ritornel_engine::{Command, LibraryStore, NewJob, ServiceSession, SettingChange};

    use super::*;
    use crate::dispatch::calls::job;
    use crate::dispatch::fingerprint;
    use crate::dispatch::playback::refused;
    use crate::lastfm::{
        Auto, IMPORT_EVERY, IMPORT_ON_OPEN_AFTER, SERVICE, SYNC_JOB, Trigger, due, keep_session, ms,
    };

    /// The most doubtful tracks one report lists.
    const MAX_DOUBTFUL: u32 = 1000;

    fn not_set_up() -> RpcError {
        RpcError::new(
            ErrorCode::Unavailable,
            "Last.fm is not set up on this server",
        )
    }

    fn unsigned(number: i64) -> u64 {
        u64::try_from(number).unwrap_or(0)
    }

    /// Whether the listener has a zone for any date, which a sync needs.
    fn zones_set(library: &LibraryStore) -> Result<bool, RpcError> {
        Ok(match library.default_listener().map_err(refused)? {
            Some(listener) => !library
                .zone_periods_of(listener)
                .map_err(refused)?
                .is_empty(),
            None => false,
        })
    }

    /// One account has one sync at a time, by this key.
    fn sync_key(session: &ServiceSession) -> String {
        session.account_name.to_lowercase()
    }

    fn receipt(key: ritornel_contract::Id, status: ReceiptStatus) -> Receipt {
        Receipt {
            command_id: key,
            status,
            revision: None,
        }
    }

    impl Core {
        fn lastfm_or_refuse(&self) -> Result<&Lastfm, RpcError> {
            self.lastfm().ok_or_else(not_set_up)
        }

        /// The session this server keeps, or why there is none.
        fn session(stores: &Stores) -> Result<ServiceSession, RpcError> {
            stores
                .server()
                .service_session(SERVICE)
                .map_err(refused)?
                .ok_or_else(|| {
                    RpcError::new(
                        ErrorCode::Unavailable,
                        "not signed in to Last.fm: sign in with `ritornelctl lastfm signin`",
                    )
                    .with_detail("reason", "not_signed_in")
                })
        }

        pub(in crate::dispatch) fn lastfm_status(
            &self,
            stores: &Stores,
        ) -> Result<LastfmStatus, RpcError> {
            let lastfm = self.lastfm_or_refuse()?;
            let session = stores.server().service_session(SERVICE).map_err(refused)?;
            let (deliveries, imported, zones_set, held) = Self::library(stores, |library| {
                let zones_set = zones_set(library)?;
                let Some(session) = &session else {
                    return Ok((Vec::new(), 0, zones_set, None));
                };
                Ok((
                    library
                        .delivery_counts(session.account)
                        .map_err(refused)?
                        .into_iter()
                        .map(|(state, count)| DeliveryCount {
                            state,
                            count: unsigned(count),
                        })
                        .collect(),
                    unsigned(library.imported_count(session.account).map_err(refused)?),
                    zones_set,
                    library.held_for_import().map_err(refused)?,
                ))
            })
            .unwrap_or_default();
            let scrobbling_since = session.as_ref().and_then(|s| s.scrobble_from_ms);
            Ok(LastfmStatus {
                available: true,
                configured: lastfm.configured(),
                account: session.as_ref().map(|s| s.account_name.clone()),
                sign_in_again: session.as_ref().is_some_and(|s| s.sign_in_again),
                scrobbling_since,
                // Said only while it holds something back: not while a new
                // sign-in holds everything back anyway.
                waiting_for_import_since: held
                    .filter(|_| {
                        scrobbling_since.is_some()
                            && !session.as_ref().is_some_and(|s| s.sign_in_again)
                    })
                    .and_then(|held| held.since_ms),
                deliveries,
                imported,
                zones_set,
                import: self.import_schedule(stores, session.as_ref()),
            })
        }

        /// The session an import would run as, or why none starts on its
        /// own: only the hub imports, and only when a sync could run.
        fn importable(&self, stores: &Stores) -> Result<ServiceSession, String> {
            if !self.is_hub() {
                return Err("this server is not the library's hub".to_owned());
            }
            if !self.lastfm().is_some_and(Lastfm::configured) {
                return Err("this server has no Last.fm API key and secret".to_owned());
            }
            let session = match stores.server().service_session(SERVICE) {
                Ok(Some(session)) => session,
                Ok(None) => return Err("not signed in".to_owned()),
                Err(failed) => return Err(failed.to_string()),
            };
            if session.sign_in_again {
                return Err("Last.fm refused the session: sign in again".to_owned());
            }
            match Self::library(stores, zones_set) {
                Ok(true) => Ok(session),
                Ok(false) => {
                    Err("no zones: add one with `ritornelctl history zone add`".to_owned())
                }
                Err(failed) => Err(failed.message),
            }
        }

        /// Starts an import if `trigger` makes one due and none is queued
        /// or running. See "Last.fm" in docs/design/history.md.
        pub(crate) fn import_if_due(&self, trigger: Trigger) -> Auto {
            self.with_stores(|stores| {
                let session = match self.importable(stores) {
                    Ok(session) => session,
                    Err(why) => return Auto::Off(why),
                };
                let (key, server) = (sync_key(&session), stores.server());
                let last = match server.active_job(SYNC_JOB, &key) {
                    Ok(Some(_)) => return Auto::Running,
                    Ok(None) => server.last_ended_job(SYNC_JOB, &key),
                    Err(failed) => Err(failed),
                };
                let last = match last {
                    Ok(last) => last.and_then(|job| job.finished_at_ms),
                    Err(failed) => return Auto::Failed(failed.to_string()),
                };
                if !due(trigger, last, self.clock.now_ms()) {
                    return Auto::NotDue;
                }
                let started = ritornel_engine::new_id(self.clock.now_ms(), &*self.entropy)
                    .map_err(refused)
                    .and_then(|idempotency_key| {
                        let params = LastfmSyncParams {
                            idempotency_key,
                            full: false,
                        };
                        self.start_sync(stores, &session, &params)
                    });
                match started {
                    Ok((job, true)) => Auto::Started(job.public_id),
                    Ok((_, false)) => Auto::Running,
                    Err(failed) => Auto::Failed(failed.message),
                }
            })
            .unwrap_or_else(|| Auto::Off("the server is stopping".to_owned()))
        }

        /// When the account was last imported and when it next is.
        fn import_schedule(
            &self,
            stores: &Stores,
            session: Option<&ServiceSession>,
        ) -> ImportSchedule {
            let server = stores.server();
            let (last, running) = session.map_or((None, false), |session| {
                let key = sync_key(session);
                (
                    server.last_ended_job(SYNC_JOB, &key).ok().flatten(),
                    server.active_job(SYNC_JOB, &key).ok().flatten().is_some(),
                )
            });
            let off = self.importable(stores).err();
            let (ended, now) = (
                last.as_ref().and_then(|job| job.finished_at_ms),
                self.clock.now_ms(),
            );
            // Never imported, or overdue: the thread starts one within a
            // minute.
            let after = |interval| {
                (off.is_none() && !running)
                    .then(|| ended.map_or(now, |ended| (ended + ms(interval)).max(now)))
            };
            ImportSchedule {
                next: after(IMPORT_EVERY),
                on_open_after: after(IMPORT_ON_OPEN_AFTER),
                last: last.as_ref().and_then(|ended| {
                    Some(LastImport {
                        ended: ended.finished_at_ms?,
                        state: job(ended).state,
                        failure: ended.error.clone(),
                    })
                }),
                running,
                off,
            }
        }

        /// Queues a sync of the account's history, or finds the one that
        /// is queued or running.
        fn start_sync(
            &self,
            stores: &Stores,
            session: &ServiceSession,
            params: &LastfmSyncParams,
        ) -> Result<(ritornel_engine::Job, bool), RpcError> {
            let command = Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<LastfmSync>(params)?,
            };
            let new = NewJob {
                kind: SYNC_JOB,
                dedupe_key: sync_key(session),
                target: session.account,
                params_json: serde_json::to_string(params).unwrap_or_default(),
                session: self.local_credential.session(),
            };
            let (started, created) = stores
                .server()
                .start_job(command, LastfmSync::NAME, new)
                .map_err(refused)?;
            if created {
                self.publish_stored_job(stores, &started);
            }
            self.jobs.wake();
            Ok((started, created))
        }

        pub(in crate::dispatch) fn lastfm_sign_in_start(&self) -> Result<SignInStarted, RpcError> {
            let lastfm = self.lastfm_or_refuse()?;
            let sign_in =
                ritornel_engine::new_id(self.clock.now_ms(), &*self.entropy).map_err(refused)?;
            let approval_url = lastfm.begin_sign_in(sign_in)?;
            Ok(SignInStarted {
                sign_in,
                approval_url,
            })
        }

        pub(in crate::dispatch) fn lastfm_sign_in_finish(
            &self,
            stores: &Stores,
            params: &SignInFinishParams,
        ) -> Result<SignInFinished, RpcError> {
            let lastfm = self.lastfm_or_refuse()?;
            let account = lastfm.finish_sign_in(params.sign_in, |account| {
                let _change = lastfm.hold();
                Self::library(stores, |library| {
                    keep_session(stores.server(), library, account).map_err(refused)
                })
                .map(|_| ())
            })?;
            // Signed in again to the same account, the plays waiting go.
            if account.is_some() {
                self.wake_scrobbler();
            }
            Ok(SignInFinished {
                receipt: receipt(
                    params.idempotency_key,
                    if account.is_some() {
                        ReceiptStatus::Done
                    } else {
                        ReceiptStatus::Accepted
                    },
                ),
                account,
            })
        }

        pub(in crate::dispatch) fn lastfm_sign_out(
            &self,
            stores: &Stores,
            params: &KeyParams,
        ) -> Result<SignedOut, RpcError> {
            let command = Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<LastfmSignOut>(params)?,
            };
            // Once this answers, nothing more is sent as the session ended.
            // Sent again after a newer sign-in, it signs nothing out.
            let change = self.lastfm().map(Lastfm::hold);
            let was_signed_in = stores
                .server()
                .change_once(
                    command,
                    LastfmSignOut::NAME,
                    SettingChange::SignOut {
                        service: SERVICE.to_owned(),
                    },
                )
                .map_err(refused)?;
            drop(change);
            Ok(SignedOut {
                receipt: receipt(params.idempotency_key, ReceiptStatus::Done),
                was_signed_in,
            })
        }

        pub(in crate::dispatch) fn lastfm_sync(
            &self,
            stores: &Stores,
            params: &LastfmSyncParams,
        ) -> Result<LastfmSyncStarted, RpcError> {
            self.lastfm_or_refuse()?.client()?;
            let session = Self::session(stores)?;
            // An imported play is dated in the zone the listener was in
            // that day, so the zones come first (history.md, "Last.fm").
            if !Self::library(stores, zones_set)? {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "say where you were first, so that each imported play falls on the right \
                     day: `ritornelctl history zone add <zone>`",
                )
                .with_detail("reason", "no_zones"));
            }
            let (started, _) = self.start_sync(stores, &session, params)?;
            Ok(LastfmSyncStarted {
                receipt: receipt(params.idempotency_key, ReceiptStatus::Accepted),
                job: job(&started),
            })
        }

        pub(in crate::dispatch) fn lastfm_set_scrobbling(
            &self,
            stores: &Stores,
            params: &ScrobblingParams,
        ) -> Result<ScrobblingSet, RpcError> {
            Self::session(stores)?;
            let command = Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<LastfmSetScrobbling>(params)?,
            };
            // Turned off, nothing more is sent once this answers. Sent again
            // after a newer change, it changes nothing, and answers with
            // scrobbling as it is.
            let change = self.lastfm().map(Lastfm::hold);
            stores
                .server()
                .change_once(
                    command,
                    LastfmSetScrobbling::NAME,
                    SettingChange::Scrobbling {
                        service: SERVICE.to_owned(),
                        on: params.on,
                    },
                )
                .map_err(refused)?;
            drop(change);
            self.wake_scrobbler();
            let session = stores.server().service_session(SERVICE).map_err(refused)?;
            Ok(ScrobblingSet {
                receipt: receipt(params.idempotency_key, ReceiptStatus::Done),
                scrobbling_since: session.and_then(|s| s.scrobble_from_ms),
            })
        }

        pub(in crate::dispatch) fn lastfm_report(
            &self,
            stores: &Stores,
            params: &LastfmReportParams,
        ) -> Result<LastfmReport, RpcError> {
            let session = Self::session(stores)?;
            let limit = params.limit.unwrap_or(50).min(MAX_DOUBTFUL);
            let report = Self::library(stores, |library| {
                library
                    .import_report(session.account, usize::try_from(limit).unwrap_or(50))
                    .map_err(refused)
            })?;
            Ok(LastfmReport {
                account: session.account_name,
                imported: unsigned(report.imported),
                plays: unsigned(report.plays),
                matched_plays: unsigned(report.matched_plays),
                echoes: unsigned(report.echoes),
                tracks: unsigned(report.tracks),
                matched_tracks: unsigned(report.matched_tracks),
                doubtful_tracks: unsigned(report.doubtful_tracks),
                doubtful_plays: unsigned(report.doubtful_plays),
                absent_tracks: unsigned(report.absent_tracks),
                absent_plays: unsigned(report.absent_plays),
                doubtful: report
                    .doubtful
                    .into_iter()
                    .map(|doubtful| DoubtfulTrack {
                        artist: doubtful.artist,
                        album: doubtful.album,
                        title: doubtful.title,
                        plays: unsigned(doubtful.plays),
                        doubt: doubtful.doubt.to_owned(),
                        candidates: doubtful
                            .candidates
                            .into_iter()
                            .map(|(title, artist)| NearlyMatched { title, artist })
                            .collect(),
                    })
                    .collect(),
                loved: unsigned(report.loved),
                loved_matched: unsigned(report.loved_matched),
                first: report.first_ms,
                last: report.last_ms,
            })
        }
    }
}

/// When the hub imports on its own, against a clock the test sets. No sync
/// runs: these look at which jobs are queued, and end them by hand.
#[cfg(all(test, feature = "lastfm"))]
mod tests {
    use std::sync::Arc;

    use ritornel_contract::{CONTRACT_VERSION, ErrorCode, Frame, Request, RequestId, SessionKind};
    use ritornel_engine::{JobState, OsEntropy, ServerStore, ServiceKey};
    use ritornel_lastfm::fake::{API_KEY, API_SECRET};
    use serde_json::{Value, json};

    use super::{Core, Stores};
    use crate::Bounds;
    use crate::data_dir::DataDir;
    use crate::lastfm::{Auto, Lastfm, LastfmRun, SERVICE, SYNC_JOB, import_tick};
    use crate::session::Session;
    use crate::testing::{Scratch, SetClock};

    /// 2026-11-16 12:00 UTC.
    const T0: i64 = 1_763_294_400_000;
    const MINUTE: i64 = 60_000;
    const HOUR: i64 = 60 * MINUTE;

    /// A server with Last.fm set up and nothing to reach: nothing here
    /// sends a request.
    fn server(clock: &Arc<SetClock>) -> (Core, Scratch) {
        let scratch = Scratch::new();
        let dir = DataDir::prepare(scratch.path().join("data")).unwrap();
        let store = ServerStore::open_in_memory(clock.clone(), Arc::new(OsEntropy)).unwrap();
        let (identity, issued) = store.ensure_identity("desk").unwrap();
        let (_, verifier) = crate::credential::ensure(&store, &dir.credential(), issued).unwrap();
        let core = Core::new(
            Stores::new(store, None),
            &identity,
            &dir,
            verifier,
            clock.clone(),
            Arc::new(OsEntropy),
            Bounds::default(),
        )
        .unwrap();
        core.set_lastfm(Lastfm::new(
            LastfmRun {
                credentials: Some(ritornel_lastfm::ApiCredentials::new(API_KEY, API_SECRET)),
                base_url: Some("http://127.0.0.1:9/2.0/".to_owned()),
                ..LastfmRun::default()
            },
            dir.root(),
        ));
        (core, scratch)
    }

    fn call(
        core: &Core,
        session: &Session,
        method: &str,
        params: Value,
    ) -> Result<Value, ErrorCode> {
        let request = Frame::Request(Request {
            id: RequestId::Number(1),
            method: method.to_owned(),
            params: Some(params),
        });
        let Some(Frame::Response(response)) = core.dispatch(session, request) else {
            panic!("a request gets a response");
        };
        response.outcome.map_err(|error| error.data.code)
    }

    fn hello(core: &Core, kind: &str) -> Arc<Session> {
        let session = core.open_session(SessionKind::Full).unwrap();
        let hello = json!({
            "name": "test", "kind": kind, "version": "1", "contractVersion": CONTRACT_VERSION
        });
        call(core, &session, "server.hello", hello).unwrap();
        session
    }

    fn key(byte: u8) -> Value {
        json!({ "idempotencyKey": ritornel_contract::Id::from_bytes([byte; 16]).to_string() })
    }

    /// Signs in as an invented account, as a sign-in Last.fm approved
    /// would.
    fn sign_in(core: &Core) {
        core.with_library(|server, library| {
            let account = library.ensure_service_account(SERVICE, "someone").unwrap();
            server
                .save_service_session(SERVICE, account, "someone", &ServiceKey::new("k"), false)
                .unwrap();
        })
        .unwrap();
    }

    fn set_zone(core: &Core, session: &Session) {
        let mut zones = key(0x20);
        zones["periods"] = json!([{ "zone": "Europe/Helsinki" }]);
        call(core, session, "history.setZones", zones).unwrap();
    }

    /// A hub, signed in, with a zone.
    fn hub(clock: &Arc<SetClock>) -> (Core, Scratch, Arc<Session>) {
        let (core, scratch) = server(clock);
        let cli = hello(&core, "cli");
        call(&core, &cli, "library.create", key(0xc0)).unwrap();
        sign_in(&core);
        set_zone(&core, &cli);
        (core, scratch, cli)
    }

    /// Looks as the Last.fm thread does when it wakes.
    fn tick(core: &Core, first: bool) -> Auto {
        import_tick(core, core.lastfm().unwrap(), first)
    }

    fn syncs(core: &Core) -> Vec<ritornel_engine::Job> {
        core.with_stores(|stores| stores.server().jobs(100).unwrap())
            .unwrap()
            .into_iter()
            .filter(|job| job.kind == SYNC_JOB)
            .collect()
    }

    /// Ends the sync that is queued, at the clock's time.
    fn end(core: &Core, state: JobState, failure: Option<&str>) {
        core.with_stores(|stores| {
            let job = stores
                .server()
                .active_job(SYNC_JOB, "someone")
                .unwrap()
                .unwrap();
            stores
                .server()
                .finish_job(job.id, state, failure.map(str::to_owned))
                .unwrap();
        })
        .unwrap();
    }

    fn schedule(core: &Core, session: &Session) -> Value {
        call(core, session, "lastfm.status", json!({})).unwrap()["import"].clone()
    }

    #[test]
    fn a_settings_command_sent_again_never_undoes_a_newer_one() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, cli) = hub(&clock);
        let scrobbling = |byte: u8, on: bool| {
            let mut params = key(byte);
            params["on"] = json!(on);
            call(&core, &cli, "lastfm.setScrobbling", params)
        };
        let on = || {
            call(&core, &cli, "lastfm.status", json!({})).unwrap()["scrobblingSince"].is_number()
        };
        // On with A, off with B, then A again, as a client retrying a reply
        // it lost would: it stays off.
        scrobbling(0xa1, true).unwrap();
        scrobbling(0xb1, false).unwrap();
        let again = scrobbling(0xa1, true).unwrap();
        assert!(!on());
        assert_eq!(again["scrobblingSince"], Value::Null);
        // The same key with other params is refused.
        assert_eq!(scrobbling(0xb1, true), Err(ErrorCode::IdempotencyMismatch));
        assert!(!on());

        // A sign-out sent again after a new sign-in signs nothing out.
        let signed_out = call(&core, &cli, "lastfm.signOut", key(0xa2)).unwrap();
        assert_eq!(signed_out["wasSignedIn"], true);
        sign_in(&core);
        let again = call(&core, &cli, "lastfm.signOut", key(0xa2)).unwrap();
        assert_eq!(again["wasSignedIn"], true);
        let status = call(&core, &cli, "lastfm.status", json!({})).unwrap();
        assert_eq!(status["account"], "someone");

        // Watching: off with A, on with B, then A again stays on.
        let watching = |byte: u8, on: bool| {
            let mut params = key(byte);
            params["on"] = json!(on);
            call(&core, &cli, "library.setWatching", params).unwrap()
        };
        watching(0xa3, false);
        watching(0xb3, true);
        watching(0xa3, false);
        assert!(
            core.with_stores(|stores| stores.server().watching().unwrap())
                .unwrap()
        );
    }

    #[test]
    fn the_hub_imports_when_it_starts_and_then_every_six_hours() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch) = server(&clock);
        let cli = hello(&core, "cli");
        call(&core, &cli, "library.create", key(0xc0)).unwrap();
        // Not signed in, then no zones: nothing starts.
        assert_eq!(tick(&core, true), Auto::Off("not signed in".to_owned()));
        sign_in(&core);
        assert!(matches!(tick(&core, true), Auto::Off(why) if why.starts_with("no zones")));
        let off = schedule(&core, &cli)["off"].as_str().unwrap().to_owned();
        assert!(off.starts_with("no zones"), "{off}");
        set_zone(&core, &cli);

        // The hub starts: an import is queued, and the next look finds it.
        let Auto::Started(started) = tick(&core, true) else {
            panic!("an import at the start");
        };
        assert_eq!(tick(&core, false), Auto::Running);
        assert_eq!(schedule(&core, &cli), json!({ "running": true }));
        let jobs = syncs(&core);
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            (jobs[0].public_id, jobs[0].state),
            (started, JobState::Queued)
        );
        assert!(
            jobs[0].params_json.contains("\"full\":false"),
            "{}",
            jobs[0].params_json
        );

        // It ends; the timer starts the next six hours after, and not
        // before.
        end(&core, JobState::Succeeded, None);
        assert_eq!(
            schedule(&core, &cli),
            json!({
                "last": { "ended": T0, "state": "succeeded" },
                "running": false,
                "next": T0 + 6 * HOUR,
                "onOpenAfter": T0 + HOUR,
            })
        );
        clock.set(T0 + 6 * HOUR - MINUTE);
        assert_eq!(tick(&core, false), Auto::NotDue);
        clock.set(T0 + 6 * HOUR);
        assert!(matches!(tick(&core, false), Auto::Started(_)));
        assert_eq!(syncs(&core).len(), 2);

        // A hub started again imports at once, however recent the last.
        end(&core, JobState::Succeeded, None);
        assert_eq!(tick(&core, false), Auto::NotDue);
        assert!(matches!(tick(&core, true), Auto::Started(_)));
    }

    #[test]
    fn opening_a_client_imports_once_the_last_import_is_an_hour_old() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, _) = hub(&clock);
        assert!(matches!(tick(&core, true), Auto::Started(_)));
        end(&core, JobState::Succeeded, None);

        // The app opens half an hour later: nothing.
        clock.set(T0 + 30 * MINUTE);
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), Auto::NotDue);
        // `ritornelctl` is no app opening, however late.
        clock.set(T0 + 2 * HOUR);
        hello(&core, "cli");
        assert_eq!(tick(&core, false), Auto::NotDue);
        assert_eq!(syncs(&core).len(), 1);
        // The app opens two hours later: an import.
        hello(&core, "desktop");
        assert!(matches!(tick(&core, false), Auto::Started(_)));
        assert_eq!(syncs(&core).len(), 2);
        // Opened again while it runs: the same one.
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), Auto::Running);
        assert_eq!(syncs(&core).len(), 2);
    }

    #[test]
    fn a_sync_by_hand_is_joined_and_counts_as_an_import() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, cli) = hub(&clock);
        let mut params = key(0x51);
        params["full"] = json!(false);
        let manual = call(&core, &cli, "lastfm.sync", params).unwrap();

        // Every trigger finds it queued and starts no other.
        assert_eq!(tick(&core, true), Auto::Running);
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), Auto::Running);
        let jobs = syncs(&core);
        assert_eq!(jobs.len(), 1);
        assert_eq!(manual["job"]["jobId"], jobs[0].public_id.to_string());

        // Once it has ended, it is the last import.
        end(&core, JobState::Succeeded, None);
        clock.set(T0 + 10 * MINUTE);
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), Auto::NotDue);
        assert_eq!(schedule(&core, &cli)["last"]["ended"], T0);
    }

    #[test]
    fn a_failure_waits_for_the_next_trigger_and_a_refused_session_stops_imports() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, cli) = hub(&clock);
        assert!(matches!(tick(&core, true), Auto::Started(_)));
        end(&core, JobState::Failed, Some("Last.fm: offline"));
        assert_eq!(
            schedule(&core, &cli)["last"],
            json!({ "ended": T0, "state": "failed", "failure": "Last.fm: offline" })
        );

        // Not tried again straight away, nor on an app opening within the
        // hour; tried again by the first trigger after it.
        clock.set(T0 + MINUTE);
        assert_eq!(tick(&core, false), Auto::NotDue);
        clock.set(T0 + 59 * MINUTE);
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), Auto::NotDue);
        clock.set(T0 + 61 * MINUTE);
        hello(&core, "desktop");
        assert!(matches!(tick(&core, false), Auto::Started(_)));
        end(&core, JobState::Failed, Some("Last.fm: offline"));

        // Last.fm refuses the session: nothing starts by itself, whatever
        // the trigger, until a new sign-in.
        core.with_stores(|stores| {
            stores
                .server()
                .refuse_service_session(SERVICE, "k")
                .unwrap()
        });
        let refused = Auto::Off("Last.fm refused the session: sign in again".to_owned());
        assert_eq!(tick(&core, true), refused);
        clock.set(T0 + 24 * HOUR);
        hello(&core, "desktop");
        assert_eq!(tick(&core, false), refused);
        assert_eq!(tick(&core, false), refused);
        let after = schedule(&core, &cli);
        assert_eq!(after["off"], "Last.fm refused the session: sign in again");
        assert_eq!(after.get("next"), None);
        assert_eq!(syncs(&core).len(), 2);
        sign_in(&core);
        assert!(matches!(tick(&core, false), Auto::Started(_)));
    }

    #[test]
    fn a_server_that_is_not_the_hub_never_imports() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch) = server(&clock);
        // A session in server.db, and no library: this server holds none.
        core.with_stores(|stores| {
            let account = ritornel_contract::Id::from_bytes([7; 16]);
            stores
                .server()
                .save_service_session(SERVICE, account, "someone", &ServiceKey::new("k"), false)
                .unwrap();
        })
        .unwrap();
        let off = Auto::Off("this server is not the library's hub".to_owned());
        assert_eq!(tick(&core, true), off);
        clock.set(T0 + 24 * HOUR);
        let desktop = hello(&core, "desktop");
        assert_eq!(tick(&core, false), off);
        assert_eq!(tick(&core, false), off);
        assert!(syncs(&core).is_empty());
        assert_eq!(
            schedule(&core, &desktop)["off"],
            "this server is not the library's hub"
        );
    }

    #[test]
    fn the_account_a_history_comes_from_is_no_music_folder() {
        let clock = Arc::new(SetClock::at(T0));
        let (core, _scratch, cli) = hub(&clock);
        let listed = call(&core, &cli, "sources.list", json!({})).unwrap();
        assert_eq!(listed["sources"], json!([]));
    }
}
