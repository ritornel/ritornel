# Plugins

Optional integrations in Ritornel are compiled-in Rust crates behind narrow traits, toggled by Cargo features on `ritornel-server`. The core server owns the library, identity resolution, listening history, audio playback, and the wire contract (`ARCHITECTURE.md`); plugins extend that core without forcing every build to carry every dependency.

This guide covers the plugin crate boundary (`crates/ritornel-source`), the shipped Last.fm and MusicBrainz plugins (`crates/ritornel-lastfm`, `crates/ritornel-musicbrainz`), and how Cargo features keep the headless hub minimal. See [`docs/internals/server.md`](server.md) for server lifecycle and [`docs/internals/platforms.md`](platforms.md) for headless Linux builds.

## Plugin model

A plugin is a Rust crate compiled into `ritornel-server` behind an optional Cargo feature. Core crates (`ritornel-contract`, `ritornel-domain`, `ritornel-engine`) never depend on any plugin crate, and a plugin crate depends on no workspace crate except `ritornel-source`. `apps/server` is the only crate that wires enabled plugins into the running server.

```text
ritornel-source ◄── ritornel-engine
ritornel-source ◄── ritornel-lastfm ◄── ritornel-server (feature = "lastfm")
ritornel-source ◄── ritornel-musicbrainz ◄── ritornel-server (feature = "musicbrainz")
```

This boundary is checked in CI by `scripts/check-deps.py`:

- `ritornel-source` depends on no other workspace crate.
- `ritornel-lastfm` depends only on `ritornel-source` among workspace crates and never links `rusqlite`.
- `ritornel-engine` never depends on `ritornel-lastfm` or `scrobl`.
- `ritornel-musicbrainz` depends only on `ritornel-source` among workspace crates and never links `rusqlite`, and `ritornel-engine` never depends on it or on `reqwest`.
- `ritornel-server --no-default-features --features musicbrainz`, the hub, links none of `ritornel-lastfm`, `scrobl`, `souvlaki`, `zbus`, `cpal`, `alsa`, or `alsa-sys`.

## Traits and storage boundary

`crates/ritornel-source` defines the traits and wire-independent data types shared between the core engine and plugins. Two plugin traits exist today:

- **`HistorySource`** (`sync(&self, sink: &mut dyn Sink, full: bool) -> Result<Synced, Error>`) — Reads plays and loved tracks from an external source and delivers them in batches to the core.
- **`Catalogue`** (`name`, `disc(disc_id) -> Question`, `ask(&Question) -> Result<Reply, Error>`) — Asks a catalogue one question and returns its `Reply` exactly as it came: the HTTP status (200, or 404 for a thing it does not know) and the body. It only fetches: the core keeps the reply and reads it itself (see [`docs/internals/corrections.md`](corrections.md), "MusicBrainz lookups").
- **`Sink`** — The handle the core passes into `HistorySource::sync`. It reads the source's last stored cursor (`cursor`), commits a `Batch` of `ImportRecord` values together with a `Span` and new opaque `cursor` in a single transaction (`commit`), reconciles the current `Love` list (`loves`), reports `Progress`, and exposes cancellation (`stopping`).

Plugins do no database storage of their own. A history source parses its external format and passes both the verbatim raw payload (`ImportRecord::payload_json`) and the parsed facts (`source_key`, `played_at_ms`, `TrackText`, `Identifiers`) into `Sink`. The core stores both in `library.db` under the plugin's `history_endpoint` and `source_instance`. Because raw payloads live in `library.db`, history derivation and identity rebuilds never require the plugin to be present or reachable. A catalogue does not parse at all: it hands each reply over as it came, and the core keeps and reads it.

Errors returned across the trait (`ritornel_source::Error`) carry an `ErrorKind` (`Source` or `Sink`) and a single-line log-safe message that never contains credentials.

Traits in `ritornel-source` are treated as unstable until at least two plugins implement them.

## Shipped crates and Cargo features

| Crate or feature | Role |
|---|---|
| `crates/ritornel-source` | `HistorySource`, `Sink`, `Catalogue`, and data types (`ImportRecord`, `Batch`, `Span`, `Love`, `TrackText`, `Identifiers`, `Progress`, `Synced`, `Question`, `Reply`, `Error`). Zero workspace dependencies. |
| `crates/ritornel-lastfm` | Last.fm plugin built on `scrobl`. Implements `HistorySource` (`LastfmSource`) for chunked, page-checked history import and loved tracks, plus sign-in (`SignIn`, `Account`), Now Playing, and scrobbling (`scrobble` module). Exposes a `fake` feature providing a loopback mock server for integration tests. |
| `crates/ritornel-musicbrainz` | MusicBrainz as a `Catalogue`, on `reqwest` with rustls and a runtime of its own: a disc ID lookup (`DISC_INCLUDES`, CD stubs off) under the User-Agent `Ritornel/<version> ( https://ritornel.app )`, at most one request a second. Exposes a `fake` feature providing a loopback mock server for tests. |
| `ritornel-server` feature `musicbrainz` | Default-on feature (`dep:ritornel-musicbrainz`) that gives the server a catalogue for `corrections.lookup`. The hub is built with it. Without it, `corrections.lookup` answers `unavailable` with the reason `no_catalogue`. |
| `ritornel-server` feature `lastfm` | Default-on feature (`dep:ritornel-lastfm`) that wires Last.fm sign-in, sync jobs, Now Playing, and scrobbling into `apps/server`. Disabling it removes all Last.fm and `scrobl` code from the binary. |

`Lastfm` inside `ritornel-lastfm` creates its own multi-thread Tokio runtime with one worker thread (`"ritornel-lastfm"`) to drive `scrobl`'s async HTTP client behind synchronous calls, shutting it down in the background on `Drop` so callers can drop it safely from any thread. `Lastfm` syncs only when invoked; scheduling and job lifecycle live in `apps/server`.

Alongside `lastfm` and `musicbrainz`, `ritornel-server` gates `audio-output` and `media-session` as default features so a headless hub (`--no-default-features --features musicbrainz`) builds with no audio, D-Bus, or Last.fm dependencies. `ritornel-source` is always built in, since the server holds a catalogue as a `Catalogue` whether or not one is compiled.

## Decisions

- **Compiled-in crates over dynamic libraries or sidecars.** Rust has no stable ABI; dynamically loaded native libraries are unsound across compiler versions, break Tokio thread-local state across library boundaries, and conflict with notarized macOS and mobile signing rules. Separate plugin processes add per-OS binary distribution, IPC versioning, and signing overhead. Compiled-in crates behind traits keep builds portable and simple across macOS, Linux, and Windows.
- **Core never depends on plugins.** Keeping dependencies pointed strictly from plugins and the engine toward `ritornel-source` guarantees optional features can be compiled out cleanly and prevents external service types from leaking into `ritornel-domain` or `ritornel-engine`.
- **Verbatim plus parsed payloads through `Sink`.** Storing each plugin's raw payload alongside extracted fields decouples library durability and rebuilds from plugin code. Records from one plugin stay isolated under its own `history_endpoint` and `source_instance`.
- **Clients on the contract are not plugins.** `ritornelctl`, the desktop app, and any future external script or adapter communicate over the wire contract rather than linking into the server. A future integration that cannot be written in Rust or must run out-of-process can connect as an ordinary contract client without adding a separate plugin host.
- **Accepted trade-offs.** Compiled-in plugins share the server's address space (a panic or fatal failure in a plugin can affect the server process) and adding a new plugin requires rebuilding `ritornel-server`.

## Later

- `Question::kind` and `includes` leave room for questions other than a disc ID, such as a search by name, on the same trait.
- `scripts/check-deps.py` already enforces the one-way isolation pattern for optional crates and `--no-default-features` hub builds so future optional features follow the same boundary.
