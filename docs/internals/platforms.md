# Platforms

Ritornel was built on macOS first. Linux and Windows came early, while the code is still small, so that nothing comes to depend on macOS by accident. This guide covers what runs on each platform today, how OS-specific code is structured, and the decisions behind each port. How playback and the server work are covered in their own guides.

## Platform support

| | macOS | Linux | Windows | Headless hub |
|---|---|---|---|---|
| Server, library, scan, Last.fm | Yes | Yes | Yes | Yes, without Last.fm |
| Sound | CoreAudio | ALSA's `default` device (PipeWire or PulseAudio on a desktop) | WASAPI, shared mode | None (`--output null` plays into nothing) |
| Media keys and Now Playing | Now Playing | MPRIS | Not yet | None |
| Background service | launchd user agent | systemd user unit | Not yet | Any service manager running `serve` |
| A hub found by its `.local` name | Bonjour | With Avahi, and nss-mdns on a satellite | Yes | As Linux |
| A hub kept from idle sleep | `caffeinate -i` | `systemd-inhibit --what=idle` | Not yet | As Linux |
| Desktop app | Yes | Wayland or X11 | Yes | None |
| App menus | The menu bar | A button in the title bar | A button in the title bar | None |
| Data directory | `~/Library/Application Support/Ritornel` | `$XDG_DATA_HOME/ritornel` | `%LOCALAPPDATA%\Ritornel` | As Linux |

The headless hub is the server built for Linux with `--no-default-features`: no sound, no media session, no Last.fm, and no system library beyond libc.

The headless player is the server built for Linux with `--no-default-features --features audio-output`: sound through ALSA's `default` device, and no media session or Last.fm. It links `libasound` and libc. It is meant for a small board such as a Raspberry Pi, with `--name` saying what the speakers are called. `scripts/player-footprint.sh` reports its size and its memory at idle on the machine it runs on.

Sound goes through `cpal` everywhere, and media keys go through `souvlaki` on macOS and `zbus` on Linux.

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
  - it computes position when asked;
  - it emits `Seeked` when the position jumps.

  Each queue entry gets its own track ID so `SetPosition` can refuse a stale one. The bus name is `org.mpris.MediaPlayer2.ritornel`.
- **The background service is a systemd user unit, not a system unit,** for the same reason macOS uses a user agent: the user's session owns the session bus and the sound server. It restarts ten seconds after an unclean exit, matching the launchd agent.
- **The desktop app starts its server detached,** in its own session and with nothing tied to the app, so the server outlives it. A server the app started carries `RITORNEL_STARTED_BY_APP=1`, so Quit can distinguish it from one the app only found.

### Windows

- **The data directory is `%LOCALAPPDATA%`, not the roaming folder.** A library and its databases do not belong on a network profile.
- **The server stops cleanly** on Ctrl+C or Ctrl+Break, the console closing, signing out or shutting down, and a plain `taskkill` (via a hidden window in `ritornel-windows`).
- **The lock is one byte past the end of `server.lock`,** held exclusively. The PID in the file stays readable, and a client probes the lock with a shared lock on the whole file.
- **Files only the user can open** carry an access list naming that user alone, where Unix uses modes `0600` and `0700`. Folders Ritornel creates receive this ACL, and files created inside inherit it; an existing data directory keeps its prior access list, as on Unix.
- **The listen clock is `QueryInterruptTimePrecise`.** It counts through sleep as the tick count does, in 100 ns steps rather than 10 to 16 ms, so back-to-back listens meet.

## Verification

**Linux** (Arch, PipeWire, Hyprland):

- 44.1, 48, and 96 kHz FLAC and 44.1 kHz MP3 play at the right pitch and length, within 0.03 dB of the files.
- Pause, seek, next, previous, end of queue, and gapless joins match the source files.
- Reported position keeps step with what is heard over three minutes.
- Changing the default output moves the stream with no gap.
- MPRIS handles every call and reports the right track and position.
- The desktop app follows the server, and menus, shortcuts, and closing the window while playing were verified in the running app.
- The server uses 2–3% CPU and 28 MB while playing.

**Windows** (Windows 11, Realtek speakers, 48 kHz shared-mode mix):

- The server test suite passes.
- 44.1, 48, and 96 kHz files play at the right pitch; pause, seek, next, previous, end of queue, and gapless joins (including sample-rate changes) match the source files.
- Reported position runs ~55 ms ahead of what is heard (output latency) without drifting over three minutes.
- The desktop app follows the server; the server uses 1–2% of one core and 27 MB while playing.
- Because the standard library cannot open a directory handle on Windows to flush it, `sync_dir` in `apps/server/src/platform/windows.rs` is currently a no-op (each file inside a backup is flushed before rename, while the directory rename itself is tracked in [#111](https://github.com/ritornel/ritornel/issues/111)).

**Not yet verified on hardware:** hardware media keys or desktop media widgets, the systemd service across logout/login, real Bluetooth or HDMI output changes, headphone unplug/device loss on Windows, and a USB output unplugged behind PipeWire, which the stall rule ("Audio pipeline" in `docs/internals/playback.md`) is there to catch.

## Later

- **`cpal`'s native PipeWire host,** if ALSA's `default` device turns out to add latency or resample where PipeWire would not.
- **Exact output,** where the device follows each track's sample rate and bit depth unchanged. The pipeline keeps room for it; each platform needs its own exclusive device path (WASAPI exclusive mode on Windows, ALSA hardware device or PipeWire on Linux).
