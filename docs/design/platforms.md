# Platforms

Ritornel was built on macOS first. Linux and Windows came early, while the code is still small, so that nothing comes to depend on the Mac by accident. This note says what each platform has today, why it was done that way, and what is still missing. How playback and the server work is the subject of their own notes.

## What runs where

| | macOS | Linux | Windows | Headless hub |
|---|---|---|---|---|
| Server, library, scan, Last.fm | Yes | Yes | Yes | Yes, without Last.fm |
| Sound | CoreAudio | ALSA's `default` device, which is PipeWire or PulseAudio on a desktop | WASAPI, shared mode | None (`--output null` plays into nothing) |
| Media keys and Now Playing | Now Playing | MPRIS | Not yet | None |
| Background service | launchd user agent | systemd user unit | Not yet | Any service manager running `serve` |
| Desktop app | Yes | Wayland or X11 | Yes | None |
| App menus | The menu bar | A button in the title bar | A button in the title bar | None |
| Data directory | `~/Library/Application Support/Ritornel` | `$XDG_DATA_HOME/ritornel` | `%LOCALAPPDATA%\Ritornel` | As Linux |

The headless hub is the server built for Linux with `--no-default-features`: no sound, no media session, no Last.fm, and no system library beyond libc.

Sound goes through cpal everywhere, and the media keys through souvlaki on macOS and zbus on Linux.

## Where the code goes

What differs between systems sits in a `platform` module in each crate that needs one. It has one file per system, offering the same plain functions, and no `cfg` of an operating system appears outside it. [`apps/desktop/src/platform/mod.rs`](../../apps/desktop/src/platform/mod.rs) is the model. Calls into Win32 live in `crates/ritornel-windows`, the one crate allowed `unsafe`.

## Decisions

### Everywhere

- **Closing the window never stops the music.** Only Quit stops a server the app started, and a server it found running is never stopped. That holds even where closing the last window usually quits the app, as on Windows and Linux.
- **One set of menus.** The menus are defined once. macOS shows them in its menu bar. On Linux and Windows they open from a button in the title bar, and the app draws its context menus itself.
- **Shortcuts follow the system.** A shortcut that means the same thing everywhere is written once with GPUI's `secondary-` modifier: ⌘ on a Mac and Ctrl elsewhere. Quit, Back and moving through text differ by system, so each system's file sets them.
- **Linking audio is optional.** The server's `audio-output` feature, on by default, is what links an audio library. On Linux that is libasound, so a default build needs ALSA's development files (`alsa-lib`, or `libasound2-dev`). The hub is built without the feature and needs none. CI's `hub` job builds it with pkg-config pointed at nothing, and `scripts/check-deps.sh` fails if cpal, ALSA, zbus or souvlaki reach the hub's dependency tree.

### Linux

- **Sound goes through ALSA's `default` device, not cpal's PipeWire or PulseAudio hosts.** ALSA needs only libasound, and on a desktop with a sound server, `default` is that server. The output opens as 32-bit float at the device's own rate and channels, as on the Mac.
- **MPRIS is served with zbus directly, not through souvlaki.** souvlaki's Linux backend wakes every 10 ms while a player is registered and panics when there is no session bus. Its position doesn't move between updates, and it never emits `Seeked`. Ours does better on all four:
  - it wakes only for a change or a call;
  - with no session bus there is simply no media session, logged only at debug, as on a hub, in a container, or in a login with no desktop;
  - it works out the position when asked;
  - it emits `Seeked` when the position jumps.

  Each queue entry gets its own track ID, so `SetPosition` can refuse a stale one. The bus name is `org.mpris.MediaPlayer2.ritornel`.
- **The background service is a systemd user unit, not a system one,** for the same reason macOS uses a user agent: the user's session has the session bus and the sound server. It restarts ten seconds after an exit that was not clean, as the launchd agent does.
- **The desktop app starts its server detached,** in its own session and with nothing tied to the app, so the server outlives it. A server the app started carries `RITORNEL_STARTED_BY_APP=1`, so that Quit can tell it from one the app only found.

### Windows

- **The data directory is `%LOCALAPPDATA%`, not the roaming folder.** A library and its databases don't belong on a network profile.
- **The server stops cleanly** on any of these:
  - Ctrl+C or Ctrl+Break;
  - the console closing;
  - signing out or shutting down;
  - a plain `taskkill`, through a hidden window.
- **The lock is one byte past the end of `server.lock`,** held exclusively. The pid in the file stays readable, and a client asks for the lock with a shared lock on the whole file.
- **Files only the user can open** have an access list that names that user alone, where Unix would use modes `0600` and `0700`. Folders Ritornel makes are given one, and what is made inside them inherits it.
- **The listen clock is `QueryInterruptTimePrecise`.** It counts through sleep as the tick count does, but in 100 ns steps rather than 10 to 16 ms, so back-to-back listens meet.

## Tested

**Linux** (Arch, PipeWire, Hyprland).

To test sound, a recording of the default output was compared with the files:
- 44.1, 48 and 96 kHz FLAC, and 44.1 kHz MP3, play at the right pitch and length, within 0.03 dB of the files.
- Pause, seek, next, previous, the end of the queue and gapless joins are right.
- The reported position keeps step with what is heard over three minutes.
- Changing the default output moves the stream with no gap.
- MPRIS takes every call and reports the right track and position.
- The desktop app follows the server.
- The server uses 2 to 3% CPU and 28 MB while playing.

The menus, the shortcuts, and closing the window with the music still playing were checked in the running app.

**Windows** (Windows 11, Realtek speakers, a 48 kHz shared-mode mix). The server's 839 tests pass.

To test sound, a loopback recording was compared with the files:
- 44.1, 48 and 96 kHz files play at the right pitch.
- Pause, seek, next, previous and the end of the queue are right.
- Every track change is gapless, including changes of sample rate.
- The reported position runs about 55 ms ahead of what is heard, which is the output latency, and doesn't drift over three minutes.
- The desktop app follows the server.
- The server uses 1 to 2% of one core and 27 MB while playing.

The absolute level couldn't be judged, because the Realtek driver applies its own gain.

**Not yet tested anywhere:**
- a hardware media key, or a desktop's media widget;
- the systemd service across a logout and login;
- a real Bluetooth or HDMI output change;
- headphones plugged in, or an output that goes away, on Windows.

## To do

Ordered by how much a person using it would notice. Remove an item when it lands.

### Everywhere

- **The audio output never sleeps after the first play.** Once something has played, the stream stays open and running, paused or stopped, until the device is lost (`OnDemand` in the engine's `playback/audio/mod.rs`). That has three costs:
  - battery on a laptop;
  - it keeps Bluetooth headphones and USB DACs awake;
  - the idle server uses about 1.3% CPU.

  This was seen on Linux. The code is shared, so the others are likely the same. The fix: close the stream after a while paused or stopped, and open it again on play.
- **Sound doesn't follow a new default output.** The stream stays on the device it opened (`device.rs`: "a default that changes is not rebuilt yet"). On Windows, plugging in headphones or docking will likely leave the music on the speakers. Linux is spared, because ALSA's `default` is PipeWire, and PipeWire moves the stream itself. The Mac is not checked.
- **Losing the output isn't logged.** The player pauses and tells clients, but the log says nothing.
- **The player pauses when the output goes away, even when the system has already moved the stream** to another device. Pausing when headphones are unplugged is right. Pausing for a move the system made itself is an open question.
- **The "no server" page says "on this Mac"** on every system.
- **Flaky tests,** each seen failing once:
  - `library_create_makes_the_running_server_a_hub_and_a_second_one_is_refused` (`apps/ritornelctl/tests/status.rs`) read "Watching: off" in CI on Linux, right after the library was made.
  - `clients_looking_at_the_lock_are_waited_out_and_do_not_refuse_a_server` (`apps/server/src/lock.rs`) was refused with the lock in use, on a busy Mac during the full check.

### Linux

- **Touchpad scrolling has no momentum.** GPUI's Wayland backend has no kinetic scrolling and drops the end of a gesture ([zed-industries/zed#14473](https://github.com/zed-industries/zed/issues/14473)). The likely fix:
  1. have GPUI report when a scroll starts and when the finger lifts;
  2. coast in the app with GTK 4's kinetic scrolling constants;
  3. check it against a real fling in a GTK app.
- **`mise run install` builds the app but installs neither it nor a launcher entry.** Until it does, MPRIS has no `DesktopEntry` for a desktop to show the app's icon.
- **When a server the app started fails before it's ready, the app shows only its exit status,** since the server's output goes nowhere so that it can outlive the app.
- **No key opens the menus** (Alt or F10), and there are no window decorations for compositors that draw none.
- **MPRIS `Stop` pauses** rather than stopping.

### Windows

- **Clicks while the CPU is busy.** With every core loaded, playback drops 6 to 15 ms at a time, one to five times a minute, and runs that much late afterwards. A plain WASAPI player under the same load doesn't. The cause isn't known yet. One suspect is that the decode thread filling the audio thread's half-second buffer runs at normal priority. Linux shows similar dropouts under load, but there `pw-play` drops as often.
- **No media keys or Now Playing.** These would be the system's media transport controls, tied to the hidden window `serve` already opens.
- **No background service.** This would be a task at sign-in, in the user's session, where the media keys and the default audio device are.
- **No CI job and no mise tasks.** Run `pwsh -File scripts/desktop.ps1` with PowerShell 7 or later.
- **A backup's folder name isn't flushed to disk.** Each file in a backup is flushed before the folder is renamed into place, but the rename itself isn't, since the standard library can't open a folder to flush it (`sync_dir` in the server's `platform/windows.rs` does nothing). A loss of power just after `ritornelctl backup` reports success could leave the backup under its unfinished dotted name. The fix: rename it with `MoveFileExW` and `MOVEFILE_WRITE_THROUGH`, through `ritornel-windows`.
- **A data directory that already exists keeps its access list,** as one keeps its mode on Unix. The default under `%LOCALAPPDATA%` is the user's alone already, so this only matters for a `--data-dir` pointed at a shared folder.

### Later

- **cpal's native PipeWire host,** if ALSA's `default` device turns out to add latency or resample where PipeWire wouldn't.
- **Exact output,** where the device follows each track's rate and depth and the samples reach it unchanged. It is parked on every platform, and the pipeline keeps it possible. Each platform needs its own way of taking the device: exclusive mode on Windows, and ALSA's hardware device or PipeWire on Linux.
