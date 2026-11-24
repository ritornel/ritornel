# Identity

Ritornel resolves local audio files and imported scrobbles into canonical recordings, releases, release groups, track appearances, and artists without ever modifying, renaming, or deleting music files. Pure extraction, normalization, recording resolution, catalog projection, and scrobble matching live in `crates/ritornel-domain/src/{payload,extract,normalize,resolve,catalog,discs,scrobble,library}.rs`; filesystem walking, audio hashing, scanning, persistence, diagnostics, and artist portraits live in `crates/ritornel-engine/src/{walk,media,scan,scan_store,resolve_store,identity_store,import_store,artist_pictures}.rs`; folder watching lives in `apps/server/src/watch.rs`.

See [`docs/internals/schema.md`](schema.md) for durable vs. rebuildable tables and insert-only `assignment` rules, [`docs/internals/server.md`](server.md) for scan jobs and data layout, and [`docs/internals/history.md`](history.md) for how bindings feed listening history.

## Scanning and audio hashing

Scanning a music folder (`scan.start`) runs as a durable job split into a read-only filesystem phase and a database commit phase (`crates/ritornel-engine/src/{walk,media,scan,scan_store}.rs`):

1. **Walk (`walk.rs`):** Lists audio files matching 25 recognized extensions (`AUDIO_EXTENSIONS`, including formats Symphonia cannot demux so they are recorded as unreadable rather than silently skipped) and folder artwork (`cover`, `folder`, or `front` with `.jpg`, `.jpeg`, or `.png`). Unreadable subdirectories (such as `lost+found`) are skipped and recorded without marking previously known items inside them missing. Folders are listed from a heap-allocated work list rather than by recursion, so a deep tree costs no stack, and a folder whose identity matches one of its ancestors (as a bind mount can make) is skipped rather than walked again. The walk captures the root's filesystem identity via `folder_id` (Unix `(dev, ino)` or Windows `(volume serial, file index)`), and a root whose identity the system will not give is refused as offline.
2. **Parallel file read (`scan.rs`, `media.rs`):** `ScanRun::prepare` reads new or changed files in batches (`ScanSettings::batch_files = 500`, `threads = available_parallelism`), never holding a SQLite transaction open while touching disk.
3. **Audio-only hashing (`HASH_SCHEME = 1`):** Each file is demuxed with `symphonia` (`=0.6.1`, without decoding) through a `Bounded` reader that hides trailing ID3v1, APEv2, and Lyrics3 tags so frame scanners never treat tag bytes as audio. `blake3` (`=1.8.7`) hashes the domain string `ritornel-audio`, scheme number, Ritornel's internal codec name, codec parameters/configuration, and every audio packet's payload bytes in order. Retagging a file never changes its `audio_asset`. Embedded pictures (up to `MAX_PICTURE_BYTES = 64 MiB`) and folder images are hashed by content; binary tags are kept as hex up to `MAX_BINARY_TAG_BYTES = 256` bytes.
4. **Commit (`scan_store.rs`):** `ScanRun::commit` writes each batch in one `library.db` transaction using a deterministic step command ID (`step_command`), inserting an immutable `source_item_revision` (`Payload`, `PAYLOAD_VERSION = 1`), creating/linking `audio_asset` and `picture` rows, and extracting `identifier_claim` and `metadata_claim` rows. The step's command receipt stores the job's counters as they are once it commits (`StepResult`), and the resolver's step stores what it wrote (`Resolved`), so a job resumed after a crash between a step's commit and its saved checkpoint skips the step and keeps its counts in `ScanSummary`; a job whose resolver step committed goes on to the disc layouts without walking the root again, and one whose layouts step committed (`StepResult::last`) is finished. A step written by an older build stored only a row count, so its counts are not recovered.

After the audio and before the resolver, the scan reads the rip logs, CUE sheets and checksum reports beside it in steps of their own, and derives disc layouts from them last. Neither moves a wall: see [`docs/internals/corrections.md`](corrections.md), "Disc layouts".

## Rescans and file moves

Rescans avoid unnecessary work and protect identity across moves, edits, and unmounted disks (`scan.rs`, `scan_store.rs`):

- **Unchanged files:** If `(size, mtime_ms)` and folder images match the stored item (and the previous revision did not fail with an I/O error), the file is skipped. A rescan that finds nothing changed writes nothing and takes no library revision.
- **Unmounted or swapped roots:** Before recording moves to known paths, missing files or replaced audio, `ScanRun` re-checks `folder_id(&self.root)` against the walked root ID. If the mount vanished or changed volume, the scan fails with `Error::RootOffline` without marking items missing. Since a walk only succeeds with a known root identity, two unknown identities never pass as the same folder.
- **Missing files:** Files absent from a reachable root set `source_item.missing_since_ms` while keeping all rows and history intact; returning clears `missing_since_ms`.
- **Moved files:** Known items whose paths vanished (or now hold different audio) are indexed by audio hash in `gone_by_audio`. When a new path has a unique matching audio hash, the existing `source_item` is re-keyed in place (`RecordOp::Move`), preserving its bindings and listening history. Once every file is read, the same match is made for files at known paths that hold other audio (`ScanRun::settle_moves`), before anything is marked missing. Files that swap paths, or rename in a cycle, keep their items. An item whose path another item's file now occupies, and whose own file the scan did not find, is retired with its rows (`RecordOp::Displace`), as when a different song takes its path. A move is made only when exactly one file holds the item's audio, so the result never depends on the order files were read. The write retires displaced items and gives moving items a temporary key before any item takes its new path, so `source_item_live_key` holds throughout.
- **Replaced audio at the same path:** When a known path holds different audio, `judge_replacements` runs the resolver policy to decide whether the new file is the same recording (new revision on the existing item, `RecordOp::Update`) or a different song (retires the old `source_item` via `retired_at_ms` and creates a new item, `RecordOp::Retire`).
- **Files that read again after a failed read:** A failed read records a revision with no audio and keeps the item and its binding. When the file reads again, it updates the same item if earlier revisions held audio and every one held that audio. Otherwise, including when no revision held audio at all, it goes to move matching and then to `judge_replacements` as replaced audio, which keeps an item that has no recording yet; another item's file moved onto the path is followed as a move. The check covers every earlier revision, not only the latest, because an item with no recording yet keeps whatever replaces its audio, so an earlier revision may hold another song. Items whose current revision has no audio take no part in move matching.

## Watching

On the hub, `apps/server/src/watch.rs` watches all music roots and artist picture folders using `notify` (`=8.2.2`, FSEvents on macOS and inotify on Linux):

- **Debounce:** After filesystem events arrive, the watcher waits `QUIET` (4 s) of silence—capped at `LONGEST` (60 s) under continuous writes—before queueing a standard scan job or picture re-import.
- **Timer fallback:** Because OS watchers can miss changes on network shares or while the hub is offline, the watcher also triggers a scan on startup when a root's last scan is older than `RESCAN_AT_START_AFTER` (1 hour) and periodically every `RESCAN_EVERY` (6 hours).
- Watching can be toggled per machine via `library.setWatching` and inspected in `server.status`.

## Extraction and normalization

### Tag extraction (`extract.rs`, `EXTRACTOR_VERSION = 3`)

`extract` is a pure function converting a stored `Payload` into `Claim` rows (`identifier_claim` and `metadata_claim`) across Vorbis comments (FLAC/Ogg), ID3v2 (including ID3v2.2 three-letter frames), ID3v1, APEv2, and MP4 atoms (including freeform `----` atoms as `<mean>:<name>`). When a file carries multiple tag blocks, each field is taken from the highest-priority block that supplies it (container-native first, ID3v2/APE next, ID3v1 last). Missing title tags fall back to `Source::Inferred` from the filename at lower precedence than `Source::FileTags`.

### Name normalization (`normalize.rs`, `NORMALIZER_VERSION = 3`)

`normalize` builds mechanical candidate keys without altering stored display names:

- Applies Unicode NFKC, lowercasing (normalizing dotted `İ` / `i\u{0307}`), NFC, `"&"` → `"and"`, and collapses runs of whitespace.
- Punctuation that separates words (hyphens/dashes, slashes, parentheses, brackets, `+`, `×`, etc.) becomes a space rather than disappearing completely, keeping word boundaries intact.
- **Combining marks stay in keys.** Accents and scripts' combining marks are preserved in stored keys so distinct names do not silently merge; Latin accents are folded only during candidate scoring (`fold_accents`).
- **Edge symbols stay in keys.** Leading or trailing symbols (`+`, `=`, `÷`, `$`, `★`, etc., ignoring emoji presentation selectors `U+FE0E`/`U+FE0F`) remain as tokens so titles or names distinguished by a symbol (such as `"Night Drives +"` vs. `"Night Drives"`) do not collide. Punctuation-only names (`"!!!"`, `"???"`) keep their characters verbatim.

## Recording resolution

Resolution runs once at the end of a scan that changed anything (`resolve.rs`, `resolve_store.rs`, `RESOLVER_VERSION = 3`, `Policy::current()` = `Policy::v3()`):

- **Deterministic fold:** Source items are processed in `first_seen_at_ms` order; each item sees only prior items, and the founding item keeps its recording across re-runs so resolving an unchanged library produces zero changes.
- **Evidence ladder:**
  1. User-pinned decisions and `identity_exclusion` rows always take precedence.
  2. Exact `same_audio` hash match binds immediately across albums.
  3. Strong identifiers (`mb_recording`, `isrc`) bind when durations agree within `identifier_duration_max_ms` (10,000 ms).
  4. `tags_duration` scoring computes weighted distance over title, artist, duration, and album (`weights = [3.0, 3.0, 3.0, 0.3]`, `duration_grace_ms = 2_000`, `duration_span_ms = 20_000`, `title_note_penalty = 0.1`), binding only when distance `<= bind_max` (`0.04`) and the closest eligible runner-up is at least `min_gap` (`0.25`) further away.
- **Policy 3 safeguards:**
  - **Version markers:** Mismatched version markers (`VERSION_MARKERS` on titles, `ALBUM_MARKERS` on album titles, unified `CAPPELLA` spellings, and song-title part numbers like `Part II` / `Pt. 2`, while ignoring album part numbers) prevent false merges. `"remaster"` is deliberately not a blocker so remasters share a recording.
  - **Cross-album and same-release guards:** `tags_bind_across_albums = false` with an exact-master exception of `across_albums_within_ms = 20` ms; `tracks_of_one_release_differ = true` prevents two different disc/track positions on the same release from merging by tags alone; eligible runner-ups are weighed independently; fullwidth brackets are stripped when forming title cores.
- **Audit trail and review:** Every `bind`, `unbind`, `merge`, or `unmerge` writes an immutable `identity_decision` row with `method`, `resolver_version`, `normalizer_version`, `score`, `runner_up_gap`, and `evidence_json`. Near misses (`< review_below = 0.25`) or withheld cross-album/same-release matches create a new recording marked `needs_review = 1`. Exact audio, identifier, and user links also record `alias_observation` pairs (with raw spellings).
- **Diagnostics (`identity_store.rs`):** `identity.dryRun` evaluates Policy 1, 2, or 3 (or custom `PolicyOverrides`) on a read-only connection without writing; `identity.review` returns flagged decisions with recomputed penalty vectors.

## Catalog projection and displayed metadata

`crates/ritornel-domain/src/catalog.rs` and `discs.rs` (`CATALOG_VERSION = 5`) project claims, the tag values answers put on files (see [`docs/internals/corrections.md`](corrections.md), "Answers"), and durable `metadata_edit` overrides into artists, artist credits, recordings, releases, release groups, track appearances, and FTS5 `search_text`:

- **Diffing writer (`resolve_store.rs`):** `Plan` compares the projected catalog against `Shown` rows in `library.db` and writes only changed rows. Running resolution when nothing changed writes nothing and takes no library revision. `library_setting` records the last applied `(extractor, resolver, normalizer, catalog)` versions so upgrades re-resolve automatically.
- **Artists and credits:** Artist strings are not heuristically split on `"&"` or commas; multi-valued tags and MBID lists create multi-artist credits with preserved join phrases. Artists are keyed in `artist_key` by MBID or normalized name. An un-ID'd credit matching an artist's unique claimed sort name (same words reordered, guarded against ambiguous sort names) binds to that artist. Albums with differing track artists and no album artist use `"Various Artists"`; albums where no file names any artist display under `"Unknown Artist"` while keeping the same grouping behavior.
- **Multi-disc releases and release groups:** `discs.rs` infers disc numbers from sibling folders (`discs_from_folders`) and from `"Disc N"` / `"CD N"` markers at the end of or before bracketed notes in album titles (`disc_from_album_title`, keeping `"Vol."` and `"Part"` out of disc markers). Releases group by durable `chosen_release_group_id`, shared release-group MBID, or shared recordings; each `release_group` projects `display_release_id`, primary/secondary types, and earliest release date/year.
- **Artist portraits (`artist_pictures.rs`):** `artists.importPictures` reads configured read-only picture folders (`picture_folder`), matches portraits by artist MBID or normalized name, and updates `artist_picture`.

## Scrobble matching

Matching text-only imported scrobbles (`scrobble_subject` source items) to the library is a pure function in `crates/ritornel-domain/src/scrobble.rs` (`MATCHER_VERSION = 2`) run by `crates/ritornel-engine/src/import_store.rs`:

- **Never creates catalog entities:** A scrobble either binds to an existing canonical recording or stays unbound.
- **Two-pass matching:** First compares normalized `(artist, title)` (`Rule::Exact`), then compares loosely (`Rule::Loose`, folding spacing, punctuation, leading `"The"`, accents, and featured artists). Album titles disambiguate multiple matching recordings and hold back single-recording matches when the scrobble's album fits none of the recording's appearances. Service-supplied MBIDs support matching and never bind alone.
- **Verdicts and re-matching:** Returns `Verdict::Bind`, `Verdict::Doubtful` (`Doubt::{Several, Album, Near}`), or `Verdict::Absent`. Doubtful matches are surfaced by `lastfm.report`. `LibraryStore::match_library` judges every track another service named again, but those the owner bound: it reads whether anything differs, and only then writes (internal write method `identity.matchScrobbles`, `MATCH_METHOD` in `import_store.rs`), judging again inside the write against what the library holds then. It runs in a scan's last phase (`Match`) after a resolve that wrote anything, and again when a scan whose resolve had committed is taken up after a stop, so a record added by a scan has its plays bound without a sync; and at the end of a Last.fm sync, which judges only that account's tracks and counts its bindings (`match_library_for`, `Scope::Account`): what the rest of the library holds is judged by the scan or the run that changed it. Applying or undoing a run of Spotify streams judges Spotify's tracks alone, in the run's own write (`bind_in` with `Scope::Spotify`), and leaves their plays to the derivation of every play that follows.
- **Spotify's tracks:** A `spotify_track` source item is judged as a scrobble's text is, by the artist, album and title of its newest stream in force ("Spotify streams" in `docs/internals/history.md`), so a stream's track binds as a scrobble of the same names does and is held back where its album fits none. Of streams that ended at the same moment the names decide, so the order archives came in never does. A track no stream in force names has the binding the matcher gave it released, with an `unbind` decision whose verdict is `unnamed`; one the owner bound keeps it.

## Decisions

- **Read-only music roots.** Ritornel never modifies, renames, or deletes user audio files; all identity decisions, edits, and corrections live in `library.db`.
- **Audio-only BLAKE3 hash over file hash.** Hashing decoded-frame boundaries/packet payloads without tag headers keeps track identity invariant across retagging while avoiding full PCM decode during scans.
- **Conservative normalization and resolution.** Keeping word boundaries, combining marks, and edge symbols—and holding back cross-album or ambiguous matches for review—favors missed merges over wrong merges, because wrong merges silently conflate distinct works and histories.
- **Pure domain logic with diffing persistence.** Keeping extraction, normalization, resolution, catalog projection, and scrobble matching pure in `ritornel-domain` makes dry runs, policy comparisons, and regression tests fast and deterministic.

## Later

- `library.db` and `ritornel-domain` already support `identity_decision` actions (`merge`, `unmerge`, `unbind`), entity redirect chains (`redirect_to_id`, `canonical_id`), `identity_exclusion`, `metadata_edit`, and `release_group.{presentation, preferred_release_id}`, awaiting interactive review and correction commands.
