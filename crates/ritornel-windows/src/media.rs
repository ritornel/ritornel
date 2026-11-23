//! The system's media transport controls: the media keys, and the player
//! Windows shows in its media flyout and on the lock screen. They are tied
//! to a window of this process, which a process with none opens hidden
//! ([`Window`](crate::window::Window)), and they are let go before that
//! window closes.
//!
//! Windows delivers each request on a thread of its own, so the handler
//! should only forward it. The controls are used from a thread that is not
//! a single-threaded apartment, as any thread of a Rust program is unless
//! it asks to be one: a cover is loaded on a thread of its own.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::StorageFile;
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::core::{HSTRING, Ref};

/// What a person asked of the player through the controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// To this position in the track, as by dragging the flyout's bar.
    Position(Duration),
}

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    Stopped,
}

/// The track shown. A field it has not is shown empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct Track<'a> {
    pub title: Option<&'a str>,
    pub artist: Option<&'a str>,
    pub album: Option<&'a str>,
    /// An image file, shown as the cover once it is read.
    pub cover: Option<&'a Path>,
}

/// The controls of one window, enabled. Dropping them takes the player off
/// the system's list.
#[derive(Debug)]
pub struct MediaControls {
    controls: SystemMediaTransportControls,
    timeline: SystemMediaTransportControlsTimelineProperties,
    tokens: [i64; 2],
    /// Counts the tracks shown, so that a cover read late is shown only
    /// with its own track.
    shown: Arc<AtomicU64>,
}

impl MediaControls {
    /// Takes the controls of `window`, an `HWND` of this process's, with
    /// each request passed to `on`, and enables them once nothing more can
    /// fail. On an error nothing is left registered.
    pub fn open(window: isize, on: impl Fn(Request) + Send + Sync + 'static) -> io::Result<Self> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // SAFETY: the call reads no memory through the handle: Windows
        // looks the window up, and refuses one that is not a window of this
        // process with an error.
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(window as *mut std::ffi::c_void)) }?;
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        let on = Arc::new(on);
        let buttons = controls.ButtonPressed(&TypedEventHandler::new({
            let on = Arc::clone(&on);
            move |_, args: Ref<'_, SystemMediaTransportControlsButtonPressedEventArgs>| {
                if let Some(request) = request_of(args.ok()?.Button()?) {
                    on(request);
                }
                Ok(())
            }
        }))?;
        let positions = controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<'_, PlaybackPositionChangeRequestedEventArgs>| {
                if let Some(at) = position_of(args.ok()?.RequestedPlaybackPosition()?) {
                    on(Request::Position(at));
                }
                Ok(())
            },
        ));
        let positions = match positions {
            Ok(token) => token,
            Err(error) => {
                let _ = controls.RemoveButtonPressed(buttons);
                return Err(error.into());
            }
        };
        let controls = Self {
            controls,
            timeline,
            tokens: [buttons, positions],
            shown: Arc::new(AtomicU64::new(0)),
        };
        // Dropped on an error, which takes both handlers off.
        controls.controls.SetIsEnabled(true)?;
        Ok(controls)
    }

    /// Shows `track` in place of what was shown. Its cover is read on a
    /// thread of its own, and shown if the track is still the one shown
    /// then; a cover that cannot be read leaves the track without one.
    pub fn set_track(&mut self, track: &Track<'_>) -> io::Result<()> {
        let display = self.controls.DisplayUpdater()?;
        display.ClearAll()?;
        display.SetType(MediaPlaybackType::Music)?;
        let music = display.MusicProperties()?;
        music.SetTitle(&HSTRING::from(track.title.unwrap_or("")))?;
        music.SetArtist(&HSTRING::from(track.artist.unwrap_or("")))?;
        music.SetAlbumTitle(&HSTRING::from(track.album.unwrap_or("")))?;
        display.Update()?;
        let this = self.shown.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(cover) = track.cover {
            // Windows takes only an absolute path with backslashes.
            let cover = HSTRING::from(std::path::absolute(cover)?.as_os_str());
            let shown = Arc::clone(&self.shown);
            std::thread::Builder::new()
                .name("ritornel-media-cover".to_owned())
                .spawn(move || {
                    let read = StorageFile::GetFileFromPathAsync(&cover)
                        .and_then(|reading| reading.join())
                        .and_then(|file| RandomAccessStreamReference::CreateFromFile(&file));
                    if let Ok(cover) = read
                        && shown.load(Ordering::SeqCst) == this
                    {
                        let _ = display.SetThumbnail(&cover).and_then(|()| display.Update());
                    }
                })?;
        }
        Ok(())
    }

    /// Shows what the player is doing, where it is in the track, and how
    /// long the track is. A track of no known length, or one the position
    /// has run past, ends where the position is.
    pub fn set_state(
        &mut self,
        playback: Playback,
        position: Duration,
        length: Option<Duration>,
    ) -> io::Result<()> {
        let end = TimeSpan::from(length.unwrap_or(position).max(position));
        self.timeline.SetStartTime(TimeSpan::default())?;
        self.timeline.SetMinSeekTime(TimeSpan::default())?;
        self.timeline.SetEndTime(end)?;
        self.timeline.SetMaxSeekTime(end)?;
        self.timeline.SetPosition(TimeSpan::from(position))?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;
        self.controls.SetPlaybackStatus(match playback {
            Playback::Playing => MediaPlaybackStatus::Playing,
            Playback::Paused => MediaPlaybackStatus::Paused,
            Playback::Stopped => MediaPlaybackStatus::Stopped,
        })?;
        Ok(())
    }
}

impl Drop for MediaControls {
    fn drop(&mut self) {
        let [buttons, positions] = self.tokens;
        let _ = self.controls.RemoveButtonPressed(buttons);
        let _ = self
            .controls
            .RemovePlaybackPositionChangeRequested(positions);
        // A cover still being read is not shown.
        self.shown.fetch_add(1, Ordering::SeqCst);
        let _ = self.controls.DisplayUpdater().and_then(|display| {
            display.ClearAll()?;
            display.Update()
        });
        let _ = self.controls.SetIsEnabled(false);
    }
}

fn request_of(button: SystemMediaTransportControlsButton) -> Option<Request> {
    Some(match button {
        SystemMediaTransportControlsButton::Play => Request::Play,
        SystemMediaTransportControlsButton::Pause => Request::Pause,
        SystemMediaTransportControlsButton::Stop => Request::Stop,
        SystemMediaTransportControlsButton::Next => Request::Next,
        SystemMediaTransportControlsButton::Previous => Request::Previous,
        _ => return None,
    })
}

/// A position another program asked for, in 100 ns ticks: none if it is
/// before the start, which any program may send.
fn position_of(at: TimeSpan) -> Option<Duration> {
    let ticks = u64::try_from(at.Duration).ok()?;
    let nanos = u32::try_from(ticks % 10_000_000 * 100).ok()?;
    Some(Duration::new(ticks / 10_000_000, nanos))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::window::Window;

    #[test]
    fn the_buttons_the_controls_take_are_the_players() {
        assert_eq!(
            request_of(SystemMediaTransportControlsButton::Pause),
            Some(Request::Pause)
        );
        assert_eq!(
            request_of(SystemMediaTransportControlsButton::FastForward),
            None
        );
    }

    #[test]
    fn a_position_before_the_start_is_ignored_and_any_other_kept() {
        assert_eq!(position_of(TimeSpan { Duration: -1 }), None);
        assert_eq!(
            position_of(TimeSpan {
                Duration: 900_000_001
            }),
            Some(Duration::new(90, 100))
        );
        assert!(position_of(TimeSpan { Duration: i64::MAX }).is_some());
    }

    #[test]
    #[ignore = "shows a player in the system's media flyout, and may take the media keys: run it by hand"]
    fn a_hidden_window_takes_the_controls_and_they_take_a_track_and_its_state() {
        let window = Window::open("Ritornel media test", |_| {}).unwrap();
        let (sent, _heard) = mpsc::channel();
        let mut controls = MediaControls::open(window.handle(), move |request| {
            let _ = sent.send(request);
        })
        .unwrap();
        let track = Track {
            title: Some("Salt Road"),
            album: Some("Field Notes"),
            cover: Some(Path::new(r"C:\no such cover.jpg")),
            ..Track::default()
        };
        controls.set_track(&track).unwrap();
        controls
            .set_state(
                Playback::Paused,
                Duration::from_secs(20),
                Some(Duration::from_secs(200)),
            )
            .unwrap();
        controls
            .set_state(Playback::Playing, Duration::from_secs(5), None)
            .unwrap();
        drop(controls);
        drop(window);
    }
}
