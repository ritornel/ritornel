//! The system's media transport controls: the media keys, and the player
//! Windows shows in its media flyout and on the lock screen. They are tied
//! to a window, which a process with none opens hidden
//! ([`Window`](crate::window::Window)).
//!
//! Windows delivers each button on a thread of its own, so the handler
//! should only forward it.

use std::io;
use std::path::Path;
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
    /// An image file, shown as the cover.
    pub cover: Option<&'a Path>,
    pub duration: Option<Duration>,
}

/// The controls of one window, enabled. Dropping them takes the player off
/// the system's list.
#[derive(Debug)]
pub struct MediaControls {
    controls: SystemMediaTransportControls,
    timeline: SystemMediaTransportControlsTimelineProperties,
    tokens: [i64; 2],
}

impl MediaControls {
    /// Takes the controls of `window`, an `HWND`, with each request passed
    /// to `on`, and enables them.
    pub fn open(window: isize, on: impl Fn(Request) + Send + Sync + 'static) -> io::Result<Self> {
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()?;
        // SAFETY: the interop's contract is a window handle, and a handle
        // that is not one is refused with an error, not read.
        let controls: SystemMediaTransportControls =
            unsafe { interop.GetForWindow(HWND(window as *mut std::ffi::c_void)) }?;
        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        let on = std::sync::Arc::new(on);
        let buttons = controls.ButtonPressed(&TypedEventHandler::new({
            let on = std::sync::Arc::clone(&on);
            move |_, args: Ref<'_, SystemMediaTransportControlsButtonPressedEventArgs>| {
                if let Some(request) = request_of(args.ok()?.Button()?) {
                    on(request);
                }
                Ok(())
            }
        }))?;
        let positions = controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<'_, PlaybackPositionChangeRequestedEventArgs>| {
                let at = Duration::from(args.ok()?.RequestedPlaybackPosition()?);
                on(Request::Position(at));
                Ok(())
            },
        ))?;
        Ok(Self {
            controls,
            timeline: SystemMediaTransportControlsTimelineProperties::new()?,
            tokens: [buttons, positions],
        })
    }

    /// Shows `track` in place of what was shown, the cover included. A
    /// cover that cannot be read is an error, after the rest is shown.
    pub fn set_track(&self, track: &Track<'_>) -> io::Result<()> {
        let display = self.controls.DisplayUpdater()?;
        display.ClearAll()?;
        display.SetType(MediaPlaybackType::Music)?;
        let music = display.MusicProperties()?;
        music.SetTitle(&HSTRING::from(track.title.unwrap_or("")))?;
        music.SetArtist(&HSTRING::from(track.artist.unwrap_or("")))?;
        music.SetAlbumTitle(&HSTRING::from(track.album.unwrap_or("")))?;
        let cover = track.cover.map(|cover| {
            let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(cover))?.join()?;
            display.SetThumbnail(&RandomAccessStreamReference::CreateFromFile(&file)?)
        });
        display.Update()?;
        let end = TimeSpan::from(track.duration.unwrap_or_default());
        self.timeline.SetStartTime(TimeSpan::default())?;
        self.timeline.SetMinSeekTime(TimeSpan::default())?;
        self.timeline.SetEndTime(end)?;
        self.timeline.SetMaxSeekTime(end)?;
        match cover {
            Some(Err(error)) => Err(io::Error::other(format!(
                "the cover could not be read: {error}"
            ))),
            _ => Ok(()),
        }
    }

    /// Shows what the player is doing, and where it is in the track.
    pub fn set_state(&self, playback: Playback, position: Duration) -> io::Result<()> {
        self.controls.SetPlaybackStatus(match playback {
            Playback::Playing => MediaPlaybackStatus::Playing,
            Playback::Paused => MediaPlaybackStatus::Paused,
            Playback::Stopped => MediaPlaybackStatus::Stopped,
        })?;
        self.timeline.SetPosition(TimeSpan::from(position))?;
        self.controls.UpdateTimelineProperties(&self.timeline)?;
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
        let _ = self
            .controls
            .DisplayUpdater()
            .and_then(|display| display.ClearAll());
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
    #[ignore = "shows a player in the system's media flyout, and may take the media keys: run it by hand"]
    fn a_hidden_window_takes_the_controls_and_shows_a_track_and_its_state() {
        let window = Window::open("Ritornel media test", |_| {}).unwrap();
        let (sent, _heard) = mpsc::channel();
        let controls = MediaControls::open(window.handle(), move |request| {
            let _ = sent.send(request);
        })
        .unwrap();
        let track = Track {
            title: Some("Salt Road"),
            album: Some("Field Notes"),
            duration: Some(Duration::from_secs(200)),
            cover: Some(Path::new(r"C:\no such cover.jpg")),
            ..Track::default()
        };
        assert!(controls.set_track(&track).is_err());
        controls
            .set_state(Playback::Paused, Duration::from_secs(20))
            .unwrap();
        drop(controls);
        drop(window);
    }
}
