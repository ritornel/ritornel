# Working on Ritornel

Ritornel is a music player for a collection you own: a server, `ritornel-server`, that owns the library, the player and the listening history, and clients that talk to it over a local socket. [`README.md`](README.md) says where it stands and what is in each folder, and [`ARCHITECTURE.md`](ARCHITECTURE.md) maps the crates, dependency rules, invariants and vocabulary.

## Commands

`mise.toml` holds the tasks and pins the tools, and `mise tasks` lists them. A new checkout needs `mise trust` and `mise install` once.

- `mise run check` runs everything CI runs.
- `mise run docs` checks `docs/internals/` citations, doc structure and rustdoc intra-doc links.
- `mise run test` runs the tests. Some run `ritornelctl` against `ritornel-server` and take the server from the whole workspace's build, which `cargo test --package` alone does not rebuild.
- `mise run schema` writes `schema/` again after a migration is added, and `mise run contract` writes `contract/contract.ts` again after a call or a wire type changes. A test fails when either is out of date.
- `mise run desktop:run` runs the desktop app on an invented library. `scripts/desktop.sh tones` runs it on a small one that plays. On Windows, use PowerShell 7 or later. `pwsh -File scripts/desktop.ps1 run`, `tones` and `app` do the same, building the server too, and `check` is what `mise run desktop:check` runs there.

## Good to know

- Dependencies point one way: contract ← domain ← engine ← server, and contract ← client ← each client program. `scripts/check-deps.sh` checks it, and a new client or plugin gets a line there.
- A call is defined once, in the method table in `crates/ritornel-contract/src/methods.rs`.
- A migration that is on `main` is not edited, because the server refuses a database whose recorded checksum differs. A schema change is a new numbered file.
- The desktop app, `apps/desktop`, is a Cargo workspace of its own, so the server builds without any desktop library.
- macOS came first, and Linux and Windows are in progress. What differs between systems sits in a `platform` module in each crate that needs one, a file per system offering the same plain functions, and no `cfg` of an operating system appears outside it but in a test whose data is the system's, such as an absolute path or how a process is killed. [`apps/desktop/src/platform/mod.rs`](apps/desktop/src/platform/mod.rs) is the example. The server runs on Linux with sound, MPRIS and a systemd user service, and the desktop app runs there on Wayland or X11. On Windows the server and the desktop app both run, with sound through WASAPI, media keys through the system's media transport controls, and a background service at sign-in. [`docs/internals/platforms.md`](docs/internals/platforms.md) says what runs where and the decisions behind each port: update it in the same PR when platform behavior changes.
- Every crate of the main workspace forbids unsafe code but `ritornel-windows`, which holds the Win32 calls the standard library cannot make (a lock on one byte, owner-only access lists, a file's identity, a rename written through to the disk, the clock since boot, a hidden window, the system's media transport controls) behind safe functions.
- On Linux the whole workspace links ALSA for the server's audio output, so it needs ALSA's development files (`alsa-lib` on Arch, `libasound2-dev` on Debian and Ubuntu). The hub, built without default features, links no system library, and CI's `hub` job holds it to that.
- On Linux the desktop app needs the development files of fontconfig, xcb and xkbcommon (`fontconfig`, `libxcb`, `libxkbcommon` and `libxkbcommon-x11` on Arch; `libfontconfig-dev`, `libxcb1-dev`, `libxkbcommon-dev` and `libxkbcommon-x11-dev` on Debian and Ubuntu). Without them `scripts/desktop.sh` names what is missing and skips. `scripts/desktop.sh app` makes `apps/desktop/target/release/bundle/ritornel/`, the app with the server it starts beside it.
- With `RITORNEL_OFFSCREEN=1`, `scripts/desktop.sh` opens the app on a display no one sees (macOS only; on Linux it refuses to start), using `RITORNEL_TEST_DISPLAY` if set or `hidden-display` when installed, and a server run with `--output null` makes no sound. Both help when an agent works on a machine someone is using.
- Shipped subsystem documentation lives in `docs/internals/` (listed in [`ARCHITECTURE.md`](ARCHITECTURE.md)). Before writing or updating docs or filing GitHub issues, read [`.agents/skills/ritornel-docs/SKILL.md`](.agents/skills/ritornel-docs/SKILL.md) or [`.agents/skills/ritornel-issues/SKILL.md`](.agents/skills/ritornel-issues/SKILL.md). Any PR that changes invariants, wire behavior, or subsystem architecture updates the matching `docs/internals/` guide in the same PR.

## Rules

- **Leave real libraries alone.** Tests and experiments pass `--data-dir` with a temporary directory. The default data directory holds a person's library and listening history.
- **Test data is invented.** Nothing from a real library or listening history is committed.
- **Licences.** A dependency carries a licence that `deny.toml` lists, and `mise run licenses` checks it.

## Writing tests that don't depend on timing

CI runs the tests in parallel on small, busy machines, Windows' slowest of all, so a test that counts on how long something takes fails there now and then. A test waits for what it checks, never for time to pass.

- **Wait on an event or a condition, not a fixed sleep.** Look again until it holds, inside the test's `WAIT`: `while partial(&scratch).is_empty() { sleep(20 ms) }`, not `sleep(500 ms)` and hope. `scripts/check-test-sleeps.sh`, part of `mise run check`, asks every sleep in a test for a `// sleep:` comment that says why no event will do.
- **Use the clock the code takes.** Where code takes a `Clock`, a `now` or tokio's time, give it a fixed or paused one: `#[tokio::test(start_paused = true)]`, `elastic.scroll_at(&mut top, wheel, max, t0 + Duration::from_millis(10))`. Add a narrow seam where it is cheap.
- **A deadline is a backstop, and says what never happened.** `assert!(Instant::now() < deadline, "the decoder never {what}")`, with a deadline of a minute. An upper bound on elapsed time is only for a claim about time, and the wait it proves against is made far longer than the bound: the stop test sets `Bounds::hub_call_wait` to a minute and asserts `took < 30 s`, rather than racing the default eight seconds.
- **Hold what a test must catch.** Hold the null output where the test looks (`AudioRenderer::held`), or play slowly enough that each step outlasts any look, rather than race a short track at speed.
- **Assert on what the test caused, not on the latest.** Follow the job a call returned. Where more may come, such as a watcher's second rescan or a delivery still sending, wait until nothing is on its way, or accept a later one: `last.job_id >= Some(rescan.job_id)`.
- **A scratch folder is always new.** `tempfile::Builder::new().prefix("ritornel-server-pages-").tempdir()`, never a name made from the process ID, which Windows soon gives out again.
- **Tests listen on loopback unless they test interface selection.** `Listen::on_port(0)` or `--listen 127.0.0.1:0`. Only `a_server_on_the_private_network_is_found_on_loopback` binds every interface, because that is what it tests; on Windows each new test binary that does raises the firewall's prompt.
- **Keep a port you rely on.** A port is yours only while you hold it: keep the listener (`unanswering.incoming().for_each(drop)`), and retry a bind to a port just let go.
- **A failed subprocess says why.** `ok(&data_dir, &["library", "create"])`, which fails with the exit code, stdout and stderr, not `assert_eq!(ritornelctl(..).0, Some(0))`. A server's stderr goes to the test (`Stdio::inherit()`), and nextest shows it with the failure.
- **Tests share nothing.** No environment variable, current directory or static that a test changes: a server gets its environment through `Command::env` or `Run`.

## Pull requests

An agent opens a pull request when the person it works for asks. The title is `type(scope): what changed`, and the body ends with the model and harness that did the work. [`CONTRIBUTING.md`](CONTRIBUTING.md) has the rest.

Keep partial work on its branch until it works end to end rather than merging something half-built onto `main`. For a feature that changes the contract or adds migrations, use one `CONTRACT_VERSION` bump and collapse migrations not yet on `main` into as few files as it needs. Changes that write the library, listening history, or backups, or send scrobbles or Now Playing, get extra review.
