# Device sync

The hub syncs a chosen part of the library to a portable player or a folder. The driver trait is `ritornel-device`; the planner, the folder family and every record are in `ritornel-engine::sync`; the server's `devices.*` calls and the job that carries a sync out are in `apps/server/src/dispatch/devices.rs`. The [transcoding guide](transcoding.md) covers the encode cache and the tagged copies a sync sends, the [contract guide](contract.md#devices) the wire, and the [desktop guide](desktop.md#devices) the Devices tab.

Only the folder family is built. The trait, the storage and the wire are shaped for mass-storage players and MTP players as well, and nothing for them is in the build yet.

## The parts

| Part | Where | What it does |
|---|---|---|
| Driver trait | `crates/ritornel-device` | `Session`: probe, storage, inventory, stat, signature, put, remove, marker, clear partials, play logs, close with or without ejecting. It depends on no workspace crate, as `ritornel-source` does not. |
| Folder family | `ritornel-engine::sync::FolderSession` | A directory behind the trait. Every test runs against it. |
| Planner | `ritornel-engine::sync::plan` | A pure function of what is wanted, what was seen and what was recorded. It opens no file and writes nothing. |
| Records | `LibraryStore::sync_*`, `ServerStore::sync_roots` | Targets, sources, baselines, runs and actions in `library.db`; a folder's path in `server.db`. |
| Calls and the run | `apps/server/src/dispatch/devices.rs`, `devices/run.rs` | The `devices.*` calls, the plan held in memory, and the `devices.sync` job. |

The hub pushes, and trusts nothing a device reports beyond what is there now. What identifies a file on a target is the hub's own record of having written it, the baseline, and never a tag. A file with no baseline is not Ritornel's, whatever it holds: it is left alone and counted as used.

An object is named for its transport. A folder or a mass-storage volume names a file by its path under the storage root (`ObjectId::Path`), with its size as its signature for planning and the BLAKE3 hash of its first and last mebibyte checked before anything destructive. An MTP device names a file by its persistent object ID (`ObjectId::Persistent`); where a device cannot hash part of a file (`Capabilities::can_sample`), nothing destructive is done.

## Storage

`library.db` holds what a sync must not lose, so it follows the library when the hub moves:

| Table | Holds |
|---|---|
| `sync_target` | One player or folder, from the first time it is seen, with its settings by value: the format as encoder settings and never a profile's name, the cover's size, the codecs it is taken to play, and for a folder what it is made for and whether it is removable. Forgetting marks it and never deletes it. |
| `sync_target_identity` | How a transport knows a target again: for a folder, its host server and an opaque root ID. |
| `sync_target_epoch` | One lifetime of a target's storage, and the root ID its marker holds. |
| `sync_source` | The subset, in order: a rule, an always or a leave-out, with what it selects. |
| `sync_object` | The baseline: one row for each lifetime of a file the hub wrote and verified. A row is closed when its file is replaced, removed or forgotten, and never deleted. |
| `sync_unmanaged` | The files in the music root that Ritornel did not write, as the last run's inventory found them. |
| `sync_run`, `sync_action` | Each run, and its frozen actions with what became of each. |

`server.db` holds `sync_root`: a folder target's root ID and its path on that machine. The library stores only the root ID, as it stores only a root's name, so a folder's path never leaves the server it is on.

The plan of a target's next sync is not stored. The server makes it again whenever its inputs move and holds the last one in memory for a few seconds.

## The subset

A target's sources are evaluated afresh for each plan; no membership is stored. A source selects everything, an artist's records, one record, or what was added in the last so many days (`SyncSelector`).

Claim order is every Always, then the rules by position, and a Leave out vetoes both. A track belongs to the first source that claims it, and a source is taken an album at a time unless it says tracks. Tracks are walked in one total order (album artist, year, album, disc, track), so a source cut short takes the same records every time.

## Format

A target has one format policy (`FormatPolicy`). A lossless master is sent as it is or encoded with the policy's own encoder settings; a lossy master is always sent as it is, since lossy is never encoded to lossy. `choose_source` decides for each track, and a track with no copy the target plays is blocked with a reason rather than failing the sync.

A device's policy never bakes gain into the audio. The master's ReplayGain figures are written as tags for the player to apply, read from the tags the scan stored with the master's revision, so no master is opened to tag one.

A folder is made for something (`SyncMadeFor`): a car stereo, a phone, another player or a plain copy. That supplies the codecs it is taken to play, the cover's size, the kind of playlist and whether an MP3 gets an ID3v1 trailer (`sync::preset`), each of which can then be set. `devices.formats` lists each format on offer with what the content would take in it; one the hub has no encoder for, or the target does not play, is listed and cannot be chosen.

## The plan

The planner compares, for each track, whether it is wanted, what is on the target at its path, and its baseline:

| Wanted | On the target | Baseline | Outcome |
|---|---|---|---|
| Yes | Nothing | None | Add |
| Yes | Present, size matches | Same projection key | Nothing |
| Yes | Present, size matches | Another key | Replace, with its reason: `format`, `audio`, `tags` or `cover` |
| Either | Present, size differs | Exists | Conflict, `changed_on_device`. Nothing is touched |
| Yes | Nothing | Exists | Add again, `missing` |
| No | Present, size matches | Exists | Remove, if its master is in the library; otherwise a conflict, `master_gone`, and it is kept |
| No | Nothing | Exists | The baseline is closed by the next run |
| Yes | A file with no baseline at the planned path | None | Blocked, `path_taken`. The file is left |
| Yes, with no copy the target plays | Any | Any | Blocked, with the reason. What is there stays |
| Any | Present | None | Not Ritornel's: left, and counted as used |

The projection key (`ProjectionKey`) is four parts: the master's audio hash, the format, the projected tags and the cover. The first part that differs is why a file is sent again. It leaves out the encoder's build and Ritornel's own identifier, so a newer encoder never sends a target's files again by itself.

Room is what is free, plus what Ritornel's own files take, less the reserve and less managed files that must stay. A folder that is always here has the limit the person set for a capacity; a removable one has its drive's. The cut is a strict prefix: units are walked in claim order and the first that does not fit ends it. Quality is never lowered to fit.

Sizes are estimates until a file is made, and the plan says so: a file's size is its baseline's where it was written under the same key, else its cached or remembered encode's, else the master's own when sent as it is, else length times a nominal bitrate.

The plan's revision names what it would do and not what it would take, so an estimate that sharpens as encodes are cached does not move it. `devices.sync` names the revision the person saw, and a plan that has moved since is refused with `plan_stale` before anything is written.

## The run

A sync is a job, `devices.sync`, and stopping one is `jobs.cancel`.

| Step | What happens |
|---|---|
| Read | The run row is opened, which is the lease: a second open run for a target is refused. Partial puts are cleared, what the last run left in doubt is checked against the target, and the target is read. Nothing below starts until this returns. |
| Plan | The plan is made again. If it is not the revision asked for, the run ends having written nothing. Otherwise its actions are frozen into `sync_action`. |
| Adopt | A first sync writes the marker, `.ritornel/root.json`, naming the target, the epoch and the root ID. |
| Copy | Removes, then replaces that shrink, then replaces that grow, then adds, a window at a time. |
| Finish | The marker is written again, naming this run, and the run's outcome is recorded. |

A window is the few actions whose transfers are prepared. In one `library.db` transaction the hub records the last window's results, baselines among them, and writes the next window down as issued; only then is the target asked. What a crash leaves in doubt is one window of actions, each with the length, digest and sampled hash it meant to leave, and the next run settles each by what the target shows.

A removal or a replace is carried out only after the target's file is found with the size and sampled hash that were recorded; anything else ends the action as a conflict and deletes nothing. A removal also looks at the master's own file just before, since a file can be deleted while its folder stays online. An add never overwrites.

A marker that is gone, or names another root, ends the epoch: the plan then stands on no baselines, everything there is a file Ritornel did not write, and syncing needs the target set up again.

At every start the hub closes each run it finds open as `cut_off`, since an open run belongs to a session an earlier boot held in memory. A sync job put back after a restart finds its run closed and ends failed.

## Names, tags and what is safe

| Part | Rule |
|---|---|
| Tags | Title, artist, album, album artist, year, disc and track, and ReplayGain track and album gain and peak. Vorbis comments in FLAC and Opus; ID3v2.3 in MP3, with ReplayGain as `TXXX` frames; an ID3v1 trailer only where the target asks. |
| Picture | An embedded baseline JPEG, never progressive, its longest side from the target: 500 pixels unless set, or none. |
| Ritornel's ID | One versioned value naming the library, the master's source item and the appearance, as `RITORNEL_ID` in a Vorbis comment or a `TXXX` frame. A recovery hint: no plan, check or readback uses it. |
| Path | One fixed layout, `Music/<album artist>/<album>/<disc>-<track> <title>`, the disc only where the album has several. What FAT forbids becomes `_`, trailing dots and spaces go, names are composed (NFC), and two paths that differ only by case are told apart by a suffix. The path under the root is capped at 250 UTF-8 bytes, shortening the title, then the album, then the artist. The path a file was written at is stored and never worked out again. |
| Scope | Ritornel writes inside the music root and `.ritornel/`, and nowhere else. |

## The folder target

A folder target is a directory on the server `devices.addFolder` was called on. It is refused if it is, holds or lies inside one of the library's folders, the server's data directory, or another target's folder. `FolderSession` resolves every path a part at a time and never follows or writes through a symbolic link inside the root.

A put is written under `.ritornel/tmp/`, hashed as it is written, flushed, and only then given its name, so a put that fails or is stopped leaves nothing by that name. A folder that is removable is ejected by `devices.eject`, on macOS only; one that is always here is not ejected.

## Decisions

- **The hub's record names a file, never a tag.** A device lists files wrongly, renames them and loses them, so a plan stands on what the hub recorded it wrote and what is there now.
- **Everything a sync must not lose is in `library.db`.** One transaction covers a baseline and the action that earned it, one backup holds both, and both follow the library when the hub moves. Only a folder's path stays behind.
- **An action is written down before the target is asked to do it.** A deletion recorded first is never read as the person's own, and a crash leaves a known, bounded set of actions to check.
- **Nothing is destroyed that the library cannot make again.** A file changed on the target, a file with no baseline, and a file whose master is gone are all left, so a library emptied by mistake never empties a player.
- **A bookkeeping write tells no one.** A run's writes say they change nothing shown, so the library's subscribers are not told of each window of a sync; the `devices` topic carries that.

## Later

- `Transport::MassStorage` and `Transport::Mtp`, `ObjectId::Persistent`, `Session::play_logs`, `SyncPlaysKind` and the `devices.plays` call are in place for players that give plays back; no family reads any yet, so `devices.plays` answers none.
- `SyncSettings::on_connect` is stored and not acted on: nothing starts a sync but `devices.sync`.
- `SyncPlaylistKind` is stored, and no playlist is written to a target yet.
- A satellite forwards every `devices.*` call to its hub but `devices.addFolder`, which it refuses: only a target attached to the hub is synced.
