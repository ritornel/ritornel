# Ritornel Architecture

Ritornel is a local-first music player built as a server and its clients. `ritornel-server` owns the library database, the audio pipeline, and the listening history; every client—the GPUI desktop app and `ritornelctl`—talks to it over a single local contract.

## Shape

```text
clients                          server
desktop app (GPUI) ──┐
                     ├─ contract (JSON-RPC over WS + HTTP media) ─► ritornel-server
ritornelctl        ──┘                                                ├── library.db (catalog, evidence, history)
                                                                      ├── server.db  (identity, playback, jobs, outbox)
                                                                      └── audio output & media session (optional)
```

Every client uses the same wire contract. The desktop app starts a bundled `ritornel-server` as a separate process (or connects to one already running) and has no in-process shortcut into the engine or database.

The server can also be built without its default features but MusicBrainz (`--no-default-features --features musicbrainz`) as a headless Linux hub: no audio output, no media session, no Last.fm plugin, and no system library beyond libc. `library.db` and `server.db` already include a server identity, hub pointer/epoch, server registry, and delivery outbox so a library can be owned by one server while listens originate elsewhere.

## Workspace and dependency direction

The main Cargo workspace (`Cargo.toml`) holds ten library crates and two binaries. The desktop app (`apps/desktop`) is an excluded Cargo workspace of its own so the server and headless hub build without GPUI or any desktop library.

| Path | Role |
|---|---|
| `crates/ritornel-build` | Build-script dependency for all three binaries. Computes their display version from release tags or explicit build inputs. |
| `crates/ritornel-contract` | Every RPC call, event, and wire type, defined once (`methods.rs`). Generates `contract/contract.ts`. |
| `crates/ritornel-domain` | Pure logic: tag extraction, normalization, recording resolution, catalog projection, history derivation, scrobble matching, and copy selection. No I/O, clock, or SQLite. |
| `crates/ritornel-engine` | SQLite stores, migrations, library scanning, cover art, audio pipeline, backups, record health, and export. No networking. |
| `crates/ritornel-client` | Shared Rust client: server discovery/launch and typed contract calls. |
| `crates/ritornel-source` | Traits and wire-independent types for optional external-source plugins (`HistorySource`, `Sink`, `Catalogue`). |
| `crates/ritornel-device` | The trait a device family implements for syncing to a player or a folder (`Session`), and its wire-independent types. Depends on no workspace crate. |
| `crates/ritornel-lastfm` | Last.fm plugin implementing `ritornel-source` behind the server's `lastfm` feature. |
| `crates/ritornel-musicbrainz` | MusicBrainz plugin implementing `ritornel-source`'s `Catalogue` behind the server's `musicbrainz` feature. |
| `crates/ritornel-windows` | Safe wrappers around the few Win32 calls the standard library cannot make (byte-range locks, owner-only ACLs, file identity, interrupt clock, hidden shutdown window, and, with its `media` feature, the system media transport controls). Empty on non-Windows. |
| `apps/server` | `ritornel-server`: socket listener, auth/sessions, job runner, file watcher, media keys/Now Playing, and platform service integration. |
| `apps/ritornelctl` | Command-line client over `ritornel-client`. |
| `apps/desktop` | GPUI desktop app (`ritornel-desktop-gpui`), built on `ritornel-client` and `ritornel-contract`. |

Dependencies point strictly one way, enforced by `scripts/check-deps.py` in CI:

```text
ritornel-contract ◄── ritornel-domain ◄── ritornel-engine ◄── ritornel-server
ritornel-contract ◄── ritornel-client ◄── ritornelctl, ritornel-desktop-gpui
ritornel-source   ◄── ritornel-engine
ritornel-source   ◄── ritornel-lastfm ◄── ritornel-server (optional feature)
ritornel-source   ◄── ritornel-musicbrainz ◄── ritornel-server (optional feature)
ritornel-device   ◄── ritornel-engine
```

Key boundary rules:

- No client (`ritornel-client`, `ritornelctl`, `ritornel-desktop-gpui`) links `ritornel-engine` or `rusqlite`.
- `ritornel-domain` does no I/O and never links `rusqlite`.
- The core engine never depends on any plugin crate (`ritornel-lastfm`, `ritornel-musicbrainz`, `scrobl`, `reqwest`), and among workspace crates a plugin depends only on `ritornel-source`.
- Every crate in the main workspace forbids `unsafe_code` except `ritornel-windows`.
- OS-specific code sits in a `platform` module per crate (one file per OS behind identical plain functions); no OS `cfg` appears outside it except in platform-specific tests. See [`docs/internals/platforms.md`](docs/internals/platforms.md).

## Core invariants

- **Servers own real resources.** Only `ritornel-server` touches SQLite databases, audio files, or audio output devices. One server holds an exclusive lock on a data directory at a time.
- **Two SQLite databases.** `library.db` holds the library (catalog, evidence, file locations, listening history, ratings); `server.db` holds server-local state (server identity, sessions, persisted playback queue, jobs, listen staging, outbox). Keeping them separate lets a library move cleanly between machines.
- **Durable vs. rebuildable.** Raw source payloads, identity decisions/exclusions/assignments, user edits, raw history observations, ratings, and corrections are durable and never overwritten in place. Extracted claims, displayed metadata, FTS search indexes, and derived play events are rebuildable projections.
- **Insert-only bindings and append-only history.** A file's binding to a recording is superseded by a new `assignment` row rather than updated in place, and durable history rows point at a `source_item` rather than directly at a `recording`, so identity fixes re-bind history cleanly on rebuild.
- **Hands-off files.** Ritornel reads music files and never modifies, renames, or deletes them. What it writes to a sync target is a copy, inside that target's music folder and its own `.ritornel/` folder.
- **Migrations are immutable.** Once a numbered migration lands on `main`, it is never edited; schema changes add a new migration file and regenerate `schema/*.sql` (`mise run schema`).
- **Generated snapshots stay in sync.** `schema/library.sql`, `schema/server.sql`, and `contract/contract.ts` are generated from code and verified by tests in `mise run check`.

## Vocabulary

| Term | Meaning |
|---|---|
| **Server** | The `ritornel-server` process that owns databases, files, and audio output on one machine. |
| **Hub** | The single server allowed to write a library database (identified by a hub pointer and epoch in `library.db`). |
| **Client** | A program (`ritornel-desktop-gpui`, `ritornelctl`) talking to one server through the contract. |
| **Contract** | The single schema of JSON-RPC calls, events, and HTTP media endpoints defined in `ritornel-contract`. |
| **Core** | What every server installation includes: library, identity, history, playback, and the contract. |
| **Plugin** | An optional crate compiled behind a Cargo feature and trait (such as `ritornel-lastfm` behind `ritornel-source`). |
| **Recording** | One musical performance or mix. Remasters share a recording; remixes do not. |
| **Release / Release group** | A release is an album as published; a release group gathers editions of the same album. |
| **Track appearance** | A recording at a specific position on a release (or without a release for loose files). |
| **Source item** | One item reported by a source (a scanned file or an imported scrobble), with its raw payload kept by revision. |
| **Audio asset** | One distinct encoded audio stream, keyed by an audio-only BLAKE3 hash so retagging does not change identity. |
| **Listen** | A raw playback observation recorded by Ritornel's player (duration, ranges heard, start/end reason, context). |
| **Play event** | A derived, deduplicated play bound to a recording and judged by a versioned play rule. |

## Subsystem guides

Detailed guides for shipped subsystems live under `docs/internals/`:

- [`docs/internals/server.md`](docs/internals/server.md) — server lifecycle, data directory, backups, SQLite rules, receipts, jobs, record health, and testing.
- [`docs/internals/platforms.md`](docs/internals/platforms.md) — macOS, Linux, Windows, and headless hub support, OS boundaries, and platform decisions.
- [`docs/internals/contract.md`](docs/internals/contract.md) — wire schema, TypeScript generation, HTTP routes and tickets, JSON-RPC framing, list views, events, commands, and cover media.
- [`docs/internals/schema.md`](docs/internals/schema.md) — two-database split, durable vs. rebuildable tables, immutability triggers, ratings, revision stamping, and migrations.
- [`docs/internals/plugins.md`](docs/internals/plugins.md) — compiled-in plugin boundary (`ritornel-source`), `ritornel-lastfm`, `ritornel-musicbrainz`, and Cargo feature gating.
- [`docs/internals/playback.md`](docs/internals/playback.md) — audio pipeline, gapless transitions, copy selection, persisted queue, frame-counted listen recording, and OS media sessions.
- [`docs/internals/history.md`](docs/internals/history.md) — immutable observations, play derivation, monotonic timekeeping, indexed queries, Last.fm sync/scrobbling, and portable export.
- [`docs/internals/corrections.md`](docs/internals/corrections.md) — sidecars kept with the audio, CD layouts and disc IDs read from them, MusicBrainz lookups behind the library's switch, the album and edition proposals worked out from them, and the answers that say yes or no to one, keep two albums apart, or undo another.
- [`docs/internals/identity.md`](docs/internals/identity.md) — read-only scanning, audio-only hashing, folder watching, normalization, recording resolution, catalog projection, and scrobble matching.
- [`docs/internals/transcoding.md`](docs/internals/transcoding.md) — encoder profiles, the encode cache, tagged copies and prepared transfers, verification commands and link-session downloads.
- [`docs/internals/device-sync.md`](docs/internals/device-sync.md) — syncing part of the library to a player or a folder: the driver trait, the planner, the run, storage, and what is never touched.
- [`docs/internals/linking.md`](docs/internals/linking.md) — server identity, hub write guard, local owner credentials, sessions, tickets, media tokens, and listener transport bounds.
- [`docs/internals/desktop.md`](docs/internals/desktop.md) — GPUI desktop workspace, server discovery, frame and whole-page navigation, virtualized walls and cover loading, History/Lean back views, queue, Settings, and offscreen testing.
