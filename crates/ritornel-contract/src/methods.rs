//! The method table: every call of the contract, defined once. See decision
//! 1 in docs/design/contract.md.
//!
//! A method is a type in this module that implements [`Method`], named for
//! its call: [`ServerHello`] is `server.hello`. The server's dispatcher and
//! the Rust client both take the name, the params type and the result type
//! from that one implementation, so they cannot disagree.
//!
//! A generator walks the table two ways. [`METHODS`] lists each method with
//! its types by name, for anything that needs only text. [`visit_methods`]
//! hands a [`MethodVisitor`] each method as a type, and [`Described`] is
//! what a generator may ask of the params and the result. The TypeScript in
//! `contract/contract.ts` is written that way, by this crate's tests.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// What sort of call a method is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    /// It changes only the session it is called on: who the client is, and
    /// what it is subscribed to.
    Session,
    /// It reads and changes nothing.
    Query,
    /// It changes something lasting. Its params carry an `idempotencyKey`
    /// and its result is a [`Receipt`](crate::Receipt), so sending it again
    /// is safe.
    Command,
}

/// One call of the contract.
pub trait Method {
    /// The name on the wire, such as `server.hello`.
    const NAME: &'static str;
    /// What sort of call it is.
    const KIND: MethodKind;
    /// Whether it needs a `full` session. A method that does not is open to
    /// any session.
    const NEEDS_FULL: bool;
    /// The params. A method that takes none has [`NoParams`](crate::NoParams).
    type Params: Serialize + DeserializeOwned + Described;
    /// The result. It is always an object, so that it can grow fields.
    type Result: Serialize + DeserializeOwned + Described;
}

/// What a generator asks of a method's params and result.
///
/// In this crate's tests it is ts-rs's `TS`: the type can be written as
/// TypeScript. A method whose params or result cannot be does not compile
/// there, so it cannot be left out of `contract/contract.ts` unnoticed. In
/// every other build it asks nothing and every type has it, so no program
/// is built with ts-rs.
#[cfg(test)]
pub trait Described: ts_rs::TS + 'static {}

#[cfg(test)]
impl<T: ts_rs::TS + 'static> Described for T {}

/// What a generator asks of a method's params and result. In this build it
/// asks nothing: see the same trait as this crate's tests have it.
#[cfg(not(test))]
pub trait Described {}

#[cfg(not(test))]
impl<T> Described for T {}

/// One row of the method table, as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    /// The name on the wire.
    pub name: &'static str,
    /// What sort of call it is.
    pub kind: MethodKind,
    /// Whether it needs a `full` session.
    pub needs_full: bool,
    /// The name of the params type in this crate.
    pub params: &'static str,
    /// The name of the result type in this crate.
    pub result: &'static str,
}

/// Something that is shown every method in turn, as a type.
pub trait MethodVisitor {
    /// Called once for each method, in the table's order.
    fn visit<M: Method>(&mut self);
}

/// Defines the table: a type for each method, [`METHODS`] and
/// [`visit_methods`].
macro_rules! methods {
    ($(
        $(#[$doc:meta])*
        $marker:ident: $name:literal, $kind:ident, needs_full: $needs_full:literal,
        $params:ident => $result:ident;
    )*) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy)]
            pub struct $marker;

            impl Method for $marker {
                const NAME: &'static str = $name;
                const KIND: MethodKind = MethodKind::$kind;
                const NEEDS_FULL: bool = $needs_full;
                type Params = crate::$params;
                type Result = crate::$result;
            }
        )*

        /// Every method of the contract, in the order of
        /// docs/design/contract.md's call list.
        pub const METHODS: &[MethodInfo] = &[$(
            MethodInfo {
                name: $name,
                kind: MethodKind::$kind,
                needs_full: $needs_full,
                params: stringify!($params),
                result: stringify!($result),
            },
        )*];

        /// Shows `visitor` every method, in [`METHODS`]' order.
        pub fn visit_methods<V: MethodVisitor>(visitor: &mut V) {
            $(visitor.visit::<$marker>();)*
        }
    };
}

methods! {
    /// `server.hello`: the handshake. The client says who it is and the
    /// server answers with its descriptor. It is the first call on a
    /// socket, and every other call is refused until it has succeeded.
    ServerHello: "server.hello", Session, needs_full: false,
    HelloParams => Descriptor;

    /// `server.status`: the health of the server, for a person or an agent
    /// explaining a failure. It names a path on the server's disk, so it
    /// needs a `full` session.
    ServerStatus: "server.status", Query, needs_full: true,
    NoParams => ServerStatus;

    /// `servers.list`: every server this one knows, itself included. A
    /// client joins the copies of a track against it.
    ServersList: "servers.list", Query, needs_full: false,
    NoParams => ServersList;

    /// `events.subscribe`: adds topics to what this session is sent.
    EventsSubscribe: "events.subscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;

    /// `events.unsubscribe`: removes topics from what this session is sent.
    /// No event on those topics follows the response.
    EventsUnsubscribe: "events.unsubscribe", Session, needs_full: false,
    TopicsParams => Subscriptions;

    /// `library.create`: creates a library on this server and makes the
    /// server its hub. A server that has a library already refuses with
    /// `conflict`.
    LibraryCreate: "library.create", Command, needs_full: true,
    LibraryCreateParams => LibraryCreated;

    /// `sources.list`: every source of the library, with where its root is
    /// on this server. It names paths, so it needs a `full` session.
    SourcesList: "sources.list", Query, needs_full: true,
    NoParams => SourcesList;

    /// `sources.add`: adds a folder on this server as a named root. The
    /// server only ever reads it. A name the library has already is
    /// refused with `conflict`.
    SourcesAdd: "sources.add", Command, needs_full: true,
    SourcesAddParams => SourceAdded;

    /// `scan.start`: starts a scan of a source as a job, and returns the
    /// job. A source that is being scanned returns the job that is running.
    ScanStart: "scan.start", Command, needs_full: true,
    ScanStartParams => ScanStarted;

    /// `jobs.list`: the newest jobs, newest first.
    JobsList: "jobs.list", Query, needs_full: true,
    NoParams => JobsList;

    /// `jobs.get`: one job, with the first of its items that failed.
    JobsGet: "jobs.get", Query, needs_full: true,
    JobGetParams => JobDetail;

    /// `jobs.cancel`: asks a job to stop. It stops after the step in
    /// flight, and what it has committed stays.
    JobsCancel: "jobs.cancel", Command, needs_full: true,
    JobCancelParams => JobCancelled;

    /// `library.stats`: what the library holds and what the scans made of
    /// it.
    LibraryStats: "library.stats", Query, needs_full: false,
    NoParams => LibraryStats;

    /// `identity.dryRun`: runs the resolver again over the stored
    /// revisions, by policy 1 or by other values put in its place, and
    /// says what that would change. It writes nothing. It names files by
    /// path, so it needs a `full` session.
    IdentityDryRun: "identity.dryRun", Query, needs_full: true,
    DryRunParams => DryRun;

    /// `identity.review`: every binding marked for review, with the
    /// candidate it nearly matched and why it did not bind. It names files
    /// by path, so it needs a `full` session.
    IdentityReview: "identity.review", Query, needs_full: true,
    NoParams => ReviewList;

    /// `list.open`: resolves a query into a view this session holds, and
    /// answers with its ID, its exact total and, if asked, its first rows.
    /// With `replaces` it closes that view and says where its anchor row
    /// is in this one.
    ListOpen: "list.open", Session, needs_full: false,
    ListOpenParams => ListOpened;

    /// `list.window`: rows of a view, read as the library has them now.
    ListWindowCall: "list.window", Query, needs_full: false,
    ListWindowParams => ListWindow;

    /// `list.sections`: where each letter, year or stretch of time begins
    /// in a view.
    ListSectionsCall: "list.sections", Query, needs_full: false,
    ViewParams => ListSections;

    /// `list.locate`: the index of a row in a view, by its ID or by the
    /// start of its name.
    ListLocate: "list.locate", Query, needs_full: false,
    ListLocateParams => ListLocated;

    /// `list.close`: ends a view.
    ListClose: "list.close", Session, needs_full: false,
    ViewParams => ListClosed;

    /// `album.get`: one release, with its tracks in disc and track order
    /// and the other versions of its group. Given a release group, the
    /// release the group is set to show.
    AlbumGet: "album.get", Query, needs_full: false,
    AlbumGetParams => AlbumPage;

    /// `artist.get`: one artist, as the Artists list has them, and the
    /// colour of their picture.
    ArtistGet: "artist.get", Query, needs_full: false,
    ArtistGetParams => ArtistPage;

    /// `media.token`: the token a page puts in the query string of a media
    /// URL. It is the same for the life of the session.
    MediaTokenCall: "media.token", Session, needs_full: false,
    NoParams => MediaToken;
    /// `playback.get`: the player's state, whole.
    PlaybackGet: "playback.get", Query, needs_full: false,
    NoParams => PlaybackState;

    /// `playback.play`: replaces the queue with what it names and plays it.
    PlaybackPlay: "playback.play", Command, needs_full: false,
    PlaybackPlayParams => PlaybackChanged;

    /// `playback.pause`: pauses. Paused already, it changes nothing.
    PlaybackPause: "playback.pause", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.resume`: plays on from where the player is: after a pause,
    /// a stop or a restart.
    PlaybackResume: "playback.resume", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.stop`: stops, and the entry starts again from its
    /// beginning.
    PlaybackStop: "playback.stop", Command, needs_full: false,
    PlaybackCommandParams => PlaybackChanged;

    /// `playback.seek`: moves the entry it names to a position.
    PlaybackSeek: "playback.seek", Command, needs_full: false,
    PlaybackSeekParams => PlaybackChanged;

    /// `playback.skip`: skips from the entry it names, forward or back.
    PlaybackSkip: "playback.skip", Command, needs_full: false,
    PlaybackSkipParams => PlaybackChanged;

    /// `queue.window`: a window of the queue, with its revision.
    QueueWindow: "queue.window", Query, needs_full: false,
    QueueWindowParams => QueueWindow;

    /// `sources.tracks`: the tracks of the files in a folder of one of this
    /// server's roots, in the order they are played. It takes a path, so
    /// it needs a `full` session.
    SourcesTracks: "sources.tracks", Query, needs_full: true,
    SourcesTracksParams => SourceTracks;

    /// `history.recent`: the newest plays, newest first. Until list views
    /// arrive, this is how history is read.
    HistoryRecent: "history.recent", Query, needs_full: false,
    HistoryRecentParams => RecentPlays;

    /// `history.onThisDay`: the plays that count on one day of the year,
    /// in every year.
    HistoryOnThisDay: "history.onThisDay", Query, needs_full: false,
    OnThisDayParams => OnThisDay;

    /// `history.rebuild`: drops the plays and derives them again from what
    /// the sources reported, in one transaction, or says what that would
    /// change.
    HistoryRebuild: "history.rebuild", Command, needs_full: true,
    HistoryRebuildParams => HistoryRebuilt;
}

/// The row of the table for the method named `name`, if there is one.
pub fn method(name: &str) -> Option<&'static MethodInfo> {
    METHODS.iter().find(|method| method.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_the_m0_call_list_and_the_calls_added_since() {
        let names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
        assert_eq!(
            names,
            [
                "server.hello",
                "server.status",
                "servers.list",
                "events.subscribe",
                "events.unsubscribe",
                "library.create",
                "sources.list",
                "sources.add",
                "scan.start",
                "jobs.list",
                "jobs.get",
                "jobs.cancel",
                "library.stats",
                "identity.dryRun",
                "identity.review",
                "list.open",
                "list.window",
                "list.sections",
                "list.locate",
                "list.close",
                "album.get",
                "artist.get",
                "media.token",
                "playback.get",
                "playback.play",
                "playback.pause",
                "playback.resume",
                "playback.stop",
                "playback.seek",
                "playback.skip",
                "queue.window",
                "sources.tracks",
                "history.recent",
                "history.onThisDay",
                "history.rebuild"
            ]
        );
        assert_eq!(method("server.status").unwrap().result, "ServerStatus");
        assert_eq!(method("server.statu"), None);
    }

    #[test]
    fn the_status_and_every_command_need_a_full_session() {
        let full: Vec<_> = METHODS
            .iter()
            .filter(|method| method.needs_full)
            .map(|method| method.name)
            .collect();
        assert_eq!(
            full,
            [
                "server.status",
                "library.create",
                "sources.list",
                "sources.add",
                "scan.start",
                "jobs.list",
                "jobs.get",
                "jobs.cancel",
                "identity.dryRun",
                "identity.review",
                "sources.tracks",
                "history.rebuild"
            ]
        );
        // Everything but reading the library and controlling playback
        // needs one, so no command is open to a limited session but the
        // player's.
        for method in METHODS {
            assert!(
                method.needs_full
                    || method.kind != MethodKind::Command
                    || method.name.starts_with("playback."),
                "{}",
                method.name
            );
        }
    }

    #[test]
    fn a_visitor_sees_every_row_as_a_type() {
        #[derive(Default)]
        struct Rows(Vec<MethodInfo>);

        impl MethodVisitor for Rows {
            fn visit<M: Method>(&mut self) {
                let short = |name: &'static str| name.rsplit("::").next().unwrap();
                self.0.push(MethodInfo {
                    name: M::NAME,
                    kind: M::KIND,
                    needs_full: M::NEEDS_FULL,
                    params: short(std::any::type_name::<M::Params>()),
                    result: short(std::any::type_name::<M::Result>()),
                });
            }
        }

        let mut rows = Rows::default();
        visit_methods(&mut rows);
        assert_eq!(rows.0, METHODS);
    }
}
