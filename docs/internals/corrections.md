# Corrections

Corrections put a library right from MusicBrainz, starting from evidence gathered beside the files. What is built so far is that evidence: the sidecars a scan keeps with the audio, and the CD layouts read from them. Pure layout reading lives in `crates/ritornel-domain/src/layout.rs`; storage and derivation in `crates/ritornel-engine/src/evidence_store.rs`; the scan's part in `crates/ritornel-engine/src/{walk,scan}.rs`; the call in `apps/server/src/dispatch/calls.rs`; and the command in `apps/ritornelctl/src/corrections.rs`.

See [`docs/internals/identity.md`](identity.md) for the scan these steps ride on, and [`docs/internals/schema.md`](schema.md) for durable and rebuildable tables.

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

## Decisions

- **Evidence moves no wall.** No table here is read by the resolver or the projector, and the writes that change them (`scan.sidecars`, `corrections.layouts`) send no `library` or `list` event (`changes` in `apps/server/src/dispatch/lists.rs`). A scan that finds only sidecars leaves the catalog untouched.
- **Bytes kept, layouts derived.** Sidecar bytes are durable and immutable, and layouts are a versioned pure function of them and the stored lengths, so a better reader is a new `LAYOUT_VERSION` and no rescan.
- **Lengths from the stored duration.** Sectors are worked out from `duration_ms`, not from a sample count the payload does not carry, because the cut to the millisecond still fixes a whole number of sectors. A wrong layout fails safe: an exact disc lookup finds nothing.
- **Read-only roots.** A sidecar is read and never written, renamed or removed, as audio is.

## Later

- `evidence_disc` and its items are the evidence that MusicBrainz lookups and album proposals will rest on; `found_json` names the raw `sidecar_revision` each layout came from so a later answer can keep its evidence by value.
