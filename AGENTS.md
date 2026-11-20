# Working on Ritornel

Ritornel is a music player for a collection you own: a server, `ritornel-server`, that owns the library, the player and the listening history, and clients that talk to it over a local socket. [`README.md`](README.md) says where it stands and what is in each folder.

## Commands

`mise.toml` holds the tasks and pins the tools, and `mise tasks` lists them. A new checkout needs `mise trust` and `mise install` once.

- `mise run check` runs everything CI runs.
- `mise run test` runs the tests. Some run `ritornelctl` against `ritornel-server` and take the server from the whole workspace's build, which `cargo test --package` alone does not rebuild.
- `mise run schema` writes `schema/` again after a migration is added, and `mise run contract` writes `contract/contract.ts` again after a call or a wire type changes. A test fails when either is out of date.
- `mise run desktop:run` runs the desktop app on an invented library. `scripts/desktop.sh tones` runs it on a small one that plays.

## Good to know

- Dependencies point one way: contract ← domain ← engine ← server, and contract ← client ← each client program. `scripts/check-deps.sh` checks it, and a new client or plugin gets a line there.
- A call is defined once, in the method table in `crates/ritornel-contract/src/methods.rs`.
- A migration that is on `main` is not edited, because the server refuses a database whose recorded checksum differs. A schema change is a new numbered file.
- The desktop app, `apps/desktop`, is a Cargo workspace of its own, so the server builds without any desktop library.
- macOS came first. Linux and Windows are in progress, so code for one platform goes behind `cfg`. The server runs on Linux with sound, MPRIS and a systemd user service, and the desktop app runs there on Wayland or X11.
- On Linux the whole workspace links ALSA for the server's audio output, so it needs ALSA's development files (`alsa-lib` on Arch, `libasound2-dev` on Debian and Ubuntu). The hub, built without default features, links no system library, and CI's `hub` job holds it to that.
- On Linux the desktop app needs the development files of fontconfig, xcb and xkbcommon (`fontconfig`, `libxcb`, `libxkbcommon` and `libxkbcommon-x11` on Arch; `libfontconfig-dev`, `libxcb1-dev`, `libxkbcommon-dev` and `libxkbcommon-x11-dev` on Debian and Ubuntu). Without them `scripts/desktop.sh` names what is missing and skips. `scripts/desktop.sh app` makes `apps/desktop/target/release/bundle/ritornel/`, the app with the server it starts beside it.
- With `RITORNEL_OFFSCREEN=1`, `scripts/desktop.sh` opens the app on a display no one sees (macOS only so far; on Linux it refuses to start), and a server run with `--output null` makes no sound. Both help when an agent works on a machine someone is using.
- Comments cite design notes under `docs/design/` and decisions by number. Those notes are not in this repository yet.

## Rules

- **Leave real libraries alone.** Tests and experiments pass `--data-dir` with a temporary directory. The default data directory holds a person's library and listening history.
- **Test data is invented.** Nothing from a real library or listening history is committed.
- **Licences.** A dependency carries a licence that `deny.toml` lists, and `mise run licenses` checks it.

## Pull requests

An agent opens a pull request when the person it works for asks. The title is `type(scope): what changed`, and the body ends with the model and harness that did the work. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the rest.
