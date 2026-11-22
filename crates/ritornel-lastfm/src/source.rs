//! A Last.fm history as a [`HistorySource`]: which spans of it to read,
//! read each as one checked scan, and hand it to the core with a cursor.
//!
//! A sync works on spans of time, `[from, to)` in seconds, oldest first,
//! from a fortnight before where the last sync stopped up to now. It asks
//! Last.fm how many scrobbles a span holds; one that holds more than
//! [`Settings::chunk`](crate::Settings) is halved, and any other, an empty
//! one included, is read as one scan, which `scrobl` checks page by page
//! and as a whole, and is handed to the core with the span's end as
//! the cursor. The core stores the span and the cursor in one transaction,
//! and a scrobble it holds already writes nothing, so a sync that is
//! stopped anywhere carries on from the last span it stored, and a sync
//! with nothing new writes nothing.
//!
//! A scrobble has no ID at Last.fm. Its source key is its time, a hash of
//! the artist, album and title it names, and its place among identical
//! scrobbles of the same second, so two plays of one track that Last.fm
//! dates to the same second stay two (docs/design/history.md, "Last.fm").
//! A span is always whole seconds, so every scrobble of one second is read
//! in the same span, and that place is the same whenever it is read. A
//! second whose scrobbles fall on both sides of a page boundary is read
//! again on its own, in one page, and that read is the one kept: Last.fm's
//! order among scrobbles of one second can change between two pages, which
//! would show one of them twice and another not at all.

use std::collections::{HashMap, HashSet};

use ritornel_source::{
    Batch, Error, HistorySource, Identifiers, ImportRecord, Love, Progress, Sink, Span, Synced,
    TrackText,
};
use scrobl::history::Window;
use scrobl::model::Scrobble;
use scrobl::{Client, ErrorKind};
use sha2::{Digest, Sha256};

use crate::Lastfm;

/// How many times a span is read before a sync gives up on it: a scrobble
/// that arrives while a span is read can make its pages disagree.
const ATTEMPTS: u32 = 3;

/// The rows asked for on each page of loved tracks: the most Last.fm gives.
const LOVED_PAGE: u32 = 1_000;

/// The rows asked for when one second is read again on its own: the most
/// Last.fm gives of recent tracks.
const SECOND_PAGE: u32 = 200;

/// The version of a cursor's shape.
const CURSOR_VERSION: u32 = 1;

/// One Last.fm account's history, read through [`Lastfm`].
#[derive(Debug)]
pub struct LastfmSource<'a> {
    lastfm: &'a Lastfm,
    account: String,
    now_s: Option<u64>,
}

impl<'a> LastfmSource<'a> {
    pub(crate) fn new(lastfm: &'a Lastfm, account: &str) -> Self {
        Self {
            lastfm,
            account: account.to_owned(),
            now_s: None,
        }
    }

    /// Reads up to `now_s` rather than up to the system clock's present.
    #[must_use]
    pub fn until(mut self, now_s: u64) -> Self {
        self.now_s = Some(now_s);
        self
    }
}

/// The cursor a batch leaves: everything before `read_before`, in seconds,
/// has been read.
fn cursor(read_before: u64) -> String {
    format!("{{\"v\":{CURSOR_VERSION},\"readBefore\":{read_before}}}")
}

/// Reads a cursor this crate wrote. One it cannot read is no cursor: the
/// sync then reads everything, which stores nothing twice.
fn read_cursor(text: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    if value.get("v")?.as_u64()? != u64::from(CURSOR_VERSION) {
        return None;
    }
    value.get("readBefore")?.as_u64()
}

/// `[from, to)` as `scrobl` takes it. A lower bound of 0 is sent as none,
/// which means the same and is the plainer request.
fn window(from: u64, to: u64) -> Result<Window, scrobl::Error> {
    if from == 0 {
        Ok(Window::before(to))
    } else {
        Window::new(from, to)
    }
}

fn failed(error: &scrobl::Error) -> Error {
    Error::source(format!("Last.fm: {error}"))
}

fn seconds_to_ms(seconds: u64) -> i64 {
    i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX)
}

/// What a record is of, as the key hashes it: an album that is absent and
/// one that is empty are told apart.
fn subject_bytes(track: &TrackText) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(track.artist.as_bytes());
    bytes.push(0);
    match &track.album {
        Some(album) => {
            bytes.push(1);
            bytes.extend_from_slice(album.as_bytes());
        }
        None => bytes.push(0),
    }
    bytes.push(0);
    bytes.extend_from_slice(track.title.as_bytes());
    bytes
}

/// A scrobble's source key: `lfm1`, its time in seconds, the first 16
/// bytes of the SHA-256 of what it names, and its place among identical
/// scrobbles of that second, from 0.
pub(crate) fn source_key(timestamp: u64, track: &TrackText, ordinal: u32) -> Vec<u8> {
    let digest = Sha256::digest(subject_bytes(track));
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(b"lfm1");
    key.extend_from_slice(&timestamp.to_be_bytes());
    key.extend_from_slice(digest.get(..16).unwrap_or_default());
    key.extend_from_slice(&ordinal.to_be_bytes());
    key
}

/// Text that is empty is none.
fn some(text: Option<&str>) -> Option<String> {
    text.filter(|text| !text.is_empty()).map(str::to_owned)
}

/// The records of a span's scrobbles, newest first as Last.fm gives them.
fn records(scrobbles: &[Scrobble]) -> Vec<ImportRecord> {
    let mut seen: HashMap<(u64, TrackText), u32> = HashMap::new();
    scrobbles
        .iter()
        .map(|scrobble| {
            let track = scrobble.track();
            let text = TrackText {
                artist: track.artist().name().to_owned(),
                album: track.album().map(|album| album.name().to_owned()),
                title: track.name().to_owned(),
            };
            let place = seen
                .entry((scrobble.timestamp(), text.clone()))
                .or_insert(0);
            let ordinal = *place;
            *place += 1;
            ImportRecord {
                source_key: source_key(scrobble.timestamp(), &text, ordinal),
                played_at_ms: seconds_to_ms(scrobble.timestamp()),
                ids: Identifiers {
                    artist_mbid: some(track.artist().mbid()),
                    album_mbid: some(track.album().and_then(|album| album.mbid())),
                    track_mbid: some(track.mbid()),
                },
                track: text,
                payload_json: scrobble.json().to_string(),
            }
        })
        .collect()
}

/// How many scrobbles Last.fm counts in `[from, to)`: one request. The
/// count only decides where to look; what is stored comes from a scan.
async fn count(client: &Client, user: &str, from: u64, to: u64) -> Result<u64, scrobl::Error> {
    let page = client
        .user(user)
        .recent_tracks()
        .window(window(from, to)?)
        .limit(1)
        .send()
        .await?;
    Ok(page.total())
}

/// Reads `[from, to)` whole, as one scan, again while its pages disagree,
/// [`ATTEMPTS`] times in all. Each second that falls across a page boundary
/// is then read again on its own.
async fn read(
    client: &Client,
    user: &str,
    from: u64,
    to: u64,
    page_size: u32,
    sink: &dyn Sink,
) -> Result<Option<Vec<Scrobble>>, Error> {
    let mut attempt = 1;
    let (mut scrobbles, split) = loop {
        match scan(client, user, from, to, page_size, sink).await {
            Err(error) if error.kind() == ErrorKind::Inconsistent && attempt < ATTEMPTS => {
                attempt += 1;
            }
            Err(error) => return Err(failed(&error)),
            Ok(None) => return Ok(None),
            Ok(Some(read)) => break read,
        }
    };
    for second in split {
        let Some((again, still_split)) = scan(client, user, second, second + 1, SECOND_PAGE, sink)
            .await
            .map_err(|error| failed(&error))?
        else {
            return Ok(None);
        };
        if !still_split.is_empty() {
            return Err(Error::source(format!(
                "Last.fm has more scrobbles at {second} than one page holds"
            )));
        }
        // Newest first: the second's scrobbles go where they were.
        scrobbles.retain(|scrobble| scrobble.timestamp() != second);
        let at = scrobbles
            .iter()
            .position(|scrobble| scrobble.timestamp() < second)
            .unwrap_or(scrobbles.len());
        scrobbles.splice(at..at, again);
    }
    Ok(Some(scrobbles))
}

/// One scan of `[from, to)`, and the seconds whose scrobbles fall on both
/// sides of a boundary between its pages. Its pages are provisional until
/// it has finished, so nothing of it leaves here before that.
async fn scan(
    client: &Client,
    user: &str,
    from: u64,
    to: u64,
    page_size: u32,
    sink: &dyn Sink,
) -> Result<Option<(Vec<Scrobble>, Vec<u64>)>, scrobl::Error> {
    let mut scan = client
        .user(user)
        .recent_tracks()
        .window(window(from, to)?)
        .limit(page_size)
        .scan()?;
    let mut scrobbles: Vec<Scrobble> = Vec::new();
    let mut split = Vec::new();
    loop {
        if sink.stopping() {
            return Ok(None);
        }
        let Some(page) = scan.next_page().await? else {
            break;
        };
        let page = page.into_value().into_scrobbles();
        if let (Some(last), Some(first)) = (scrobbles.last(), page.first())
            && last.timestamp() == first.timestamp()
            && split.last() != Some(&first.timestamp())
        {
            split.push(first.timestamp());
        }
        scrobbles.extend(page);
    }
    scan.finish()?;
    Ok(Some((scrobbles, split)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct LovedPaging {
    total: u64,
    pages: u32,
    per_page: u32,
}

impl LovedPaging {
    /// Can the rows collected through this page replace the saved list?
    fn complete(self, page: u32, rows: usize, read: u64) -> Result<bool, Error> {
        let expected = self.total.min(u64::from(page) * u64::from(self.per_page));
        if read != expected {
            return Err(Error::source(format!(
                "Last.fm's loved-track page {page} accounts for {read} tracks, expected {expected}"
            )));
        }
        let last = page >= self.pages;
        if read == self.total && last && (rows != 0 || self.total == 0) {
            return Ok(true);
        }
        if rows == 0 || last || read >= self.total {
            return Err(Error::source(
                "Last.fm's loved-track pages disagree with their total",
            ));
        }
        Ok(false)
    }
}

/// Read the loved list before replacing it. Inconsistent paging or a
/// repeated track can mean another track slipped across a page boundary.
async fn loved(client: &Client, user: &str, sink: &dyn Sink) -> Result<Option<Vec<Love>>, Error> {
    let mut loves = Vec::new();
    let mut first_paging = None;
    let mut page_number = 1;
    loop {
        if sink.stopping() {
            return Ok(None);
        }
        let page = client
            .user(user)
            .loved_tracks()
            .limit(LOVED_PAGE)
            .page(page_number)
            .send()
            .await
            .map_err(|error| failed(&error))?;
        if page.number() != page_number {
            return Err(Error::source(
                "Last.fm returned a different loved-track page than requested",
            ));
        }
        // Another account's list would replace this one's.
        if page
            .subject()
            .is_some_and(|subject| !subject.eq_ignore_ascii_case(user))
        {
            return Err(Error::source(
                "Last.fm returned another account's loved tracks than the one asked for",
            ));
        }
        let paging = LovedPaging {
            total: page.total(),
            pages: page.total_pages(),
            per_page: page.per_page(),
        };
        let expected = *first_paging.get_or_insert(paging);
        if paging != expected {
            return Err(Error::source(format!(
                "Last.fm's loved-track list changed on page {page_number}: the total, page count or page size differs from the first page"
            )));
        }
        let rows = page.len();
        loves.extend(page.items().iter().map(|row| Love {
            artist: row.track().artist().name().to_owned(),
            title: row.track().name().to_owned(),
            loved_at_ms: seconds_to_ms(row.loved_at()),
        }));
        if paging.complete(page_number, rows, loves.len() as u64)? {
            let mut seen = HashSet::new();
            if !loves
                .iter()
                .all(|row| seen.insert((&row.artist, &row.title)))
            {
                return Err(Error::source(
                    "Last.fm repeated a loved track while reading the list",
                ));
            }
            return Ok(Some(loves));
        }
        page_number += 1;
    }
}

impl LastfmSource<'_> {
    async fn run(&self, sink: &mut dyn Sink, full: bool) -> Result<Synced, Error> {
        let client = &self.lastfm.client;
        let settings = &self.lastfm.settings;
        let user = self.account.as_str();
        let now = self.now_s.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs())
        });
        let previous_cursor = sink.cursor()?.as_deref().and_then(read_cursor);
        let start = match previous_cursor {
            Some(read_before) if !full => read_before.min(now).saturating_sub(settings.recent_s),
            _ => 0,
        };
        if sink.stopping() {
            return Ok(Synced {
                stopped: true,
                ..Synced::default()
            });
        }
        let mut synced = Synced {
            source_total: Some(count(client, user, 0, now).await.map_err(|e| failed(&e))?),
            ..Synced::default()
        };
        let to_read = if start == 0 {
            synced.source_total
        } else {
            if sink.stopping() {
                synced.stopped = true;
                return Ok(synced);
            }
            Some(
                count(client, user, start, now)
                    .await
                    .map_err(|e| failed(&e))?,
            )
        };
        sink.progress(Progress {
            read: 0,
            total: to_read,
            label: Some("reading the history".to_owned()),
        });

        // The last span pushed is the next one read, so the history is read
        // oldest first and each span's end is a cursor that holds.
        let mut spans = vec![(start, now, to_read)];
        while let Some((from, to, counted)) = spans.pop() {
            if from >= to {
                continue;
            }
            if sink.stopping() {
                synced.stopped = true;
                return Ok(synced);
            }
            let theirs = match counted {
                Some(total) => total,
                None => count(client, user, from, to)
                    .await
                    .map_err(|e| failed(&e))?,
            };
            // A span Last.fm counts empty is still read, as one checked
            // scan of a single page, and handed over: what the core holds
            // there and Last.fm no longer has is what has gone.
            if theirs > settings.chunk && to - from > 1 {
                let middle = from + (to - from) / 2;
                spans.push((middle, to, None));
                spans.push((from, middle, None));
                continue;
            }
            let Some(scrobbles) = read(client, user, from, to, settings.page_size, sink).await?
            else {
                synced.stopped = true;
                return Ok(synced);
            };
            let records = records(&scrobbles);
            synced.read += records.len() as u64;
            let stored = sink.commit(Batch {
                records,
                span: Span {
                    from_ms: seconds_to_ms(from),
                    to_ms: seconds_to_ms(to),
                },
                cursor: cursor(to.max(previous_cursor.unwrap_or(0))),
            })?;
            synced.new += stored.new;
            synced.gone += stored.gone;
            sink.progress(Progress {
                read: synced.read,
                total: to_read,
                label: Some("reading the history".to_owned()),
            });
        }

        if sink.stopping() {
            synced.stopped = true;
            return Ok(synced);
        }
        sink.progress(Progress {
            read: synced.read,
            total: to_read,
            label: Some("reading the loved tracks".to_owned()),
        });
        let Some(loves) = loved(client, user, sink).await? else {
            synced.stopped = true;
            return Ok(synced);
        };
        if sink.stopping() {
            synced.stopped = true;
            return Ok(synced);
        }
        synced.loves = Some(sink.loves(loves)?);
        Ok(synced)
    }
}

impl HistorySource for LastfmSource<'_> {
    fn sync(&self, sink: &mut dyn Sink, full: bool) -> Result<Synced, Error> {
        self.lastfm.block(self.run(sink, full))
    }
}

#[cfg(test)]
mod tests;
