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
            deliveries: Vec::new(),
            imported: 0,
            zones_set: false,
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
    use ritornel_contract::methods::{LastfmSync, Method};
    use ritornel_contract::{DeliveryCount, DoubtfulTrack, NearlyMatched, Receipt, ReceiptStatus};
    use ritornel_engine::{Command, NewJob, ServiceSession};

    use super::*;
    use crate::dispatch::calls::job;
    use crate::dispatch::fingerprint;
    use crate::dispatch::playback::refused;
    use crate::lastfm::{SERVICE, SYNC_JOB, keep_session};

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
            let (deliveries, imported, zones_set) = Self::library(stores, |library| {
                let zones_set = match library.default_listener().map_err(refused)? {
                    Some(listener) => !library
                        .zone_periods_of(listener)
                        .map_err(refused)?
                        .is_empty(),
                    None => false,
                };
                let Some(session) = &session else {
                    return Ok((Vec::new(), 0, zones_set));
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
                ))
            })
            .unwrap_or_default();
            Ok(LastfmStatus {
                available: true,
                configured: lastfm.configured(),
                account: session.as_ref().map(|s| s.account_name.clone()),
                sign_in_again: session.as_ref().is_some_and(|s| s.sign_in_again),
                scrobbling_since: session.as_ref().and_then(|s| s.scrobble_from_ms),
                deliveries,
                imported,
                zones_set,
            })
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
                Self::library(stores, |library| {
                    keep_session(stores.server(), library, account).map_err(refused)
                })
                .map(|_| ())
            })?;
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
            let was_signed_in = stores
                .server()
                .remove_service_session(SERVICE)
                .map_err(refused)?;
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
            let zones_set = Self::library(stores, |library| {
                let listener = library
                    .default_listener()
                    .map_err(refused)?
                    .ok_or_else(|| {
                        RpcError::new(ErrorCode::Internal, "the library has no listener")
                    })?;
                Ok(!library
                    .zone_periods_of(listener)
                    .map_err(refused)?
                    .is_empty())
            })?;
            if !zones_set {
                return Err(RpcError::new(
                    ErrorCode::Invalid,
                    "say where you were first, so that each imported play falls on the right \
                     day: `ritornelctl history zone add <zone>`",
                )
                .with_detail("reason", "no_zones"));
            }
            let command = Command {
                id: params.idempotency_key,
                fingerprint: fingerprint::<LastfmSync>(params)?,
            };
            let new = NewJob {
                kind: SYNC_JOB,
                dedupe_key: session.account_name.to_lowercase(),
                target: session.account,
                params_json: serde_json::to_string(params).unwrap_or_default(),
                session: self.local_credential.session(),
            };
            let (started, created) = stores
                .server()
                .start_job(command, LastfmSync::NAME, new)
                .map_err(refused)?;
            if created {
                self.publish_job(&started);
            }
            self.jobs.wake();
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
            let session = stores
                .server()
                .set_scrobbling(SERVICE, params.on)
                .map_err(refused)?;
            self.wake_scrobbler();
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
