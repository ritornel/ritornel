//! The history calls: recent listening, "on this day", and the rebuild.
//! See "Queries" and "Incremental updates and rebuilds" in
//! docs/internals/history.md.
//!
//! Direct history queries serve history reads without opening a list view
//! (see "Large lists" in docs/internals/contract.md).

use ritornel_contract::methods::{HistoryRebuild, HistorySetZones, Method};
use ritornel_contract::{
    ErrorCode, HeardAlbums, HeardArtists, HeardListParams, HistoryAlbumParams, HistoryArtistParams,
    HistoryOf, HistoryRebuildParams, HistoryRebuilt, HistoryRecentParams, HistoryStream, OnThisDay,
    OnThisDayParams, Play, PlayCursor, PlayOutcome, PlayedListen, PlayedTrack, Receipt,
    ReceiptStatus, RecentPlays, RpcError, SetZonesParams, TrackDetail, TrackGetParams,
    TrackHistory, TrackHistoryParams, TrackLove, ZonePeriod, ZonePeriods, ZonesSet,
};
use ritornel_engine::{
    Command, Id, LibraryStore, PlayRow, SystemTimekeeper, Timekeeper, Today, WriteRequest,
    ZonePeriodRow,
};

use super::playback::refused;
use super::{Core, Stores, fingerprint};

/// The most plays one call returns.
const MAX_PLAYS: u32 = 500;

fn unsigned(number: i64) -> u64 {
    u64::try_from(number).unwrap_or(0)
}

/// A local date as `YYYY-MM-DD`.
fn date(yyyymmdd: i64) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        yyyymmdd / 10_000,
        yyyymmdd / 100 % 100,
        yyyymmdd % 100
    )
}

/// A play as the contract carries it.
fn play(row: PlayRow) -> Play {
    Play {
        play_id: row.play_id,
        at: row.at_ms,
        local_date: row.local_date.map(date),
        local_hour: row.local_hour.and_then(|hour| u32::try_from(hour).ok()),
        zone: row.zone,
        utc_offset_minutes: row
            .offset_minutes
            .and_then(|minutes| i32::try_from(minutes).ok()),
        clock_basis: row.clock_basis,
        track: PlayedTrack {
            appearance_id: row.appearance,
            recording_id: row.recording,
            title: row.title,
            artist: row.artist,
            album: row.album,
        },
        ms_played: row.ms_played.map(unsigned),
        duration_ms: row.track_duration_ms.map(unsigned),
        outcome: match row.outcome.as_str() {
            "listened" => PlayOutcome::Listened,
            "skipped" => PlayOutcome::Skipped,
            "partial" => PlayOutcome::Partial,
            _ => PlayOutcome::Unknown,
        },
        counts: row.counts,
        excluded: row.excluded,
        listens: row
            .listens
            .into_iter()
            .map(|listen| PlayedListen {
                listen_id: listen.listen_id,
                started_at: listen.started_at_ms,
                ended_at: listen.ended_at_ms,
                start_reason: listen.start_reason,
                end_reason: listen.end_reason,
                start_position_ms: unsigned(listen.start_position_ms),
                end_position_ms: unsigned(listen.end_position_ms),
                ms_played: unsigned(listen.ms_played),
                continues: listen.continues,
                zone: listen.zone,
            })
            .collect(),
    }
}

/// Reads `YYYY-MM-DD` as YYYYMMDD.
fn full_date(text: &str) -> Option<i64> {
    let mut parts = text.splitn(3, '-');
    let (year, month, day): (i64, i64, i64) = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    ((1..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day))
        .then_some(year * 10_000 + month * 100 + day)
}

/// The day an instant falls on in a zone, as `YYYY-MM-DD`.
fn day_of(at_ms: i64, zone: &str) -> Option<String> {
    ritornel_engine::local_time(at_ms, zone).map(|local| date(local.date))
}

/// The days of each month in a leap year: every day some year has.
const DAYS_IN: [i64; 12] = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// Reads `MM-DD` as MMDD, if some year has that day.
fn month_day(day: &str) -> Option<i64> {
    let (month, day) = day.split_once('-')?;
    let (month, day): (i64, i64) = (month.parse().ok()?, day.parse().ok()?);
    let days = DAYS_IN.get(usize::try_from(month.checked_sub(1)?).ok()?)?;
    (1..=*days).contains(&day).then_some(month * 100 + day)
}

impl Core {
    /// The listener history is read for: the library's first, which is the
    /// one the player plays for.
    fn listener(library: &LibraryStore) -> Result<Id, RpcError> {
        library
            .default_listener()
            .map_err(refused)?
            .ok_or_else(|| RpcError::new(ErrorCode::Internal, "the library has no listener"))
    }

    pub(super) fn history_recent(
        &self,
        stores: &Stores,
        params: &HistoryRecentParams,
    ) -> Result<RecentPlays, RpcError> {
        let limit = params.limit.unwrap_or(20).clamp(1, MAX_PLAYS);
        let after = params.before.map(|cursor| ritornel_engine::PlayCursor {
            at_ms: cursor.at,
            play_id: cursor.play_id,
        });
        // Counted before the plays are read: a delivery commits to the
        // library before it leaves the outbox, so a closed listen not counted
        // here is in the plays. Counted after, one could be in neither.
        let undelivered = unsigned(stores.server().undelivered_listens().map_err(refused)?);
        let plays = Self::library(stores, |library| {
            library
                .recent_plays(Self::listener(library)?, limit, after)
                .map_err(refused)
        })?;
        let full = plays.len() == usize::try_from(limit).unwrap_or(usize::MAX);
        let next = plays.last().filter(|_| full).map(|last| {
            let cursor = last.cursor();
            PlayCursor {
                at: cursor.at_ms,
                play_id: cursor.play_id,
            }
        });
        Ok(RecentPlays {
            plays: plays.into_iter().map(play).collect(),
            next,
            undelivered,
        })
    }

    pub(super) fn history_on_this_day(
        &self,
        stores: &Stores,
        params: &OnThisDayParams,
    ) -> Result<OnThisDay, RpcError> {
        let wanted = match &params.day {
            Some(day) => month_day(day).ok_or_else(|| {
                RpcError::new(
                    ErrorCode::Invalid,
                    "a day is written MM-DD, and is a day some year has",
                )
                .with_detail("field", "day")
            })?,
            // Today, where this server is.
            None => {
                let zone = SystemTimekeeper.zone().unwrap_or_else(|| "UTC".to_owned());
                ritornel_engine::local_time(self.clock.now_ms(), &zone)
                    .map_or(101, |local| local.month_day)
            }
        };
        let plays = Self::library(stores, |library| {
            library
                .plays_on_this_day(Self::listener(library)?, wanted)
                .map_err(refused)
        })?;
        Ok(OnThisDay {
            day: format!("{:02}-{:02}", wanted / 100, wanted % 100),
            plays: plays.into_iter().map(play).collect(),
        })
    }

    /// Now, and the date it is where this server is: what a page's
    /// sentence and Lean back's facts are said against, and where History
    /// ends.
    fn today(&self) -> Today {
        let now_ms = self.clock.now_ms();
        let zone = SystemTimekeeper.zone().unwrap_or_else(|| "UTC".to_owned());
        let date =
            ritornel_engine::local_time(now_ms, &zone).map_or(19_700_101, |local| local.date);
        Today {
            now_ms,
            year: i32::try_from(date / 10_000).unwrap_or(1970),
            month: u32::try_from(date / 100 % 100).unwrap_or(1),
            date,
        }
    }

    pub(super) fn history_stream(&self, stores: &Stores) -> Result<HistoryStream, RpcError> {
        let today = self.today();
        let started = std::time::Instant::now();
        let stream = Self::library(stores, |library| {
            library
                .history_stream(Self::listener(library)?, today)
                .map_err(refused)
        })?;
        tracing::debug!(
            ms = started.elapsed().as_secs_f64() * 1000.0,
            months = stream.months,
            "read the history stream"
        );
        Ok(stream)
    }

    pub(super) fn history_artist(
        &self,
        stores: &Stores,
        params: &HistoryArtistParams,
    ) -> Result<HistoryOf, RpcError> {
        let today = self.today();
        Self::library(stores, |library| {
            let listener = Self::listener(library)?;
            let found = match (params.artist_id, &params.key) {
                (Some(artist), None) => library.artist_history(listener, artist, today),
                (None, Some(key)) => library.heard_artist_history(listener, key.clone(), today),
                _ => {
                    return Err(RpcError::new(
                        ErrorCode::Invalid,
                        "name the artist by its ID or by its key, and not both",
                    ));
                }
            };
            found
                .map_err(refused)?
                .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "no such artist was heard"))
        })
    }

    pub(super) fn history_album(
        &self,
        stores: &Stores,
        params: &HistoryAlbumParams,
    ) -> Result<HistoryOf, RpcError> {
        let today = self.today();
        Self::library(stores, |library| {
            let listener = Self::listener(library)?;
            let found = match (params.album, &params.artist_key, &params.key) {
                (Some(album), None, None) => library.album_history(listener, album, today),
                (None, Some(artist), Some(key)) => {
                    library.heard_album_history(listener, artist.clone(), key.clone(), today)
                }
                _ => {
                    return Err(RpcError::new(
                        ErrorCode::Invalid,
                        "name the album by its ref, or by its artist's key and its own",
                    ));
                }
            };
            found
                .map_err(refused)?
                .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "no such album was heard"))
        })
    }

    pub(super) fn history_artists(
        &self,
        stores: &Stores,
        params: &HeardListParams,
    ) -> Result<HeardArtists, RpcError> {
        Self::library(stores, |library| {
            library.heard_artists(*params).map_err(refused)
        })
    }

    pub(super) fn history_albums(
        &self,
        stores: &Stores,
        params: &HeardListParams,
    ) -> Result<HeardAlbums, RpcError> {
        Self::library(stores, |library| {
            library.heard_albums(*params).map_err(refused)
        })
    }

    pub(super) fn history_track(
        &self,
        stores: &Stores,
        params: &TrackHistoryParams,
    ) -> Result<TrackHistory, RpcError> {
        let today = self.today();
        Self::library(stores, |library| {
            library
                .track_history(Self::listener(library)?, params.appearance_id, today)
                .map_err(refused)?
                .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "the library has no such track"))
        })
    }

    pub(super) fn history_zones(&self, stores: &Stores) -> Result<ZonePeriods, RpcError> {
        let periods = Self::library(stores, |library| {
            library
                .zone_periods_of(Self::listener(library)?)
                .map_err(refused)
        })?;
        Ok(ZonePeriods {
            periods: periods
                .into_iter()
                .map(|period| ZonePeriod {
                    from: period.from_ms.and_then(|at| day_of(at, &period.zone)),
                    until: period.to_ms.and_then(|at| day_of(at, &period.zone)),
                    zone: period.zone,
                })
                .collect(),
        })
    }

    pub(super) fn history_set_zones(
        &self,
        stores: &Stores,
        params: &SetZonesParams,
    ) -> Result<ZonesSet, RpcError> {
        let invalid = |message: &str| {
            RpcError::new(ErrorCode::Invalid, message).with_detail("field", "periods")
        };
        let mut periods = Vec::with_capacity(params.periods.len());
        for period in &params.periods {
            if !ritornel_engine::known_zone(&period.zone) {
                return Err(invalid(
                    "a zone is an IANA name the zone database knows, such as Europe/London",
                ));
            }
            let bound = |day: &Option<String>| -> Result<Option<i64>, RpcError> {
                day.as_deref()
                    .map(|day| {
                        full_date(day)
                            .and_then(|day| ritornel_engine::start_of_day_ms(day, &period.zone))
                            .ok_or_else(|| invalid("a day is written YYYY-MM-DD"))
                    })
                    .transpose()
            };
            periods.push(ZonePeriodRow {
                zone: period.zone.clone(),
                from_ms: bound(&period.from)?,
                to_ms: bound(&period.until)?,
            });
        }
        let key = params.idempotency_key;
        let request = WriteRequest {
            method: HistorySetZones::NAME,
            command: Some(Command {
                id: key,
                fingerprint: fingerprint::<HistorySetZones>(params)?,
            }),
            client: self.client(),
        };
        let answer = Self::library(stores, |library| {
            let listener = Self::listener(library)?;
            library
                .set_zone_periods(request, listener, periods, |changed| changed.to_string())
                .map_err(refused)
        })?;
        Ok(ZonesSet {
            receipt: Receipt {
                command_id: key,
                status: ReceiptStatus::Done,
                revision: None,
            },
            changed: answer.parse().unwrap_or(0),
        })
    }

    pub(super) fn tracks_get(
        &self,
        stores: &Stores,
        params: &TrackGetParams,
    ) -> Result<TrackDetail, RpcError> {
        let found = Self::library(stores, |library| {
            library.track_detail(params.track_id).map_err(refused)
        })?
        .ok_or_else(|| RpcError::new(ErrorCode::NotFound, "the library has no such track"))?;
        Ok(TrackDetail {
            track_id: found.appearance,
            recording_id: found.recording,
            title: found.title,
            artist: found.artist,
            album: found.album,
            plays: unsigned(found.plays),
            last_played: found.last_played_ms,
            loves: found
                .loves
                .into_iter()
                .map(|love| TrackLove {
                    by: love.by,
                    at: love.at_ms,
                })
                .collect(),
        })
    }

    pub(super) fn history_rebuild(
        &self,
        stores: &Stores,
        params: &HistoryRebuildParams,
    ) -> Result<HistoryRebuilt, RpcError> {
        let key = params.idempotency_key;
        let dry_run = params.dry_run;
        let request = WriteRequest {
            method: HistoryRebuild::NAME,
            command: Some(Command {
                id: key,
                fingerprint: fingerprint::<HistoryRebuild>(params)?,
            }),
            client: self.client(),
        };
        let answer = Self::library(stores, |library| {
            library
                .rebuild_history(request, dry_run, move |rebuilt| {
                    let answer = HistoryRebuilt {
                        receipt: Receipt {
                            command_id: key,
                            status: ReceiptStatus::Done,
                            revision: rebuilt.revision.map(unsigned),
                        },
                        dry_run,
                        before: unsigned(rebuilt.before),
                        after: unsigned(rebuilt.after),
                        changed: unsigned(rebuilt.changed),
                    };
                    // These few fields always write.
                    serde_json::to_string(&answer).unwrap_or_default()
                })
                .map_err(refused)
        })?;
        serde_json::from_str(&answer).map_err(|error| {
            RpcError::new(
                ErrorCode::Internal,
                format!(
                    "the stored result of {} could not be read: {error}",
                    HistoryRebuild::NAME
                ),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_month_and_day_and_a_date_is_written_in_full() {
        assert_eq!(month_day("11-15"), Some(1115));
        assert_eq!(month_day("1-5"), Some(105));
        assert_eq!(month_day("02-29"), Some(229));
        assert_eq!(month_day("12-31"), Some(1231));
        for wrong in [
            "13-01", "00-10", "11-32", "02-30", "02-31", "04-31", "11-31", "1115", "11-", "nov-15",
        ] {
            assert_eq!(month_day(wrong), None, "{wrong}");
        }
        assert_eq!(date(20_261_115), "2026-11-15");
    }

    #[test]
    fn a_zone_boundary_on_a_day_its_zone_skipped_is_where_that_day_would_begin() {
        // Samoa skipped 30 December 2011, moving across the date line. A
        // period starting that day starts when the 29th ends, which is when
        // the 31st begins, and is read back as the 31st.
        let start = |date| ritornel_engine::start_of_day_ms(date, "Pacific/Apia").unwrap();
        let skipped = start(20_111_230);
        assert_eq!(skipped, start(20_111_231));
        assert_eq!(skipped - start(20_111_229), 86_400_000);
        assert_eq!(
            day_of(skipped, "Pacific/Apia").as_deref(),
            Some("2011-12-31")
        );
        // A day no year has is refused.
        assert_eq!(
            full_date("2026-02-31").and_then(|day| ritornel_engine::start_of_day_ms(day, "UTC")),
            None
        );
    }
}
