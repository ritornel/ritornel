# Ritornel

A music player for a collection you own.

Ritornel reads the music files you have, plays them, and keeps a record of everything you listen to. That record stays on your machine, and the app uses it to show you your own history: when you found a record, the years you played it most, what you were listening to on this day.

It is a server and its clients. `ritornel-server` owns the library, the player and the history. The desktop app and the command-line client, `ritornelctl`, talk to it over a local socket. All of it is written in Rust, and the desktop app is drawn with [GPUI](https://www.gpui.rs).

## Where it stands

Ritornel is early. Its author uses it every day and no one else does yet. There are no releases and nothing to download: to try it, you build it. Anything may change without notice, the database format included.

What works today:

- **A library from your folders.** A scan reads FLAC, ALAC, AAC, MP3, Ogg Vorbis, Opus, WAV and AIFF files, works out which files are the same recording, and then watches the folders for changes.
- **Playback.** Gapless, with ReplayGain, shuffle and repeat, a queue, the media keys and the system's Now Playing.
- **A desktop app.** Walls of artists and albums, a page for each artist and album, search, a queue, a full-window view of what is playing, and a page for your listening history.
- **A listening history.** Every play is kept locally. A Last.fm history can be imported, and plays can be scrobbled, with an API key of your own.

macOS is the furthest along, and the only platform where all of this works today. Linux and Windows are in progress.

Planned:

- Syncing between machines, from a hub
- Syncing to portable players
- Tools for fixing tags and artwork

## Building it

For now you need a Mac with Xcode's command-line tools, and [mise](https://mise.jdx.dev), which installs everything else at the versions this repository pins.

```sh
mise trust
mise install
mise run check         # everything CI runs
mise run desktop:run   # the app on an invented library of 8,600 albums
mise run install       # a release build, installed for everyday use
```

`mise run desktop:run` touches no music or library of yours: it makes a library of invented artists and albums in `apps/desktop/.invented` and serves it silently on a port of its own. It does share the installed app's remembered view settings, such as which wall is open. The invented library has no audio. `scripts/desktop.sh tones` runs the app on a small one that plays.

`mise run install` puts `ritornel-server` and `ritornelctl` in `~/.local/bin` and `Ritornel.app` in `/Applications`. The app starts its own server and keeps its data in `~/Library/Application Support/Ritornel`. To add music, open Settings in the app, or run `ritornelctl library create` and then `ritornelctl scan <folder>`.

## Last.fm

Importing a history and scrobbling need a Last.fm API key of your own, which you can ask for at [last.fm/api](https://www.last.fm/api/account/create). Put the key and its secret in a file named `lastfm.env` in the data directory, readable by you alone:

```sh
SCROBL_API_KEY=your key
SCROBL_API_SECRET=your secret
```

The server reads the file when it starts, so start it again: quit and reopen the app, or restart a server you run yourself. Then sign in from the app's Settings, or with `ritornelctl lastfm signin`.

## What is where

| Path | What it is |
| --- | --- |
| `apps/server` | `ritornel-server`: the socket, the jobs, the player and the media keys |
| `apps/ritornelctl` | The command-line client |
| `apps/desktop` | The desktop app, a Cargo workspace of its own |
| `crates/ritornel-contract` | Every call, event and wire type, defined once |
| `crates/ritornel-client` | How a client finds a server and talks to it |
| `crates/ritornel-domain` | Reading tags, deciding which files are one recording, and deriving plays, with no I/O |
| `crates/ritornel-engine` | The databases, the scan and the audio pipeline |
| `crates/ritornel-source`, `crates/ritornel-lastfm` | The plugin traits, and Last.fm as the first plugin |
| `contract/contract.ts` | The contract as TypeScript, generated from the Rust |
| `schema/` | The databases' schemas, generated from the migrations |
| `fixtures/` | Small synthetic audio files for the tests |

[`AGENTS.md`](AGENTS.md) has the commands and the few rules, for people and for coding agents.

## Design notes and history

Comments throughout the code cite design notes, such as `docs/design/server.md`, decisions by number, and now and then a prototype or a spike. All of that was written while Ritornel was being worked out in private. The notes are being added as each is brought up to date; the first is [`docs/design/platforms.md`](docs/design/platforms.md), what runs on each platform and what each still lacks.

For the same reason this repository's history begins as a few large commits, one for each milestone of that private work. An earlier desktop app, built with Tauri and Svelte, came before the GPUI one. Some comments mention it as `apps/desktop-tauri`, and it is not here either.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). In short: open an issue before a large change.

## Licence

MIT. See [`LICENSE`](LICENSE). A few files are adapted from WebKit and from Zed, and carry their original notices; [`LICENSES/README.md`](LICENSES/README.md) lists them.
