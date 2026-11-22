use std::collections::BTreeMap;
use std::time::Duration;

use ritornel_source::{
    Batch, Error, HistorySource, Love, LovesStored, Progress, Sink, Stored, TrackText,
};

use super::*;
use crate::fake::{API_KEY, API_SECRET, FakeLastfm, FakeScrobble};
use crate::{ApiCredentials, Lastfm, Settings};

/// The present, for every sync here.
const NOW: u64 = 1_800_000_000;

/// A sink that keeps what it is handed in memory, as the core would: a
/// record stored before writes nothing, and a batch with nothing new
/// writes nothing, its cursor included.
#[derive(Default)]
struct Memory {
    on_stop_check: Option<Box<dyn Fn() -> bool>>,
    records: BTreeMap<Vec<u8>, ImportRecord>,
    cursor: Option<String>,
    writes: usize,
    loves: Vec<Love>,
    /// Stop once this many batches with records have been handed over.
    stop_after: Option<usize>,
    batches: usize,
}

impl Sink for Memory {
    fn cursor(&mut self) -> Result<Option<String>, Error> {
        Ok(self.cursor.clone())
    }

    fn commit(&mut self, batch: Batch) -> Result<Stored, Error> {
        if !batch.records.is_empty() {
            self.batches += 1;
        }
        let read: std::collections::BTreeSet<Vec<u8>> = batch
            .records
            .iter()
            .map(|record| record.source_key.clone())
            .collect();
        let gone = self
            .records
            .values()
            .filter(|held| {
                held.played_at_ms >= batch.span.from_ms
                    && held.played_at_ms < batch.span.to_ms
                    && !read.contains(&held.source_key)
            })
            .count() as u64;
        let mut new = 0;
        for record in batch.records {
            if !self.records.contains_key(&record.source_key) {
                self.records.insert(record.source_key.clone(), record);
                new += 1;
            }
        }
        if new > 0 {
            self.cursor = Some(batch.cursor);
            self.writes += 1;
        }
        Ok(Stored { new, gone })
    }

    fn loves(&mut self, loves: Vec<Love>) -> Result<LovesStored, Error> {
        let loved = loves.len() as u64;
        self.loves = loves;
        Ok(LovesStored {
            loved,
            ..LovesStored::default()
        })
    }

    fn progress(&mut self, _: Progress) {}

    fn stopping(&self) -> bool {
        self.on_stop_check.as_ref().is_some_and(|check| check())
            || self.stop_after.is_some_and(|after| self.batches >= after)
    }
}

/// An invented history: `n` scrobbles, one every ten minutes up to an hour
/// before [`NOW`], over a few invented artists.
fn history(n: u64) -> Vec<FakeScrobble> {
    (0..n)
        .map(|i| {
            let artist = format!("Invented Artist {}", i % 7);
            let title = format!("Invented Song {}", i % 13);
            FakeScrobble::new(NOW - 3_600 - i * 600, &artist, "Invented Album", &title)
        })
        .collect()
}

fn lastfm(fake: &FakeLastfm, chunk: u64) -> Lastfm {
    Lastfm::new(
        ApiCredentials::new(API_KEY, API_SECRET),
        Settings {
            base_url: Some(fake.base_url()),
            min_interval: Duration::ZERO,
            chunk,
            page_size: 4,
            recent_s: 14 * 86_400,
        },
    )
    .unwrap()
}

#[test]
fn a_first_sync_reads_the_whole_history_in_checked_spans_and_a_second_writes_nothing() {
    let fake = FakeLastfm::start("someone", history(40));
    fake.set_loved(vec![("Invented Artist 1", "Invented Song 2", NOW - 9_000)]);
    let lastfm = lastfm(&fake, 9);
    let mut sink = Memory::default();
    let synced = lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, false)
        .unwrap();
    assert_eq!((synced.read, synced.new), (40, 40));
    assert_eq!(synced.source_total, Some(40));
    assert_eq!(sink.records.len(), 40);
    assert_eq!(sink.cursor.as_deref().and_then(read_cursor), Some(NOW));
    assert_eq!(sink.loves.len(), 1);
    assert_eq!(
        sink.loves[0].loved_at_ms,
        i64::try_from(NOW - 9_000).unwrap() * 1000
    );
    // Spans of at most 9: more than one, oldest first.
    let writes = sink.writes;
    assert!(writes >= 5, "{writes}");

    let before = fake.seen().len();
    let synced = lastfm
        .source("someone")
        .until(NOW + 60)
        .sync(&mut sink, false)
        .unwrap();
    assert_eq!(synced.new, 0);
    assert_eq!(
        sink.writes, writes,
        "a second sync with nothing new writes nothing"
    );
    // It read only the fortnight before where the first stopped.
    let from = fake
        .seen()
        .into_iter()
        .skip(before)
        .filter(|seen| seen.method == "user.getRecentTracks")
        .filter_map(|seen| seen.params.get("from").cloned())
        .map(|from| from.parse::<u64>().unwrap())
        .min();
    assert_eq!(from, Some(NOW - 14 * 86_400));
}

#[test]
fn a_sync_stopped_part_way_carries_on_from_the_last_span_it_stored() {
    let fake = FakeLastfm::start("someone", history(60));
    let lastfm = lastfm(&fake, 8);
    let mut sink = Memory {
        stop_after: Some(3),
        ..Memory::default()
    };
    let synced = lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, false)
        .unwrap();
    assert!(synced.stopped);
    let part = sink.records.len();
    assert!(part > 0 && part < 60, "{part}");
    sink.stop_after = None;
    let synced = lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, false)
        .unwrap();
    assert!(!synced.stopped);
    assert_eq!(sink.records.len(), 60);
    assert_eq!(synced.new as usize, 60 - part);
}

#[test]
fn identical_plays_in_one_second_are_two_records_with_keys_that_read_again_the_same() {
    let twice = vec![
        FakeScrobble::new(NOW - 100, "Invented Artist", "Invented Album", "Same Song"),
        FakeScrobble::new(NOW - 100, "Invented Artist", "Invented Album", "Same Song"),
        FakeScrobble::new(NOW - 100, "Invented Artist", "Invented Album", "Other Song"),
    ];
    let fake = FakeLastfm::start("someone", twice);
    let lastfm = lastfm(&fake, 100);
    let mut sink = Memory::default();
    lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, false)
        .unwrap();
    assert_eq!(sink.records.len(), 3);
    let mut again = Memory::default();
    lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut again, true)
        .unwrap();
    assert_eq!(
        sink.records.keys().collect::<Vec<_>>(),
        again.records.keys().collect::<Vec<_>>()
    );
    let record = sink.records.values().next().unwrap();
    assert_eq!(
        record.played_at_ms,
        i64::try_from(NOW - 100).unwrap() * 1000
    );
    assert_eq!(
        record.track,
        TrackText {
            artist: "Invented Artist".to_owned(),
            album: Some("Invented Album".to_owned()),
            title: record.track.title.clone(),
        }
    );
    let payload: serde_json::Value = serde_json::from_str(&record.payload_json).unwrap();
    assert_eq!(payload["date"]["uts"], (NOW - 100).to_string());
}

#[test]
fn a_key_tells_an_absent_album_from_an_empty_one() {
    let with = |album: Option<&str>| TrackText {
        artist: "A".to_owned(),
        album: album.map(str::to_owned),
        title: "T".to_owned(),
    };
    assert_ne!(
        source_key(1, &with(None), 0),
        source_key(1, &with(Some("")), 0)
    );
    assert_ne!(source_key(1, &with(None), 0), source_key(1, &with(None), 1));
    assert_eq!(source_key(1, &with(None), 0).len(), 32);
}

#[test]
fn a_stretch_emptied_at_last_fm_is_still_read_so_what_is_gone_is_seen() {
    // Twenty recent plays, and five in 2005, years before them.
    let old = |i: u64| {
        FakeScrobble::new(
            1_120_000_000 + i * 600,
            "Old Artist",
            "Old Album",
            "Old Song",
        )
    };
    let mut scrobbles = history(20);
    scrobbles.extend((0..5).map(old));
    let fake = FakeLastfm::start("someone", scrobbles);
    let lastfm = lastfm(&fake, 9);
    let mut sink = Memory::default();
    lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, false)
        .unwrap();
    assert_eq!(sink.records.len(), 25);
    // The five of 2005 are deleted at Last.fm: the stretch that held them
    // now holds nothing.
    for i in 0..5 {
        fake.remove_at(1_120_000_000 + i * 600);
    }
    let synced = lastfm
        .source("someone")
        .until(NOW)
        .sync(&mut sink, true)
        .unwrap();
    assert_eq!((synced.new, synced.gone), (0, 5));
    assert_eq!(sink.records.len(), 25, "nothing is removed");
}

#[test]
fn an_interrupted_rescan_keeps_the_previously_read_boundary() {
    for full in [false, true] {
        let fake = FakeLastfm::start("someone", history(15));
        let lastfm = lastfm(&fake, 5);
        let mut sink = Memory {
            cursor: Some(cursor(NOW)),
            stop_after: Some(1),
            ..Memory::default()
        };
        let synced = lastfm
            .source("someone")
            .until(NOW + 10)
            .sync(&mut sink, full)
            .unwrap();
        assert!(synced.stopped && synced.new > 0);
        assert_eq!(sink.cursor, Some(cursor(NOW)));
    }
}

#[test]
fn one_span_is_counted_once() {
    let fake = FakeLastfm::start("someone", history(2));
    lastfm(&fake, 10)
        .source("someone")
        .until(NOW)
        .sync(&mut Memory::default(), true)
        .unwrap();
    assert_eq!(
        fake.seen()
            .iter()
            .filter(|call| call.method == "user.getRecentTracks"
                && call.params.get("limit").is_some_and(|n| n == "1"))
            .count(),
        1
    );
}

#[test]
fn milliseconds_saturate_at_the_last_representable_instant() {
    let last = i64::MAX as u64 / 1000;
    assert_eq!(seconds_to_ms(last), last as i64 * 1000);
    assert_eq!(seconds_to_ms(last + 1), i64::MAX);
    assert_eq!(seconds_to_ms(u64::MAX), i64::MAX);
}

#[test]
fn cancelling_between_pages_discards_the_incomplete_span_or_loved_list() {
    for method in ["user.getRecentTracks", "user.getLovedTracks"] {
        let fake = std::sync::Arc::new(FakeLastfm::start("someone", history(12)));
        fake.set_loved(vec![("Invented", "Song", NOW); 1001]);
        let mut sink = Memory {
            on_stop_check: Some(Box::new({
                let fake = fake.clone();
                move || {
                    fake.seen().iter().any(|req| {
                        req.method == method && req.params.get("limit").is_some_and(|n| n != "1")
                    })
                }
            })),
            ..Memory::default()
        };
        let synced = lastfm(&fake, 20)
            .source("someone")
            .until(NOW)
            .sync(&mut sink, true)
            .unwrap();
        assert!(synced.stopped);
        assert!(synced.loves.is_none());
        assert_eq!(
            fake.seen()
                .iter()
                .filter(
                    |req| req.method == method && req.params.get("limit").is_some_and(|n| n != "1")
                )
                .count(),
            1
        );
        if method == "user.getRecentTracks" {
            assert!(sink.records.is_empty());
        }
    }
}

#[test]
fn a_partial_last_page_cannot_replace_loved_tracks() {
    assert!(loved_page_complete(1, 2, false, true).is_err());
    assert!(loved_page_complete(1, 2, true, false).is_err());
    assert!(loved_page_complete(2, 1, false, true).is_err());
    assert!(loved_page_complete(0, 0, true, true).unwrap());
    assert!(!loved_page_complete(1, 2, false, false).unwrap());
    assert!(loved_page_complete(2, 2, false, true).unwrap());
}

#[test]
fn an_unlove_between_pages_cannot_clear_a_different_track() {
    let fake = std::sync::Arc::new(FakeLastfm::start("someone", vec![]));
    let loved: Vec<String> = (0..1002).map(|i| format!("Invented Song {i}")).collect();
    fake.set_loved(
        loved
            .iter()
            .map(|t| ("Invented Artist", t.as_str(), NOW))
            .collect(),
    );
    let mut sink = Memory {
        loves: vec![Love {
            artist: "Invented Artist".into(),
            title: loved[1000].clone(),
            loved_at_ms: seconds_to_ms(NOW),
        }],
        on_stop_check: Some(Box::new({
            let fake = fake.clone();
            move || {
                if fake
                    .seen()
                    .iter()
                    .any(|req| req.method == "user.getLovedTracks")
                {
                    fake.set_loved(
                        loved
                            .iter()
                            .skip(1)
                            .map(|t| ("Invented Artist", t.as_str(), NOW))
                            .collect(),
                    );
                }
                false
            }
        })),
        ..Default::default()
    };
    let result = lastfm(&fake, 10)
        .source("someone")
        .until(NOW)
        .sync(&mut sink, true);
    assert!(result.is_err(), "{result:?}");
    assert_eq!(sink.loves.len(), 1);
    assert_eq!(sink.loves[0].title, "Invented Song 1000");
}
