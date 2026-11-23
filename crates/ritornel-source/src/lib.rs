//! The traits a plugin implements, and what it hands the core through them.
//! See "Traits and storage boundary" in docs/internals/plugins.md.
//!
//! There is one so far, [`HistorySource`]: something that brings plays in
//! from elsewhere, under its own history endpoint, through the one ingest
//! call. Last.fm (`ritornel-lastfm`) is the first. The metadata-source
//! trait joins it with the first lookup.
//!
//! A plugin reads its format and hands over each record as it was read with
//! the facts it took from it. The core stores both, so a rebuild never
//! needs the plugin. A plugin never writes anything itself: the core gives
//! it a [`Sink`], which writes what it is handed in one transaction with
//! the cursor that says how far the source has read.
//!
//! The trait is not stable: "Traits and storage boundary" in
//! docs/internals/plugins.md calls none stable until two plugins have used it.

use std::fmt;

/// What a source says a play was of, as text. It is all a source with no
/// files can say, and it is what a play of it is matched to the library by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TrackText {
    /// The artist, as the source spells it.
    pub artist: String,
    /// The album, when the source names one.
    pub album: Option<String>,
    /// The title.
    pub title: String,
}

/// The identifiers a source attaches to a record. They support a match and
/// never make one alone ("Scrobble matching" in docs/internals/identity.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identifiers {
    /// The artist's MusicBrainz ID, as the source gives it.
    pub artist_mbid: Option<String>,
    /// The album's.
    pub album_mbid: Option<String>,
    /// The track's.
    pub track_mbid: Option<String>,
}

/// One play a history source read: the record as it came, and the facts
/// taken from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRecord {
    /// Its key at its source: unique there, made from what the record says
    /// and never from a clock of ours, so that reading it again gives the
    /// same key.
    pub source_key: Vec<u8>,
    /// When it was played, as the source dates it, in Unix milliseconds
    /// UTC.
    pub played_at_ms: i64,
    /// What it was of.
    pub track: TrackText,
    /// What the source identified it by.
    pub ids: Identifiers,
    /// The record exactly as it was read.
    pub payload_json: String,
}

/// A stretch of time a source read whole: every record it holds from
/// `from_ms`, included, to `to_ms`, not included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// The start, in Unix milliseconds UTC.
    pub from_ms: i64,
    /// The end, not included.
    pub to_ms: i64,
}

/// What a source hands the core to store at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    /// Every record the source holds in `span`.
    pub records: Vec<ImportRecord>,
    /// The stretch they are all of.
    pub span: Span,
    /// How far the source has read once this batch is stored, in a form
    /// only the source reads. It is stored with the records, in the same
    /// transaction, and handed back by [`Sink::cursor`].
    pub cursor: String,
}

/// What became of a batch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stored {
    /// Records that were new, and are now stored.
    pub new: u64,
    /// Records stored before for the span that the source no longer
    /// holds. They stay: a record is never removed.
    pub gone: u64,
    /// Of those, the ones a full read found an earlier read stored twice,
    /// in place of another record of the same second it missed. Each is
    /// left out of the plays, and stays. None on a read that is not full.
    pub set_aside: u64,
}

/// A track the listener marked as loved at a source, with when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Love {
    /// The artist, as the source spells it.
    pub artist: String,
    /// The title.
    pub title: String,
    /// When it was loved, in Unix milliseconds UTC.
    pub loved_at_ms: i64,
}

/// What became of the loved tracks, as the list stands now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LovesStored {
    /// Tracks in the list.
    pub loved: u64,
    /// Loves that were not stored before.
    pub new: u64,
    /// Loves stored before that the list no longer has, now cleared.
    pub cleared: u64,
}

/// How far a sync has got, for whoever is watching.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Records read so far.
    pub read: u64,
    /// Records the source says there are to read, once it knows.
    pub total: Option<u64>,
    /// What it is doing.
    pub label: Option<String>,
}

/// What a sync did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Synced {
    /// Records read.
    pub read: u64,
    /// Of those, the ones that were new.
    pub new: u64,
    /// Records stored before that the source no longer holds.
    pub gone: u64,
    /// How many records the source holds in all, when it said.
    pub source_total: Option<u64>,
    /// The loved tracks, when the source has them.
    pub loves: Option<LovesStored>,
    /// Whether it stopped early because it was asked to. What it stored
    /// stays, and the next sync carries on from there.
    pub stopped: bool,
}

/// Where a source writes, and what it asks of the core.
pub trait Sink {
    /// The cursor the last stored batch left, if any batch is stored.
    fn cursor(&mut self) -> Result<Option<String>, Error>;
    /// Stores a batch and its cursor, in one transaction. A record stored
    /// before writes nothing, and a batch with nothing new moves no cursor
    /// and writes nothing, but what the core sets aside on a full read.
    fn commit(&mut self, batch: Batch) -> Result<Stored, Error>;
    /// Makes the stored loved tracks the list as it stands now.
    fn loves(&mut self, loves: Vec<Love>) -> Result<LovesStored, Error>;
    /// Says how far the sync has got.
    fn progress(&mut self, progress: Progress);
    /// Whether the sync should stop at the next point where it can.
    fn stopping(&self) -> bool;
}

/// Something that brings plays in from elsewhere: Last.fm, later a device's
/// log.
pub trait HistorySource {
    /// Reads what is new since the sink's cursor, and hands it to the sink
    /// a batch at a time. With `full` it reads everything again; what is
    /// stored already writes nothing.
    fn sync(&self, sink: &mut dyn Sink, full: bool) -> Result<Synced, Error>;
}

/// Why a sync failed. `Display` is one line that is safe to log: it never
/// holds a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

/// What failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The source: it could not be reached, or what it said was wrong.
    Source,
    /// The core, storing what it was handed.
    Sink,
}

impl Error {
    /// An error of the source.
    pub fn source(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Source,
            message: message.into(),
        }
    }

    /// An error of the sink.
    pub fn sink(message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Sink,
            message: message.into(),
        }
    }

    /// What failed.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
