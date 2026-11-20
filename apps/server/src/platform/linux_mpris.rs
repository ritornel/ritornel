//! MPRIS on Linux: the player on the session bus as
//! `org.mpris.MediaPlayer2.ritornel`, which desktop shells, `playerctl` and
//! the media keys of every Linux desktop talk to. See "OS integration" in
//! docs/design/playback.md and the MPRIS specification,
//! <https://specifications.freedesktop.org/mpris-spec/latest/>.
//!
//! It is served with zbus on the server's own async runtime, so it adds no
//! thread and polls nothing. What it shows is worked out from the player's
//! state by [`Shown::of`], which is pure; the interfaces only read it.
//! A server with no session bus (a hub, or a login with no desktop) has
//! none, and says so only in its debug log.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ritornel_engine::Id;
use ritornel_engine::playback::{PlayerState, Status};
use tokio::runtime::Handle;
use tracing::{debug, warn};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::{Connection, ObjectPath, Variant, interface};

use crate::media_session::{Key, Keys};

/// The object every MPRIS player serves its interfaces at.
const PATH: &str = "/org/mpris/MediaPlayer2";

/// The bus name. A second server on the same login takes an instance name
/// beside it, as the specification asks.
const NAME: &str = "org.mpris.MediaPlayer2.ritornel";

/// The track ID MPRIS reserves for no track.
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// How far the position may drift from where playing would have taken it
/// before MPRIS is told it was sought. A seek asked for is told however
/// small.
const SEEKED_AFTER: Duration = Duration::from_secs(1);

/// How long the session bus has to let the player in and give it its name.
/// A bus that takes the connection and never answers is no bus.
const CONNECT_WITHIN: Duration = Duration::from_secs(3);

/// The connection that serves the player. Dropping it takes the player off
/// the bus.
pub(crate) struct Controls {
    connection: Connection,
    runtime: Handle,
    /// The cover of the track the player is on, as a file URL.
    art_url: Option<String>,
}

impl Controls {
    /// Connects to the session bus and serves the player there. `None`,
    /// logged at debug, when there is no session bus or it does not
    /// answer within [`CONNECT_WITHIN`].
    pub(crate) fn open(keys: Keys, runtime: &Handle) -> Option<Self> {
        Self::open_on(
            zbus::connection::Builder::session(),
            keys,
            runtime,
            CONNECT_WITHIN,
        )
    }

    fn open_on(
        bus: zbus::connection::Builder<'static>,
        keys: Keys,
        runtime: &Handle,
        within: Duration,
    ) -> Option<Self> {
        let runtime = runtime.clone();
        let opened = runtime.block_on(async {
            let deadline = tokio::time::Instant::now() + within;
            let connection = match tokio::time::timeout_at(deadline, connect(bus, keys)).await {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    debug!(%error, "no session bus, so no MPRIS");
                    return None;
                }
                Err(_) => {
                    debug!(?within, "the session bus did not answer, so no MPRIS");
                    return None;
                }
            };
            match tokio::time::timeout_at(deadline, take_name(&connection)).await {
                Ok(Ok(name)) => debug!(name, "MPRIS is on the session bus"),
                Ok(Err(error)) => {
                    warn!(%error, "MPRIS could not take a name on the session bus");
                    return None;
                }
                Err(_) => {
                    debug!(?within, "the session bus did not answer, so no MPRIS");
                    return None;
                }
            }
            Some(connection)
        })?;
        Some(Self {
            connection: opened,
            runtime,
            art_url: None,
        })
    }

    /// Takes the cover of the track the player is now on. The state that
    /// comes with it shows the track.
    pub(crate) fn set_track(&mut self, _: &PlayerState, cover: Option<&str>) {
        self.art_url = cover.map(str::to_owned);
    }

    /// Shows the track, the status, the position and the volume.
    pub(crate) fn set_state(&mut self, state: &PlayerState) {
        self.update(Shown::of(state, self.art_url.clone()));
    }

    fn update(&self, next: Shown) {
        let updated = self.runtime.block_on(async {
            let player = self.player().await?;
            let emitter = player.signal_emitter();
            let mut serving = player.get_mut().await;
            let before = serving.shown.clone();
            let expected = serving.position_now();
            serving.shown = next;
            serving.at = Instant::now();
            let now = &serving.shown;
            if now.status != before.status {
                serving.playback_status_changed(emitter).await?;
            }
            if now.metadata() != before.metadata() {
                serving.metadata_changed(emitter).await?;
            }
            if now.volume.to_bits() != before.volume.to_bits() {
                serving.volume_changed(emitter).await?;
            }
            if now.can_play() != before.can_play() {
                serving.can_play_changed(emitter).await?;
                serving.can_pause_changed(emitter).await?;
                serving.can_go_next_changed(emitter).await?;
                serving.can_go_previous_changed(emitter).await?;
            }
            if now.can_seek() != before.can_seek() {
                serving.can_seek_changed(emitter).await?;
            }
            if let Some(position) = sought(&before, expected, now) {
                Player::seeked(emitter, position).await?;
            }
            zbus::Result::Ok(())
        });
        if let Err(error) = updated {
            warn!(%error, "MPRIS could not be told the player's state");
        }
    }

    async fn player(&self) -> zbus::Result<zbus::object_server::InterfaceRef<Player>> {
        self.connection
            .object_server()
            .interface::<_, Player>(PATH)
            .await
    }
}

async fn connect(bus: zbus::connection::Builder<'static>, keys: Keys) -> zbus::Result<Connection> {
    let connection = bus.build().await?;
    let server = connection.object_server();
    server.at(PATH, Root).await?;
    server
        .at(
            PATH,
            Player {
                shown: Shown::default(),
                at: Instant::now(),
                keys,
            },
        )
        .await?;
    Ok(connection)
}

/// Takes the player's bus name, or an instance name if another server on
/// this login has it.
async fn take_name(connection: &Connection) -> zbus::Result<String> {
    let flags = RequestNameFlags::DO_NOT_QUEUE;
    for name in [NAME.to_owned(), instance_name(std::process::id())] {
        match connection
            .request_name_with_flags(name.as_str(), flags)
            .await?
        {
            RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => return Ok(name),
            RequestNameReply::Exists | RequestNameReply::InQueue => {}
        }
    }
    Err(zbus::Error::NameTaken)
}

fn instance_name(pid: u32) -> String {
    format!("{NAME}.instance{pid}")
}

/// What MPRIS is shown, from the player's state.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    status: Status,
    /// The entry the player is on.
    entry: Option<Id>,
    /// The entry's track ID, an object path.
    track_id: String,
    title: Option<String>,
    album: Option<String>,
    artist: Option<String>,
    art_url: Option<String>,
    /// In microseconds, as MPRIS counts.
    length: Option<i64>,
    /// Where the player was, in microseconds, when this was shown.
    position: i64,
    volume: f64,
    /// The player's count of the seeks asked for.
    seeks: u64,
}

impl Default for Shown {
    fn default() -> Self {
        Self {
            status: Status::Stopped,
            entry: None,
            track_id: NO_TRACK.to_owned(),
            title: None,
            album: None,
            artist: None,
            art_url: None,
            length: None,
            position: 0,
            volume: 1.0,
            seeks: 0,
        }
    }
}

/// A metadata value, before it is put on the bus.
#[derive(Debug, Clone, PartialEq)]
enum Meta {
    Path(String),
    Text(String),
    Texts(Vec<String>),
    Micros(i64),
}

fn micros(ms: i64) -> i64 {
    ms.saturating_mul(1000)
}

impl Shown {
    fn of(state: &PlayerState, art_url: Option<String>) -> Self {
        let now = state.current.as_ref();
        let track = now.and_then(|now| now.track.as_ref());
        Self {
            status: state.status,
            entry: now.map(|now| now.entry_id),
            track_id: now.map_or_else(|| NO_TRACK.to_owned(), |now| track_id(&now.entry_id)),
            title: track.and_then(|track| track.title.clone()),
            album: track.and_then(|track| track.album.clone()),
            artist: track.and_then(|track| track.artist.clone()),
            art_url: now.and(art_url),
            length: state.duration_ms.map(micros),
            position: micros(state.position_ms.max(0)),
            volume: if state.muted { 0.0 } else { state.volume },
            seeks: state.seeks,
        }
    }

    fn has_track(&self) -> bool {
        self.track_id != NO_TRACK
    }

    fn can_play(&self) -> bool {
        self.has_track()
    }

    fn can_seek(&self) -> bool {
        self.has_track() && self.length.is_some()
    }

    fn status_name(&self) -> &'static str {
        match self.status {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
    }

    /// The `Metadata` property, by key.
    fn metadata(&self) -> Vec<(&'static str, Meta)> {
        let mut entries = vec![("mpris:trackid", Meta::Path(self.track_id.clone()))];
        if !self.has_track() {
            return entries;
        }
        if let Some(length) = self.length {
            entries.push(("mpris:length", Meta::Micros(length)));
        }
        if let Some(url) = &self.art_url {
            entries.push(("mpris:artUrl", Meta::Text(url.clone())));
        }
        if let Some(title) = &self.title {
            entries.push(("xesam:title", Meta::Text(title.clone())));
        }
        if let Some(album) = &self.album {
            entries.push(("xesam:album", Meta::Text(album.clone())));
        }
        if let Some(artist) = &self.artist {
            entries.push(("xesam:artist", Meta::Texts(vec![artist.clone()])));
        }
        entries
    }

    /// Where the player is `elapsed` after this was shown: it moves only
    /// while playing, and not past the track's end.
    fn position_after(&self, elapsed: Duration) -> i64 {
        if self.status != Status::Playing {
            return self.position;
        }
        let elapsed = i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX);
        let position = self.position.saturating_add(elapsed);
        self.length.map_or(position, |length| position.min(length))
    }
}

/// An entry's track ID: an object path of our own, as MPRIS asks.
fn track_id(entry: &impl std::fmt::Display) -> String {
    let element: String = entry
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("/dev/ritornel/entry/{element}")
}

/// The position to announce with `Seeked`, when the player is still on the
/// same track and was asked to seek, by any client, or is not where
/// playing would have taken it.
fn sought(before: &Shown, expected: i64, now: &Shown) -> Option<i64> {
    let asked = now.seeks != before.seeks;
    let drifted = now.position.abs_diff(expected)
        > u64::try_from(SEEKED_AFTER.as_micros()).unwrap_or(u64::MAX);
    (now.has_track() && now.track_id == before.track_id && (asked || drifted))
        .then_some(now.position)
}

/// A seek to `position`, in microseconds, in the entry shown, which the
/// player ignores if it has moved on from that entry by the time it
/// carries the key out.
fn seek_in(shown: &Shown, position: i64) -> Option<Key> {
    Some(Key::SeekIn {
        entry: shown.entry?,
        position_ms: u64::try_from(position / 1000).unwrap_or(0),
    })
}

/// What `Seek` asks for: an offset from where the player is now. Past the
/// end is the next track, as the specification says; before the start is
/// the start.
fn seek_by(shown: &Shown, now: i64, offset: i64) -> Option<Key> {
    if !shown.can_seek() {
        return None;
    }
    let to = now.saturating_add(offset).max(0);
    if shown.length.is_some_and(|length| to > length) {
        return Some(Key::Next);
    }
    seek_in(shown, to)
}

/// What `SetPosition` asks for: a position in the track it names, which
/// is ignored if that is no longer the track or the position is outside it.
fn set_position(shown: &Shown, track: &str, position: i64) -> Option<Key> {
    let inside = position >= 0 && shown.length.is_none_or(|length| position <= length);
    if shown.can_seek() && track == shown.track_id && inside {
        seek_in(shown, position)
    } else {
        None
    }
}

/// `org.mpris.MediaPlayer2`: the application. The server has no window to
/// raise and is not quit from a shell's media widget.
struct Root;

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn identity(&self) -> &str {
        "Ritornel"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`: the player's state, and the keys.
struct Player {
    shown: Shown,
    /// When `shown` was.
    at: Instant,
    keys: Keys,
}

impl Player {
    fn position_now(&self) -> i64 {
        self.shown.position_after(self.at.elapsed())
    }

    fn press(&self, key: Option<Key>) {
        if let Some(key) = key {
            (self.keys)(key);
        }
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.press(Some(Key::Next));
    }

    fn previous(&self) {
        self.press(Some(Key::Previous));
    }

    fn pause(&self) {
        self.press(Some(Key::Pause));
    }

    fn play_pause(&self) {
        self.press(Some(Key::Toggle));
    }

    /// The player has no stop a listener would miss: it pauses.
    fn stop(&self) {
        self.press(Some(Key::Pause));
    }

    fn play(&self) {
        self.press(Some(Key::Play));
    }

    fn seek(&self, offset: i64) {
        self.press(seek_by(&self.shown, self.position_now(), offset));
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        self.press(set_position(&self.shown, track_id.as_str(), position));
    }

    fn open_uri(&self, _uri: String) -> zbus::fdo::Result<()> {
        Err(zbus::fdo::Error::NotSupported(
            "Ritornel plays from its library only".to_owned(),
        ))
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        self.shown.status_name()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<&'static str, Variant<'static>> {
        self.shown
            .metadata()
            .into_iter()
            .filter_map(|(key, value)| {
                let value = match value {
                    Meta::Path(path) => Variant::from(ObjectPath::try_from(path).ok()?),
                    Meta::Text(text) => Variant::from(text),
                    Meta::Texts(texts) => Variant::from(texts),
                    Meta::Micros(micros) => Variant::from(micros),
                };
                Some((key, value))
            })
            .collect()
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        self.shown.volume
    }

    #[zbus(property)]
    fn set_volume(&mut self, volume: f64) {
        self.press(Some(Key::Volume(volume.clamp(0.0, 1.0))));
    }

    /// Read when asked, from where the player was and how long it has
    /// played since; MPRIS never signals it.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.position_now()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.shown.can_play()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.shown.can_play()
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.shown.can_play()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.shown.can_play()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.shown.can_seek()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use ritornel_engine::Id;
    use ritornel_engine::playback::{NowPlaying, Repeat};

    use super::*;

    fn id(n: u8) -> Id {
        Id::from_bytes([n; 16])
    }

    /// The player on an entry. What the track shows is the library's,
    /// which a test outside the engine cannot make, so it is added to what
    /// [`Shown::of`] makes by [`titled`].
    fn state(status: Status, position_ms: i64) -> PlayerState {
        PlayerState {
            revision: 1,
            status,
            current: Some(NowPlaying {
                entry_id: id(1),
                index: 0,
                track: None,
                format: None,
            }),
            position_ms,
            duration_ms: Some(200_000),
            queue_revision: 1,
            queue_length: 1,
            shuffle: false,
            repeat: Repeat::Off,
            output: ("null".to_owned(), 48_000, 2),
            volume: 0.5,
            muted: false,
            seeks: 0,
        }
    }

    fn titled(shown: Shown) -> Shown {
        Shown {
            title: Some("Tone in A".to_owned()),
            album: Some("Half a Second".to_owned()),
            artist: Some("The Invented".to_owned()),
            ..shown
        }
    }

    #[test]
    fn the_track_is_shown_as_mpris_and_xesam_metadata() {
        let shown = Shown::of(
            &state(Status::Playing, 1500),
            Some("file:///cache/a.jpg".to_owned()),
        );
        assert_eq!(shown.status_name(), "Playing");
        assert_eq!(shown.position, 1_500_000);
        assert_eq!(
            titled(shown.clone()).metadata(),
            vec![
                (
                    "mpris:trackid",
                    Meta::Path(
                        "/dev/ritornel/entry/01010101_0101_0101_0101_010101010101".to_owned()
                    )
                ),
                ("mpris:length", Meta::Micros(200_000_000)),
                ("mpris:artUrl", Meta::Text("file:///cache/a.jpg".to_owned())),
                ("xesam:title", Meta::Text("Tone in A".to_owned())),
                ("xesam:album", Meta::Text("Half a Second".to_owned())),
                ("xesam:artist", Meta::Texts(vec!["The Invented".to_owned()])),
            ]
        );
        // A track the library no longer has shows what is known of it.
        assert_eq!(shown.metadata().len(), 3);
        assert!(shown.can_play() && shown.can_seek());
        // Every track ID is a valid object path.
        assert!(ObjectPath::try_from(shown.track_id.as_str()).is_ok());
    }

    #[test]
    fn with_no_entry_there_is_no_track_and_nothing_to_control() {
        let mut empty = state(Status::Stopped, 0);
        empty.current = None;
        empty.duration_ms = None;
        let shown = Shown::of(&empty, Some("file:///stale.jpg".to_owned()));
        assert_eq!(shown.status_name(), "Stopped");
        assert_eq!(
            shown.metadata(),
            vec![("mpris:trackid", Meta::Path(NO_TRACK.to_owned()))]
        );
        assert!(!shown.can_play() && !shown.can_seek());
        assert_eq!(seek_by(&shown, 0, 5_000_000), None);
    }

    #[test]
    fn muted_is_volume_nothing() {
        let mut muted = state(Status::Paused, 0);
        muted.muted = true;
        assert!(Shown::of(&muted, None).volume.abs() < f64::EPSILON);
        assert!((Shown::of(&state(Status::Paused, 0), None).volume - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn the_position_moves_only_while_playing_and_stops_at_the_end() {
        let playing = Shown::of(&state(Status::Playing, 10_000), None);
        assert_eq!(playing.position_after(Duration::from_secs(2)), 12_000_000);
        assert_eq!(
            playing.position_after(Duration::from_secs(1000)),
            200_000_000
        );
        let paused = Shown::of(&state(Status::Paused, 10_000), None);
        assert_eq!(paused.position_after(Duration::from_secs(2)), 10_000_000);
    }

    #[test]
    fn a_seek_asked_for_is_told_however_small() {
        let before = Shown::of(&state(Status::Playing, 10_000), None);
        let mut half_a_second = state(Status::Playing, 10_500);
        half_a_second.seeks = 1;
        let after = Shown::of(&half_a_second, None);
        assert_eq!(sought(&before, 10_000_000, &after), Some(10_500_000));
        // The same state shown again, after a pause, is no new seek.
        let mut paused = half_a_second.clone();
        paused.status = Status::Paused;
        assert_eq!(sought(&after, 10_500_000, &Shown::of(&paused, None)), None);
    }

    #[test]
    fn a_jump_on_the_same_track_is_a_seek_and_playing_on_is_not() {
        let before = Shown::of(&state(Status::Playing, 10_000), None);
        let played_on = Shown::of(&state(Status::Playing, 12_100), None);
        assert_eq!(sought(&before, 12_000_000, &played_on), None);
        let jumped = Shown::of(&state(Status::Playing, 90_000), None);
        assert_eq!(sought(&before, 12_000_000, &jumped), Some(90_000_000));
        let mut next = state(Status::Playing, 0);
        if let Some(now) = &mut next.current {
            now.entry_id = id(9);
        }
        assert_eq!(sought(&before, 12_000_000, &Shown::of(&next, None)), None);
    }

    #[test]
    fn seek_and_set_position_become_the_players_seek() {
        let shown = Shown::of(&state(Status::Playing, 10_000), None);
        let seek = |position_ms| {
            Some(Key::SeekIn {
                entry: id(1),
                position_ms,
            })
        };
        assert_eq!(seek_by(&shown, 10_000_000, 5_000_000), seek(15_000));
        assert_eq!(seek_by(&shown, 10_000_000, -60_000_000), seek(0));
        assert_eq!(seek_by(&shown, 10_000_000, 500_000_000), Some(Key::Next));
        // The seek names the entry it was asked of, so that it lands
        // nowhere if a next goes before it.
        assert_eq!(
            set_position(&shown, &shown.track_id, 42_000_000),
            seek(42_000)
        );
        assert_eq!(
            set_position(&shown, "/dev/ritornel/entry/other", 42_000_000),
            None
        );
        assert_eq!(set_position(&shown, &shown.track_id, -1), None);
        assert_eq!(set_position(&shown, &shown.track_id, 300_000_000), None);
    }

    /// A bus that takes the connection and never answers is given up on,
    /// so that neither a play nor the server's stop waits on it for ever.
    #[test]
    fn a_bus_that_never_answers_is_no_bus() {
        let dir = std::env::temp_dir().join(format!("ritornel-mpris-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("bus");
        let _ = std::fs::remove_file(&socket);
        // Listening, it takes each connection into its backlog, and never
        // answers.
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // As the server's: a handle drives a current-thread runtime's
        // timers and sockets only while it is run elsewhere.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let address = format!("unix:path={}", socket.display());
        let bus = zbus::connection::Builder::address(address.as_str());
        let keys: Keys = std::sync::Arc::new(|_| {});
        let started = Instant::now();
        let opened = Controls::open_on(bus, keys, runtime.handle(), Duration::from_millis(200));
        assert!(opened.is_none());
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(200), "{waited:?}");
        assert!(waited < Duration::from_secs(5), "{waited:?}");
        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_server_takes_an_instance_name() {
        assert_eq!(
            instance_name(4312),
            "org.mpris.MediaPlayer2.ritornel.instance4312"
        );
    }
}
