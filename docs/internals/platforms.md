# Platforms

Ritornel was built on macOS first. Linux and Windows came early, while the code is still small, so that nothing comes to depend on macOS by accident. This guide covers what runs on each platform today, how OS-specific code is structured, and the decisions behind each port. How playback and the server work are covered in their own guides.

## Platform support

| | macOS | Linux | Windows | Headless hub |
|---|---|---|---|---|
| Server, library, scan, Last.fm | Yes | Yes | Yes | Yes, without Last.fm |
| Sound | CoreAudio | ALSA's `default` device (PipeWire or PulseAudio on a desktop) | WASAPI, shared mode | None (`--output null` plays into nothing) |
| Media keys and Now Playing | Now Playing | MPRIS | System Media Transport Controls | None |
| Background service | launchd user agent | systemd user unit | Task Scheduler task at sign-in | Any service manager running `serve` |
| A hub found by its `.local` name | Bonjour | With Avahi, and nss-mdns on a satellite | Yes | As Linux |
| A hub kept from idle sleep | `caffeinate -i` | `systemd-inhibit --what=idle` | Not yet | As Linux |
| Desktop app | Yes | Wayland or X11 | Yes | None |
| Installed app (`mise run install`) | `/Applications/Ritornel.app` | `~/.local/lib/ritornel`, with a launcher entry and an icon | Not yet | None |
| App menus | The menu bar | A button in the title bar | A button in the title bar | None |
| Touchpad scrolling that coasts | AppKit's momentum | On Wayland, GTK 4's coast in patched GPUI; not on X11 | Direct Manipulation's inertia | None |
| Data directory | `~/Library/Application Support/Ritornel` | `$XDG_DATA_HOME/ritornel` | `%LOCALAPPDATA%\Ritornel` | As Linux |

The headless hub is the server built for Linux with `--no-default-features`: no sound, no media session, no Last.fm, and no system library beyond libc.

The headless player is the server built for Linux with `--no-default-features --features audio-output`: sound through ALSA's `default` device, and no media session or Last.fm. It links `libasound` and libc. It is meant for a small board such as a Raspberry Pi, with `--name` saying what the speakers are called. `scripts/player-footprint.sh` reports its size and its memory at idle on the machine it runs on.

Sound goes through `cpal` everywhere, and media keys go through `souvlaki` on macOS, `zbus` on Linux, and the `windows` crate's WinRT bindings in `ritornel-windows` on Windows.

## Code organization

What differs between systems sits in a `platform` module in each crate that needs one. It has one file per system offering the same plain functions, and no `cfg` of an operating system appears outside it except in tests whose data is system-specific (such as an absolute path or how a process is killed). `apps/desktop/src/platform/mod.rs` is the model. Calls into Win32 live in `crates/ritornel-windows`, the one crate in the main workspace allowed `unsafe`.

## Decisions

### Everywhere

- **Closing the window never stops the music.** Only Quit stops a server the app started, and a server it found running is never stopped. That holds even on Windows and Linux, where closing the last window usually quits an app.
- **One set of menus.** The menus are defined once. macOS shows them in its menu bar. On Linux and Windows they open from a button in the title bar, and the app draws its context menus itself.
- **Shortcuts follow the system.** A shortcut that means the same thing everywhere is written once with GPUI's `secondary-` modifier: `⌘` on macOS and `Ctrl` elsewhere. Quit, Back, and moving through text differ by system, so each system's file sets them.
- **Linking audio is optional.** The server's `audio-output` feature, on by default, is what links an audio library. On Linux that is `libasound`, so a default build needs ALSA's development files (`alsa-lib` or `libasound2-dev`). The hub is built without the feature and needs none. CI's `hub` job builds it with `PKG_CONFIG_LIBDIR=/nonexistent`, and `scripts/check-deps.sh` fails if `cpal`, ALSA, `zbus`, or `souvlaki` reach the hub's dependency tree.

### Linux

- **The arm64 player is built natively in CI, not cross-compiled.** CI's `player-arm64` job builds and tests the headless player on GitHub's arm64 Linux runner, which links ALSA as any Linux machine does. Cross-compiling would need an aarch64 sysroot with ALSA's development files. The job runs beside the others, so the check takes no longer.
- **Sound goes through ALSA's `default` device, not `cpal`'s PipeWire or PulseAudio hosts.** ALSA needs only `libasound`, and on a desktop with a sound server, `default` routes to that server. The output opens as 32-bit float at the device's own rate and channels, as on macOS.
- **MPRIS is served with `zbus` directly, not through `souvlaki`.** `souvlaki`'s Linux backend wakes every 10 ms while a player is registered, panics when there is no session bus, does not advance reported position between updates, and never emits `Seeked`. The `zbus` implementation avoids all four:
  - it wakes only for a change or a call;
  - with no session bus there is simply no media session, logged only at debug, as on a hub, in a container, or in a login with no desktop;
  - a bus that does not take an update within a second is left, with a warning, and MPRIS stays off until the server starts again, so a bus that has stopped reading holds neither the media session nor the server's stop;
  - it computes position when asked;
  - it emits `Seeked` when the position jumps.

  Each queue entry gets its own track ID so `SetPosition` can refuse a stale one. The bus name is `org.mpris.MediaPlayer2.ritornel`.
- **The background service is a systemd user unit, not a system unit,** for the same reason macOS uses a user agent: the user's session owns the session bus and the sound server. It restarts ten seconds after an unclean exit, matching the launchd agent.
- **The installed app is a folder in `~/.local/lib`, not in the data directory.** `scripts/install.sh` copies the bundle `scripts/desktop.sh app` makes, the app with its server beside it, to `~/.local/lib/ritornel`, staged and swapped in as on macOS. `$XDG_DATA_HOME/ritornel` is the data directory and is never written by an install: on both systems the script replaces only a folder that already holds the app, and stops before writing anything when another folder is in the way. Without the desktop app's libraries, `scripts/desktop.sh app` skips the app and the script installs the programs alone. The launcher entry and the 512-pixel icon go in `$XDG_DATA_HOME/applications` and `$XDG_DATA_HOME/icons/hicolor`, named `dev.ritornel.desktop` after the window's app ID, which is how a launcher, a Wayland compositor (`app_id`), X11 (`WM_CLASS`) and MPRIS (`DesktopEntry`) match the window and the media session to the entry and its icon.
- **A touchpad's scroll on Wayland coasts in a patched copy of GPUI's Linux backend, not in the app.** GPUI ignores Wayland's `axis_stop`, so its scroll events carry no phase and the page stopped dead at the lift. [ritornel/gpui](https://github.com/ritornel/gpui), Ritornel's copy of the published `gpui-pre-linux`, put in its place with `[patch.crates-io]` at a pinned commit, reports the scroll as `Started`, `Moved` and `Ended` and then sends the coast as `Moved` events, one a frame, as macOS sends momentum and Windows inertia. Coasting there moves every scrolling view the same way, GPUI's own lists included, where the app would need a coast in each page. The coast, in the fork's `src/linux/wayland/kinetic.rs`, follows the numbers GTK 4 uses in `GtkScrolledWindow`: the speed over the last 150 ms before the lift, slowing at a rate of 4 a second, a flick that catches a coast adding to it, and stopping on a click, the pointer leaving the window, or fingers resting on the touchpad where the compositor offers the pointer gestures protocol's version 3. The app pins the commit of a `patched/v<version>-<n>` tag there, of the same version as its other `gpui-pre-*` pins, and the fork's README says how a new snapshot gets its own patched branch. A `[patch]` that stops matching the pins is only a warning, so `scripts/desktop.sh check` fails unless Cargo resolves `gpui-pre-linux` for Linux from that repository. X11 has no lift event to start a coast from.
- **The desktop app starts its server detached,** in its own session and with nothing tied to the app, so the server outlives it. A server the app started carries `RITORNEL_STARTED_BY_APP=1`, so Quit can distinguish it from one the app only found.

### Windows

- **The data directory is `%LOCALAPPDATA%`, not the roaming folder.** A library and its databases do not belong on a network profile.
- **The server stops cleanly** on Ctrl+C or Ctrl+Break, the console closing, signing out or shutting down, and a plain `taskkill` (via a hidden window in `ritornel-windows`).
- **The lock is one byte past the end of `server.lock`,** held exclusively. The PID in the file stays readable, and a client probes the lock with a shared lock on the whole file.
- **Files only the user can open** carry an access list naming that user alone, where Unix uses modes `0600` and `0700`. Files and folders Ritornel creates get this list in the call that creates them (`CreateFileW` and `CreateDirectoryW` with a protected descriptor, in `ritornel_windows::private`), so no other user can open one before it is closed. A file system that keeps no access list, such as FAT or exFAT, is refused rather than written to.
- **A private file's privacy comes from its folder.** SQLite creates a database's `-wal` and `-shm` files itself, and on Windows they take their access list from the folder, where on Unix SQLite gives them the database's mode. So once a server holds the data directory's lock, it closes the directory and its `logs` folder unless only the user can open what is made in them (`close_dir_to_others`, `ritornel_windows::private::close_folder`). The files directly in the folder keep the access they had, rather than take the new list, so the checks of the local credential and of `lastfm.env` still see that others could read them, and the credential is replaced. A folder another user owns, or whose list others may change, is refused, since a handle opened to change it outlasts a new list. On Unix a folder that is there keeps its mode.
- **A private file is owned by the user, the system or the administrators.** An owner may always read and change an access list, so `is_private` checks the owner as well as the entries, and on a folder it also counts the entries the folder passes on to new files. Closing a file another user owns is refused. The Last.fm credentials file is held to the same check.
- **A finished backup's rename asks to be written through to the disk** (`MoveFileExW` with `MOVEFILE_WRITE_THROUGH`, as `ritornel_windows::file::rename_through`), where Unix syncs the folder that holds it. A folder handle opened for writing can be flushed on NTFS, but that is not promised on every file system a backup may go to, such as a network share, and a flush that failed would discard a backup that is whole. So `sync_dir` does nothing on Windows, and `rename_dir` returns once Windows says the dotted folder has its plain name on the disk. Microsoft states that guarantee plainly only for a move that copies; a rename on one volume relies on the file system, on NTFS its journal, and has not been tested against a cut in power. Each file inside was flushed before. A path too long for its plain form is passed in the `\\?\` form, as the standard library passes it, and the rename never replaces what is at the new name, an empty folder included.
- **The background service is a Task Scheduler task at sign-in, not a Windows service.** A service runs in session 0, away from the user's default audio device and media controls, as a system daemon would on macOS. Each user has a task of their own, `dev.ritornel.server.<user>`, since Task Scheduler's names are the machine's. It starts with the user's sign-in token at normal priority; Task Scheduler's default priority is below normal, which a player cannot have. It runs `conhost.exe --headless`, so no console window opens, and that runs `ritornel-server service run`, the supervisor (`windows_service.rs`). The task's state and definition are read through PowerShell's `Get-ScheduledTask`, which, unlike `schtasks`, tells a missing task from a failed question and names the state in English on any system.
- **The supervisor, not Task Scheduler, starts the server again.** The headless console does not pass on its program's exit code, so Task Scheduler cannot tell a crash from a clean stop. The supervisor runs `serve --owned` and starts it again ten seconds after an exit that was not clean, as the systemd unit does, writing why to `service.log`, which only the user can read. A clean stop, as by `ritornelctl server stop`, ends it until the next sign-in. Ending the task in Task Scheduler ends only the console, so `service uninstall` and a reinstall ask the supervisor's hidden window, `Ritornel service`, to close instead: it closes the server's input, waits up to 15 seconds for it to stop, and then ends it, as systemd does. `install` returns once the new supervisor has opened its window, which it does once the data directory and its log are ready, and `uninstall` removes the task before it stops the supervisor, so nothing starts it again. A task running in another session, whose window cannot be reached from this one, is refused rather than reported as removed.
- **The media keys and the media flyout come from the System Media Transport Controls, tied to the server's hidden window,** the one that hears `taskkill` and the end of the session. Windows delivers their requests on a thread of its own, so they need neither the main thread nor the runtime, and the controls are let go as the server stops, while the window is still open. Play, pause, stop, next, previous and a position chosen on the flyout's bar reach the player; stop pauses, as on macOS, and a position before the start is ignored.
- **The controls are `ritornel_windows::media`, not `souvlaki`'s Windows backend.** `souvlaki` keeps a field the next track does not have, such as the last album, has no way to clear a cover, and stops the whole update on a cover it cannot load. The wrapper clears what was shown before each track, reads its cover on a thread of its own and shows it only if that track is still the one shown, so a slow disk never holds up the keys, and sends the track's length with each position, so the two always agree. It registers nothing if any step fails. Its only unsafe call is `GetForWindow`, which ties the controls to the window.
- **The listen clock is `QueryInterruptTimePrecise`.** It counts through sleep as the tick count does, in 100 ns steps rather than 10 to 16 ms, so back-to-back listens meet.

## Verification

**Linux** (Arch, PipeWire, Hyprland):

- 44.1, 48, and 96 kHz FLAC and 44.1 kHz MP3 play at the right pitch and length, within 0.03 dB of the files.
- Pause, seek, next, previous, end of queue, and gapless joins match the source files.
- Reported position keeps step with what is heard over three minutes.
- Changing the default output moves the stream with no gap.
- MPRIS handles every call and reports the right track and position.
- The desktop app follows the server, and menus, shortcuts, and closing the window while playing were verified in the running app.
- The coast's arithmetic is tested in ritornel/gpui's CI.
- The server uses 2–3% CPU and 28 MB while playing.

**Windows** (Windows 11, Realtek speakers, 48 kHz shared-mode mix):

- The server test suite passes.
- CI runs clippy and the whole workspace's tests on GitHub's `windows-latest` runner, and checks, tests and builds the desktop app there with `scripts/desktop.ps1 check`.
- 44.1, 48, and 96 kHz files play at the right pitch; pause, seek, next, previous, end of queue, and gapless joins (including sample-rate changes) match the source files.
- Reported position runs ~55 ms ahead of what is heard (output latency) without drifting over three minutes.
- The desktop app follows the server; the server uses 1–2% of one core and 27 MB while playing.
- The background service installs, runs with no console window at normal priority, starts the server again 10 seconds after it is killed, stops for `ritornelctl server stop`, is replaced while running, and uninstalls at once after an install with nothing left running. A real sign-out and sign-in has not been tried.
- The media controls, driven through Windows' own session manager (`GlobalSystemMediaTransportControlsSessionManager`) on a server playing to `--output null`, show the track, its album, its cover and its length, and clear what the track before had. Play, pause, a position on the bar, and next reach the player, and the controls leave when the server stops. Hardware media keys and the flyout itself have not been tried by hand.

**arm64 Linux** (the headless player in release, on GitHub's Ubuntu 24.04 arm64 runner, measured by `scripts/player-footprint.sh`):

- The engine's and the server's tests pass, the decoding ones bit for bit.
- `ritornel-server` is 18 MiB as built and 13 MiB stripped, and links `libasound` and libc only.
- The server holds 11 MB resident at idle with an empty data directory, 2 MB of it its own memory and the rest its mapped program.

**Not yet verified on hardware:** hardware media keys or desktop media widgets, the systemd service across logout/login, real Bluetooth or HDMI output changes, headphone unplug/device loss on Windows, the headless player on a Raspberry Pi, and a USB output unplugged behind PipeWire, which the stall rule ("Audio pipeline" in `docs/internals/playback.md`) is there to catch.

## Later

- **`cpal`'s native PipeWire host,** if ALSA's `default` device turns out to add latency or resample where PipeWire would not.
- **Exact output,** where the device follows each track's sample rate and bit depth unchanged. The pipeline keeps room for it; each platform needs its own exclusive device path (WASAPI exclusive mode on Windows, ALSA hardware device or PipeWire on Linux).
