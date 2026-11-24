# Corrections

Corrections put a library right from MusicBrainz, starting from evidence gathered beside the files. What is built so far is that evidence: the sidecars a scan keeps with the audio, the CD layouts read from them, MusicBrainz's replies about those discs, and the album proposals worked out from them, which can be shown and previewed but not yet answered. Pure layout and reply reading, proposing and previewing live in `crates/ritornel-domain/src/{layout,reply,propose,values}.rs`; storage and derivation in `crates/ritornel-engine/src/{evidence_store,lookup_store,proposal_store}.rs`; the scan's part in `crates/ritornel-engine/src/{walk,scan}.rs`; the fetching in `crates/ritornel-musicbrainz`; the calls in `apps/server/src/dispatch/corrections.rs`; and the commands in `apps/ritornelctl/src/corrections.rs`.

See [`docs/internals/identity.md`](identity.md) for the scan these steps ride on, [`docs/internals/schema.md`](schema.md) for durable and rebuildable tables, and [`docs/internals/plugins.md`](plugins.md) for the `Catalogue` trait.

## Disc layouts

A layout is where each track of a CD starts and where its audio ends, in sectors of 1/75 second. It gives the disc's MusicBrainz disc ID, which an exact lookup asks about. Rips carry their layout in what the ripper left beside the audio, and a scan keeps those files so the layout can be worked out again at any time without reading the audio.

### Sidecars

The walk (`walk.rs`) lists every file that may be a sidecar: `.cue`, `.log` and `.accurip` files, and `.txt` and `.nfo` files, each at most `SIDECAR_MAX_BYTES` (1 MiB). The scan keeps those that sit near audio: in a folder of audio, in a folder inside one (such as `Misc/` or `Support/`), or in the folder above one. A `.txt` or `.nfo` file is kept only if it reads as a rip log (`layout::keeps`): it names Exact Audio Copy, X Lossless Decoder or cdda2wav.

Sidecars are read after the audio and before the resolver, in a phase of the scan job of their own (`Phase::Sidecars`): each step reads files whose sizes, as the walk found them, come to at most `SIDECAR_BATCH_BYTES` (8 MiB), kept or not, and never more than one byte past the cap from any file, and each batch that changes anything is recorded in one library write (`SIDECARS_METHOD`, `scan.sidecars`):

- **`sidecar`** is one file by `(source_instance_id, rel_path)`, with its size and modification time so an unchanged file is not read again, and `missing_since_ms` set when a scan of a reachable root no longer finds it, or finds it no longer a sidecar, such as a text file that no longer reads as a rip log. A file in a folder the walk could not read is left as it was, and a folder of audio the walk could not read still counts as one, so the log above it is kept.
- **`sidecar_revision`** holds the bytes as read, deduplicated per sidecar by their BLAKE3 hash, and is immutable (`sidecar_revision_no_update`, `sidecar_revision_no_delete`, and `sidecar_revision_no_replace` against an `INSERT OR REPLACE`). A file that changes and changes back takes its earlier revision again.

Bytes are stored as found. Rip logs come as UTF-16 with a byte order mark, as UTF-8, and as Windows-1252, and `layout::decode` reads each.

### Reading a layout

`layout::layouts` (`LAYOUT_VERSION = 1`) is a pure function from each folder's audio files and the sidecars near it to that folder's layouts. A folder's own sidecars count, and those in a folder inside it that holds no audio. The sidecars of a folder above it that holds no audio count only for a layout that fits the folder's files exactly, as one log of a two-disc set does. Each layout is found in one or more ways (`Method`):

| Method | From | Checked by |
|---|---|---|
| `log` | A drive's table of sectors in a rip log, as EAC and XLD print it, or the start sectors cdda2wav prints | Nothing more: the drive read the disc. An end sector is the track's last, and on an enhanced CD the audio ends where the 11,400-sector gap before the data session begins |
| `cue` | A CUE sheet, with each `INDEX 01` taken from its `FILE` and the exact length of each file it names. A declared `PREGAP` or `POSTGAP` is added. A data track after the audio ends the audio where its `INDEX 00` says | The CDDB ID the sheet itself prints in `REM DISCID`, if any, counting a data track. A sheet that disagrees gives a **provisional** layout |
| `renamed_cue` | A CUE sheet whose files were renamed since: its `FILE` names matched to the folder's files by track number | The CDDB ID the sheet prints, or with none printed, every file's title ending the name the sheet gave it |
| `accuraterip` | The files' lengths in order | An AccurateRip disc ID printed in an `.accurip` report, with its pregap |
| `cddb` | The files' lengths in order | A CDDB ID any CUE sheet of the folder prints, when a renamed sheet was the only other way found |

A file's length in sectors comes from the stored `duration_ms` of its audio, for lossless stereo at 44.1 kHz only (`layout::sectors`). A number in a sidecar that no CD can hold, past 32 bits with the lead-in, is read as nothing. The stored length is cut to the millisecond, and a sector is 13⅓ ms, so the count is exact for a file that is a whole number of sectors; most files that are not are seen not to be. The disc ID is computed as MusicBrainz's "Disc ID Calculation" defines it: SHA-1 over the first and last track numbers, the lead-out and 99 offset slots, each offset with the 150-sector lead-in, in MusicBrainz's base64 alphabet.

Two layouts of one folder's files that disagree are both kept, such as a drive's log and the files of a rip that stripped its gaps. Each layout places a file at each position when every position has exactly one file of that length and, where the file has one, that track number. A folder of one file the length of a disc of several tracks is a whole-disc image; a disc of one track that file fills is that track.

### Derivation

The layouts are derived (`LAYOUTS_METHOD`, `corrections.layouts`) as the last step of a scan job when they are due: a write since the last derivation said so, or they were derived by another `LAYOUT_VERSION` than the build's (`library_setting` key `corrections.layout_version`). A write of sidecars, and a write of files in a library that has sidecars, says so in its own transaction (`corrections.layouts_pending`), so a scan that stops before it derives leaves them due for the next. Derivation reads only stored rows: the live sidecars' current revisions, the live files' audio assets, and their own `track_number` and `title` tags. It writes the difference from what is stored to the rebuildable tables, and clears the mark that they are due even when there is none, so a derivation always takes a revision:

- **`evidence_disc`** is one layout of one folder, by `(source_instance_id, folder, disc_id)`: the track offsets, the lead-out, each way it was found with the `sidecar_revision` it was read from (`found_json`), whether it is provisional, whether one file holds it, and the `layout_version` it was derived by.
- **`evidence_disc_item`** is the source item at each position, or at position 0 for a whole-disc image.

### Queries

`corrections.discs` (query, `full` session) lists every layout with its root, folder, disc ID, track count, length, methods and whether its files were matched, with counts of folders holding audio and of those with a layout that is not provisional. With `folder` it lists one folder's layouts with the file at each position. `ritornelctl corrections discs [<folder>]` prints it.

## MusicBrainz lookups

MusicBrainz is asked about a disc by its disc ID, and each reply is kept as it came. Nothing it says reaches what the app shows: a reply only adds evidence.

### The switch

A library is not asked about until its `lookups` setting is on, and with it on, only an album or a folder that `corrections.lookup` names is asked about: nothing is looked up by itself, after a scan or otherwise. `library.settings` (query) reports it as a `Setting`, with `setBy` `default` until a call sets it, and `library.setSettings` (command) changes it, taking an idempotency key and the settings to change (both `full` sessions, and forwarded to the hub on a satellite, as other library calls are). It is kept in `library_setting` under `corrections.lookups`; absent is off. `ritornelctl corrections musicbrainz on | off` sets it. What leaves the machine is the disc ID and the fixed inclusions: never a path, a file, or anything about who runs it. The User-Agent is `Ritornel/<version> ( https://ritornel.app )` (`USER_AGENT` in `ritornel-musicbrainz`).

### Asking about one album

`corrections.lookup` (command, `full` session) names one album, a release by its ID or a folder by its path under its root (with `root` when more than one root has that path), and asks about the disc IDs of the layouts in the folders it covers (`discs_of`). It is refused as `unavailable` with the reason `lookups_off` while the switch is off, `no_catalogue` on a server built without the `musicbrainz` feature, as `invalid` when the folder is in more than one root and none is named, and as `not_found` with `no_discs` for an album with no layout. The server advertises the `corrections.lookup` capability when it has a catalogue.

- **Asked once.** A disc that has a current reply to the same request is not asked again unless the call says `again`. A "no such disc" (404) is a reply and counts. A request is the catalogue, the kind of question, its key and what it asked to be included (`DISC_INCLUDES`: `artist-credits+release-groups+recordings`, with CD stubs off), so a reader that needs more asks a new request.
- **Asked, then kept.** The catalogue is asked before the library is written. The replies and the receipt are then kept in one library write (`corrections.lookup`), so the same idempotency key is answered from the receipt and asks nothing. One lookup runs at a time, from the check of its receipt to its commit, so no two ask the same thing at once, and the switch is read again before each request, so a lookup turned off on its way stops there.
- **Only a reply is kept.** A reply is JSON in UTF-8 that reads as an answer about the disc asked: its releases, or MusicBrainz's own "no such disc". Anything else (a timeout, no network, 503 or 429, any other refusal, a redirect, a page from something in the way) is that disc's `failed` outcome with a one-line reason, nothing is kept for it, and the reply it had stays current.
- **Paced.** `ritornel-musicbrainz` sends at most one request a second from the server, whatever the number of callers, holds its turn until the reply is in, follows no redirect and retries nothing on its own.
- **Reported.** The result gives each disc's outcome (`replied`, `not_found`, `kept`, `failed`) and the albums its current reply names: each release group with its title, credit, first year and how many of the reply's releases are of it. `ritornelctl corrections lookup <folder> [--root <name>] | --album <release> [--again]` prints it.

### Replies and facts

- **`lookup_response`** is one reply, verbatim: the catalogue, request kind, key and inclusions, the status (200 or 404), the body and its BLAKE3 hash, and when it came. It is immutable (`lookup_response_no_update`, `lookup_response_no_delete`, and `lookup_response_no_replace` against an `INSERT OR REPLACE`). The newest reply to a request is its current one; a reply the same as the current one is not kept again, and one that differs is kept as the new current one.
- **`evidence_claim`** holds the facts each reply states, per release, read by `reply::read_disc_reply` (`READER_VERSION = 1`): the release's title, status, date, country and barcode; its credit (`album_artist`, `album_artist_join`, `mb_album_artist`); its album (`mb_release_group`, `release_group_title`, `primary_type`, `secondary_type`, `first_release_date`) and the album's own credit (`group_artist`, `group_artist_join`, `mb_group_artist`); how many media it has; and which of them the disc is, with its track count (`disc_number`, `track_count`). Each fact keeps its position, so a join phrase stays with its name. Facts read by another reader version are read again from every kept reply, with no network, when the server starts (`FACTS_METHOD`, `corrections.facts`) and in the next lookup's write (`library_setting` key `corrections.reader_version`); a kept reply the new reader cannot read states nothing.

## Album proposals

A proposal says that one disc of the library is one album: a MusicBrainz release group. It is derived, as plays are, and nothing it says is shown on a wall or written into the library's catalog.

### Working them out

`propose::propose` (`PROPOSER_VERSION = 1`) is a pure function of the disc layouts, the current reply about each disc ID and the files' facts:

- **The subject is one disc:** the files a folder's layouts place, among layouts that are not provisional and have a current reply. A disc whose replies name no release gets no proposal, and nor does a disc with a file that names neither its album nor any artist: a yes would give it the album's title and no credit, and it would show under no artist with songs named for their files, so it waits until its songs and credit can be answered too.
- **Each candidate is an album its replies name,** with its title, the album's own credit, kind, first date, and how many of the replies' releases are of it; the likeliest, with the most releases, first.
- **What a yes would put on the files** (`values`): every file gets `mb_release_group`, and the album's `primary_type` and `original_date` where MusicBrainz knows them. Where other discs of the library are on the same album, each the same single medium of it in every release that holds it and no two the same, the yes joins them: each file gets its disc's `disc_number`, and the files of every disc but the first take the first disc's `album`, `album_artist` and `mb_album_artist` where their own differ. A file with no album title gets the album's. The candidate keeps the files of the first disc it joins its disc onto (`onto`), whose title and credit it takes, so what a yes puts together is known before it is said.
- **What that would change** is worked out cheaply: `moves` when files would join or part, take another album's ID, or share a tile with another disc of the album without joining it; `gains` when a kind or a first year would differ from what the files say; `knows` for an ID alone. The preview says exactly.
- **An ID is computed** from the kind and the anchor, the subject's first-seen file, by its public ID, so a proposal worked out again keeps its ID. Proposals that share their first candidate share a group, named by the proposal whose anchor is the first-seen file among them. A fingerprint is a digest of all of a proposal.

Proposals are worked out again in the same write as anything they rest on: a derivation of the layouts, and a lookup's replies. Only those that differ, by fingerprint, are rewritten (`proposal`, `proposal_item`, `proposal_candidate` with each candidate's `values_json` and `onto_json`, and `proposal_evidence`). A new proposer version is worked out again from stored rows when the server starts (`library_setting` key `corrections.proposer_version`), as are a new reader or layout version.

### Tag values

A value replaces the file's own tag for its field before anything is formed (`values::apply`), for the fields `album`, `album_artist`, `mb_album_artist`, `disc_number`, `mb_release_group`, `primary_type` and `original_date`, and the item holds that field (`Item::held`). The disc rules (`discs::apply`) leave a held album title or disc number as it is. So far values are only previewed.

### Showing and previewing

- `corrections.summary` counts the open proposals and their groups, by what their likeliest candidate would change, and says whether lookups are on.
- `corrections.list` lists every proposal by group, with its candidates but without their values. `corrections.get` gives one whole: its files, each disc it rests on with how its layout was found and how many releases the reply names, and each candidate with the values a yes would put on each file.
- `corrections.preview` takes proposals, each with a candidate or its likeliest, and runs the projector twice, read-only (`values::preview`): on the library as it is, and with every chosen value in place. It reports the albums the touched files and everything else on those albums are on now, and after: each one's title, credit, kind, first year, releases, discs and songs. It also counts the songs the values would join into others (`songsJoined`), such as two editions' copies of one song that are two songs now and would be one, and those they would part from the one they are on (`songsParted`); `ritornelctl corrections preview` says both first, as they move what plays and ratings count on.

`ritornelctl corrections list`, `show <proposal>` and `preview <proposal>... [--candidate <release group>]` print them.

## Decisions

- **Evidence moves no wall.** No table here is read by the resolver or the projector, and the writes that change them (`scan.sidecars`, `corrections.layouts`, `library.setSettings`, `corrections.lookup`, `corrections.facts`) send no `library` or `list` event (`changes` in `apps/server/src/dispatch/lists.rs`). A scan that finds only sidecars leaves the catalog untouched.
- **Bytes kept, layouts derived.** Sidecar bytes are durable and immutable, and layouts are a versioned pure function of them and the stored lengths, so a better reader is a new `LAYOUT_VERSION` and no rescan.
- **Lengths from the stored duration.** Sectors are worked out from `duration_ms`, not from a sample count the payload does not carry, because the cut to the millisecond still fixes a whole number of sectors. A wrong layout fails safe: an exact disc lookup finds nothing.
- **Read-only roots.** A sidecar is read and never written, renamed or removed, as audio is.
- **Off until switched on.** Asking a service about a library sends something about it, so a library is never asked about until its own switch says so. Sidecars are read regardless, since nothing leaves the machine.
- **The plugin only fetches.** Replies are read in the domain crate, so the facts are rebuilt from kept replies with no plugin and no network, and a dry run never needs the network.
- **A failure is not a reply.** Only a reply MusicBrainz gave is kept; a failure leaves the disc to be asked again.
- **Proposals hang on files.** A proposal's subject and values name source items, and its ID comes from its anchor file, so a proposal outlives a rescan, a re-resolve and a rebuild of the proposals.
- **A preview is the projector.** The preview runs the same pure projector a scan does, on the library with the values in place, so what it shows is what a yes would make.

## Later

- `proposal_evidence` names the exact reply each proposal rests on, and `found_json` the sidecar revision each layout came from, so an answer can keep its evidence by value.
