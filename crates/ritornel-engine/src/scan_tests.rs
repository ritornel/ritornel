//! The scan against copies of the audio fixtures in a temporary root: what
//! a first scan records and resolves, the rescan cases in "Rescans and file
//! moves" in docs/internals/identity.md, that a root is only ever read, and
//! that a scan cut off after any step converges to what an uninterrupted one
//! makes.
//!
//! `a.flac`, `b.flac`, `a.mp3` and `a.m4a` are tagged as one invented song,
//! "Paper Kites", and `b.flac` holds other audio than the rest. The
//! `retagged` files hold the audio of `a` under the tags of another song.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ritornel_contract::ScanSummary;

use crate::media_fixtures::fixtures;
use crate::platform::{mode_of, seal};
use crate::testing::{CountingEntropy, FixedClock, Scratch};
use crate::{
    Command, Error, Fingerprint, Id, JobState, LibraryStore, NewJob, Role, ScanSettings,
    ServerIdentity, ServerStore, Source, WriteRequest, run_scan,
};

const T0: i64 = 1_763_000_000_000;

fn id(byte: u8) -> Id {
    Id::from_bytes([byte; 16])
}

/// A server with a library in memory, and one root in a temporary folder.
pub(crate) struct Rig {
    scratch: Scratch,
    clock: Arc<FixedClock>,
    pub(crate) server: ServerStore,
    pub(crate) library: LibraryStore,
    pub(crate) main: Source,
    commands: u8,
    boot: u8,
}

impl Rig {
    pub(crate) fn new() -> Self {
        let scratch = Scratch::new();
        let clock = Arc::new(FixedClock::at(T0));
        let entropy = Arc::new(CountingEntropy::default());
        let server = ServerStore::open_in_memory(clock.clone(), entropy.clone()).unwrap();
        let hub = ServerIdentity {
            server_id: id(0x11),
            public_key: [0; 32],
            name: "desk".to_owned(),
            role: Role::Unassigned,
            library_id: None,
            created_at_ms: T0,
        };
        let creation = WriteRequest {
            method: "library.create",
            command: None,
            client: None,
        };
        let (library, _) = LibraryStore::create_in_memory(
            &hub,
            creation,
            |_| String::new(),
            clock.clone(),
            entropy,
        )
        .unwrap();
        let mut rig = Self {
            scratch,
            clock,
            server,
            library,
            main: Source {
                row: 0,
                public_id: id(0),
                name: String::new(),
                kind: String::new(),
                on_this_server: true,
            },
            commands: 0,
            boot: 0xb0,
        };
        rig.main = rig.add_root("main");
        rig
    }

    fn command(&mut self) -> Command {
        self.commands += 1;
        Command {
            id: id(self.commands),
            fingerprint: Fingerprint::of("test", &[self.commands]),
        }
    }

    /// Adds a root named `name` in a folder of that name.
    fn add_root(&mut self, name: &str) -> Source {
        let path = self.scratch.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        self.server.add_root(name, path.to_str().unwrap()).unwrap();
        let request = WriteRequest {
            method: "sources.add",
            command: Some(self.command()),
            client: None,
        };
        self.library
            .add_source(request, name, |source, _| source.public_id.to_string())
            .unwrap();
        self.library
            .sources()
            .unwrap()
            .into_iter()
            .find(|source| source.name == name)
            .unwrap()
    }

    fn root(&self) -> PathBuf {
        self.scratch.path().join("main")
    }

    /// Puts a copy of a fixture at a path under the main root, with a time
    /// of its own, so that a file replaced within a millisecond still
    /// reads as changed.
    pub(crate) fn put(&mut self, path: &str, fixture: &str) {
        let to = self.root().join(path);
        self.put_at(&to, fixture);
    }

    fn put_at(&mut self, to: &Path, fixture: &str) {
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(fixtures().join(fixture), to).unwrap();
        self.stamp(to);
    }

    fn stamp(&mut self, path: &Path) {
        self.commands += 1;
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000 + 60 * u64::from(self.commands));
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    /// Runs a scan of a source as a claimed job.
    pub(crate) fn try_scan(
        &mut self,
        source: &Source,
        settings: ScanSettings,
    ) -> Result<ScanSummary, Error> {
        let new = NewJob {
            kind: "scan",
            dedupe_key: source.name.clone(),
            target: source.public_id,
            params_json: "{}".to_owned(),
            session: None,
        };
        let command = self.command();
        self.server.start_job(command, "scan.start", new).unwrap();
        self.resume(settings)
    }

    /// Claims the queued job under a new run of the server and carries it
    /// on to its end.
    pub(crate) fn resume(&mut self, settings: ScanSettings) -> Result<ScanSummary, Error> {
        self.boot += 1;
        self.clock.set(self.clock_ms() + 1000);
        self.server
            .requeue_jobs_of_other_runs(id(self.boot))
            .unwrap();
        let job = self.server.claim_job(id(self.boot)).unwrap().unwrap();
        let row = job.id;
        let scanned = run_scan(job, &self.server, &self.library, settings);
        match &scanned {
            Ok(_) => self
                .server
                .finish_job(row, JobState::Succeeded, None)
                .unwrap(),
            // A fail point is a crash: the job is left as the run left it.
            Err(Error::FailPoint) => return scanned,
            Err(error) => self
                .server
                .finish_job(row, JobState::Failed, Some(error.to_string()))
                .unwrap(),
        };
        scanned
    }

    fn clock_ms(&self) -> i64 {
        use crate::Clock;
        self.clock.now_ms()
    }

    pub(crate) fn scan(&mut self) -> ScanSummary {
        let source = self.main.clone();
        self.try_scan(&source, ScanSettings::default()).unwrap()
    }

    pub(crate) fn rows(&self, sql: &str) -> Vec<String> {
        self.library
            .read(|connection| {
                let mut statement = connection.prepare(sql)?;
                let columns = statement.column_count();
                let rows = statement.query_map([], |row| {
                    (0..columns)
                        .map(|column| {
                            Ok(match row.get_ref(column)? {
                                rusqlite::types::ValueRef::Null => "-".to_owned(),
                                rusqlite::types::ValueRef::Integer(number) => number.to_string(),
                                rusqlite::types::ValueRef::Real(number) => number.to_string(),
                                rusqlite::types::ValueRef::Text(text) => {
                                    String::from_utf8_lossy(text).into_owned()
                                }
                                rusqlite::types::ValueRef::Blob(bytes) => {
                                    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
                                }
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|columns| columns.join("|"))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .unwrap()
    }

    pub(crate) fn one(&self, sql: &str) -> String {
        let rows = self.rows(sql);
        assert_eq!(rows.len(), 1, "{sql}: {rows:?}");
        rows.into_iter().next().unwrap()
    }

    pub(crate) fn revision(&self) -> i64 {
        self.library.meta().unwrap().revision
    }

    /// Every row of every table, as text: what a write would change.
    pub(crate) fn dump(&self) -> Vec<String> {
        let tables = self.rows(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'search_text_%' ORDER BY name",
        );
        tables
            .iter()
            .flat_map(|table| {
                self.rows(&format!("SELECT '{table}', * FROM {table} ORDER BY rowid"))
            })
            .collect()
    }

    /// What the library is, with no key and no time in it: each file with
    /// what it is bound to and what that shows.
    fn shape(&self) -> Vec<String> {
        self.rows(
            "SELECT si.provider_key, si.missing_since_ms IS NOT NULL, si.retired_at_ms IS NOT NULL,
                    (SELECT count(*) FROM source_item_revision sr WHERE sr.source_item_id = si.id),
                    d.method, d.needs_review, r.display_title, rc.display_text, ta.display_title,
                    ta.display_disc_number, ta.display_track_number, rel.display_title, relc.display_text,
                    rel.display_date, g.display_primary_type, g.display_first_year,
                    (SELECT count(*) FROM assignment o WHERE o.recording_id = a.recording_id
                       AND o.superseded_at_ms IS NULL),
                    (SELECT count(*) FROM release o WHERE o.release_group_id = g.id)
             FROM source_item si
             LEFT JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             LEFT JOIN identity_decision d ON d.id = a.decision_id
             LEFT JOIN recording r ON r.id = a.recording_id
             LEFT JOIN artist_credit rc ON rc.id = r.display_artist_credit_id
             LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
             LEFT JOIN release rel ON rel.id = ta.release_id
             LEFT JOIN artist_credit relc ON relc.id = rel.display_artist_credit_id
             LEFT JOIN release_group g ON g.id = rel.release_group_id
             ORDER BY si.provider_key, si.id",
        )
    }
}

/// An album folder of the four files tagged as one song, a second song,
/// and two files that are not audio.
fn album(rig: &mut Rig) {
    rig.put("Moss Arcade/Tin Weather/02 Paper Kites.flac", "a.flac");
    rig.put("Moss Arcade/Tin Weather/copy.mp3", "a.mp3");
    rig.put("Elsewhere/another.flac", "a.retagged.ogg");
    rig.put("Elsewhere/broken.m4a", "no-packets.m4a");
    rig.put("Elsewhere/not-audio.flac", "not-audio.flac");
}

#[test]
fn a_first_scan_records_every_file_and_resolves_the_ones_it_can_read() {
    let mut rig = Rig::new();
    album(&mut rig);
    let summary = rig.scan();
    assert_eq!(
        summary,
        ScanSummary {
            files: 5,
            new: 5,
            unreadable: 2,
            bytes_read: summary.bytes_read,
            assignments: 3,
            recordings: 2,
            releases: 1,
            ..ScanSummary::default()
        }
    );
    assert!(summary.bytes_read > 10_000);
    // Three steps: the one batch of files, and the resolution.
    assert_eq!(
        rig.rows("SELECT method FROM library_revision ORDER BY rev"),
        ["library.create", "sources.add", "scan.step", "scan.step"]
    );
    assert_eq!(
        rig.shape(),
        [
            "Elsewhere/another.flac|0|0|1|new|0|Another Name Entirely|Somebody Else|Another Name Entirely|-|-|-|-|-|-|-|1|0",
            "Elsewhere/broken.m4a|0|0|1|-|-|-|-|-|-|-|-|-|-|-|-|0|0",
            "Elsewhere/not-audio.flac|0|0|1|-|-|-|-|-|-|-|-|-|-|-|-|0|0",
            "Moss Arcade/Tin Weather/02 Paper Kites.flac|0|0|1|new|0|Paper Kites|Moss Arcade|Paper Kites|1|2|Tin Weather|Moss Arcade|2019-03-08|ep|2004|2|1",
            "Moss Arcade/Tin Weather/copy.mp3|0|0|1|tags_duration|0|Paper Kites|Moss Arcade|Paper Kites|-|2|Tin Weather|Moss Arcade|2019-03-08|ep|2004|2|1",
        ]
    );
    // A file that cannot be read stays, with the error in its payload.
    assert_eq!(
        rig.rows(
            "SELECT si.provider_key, json_extract(sr.payload_json, '$.error.kind'), sr.audio_asset_id
             FROM source_item si JOIN source_item_revision sr ON sr.id = si.current_revision_id
             WHERE sr.audio_asset_id IS NULL ORDER BY 1"
        ),
        ["Elsewhere/broken.m4a|malformed|-", "Elsewhere/not-audio.flac|unsupported|-"]
    );
    // Claims are this extractor's, and the embedded picture is the cover.
    assert_eq!(
        rig.one(
            "SELECT count(DISTINCT extractor_version), min(extractor_version) FROM metadata_claim"
        ),
        format!("1|{}", ritornel_domain::EXTRACTOR_VERSION)
    );
    assert_eq!(
        rig.rows("SELECT namespace, value FROM identifier_claim WHERE scope = 'release_group'"),
        ["mb_release_group|99999999-8888-4777-8666-555555555555"]
    );
    assert_eq!(
        rig.one(
            "SELECT p.mime_type, p.width, p.height, rp.source_kind, r.display_picture_id = p.id
             FROM picture p JOIN revision_picture rp ON rp.picture_id = p.id, release r"
        ),
        "image/png|8|8|embedded|1"
    );
    // The text search index holds each appearance.
    assert_eq!(
        rig.rows("SELECT title, artist, album FROM search_text WHERE search_text MATCH 'kites' ORDER BY rowid"),
        ["paper kites|moss arcade|tin weather", "paper kites|moss arcade|tin weather"]
    );
    assert_eq!(
        rig.one("SELECT value_json FROM library_setting"),
        format!(
            r#"{{"extractor":{},"resolver":{},"normalizer":{},"catalog":{}}}"#,
            ritornel_domain::EXTRACTOR_VERSION,
            ritornel_domain::RESOLVER_VERSION,
            ritornel_domain::NORMALIZER_VERSION,
            ritornel_domain::CATALOG_VERSION
        )
    );
    // Every row a scan wrote is stamped with the revision that wrote it.
    assert_eq!(
        rig.one("SELECT count(*) FROM assignment WHERE rev <> 4"),
        "0"
    );
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE rev NOT IN (3, 4)"),
        "0"
    );

    let stats = rig.library.stats().unwrap();
    assert_eq!(
        (stats.files, stats.files_unreadable, stats.files_missing),
        (5, 2, 0)
    );
    assert_eq!(
        (stats.recordings, stats.appearances, stats.loose_tracks),
        (2, 3, 1)
    );
    assert_eq!(
        (stats.releases, stats.release_groups, stats.artists),
        (1, 1, 2)
    );
    assert_eq!(
        (
            stats.groups_with_kind,
            stats.groups_with_first_year,
            stats.groups_with_first_year_from_tag
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            stats.needs_review,
            stats.unresolved,
            stats.files_with_musicbrainz_id
        ),
        (0, 0, 2)
    );
    assert_eq!(
        (
            stats.pictures,
            stats.releases_with_cover,
            stats.library_revision
        ),
        (1, 1, 4)
    );
    let named = |counts: &[ritornel_contract::Count]| -> Vec<String> {
        counts
            .iter()
            .map(|count| format!("{} {}", count.name, count.count))
            .collect()
    };
    assert_eq!(named(&stats.codecs), ["flac 1", "mp3 1", "vorbis 1"]);
    assert_eq!(named(&stats.decisions), ["new 2", "tags_duration 1"]);
    assert_eq!(named(&stats.kinds), ["ep 1"]);
    assert_eq!(stats.unreadable_reasons.len(), 2);
}

#[test]
fn a_rescan_with_no_changes_writes_nothing_and_takes_no_library_revision() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    let (revision, before) = (rig.revision(), rig.dump());

    let summary = rig.scan();
    assert_eq!(
        summary,
        ScanSummary {
            files: 5,
            unchanged: 5,
            ..ScanSummary::default()
        }
    );
    assert_eq!(rig.revision(), revision);
    assert_eq!(rig.dump(), before);
    // And again: quiet is quiet however often.
    rig.scan();
    assert_eq!((rig.revision(), rig.dump()), (revision, before));
}

#[test]
fn a_moved_file_keeps_its_item_and_its_binding() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    let before = rig.one(
        "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                          ON a.source_item_id = si.id WHERE si.provider_key LIKE '%copy.mp3'",
    );
    let root = rig.root();
    std::fs::create_dir_all(root.join("Sorted/Moss Arcade")).unwrap();
    std::fs::rename(
        root.join("Moss Arcade/Tin Weather/copy.mp3"),
        root.join("Sorted/Moss Arcade/02.mp3"),
    )
    .unwrap();

    let summary = rig.scan();
    assert_eq!(
        (
            summary.moved,
            summary.new,
            summary.missing,
            summary.unchanged
        ),
        (1, 0, 0, 4)
    );
    // The same item, re-keyed, with a new revision, and the same assignment
    // row: nothing about the binding was written.
    assert_eq!(
        rig.one(
            "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                 ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 WHERE si.provider_key = 'Sorted/Moss Arcade/02.mp3'"
        ),
        before
    );
    assert_eq!(rig.one("SELECT count(*) FROM source_item"), "5");
    assert_eq!(
        rig.rows("SELECT revision_no, json_extract(payload_json, '$.path') FROM source_item_revision
                  WHERE source_item_id = (SELECT id FROM source_item WHERE provider_key LIKE 'Sorted%') ORDER BY 1"),
        ["1|Moss Arcade/Tin Weather/copy.mp3", "2|Sorted/Moss Arcade/02.mp3"]
    );
    assert_eq!(rig.one("SELECT count(*) FROM assignment"), "3");
}

#[test]
fn a_file_moved_to_another_root_is_a_new_item_bound_by_the_same_audio() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    let other = rig.add_root("other");
    let to = rig.scratch.path().join("other/Tin Weather/02.flac");
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    std::fs::rename(
        rig.root()
            .join("Moss Arcade/Tin Weather/02 Paper Kites.flac"),
        &to,
    )
    .unwrap();

    let summary = rig.try_scan(&other, ScanSettings::default()).unwrap();
    assert_eq!((summary.new, summary.moved), (1, 0));
    let summary = rig.scan();
    assert_eq!((summary.missing, summary.unchanged), (1, 4));
    // The old item is missing and keeps its rows. The new one is the same
    // recording, by the same audio, and the asset is one.
    assert_eq!(
        rig.rows(
            "SELECT src.name, si.missing_since_ms IS NOT NULL, d.method, a.recording_id, sr.audio_asset_id
             FROM source_item si JOIN source_instance src ON src.id = si.source_instance_id
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN identity_decision d ON d.id = a.decision_id
             JOIN source_item_revision sr ON sr.id = si.current_revision_id
             WHERE si.provider_key LIKE '%02%.flac' ORDER BY si.id"
        ),
        ["main|1|new|2|2", "other|0|same_audio|2|2"]
    );
}

#[test]
fn a_copy_is_a_new_item_with_the_same_asset_and_a_second_location() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    rig.put("Backup/02 Paper Kites.flac", "a.flac");
    let summary = rig.scan();
    assert_eq!((summary.new, summary.moved, summary.unchanged), (1, 0, 5));
    assert_eq!(
        rig.rows(
            "SELECT rel_path, missing_since_ms FROM asset_location
             WHERE audio_asset_id = (SELECT audio_asset_id FROM asset_location WHERE rel_path LIKE 'Backup%')
             ORDER BY 1"
        ),
        ["Backup/02 Paper Kites.flac|-", "Moss Arcade/Tin Weather/02 Paper Kites.flac|-"]
    );
    assert_eq!(
        rig.one("SELECT d.method FROM assignment a JOIN identity_decision d ON d.id = a.decision_id
                 JOIN source_item si ON si.id = a.source_item_id WHERE si.provider_key LIKE 'Backup%'"),
        "same_audio"
    );
    assert_eq!(rig.one("SELECT count(*) FROM recording"), "2");
}

#[test]
fn a_retagged_file_keeps_its_item_and_asset_and_shows_its_new_tags() {
    let mut rig = Rig::new();
    rig.put("Moss Arcade/Tin Weather/02 Paper Kites.flac", "a.flac");
    rig.put("Loose/tone.wav", "a.wav");
    rig.scan();
    let item = "(SELECT id FROM source_item WHERE provider_key LIKE '%Paper Kites.flac')";
    let before = rig.one(&format!(
        "SELECT si.id, sr.audio_asset_id, si.current_recording_id FROM source_item si
         JOIN source_item_revision sr ON sr.id = si.current_revision_id WHERE si.id = {item}"
    ));
    rig.put(
        "Moss Arcade/Tin Weather/02 Paper Kites.flac",
        "a.retagged.flac",
    );

    let summary = rig.scan();
    assert_eq!((summary.changed, summary.new, summary.retired), (1, 0, 0));
    // The same item, asset and recording: the recording was made for this
    // file, and the file keeps it under its new tags.
    assert_eq!(
        rig.one(&format!(
            "SELECT si.id, sr.audio_asset_id, si.current_recording_id FROM source_item si
             JOIN source_item_revision sr ON sr.id = si.current_revision_id WHERE si.id = {item}"
        )),
        before
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT count(*) FROM source_item_revision WHERE source_item_id = {item}"
        )),
        "2"
    );
    assert_eq!(rig.one("SELECT count(*) FROM audio_asset"), "2");
    // Its claims were rebuilt from the new revision, and what it shows
    // follows them.
    assert_eq!(
        rig.rows(&format!(
            "SELECT c.value, c.revision_id = si.current_revision_id FROM metadata_claim c
             JOIN source_item si ON si.id = c.source_item_id
             JOIN metadata_source s ON s.id = c.metadata_source_id
             WHERE c.source_item_id = {item} AND c.field = 'title' AND s.kind = 'file_tags'"
        )),
        ["Another Name Entirely|1"]
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT count(*) FROM identifier_claim WHERE source_item_id = {item}"
        )),
        "0"
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT ta.display_title, ta.release_id FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE a.source_item_id = {item} AND a.superseded_at_ms IS NULL"
        )),
        "Another Name Entirely|-"
    );
    // The old assignment was superseded, never updated.
    assert_eq!(
        rig.rows(&format!(
            "SELECT superseded_at_ms IS NOT NULL, superseded_by_id IS NOT NULL FROM assignment
             WHERE source_item_id = {item} ORDER BY id"
        )),
        ["1|1", "0|0"]
    );
    assert_eq!(
        rig.rows("SELECT rowid FROM search_text WHERE search_text MATCH 'entirely'")
            .len(),
        1
    );
    assert_eq!(
        rig.rows("SELECT rowid FROM search_text WHERE search_text MATCH 'kites'")
            .len(),
        0
    );
}

#[test]
fn the_same_song_encoded_again_keeps_its_item_with_a_new_asset() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.put("Tin Weather/elsewhere.flac", "a.retagged.flac");
    rig.scan();
    let before = rig.one("SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                          ON a.source_item_id = si.id WHERE si.provider_key = 'Tin Weather/02.flac'");
    // Other audio under the same song's tags: a re-rip.
    rig.put("Tin Weather/02.flac", "a.m4a");

    let summary = rig.scan();
    assert_eq!((summary.changed, summary.retired, summary.new), (1, 0, 0));
    assert_eq!(
        rig.one(
            "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                 ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 WHERE si.provider_key = 'Tin Weather/02.flac' AND si.retired_at_ms IS NULL"
        ),
        before
    );
    // Only this path's location left the old asset.
    assert_eq!(
        rig.rows("SELECT aa.codec, (SELECT count(*) FROM asset_location l WHERE l.audio_asset_id = aa.id)
                  FROM audio_asset aa ORDER BY aa.id"),
        ["flac|0", "flac|1", "aac|1"]
    );
    assert_eq!(
        rig.one("SELECT count(*), sum(retired_at_ms IS NOT NULL) FROM source_item"),
        "2|0"
    );
}

#[test]
fn a_different_song_saved_over_an_old_file_name_gets_a_new_source_item() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let old = rig.one(
        "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                       ON a.source_item_id = si.id",
    );
    rig.put("Tin Weather/02.flac", "a.retagged.flac");

    let summary = rig.scan();
    assert_eq!((summary.retired, summary.changed, summary.new), (1, 0, 0));
    // The old item is retired with its assignment, and a new item has the
    // path and a recording of its own.
    assert_eq!(
        rig.rows(
            "SELECT si.provider_key, si.retired_at_ms IS NOT NULL, a.superseded_at_ms IS NULL, r.display_title
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
             JOIN recording r ON r.id = a.recording_id ORDER BY si.id"
        ),
        ["Tin Weather/02.flac|1|1|Paper Kites", "Tin Weather/02.flac|0|1|Another Name Entirely"]
    );
    assert_eq!(
        rig.one(
            "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                        ON a.source_item_id = si.id WHERE si.retired_at_ms IS NOT NULL"
        ),
        old
    );
    assert_eq!(rig.one("SELECT count(*) FROM asset_location"), "1");
    // And then nothing: the next scan is quiet.
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_moved_file_keeps_its_item_when_other_audio_takes_its_old_path() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let before = rig.one(
        "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                          ON a.source_item_id = si.id",
    );
    let root = rig.root();
    std::fs::rename(
        root.join("Tin Weather/02.flac"),
        root.join("Tin Weather/moved.flac"),
    )
    .unwrap();
    rig.put("Tin Weather/02.flac", "a.retagged.flac");

    let summary = rig.scan();
    assert_eq!(
        (summary.moved, summary.new, summary.retired, summary.changed),
        (1, 1, 0, 0)
    );
    // The item followed its file, with the same assignment, and its
    // revisions say where it has been.
    assert_eq!(
        rig.one(
            "SELECT si.id, a.id, a.recording_id FROM source_item si JOIN assignment a
                 ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 WHERE si.provider_key = 'Tin Weather/moved.flac'"
        ),
        before
    );
    assert_eq!(
        rig.rows(
            "SELECT si.id, sr.revision_no, json_extract(sr.payload_json, '$.path')
             FROM source_item_revision sr JOIN source_item si ON si.id = sr.source_item_id
             ORDER BY si.id, sr.revision_no"
        ),
        [
            "1|1|Tin Weather/02.flac",
            "1|2|Tin Weather/moved.flac",
            "2|1|Tin Weather/02.flac"
        ]
    );
    // The audio now at the old path is a new item of its own, and nothing
    // was retired.
    assert_eq!(
        rig.rows(
            "SELECT si.provider_key, si.retired_at_ms IS NOT NULL, r.display_title
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
               AND a.superseded_at_ms IS NULL
             JOIN recording r ON r.id = a.recording_id ORDER BY si.id"
        ),
        [
            "Tin Weather/moved.flac|0|Paper Kites",
            "Tin Weather/02.flac|0|Another Name Entirely"
        ]
    );
    // And then nothing: the next scan is quiet.
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_file_renamed_into_a_moved_files_old_path_keeps_its_item() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.put("Tin Weather/03.flac", "a.retagged.flac");
    rig.scan();
    let items = |rig: &Rig| {
        rig.rows(
            "SELECT si.id, si.provider_key, sr.audio_asset_id FROM source_item si
             JOIN source_item_revision sr ON sr.id = si.current_revision_id ORDER BY si.id",
        )
    };
    assert_eq!(
        items(&rig),
        ["1|Tin Weather/02.flac|1", "2|Tin Weather/03.flac|2"]
    );
    // 02 moves aside, and 03 moves into its place.
    let root = rig.root();
    std::fs::rename(
        root.join("Tin Weather/02.flac"),
        root.join("Tin Weather/01.flac"),
    )
    .unwrap();
    std::fs::rename(
        root.join("Tin Weather/03.flac"),
        root.join("Tin Weather/02.flac"),
    )
    .unwrap();

    let summary = rig.scan();
    assert_eq!(
        (summary.moved, summary.new, summary.retired, summary.missing),
        (2, 0, 0, 0)
    );
    assert_eq!(
        items(&rig),
        ["1|Tin Weather/01.flac|1", "2|Tin Weather/02.flac|2"]
    );
}

#[test]
fn files_each_renamed_into_the_next_ones_place_keep_their_items() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.put("Tin Weather/03.mp3", "a.mp3");
    rig.scan();
    let items = |rig: &Rig| {
        rig.rows(
            "SELECT si.id, si.provider_key, sr.audio_asset_id FROM source_item si
             JOIN source_item_revision sr ON sr.id = si.current_revision_id ORDER BY si.id",
        )
    };
    let before = items(&rig);
    // Each moves up one, the last first: 03 to 04, 02 to 03, 01 to 02.
    let root = rig.root().join("Tin Weather");
    std::fs::rename(root.join("03.mp3"), root.join("04.mp3")).unwrap();
    std::fs::rename(root.join("02.flac"), root.join("03.mp3")).unwrap();
    std::fs::rename(root.join("01.flac"), root.join("02.flac")).unwrap();

    let summary = rig.scan();
    assert_eq!(
        (summary.moved, summary.new, summary.retired, summary.missing),
        (3, 0, 0, 0)
    );
    let after = items(&rig);
    let renamed = |row: &String, from: &str, to: &str| row.replace(from, to);
    assert_eq!(
        after,
        [
            renamed(&before[0], "01.flac", "02.flac"),
            renamed(&before[1], "02.flac", "03.mp3"),
            renamed(&before[2], "03.mp3", "04.mp3"),
        ]
    );
}

/// Each live item's ID, path, audio and current assignment row, by ID.
fn bound_items(rig: &Rig) -> Vec<String> {
    rig.rows(
        "SELECT si.id, si.provider_key, sr.audio_asset_id, a.id FROM source_item si
         JOIN source_item_revision sr ON sr.id = si.current_revision_id
         LEFT JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         WHERE si.retired_at_ms IS NULL ORDER BY si.id",
    )
}

#[test]
fn files_that_swap_paths_or_move_round_a_ring_keep_their_items() {
    for ring in [2, 3] {
        let mut rig = Rig::new();
        let files = [
            ("01.flac", "a.flac"),
            ("02.flac", "b.flac"),
            ("03.mp3", "a.mp3"),
        ];
        for (file, fixture) in &files[..ring] {
            rig.put(&format!("Tin Weather/{file}"), fixture);
        }
        rig.scan();
        let before = bound_items(&rig);
        // Each file takes the next one's path, and the last the first's.
        let root = rig.root().join("Tin Weather");
        let aside = rig.scratch.path().join("aside");
        std::fs::rename(root.join(files[ring - 1].0), &aside).unwrap();
        for at in (1..ring).rev() {
            std::fs::rename(root.join(files[at - 1].0), root.join(files[at].0)).unwrap();
        }
        std::fs::rename(&aside, root.join(files[0].0)).unwrap();

        let summary = rig.scan();
        assert_eq!(
            (
                summary.moved,
                summary.new,
                summary.retired,
                summary.changed,
                summary.missing
            ),
            (ring as u64, 0, 0, 0, 0),
            "{ring}"
        );
        // The same items and bindings, each at the next path.
        let path = |row: &String| row.split('|').nth(1).unwrap().to_owned();
        let expected: Vec<String> = (0..ring)
            .map(|at| before[at].replace(&path(&before[at]), &path(&before[(at + 1) % ring])))
            .collect();
        assert_eq!(bound_items(&rig), expected, "{ring}");
        assert_eq!(
            rig.one("SELECT count(*) FROM source_item"),
            ring.to_string()
        );
        // And then nothing: the next scan is quiet.
        let revision = rig.revision();
        rig.scan();
        assert_eq!(rig.revision(), revision, "{ring}");
    }
}

#[test]
fn a_file_moved_onto_another_files_path_keeps_its_item_and_retires_the_other() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let before = bound_items(&rig);
    // 02 is moved onto 01, in its place.
    let root = rig.root().join("Tin Weather");
    std::fs::rename(root.join("02.flac"), root.join("01.flac")).unwrap();

    let summary = rig.scan();
    assert_eq!(
        (
            summary.moved,
            summary.new,
            summary.retired,
            summary.changed,
            summary.missing
        ),
        (1, 0, 1, 0, 0)
    );
    // The moved file's item is at 01 with its binding. The item whose file
    // it replaced is retired, with everything it had.
    assert_eq!(
        bound_items(&rig),
        [before[1].replace("Tin Weather/02.flac", "Tin Weather/01.flac")]
    );
    assert_eq!(
        rig.rows(
            "SELECT si.id, si.provider_key, si.missing_since_ms IS NULL, a.id
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
               AND a.superseded_at_ms IS NULL
             WHERE si.retired_at_ms IS NOT NULL"
        ),
        [{
            let row: Vec<&str> = before[0].split('|').collect();
            format!("{}|{}|1|{}", row[0], row[1], row[3])
        }]
    );
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_file_moved_onto_a_path_and_new_audio_at_its_own_keep_apart() {
    for unreadable_first in [false, true] {
        let mut rig = Rig::new();
        let first = if unreadable_first {
            "not-audio.flac"
        } else {
            "a.flac"
        };
        rig.put("Tin Weather/01.flac", first);
        rig.put("Tin Weather/02.flac", "b.flac");
        rig.scan();
        let before = bound_items(&rig);
        // 02 is moved onto 01, and other audio is put where 02 was.
        let root = rig.root().join("Tin Weather");
        std::fs::rename(root.join("02.flac"), root.join("01.flac")).unwrap();
        rig.put("Tin Weather/02.flac", "a.retagged.opus");

        let summary = rig.scan();
        assert_eq!(
            (summary.moved, summary.new, summary.retired, summary.missing),
            (1, 1, 1, 0),
            "{unreadable_first}"
        );
        // 02's item followed its file, 01's is retired, and the audio at
        // 02 is an item of its own.
        let after = bound_items(&rig);
        assert_eq!(
            after[0],
            before[1].replace("Tin Weather/02.flac", "Tin Weather/01.flac"),
            "{unreadable_first}"
        );
        assert_eq!(after.len(), 2, "{unreadable_first}");
        assert!(
            after[1].contains("|Tin Weather/02.flac|"),
            "{unreadable_first}"
        );
        assert_eq!(
            rig.one(&format!(
                "SELECT retired_at_ms IS NOT NULL FROM source_item WHERE id = {}",
                before[0].split('|').next().unwrap()
            )),
            "1"
        );
        let revision = rig.revision();
        rig.scan();
        assert_eq!(rig.revision(), revision, "{unreadable_first}");
    }
}

#[test]
fn an_item_whose_audio_is_now_at_two_known_paths_follows_neither() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.put("Tin Weather/03.flac", "b.no-artist.flac");
    rig.scan();
    let first = rig.one("SELECT id FROM source_item WHERE provider_key = 'Tin Weather/01.flac'");
    // 01 is moved onto 02, and a copy of it is put over 03: which one the
    // item went to is not known, whatever order they are read in.
    let root = rig.root().join("Tin Weather");
    std::fs::copy(root.join("01.flac"), root.join("03.flac")).unwrap();
    std::fs::rename(root.join("01.flac"), root.join("02.flac")).unwrap();

    let summary = rig.scan();
    assert_eq!((summary.moved, summary.missing), (0, 1));
    assert_eq!(
        rig.one(&format!(
            "SELECT provider_key, missing_since_ms IS NOT NULL, retired_at_ms IS NULL
             FROM source_item WHERE id = {first}"
        )),
        "Tin Weather/01.flac|1|1"
    );
}

#[test]
fn a_swap_cut_off_after_its_moves_are_written_keeps_its_items_and_its_counts() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let before = bound_items(&rig);
    let root = rig.root().join("Tin Weather");
    let aside = rig.scratch.path().join("aside");
    std::fs::rename(root.join("01.flac"), &aside).unwrap();
    std::fs::rename(root.join("02.flac"), root.join("01.flac")).unwrap();
    std::fs::rename(&aside, root.join("02.flac")).unwrap();
    // Both files are read with nothing to write, so the moves are the
    // first step.
    let source = rig.main.clone();
    let settings = ScanSettings {
        fail_after_step: Some(1),
        ..ScanSettings::default()
    };
    assert!(matches!(
        rig.try_scan(&source, settings),
        Err(Error::FailPoint)
    ));
    let resumed = rig.resume(ScanSettings::default()).unwrap();
    assert_eq!((resumed.moved, resumed.retired, resumed.new), (2, 0, 0));
    let path = |row: &String| row.split('|').nth(1).unwrap().to_owned();
    assert_eq!(
        bound_items(&rig),
        [
            before[0].replace(&path(&before[0]), &path(&before[1])),
            before[1].replace(&path(&before[1]), &path(&before[0])),
        ]
    );
}

#[test]
fn a_file_that_vanishes_is_marked_missing_and_reclaims_its_rows_when_it_returns() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    let path = rig.root().join("Moss Arcade/Tin Weather/copy.mp3");
    let kept = rig.scratch.path().join("kept.mp3");
    std::fs::rename(&path, &kept).unwrap();
    let rows = "SELECT si.id, si.current_revision_id, a.id FROM source_item si
                JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                WHERE si.provider_key LIKE '%copy.mp3'";
    let before = rig.one(rows);

    let summary = rig.scan();
    assert_eq!((summary.missing, summary.unchanged), (1, 4));
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE missing_since_ms IS NOT NULL"),
        "1"
    );
    assert_eq!(rig.one(rows), before);
    assert_eq!(rig.library.stats().unwrap().files_missing, 1);
    // Still gone: nothing more to say.
    let revision = rig.revision();
    assert_eq!(rig.scan().missing, 0);
    assert_eq!(rig.revision(), revision);

    std::fs::rename(&kept, &path).unwrap();
    let summary = rig.scan();
    assert_eq!((summary.returned, summary.new, summary.changed), (1, 0, 0));
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE missing_since_ms IS NOT NULL"),
        "0"
    );
    assert_eq!(rig.one(rows), before);
}

#[test]
fn a_folder_that_cannot_be_read_is_passed_over_and_its_files_are_not_taken_for_gone() {
    use crate::platform::set_listable;
    let mut rig = Rig::new();
    album(&mut rig);
    let lost = rig.root().join("lost+found");
    std::fs::create_dir(&lost).unwrap();
    // As a disk's `lost+found` is.
    set_listable(&lost, false);
    // The rest of the root is scanned.
    assert_eq!(rig.scan().files, 5);

    // A folder of known files closed since: they are not missing.
    let album = rig.root().join("Moss Arcade");
    set_listable(&album, false);
    let summary = rig.scan();
    set_listable(&album, true);
    set_listable(&lost, true);
    assert_eq!((summary.files, summary.missing), (3, 0));
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE missing_since_ms IS NOT NULL"),
        "0"
    );
    assert_eq!(rig.scan().unchanged, 5);
}

#[test]
fn a_root_that_is_unreachable_or_empty_fails_the_scan_and_marks_nothing_missing() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.scan();
    let (revision, before) = (rig.revision(), rig.dump());
    let (root, away) = (rig.root(), rig.scratch.path().join("unmounted"));
    std::fs::rename(&root, &away).unwrap();
    let main = rig.main.clone();

    // Not there; there and empty; there with only what macOS leaves at an
    // unmounted share's mount point; there with no audio at all.
    for step in 0..4 {
        match step {
            1 => std::fs::create_dir(&root).unwrap(),
            2 => std::fs::write(root.join(".DS_Store"), b"Bud1").unwrap(),
            3 => std::fs::write(root.join("notes.txt"), b"not audio").unwrap(),
            _ => {}
        }
        let refused = rig.try_scan(&main, ScanSettings::default());
        assert!(
            matches!(&refused, Err(Error::RootOffline { path }) if *path == root),
            "{refused:?}"
        );
        assert_eq!((rig.revision(), rig.dump()), (revision, before.clone()));
    }
    let failed = rig.server.jobs(1).unwrap().remove(0);
    assert_eq!(failed.state, JobState::Failed);
    assert!(failed.error.unwrap().contains("offline"));

    // Mounted again, everything is where it was.
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::rename(&away, &root).unwrap();
    assert_eq!(rig.scan().unchanged, 5);
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_folders_image_is_recorded_for_each_file_and_replacing_it_reads_them_again() {
    /// The header of a PNG of this size, which is all that is read of it.
    fn png(width: u8, height: u8) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 0, width, 0, 0, 0, height, 8, 2, 0, 0, 0]);
        png
    }
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "b.flac");
    rig.put("Tin Weather/02.mp3", "a.retagged.mp3");
    let cover = rig.root().join("Tin Weather/cover.png");
    std::fs::write(&cover, png(30, 20)).unwrap();
    rig.stamp(&cover);
    rig.scan();
    assert_eq!(
        rig.rows(
            "SELECT rp.source_kind, rp.file_name, p.width, p.height, p.mime_type
                  FROM revision_picture rp JOIN picture p ON p.id = rp.picture_id ORDER BY rp.id"
        ),
        [
            "folder_image|cover.png|30|20|image/png",
            "folder_image|cover.png|30|20|image/png"
        ]
    );
    assert_eq!(rig.one("SELECT count(*) FROM picture"), "1");
    assert_eq!(
        rig.one("SELECT count(*) FROM release WHERE display_picture_id = 1"),
        "1"
    );
    let revision = rig.revision();
    assert_eq!(rig.scan().unchanged, 2);
    assert_eq!(rig.revision(), revision);

    // A new image writes new revisions for that folder's files.
    std::fs::write(&cover, png(40, 40)).unwrap();
    rig.stamp(&cover);
    let summary = rig.scan();
    assert_eq!((summary.changed, summary.unchanged), (2, 0));
    assert_eq!(rig.one("SELECT count(*) FROM source_item_revision"), "4");
    assert_eq!(
        rig.one("SELECT p.width FROM release r JOIN picture p ON p.id = r.display_picture_id"),
        "40"
    );
}

/// Every file and folder under a path: its name, size, time and mode (on
/// Windows, whether it is read-only).
fn listing(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::metadata(&path).unwrap();
            found.push(format!(
                "{} {} {:?} {}",
                path.strip_prefix(root).unwrap().display(),
                if metadata.is_dir() { 0 } else { metadata.len() },
                metadata.modified().unwrap(),
                mode_of(&metadata)
            ));
            if metadata.is_dir() {
                folders.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_scan_only_reads_a_root_that_cannot_be_written() {
    let mut rig = Rig::new();
    album(&mut rig);
    rig.put("Moss Arcade/Tin Weather/cover.jpg", "not-audio.flac");
    let root = rig.root();
    // Nothing under the root can be written, renamed or deleted, so a
    // scan that tried to would fail.
    seal(&root, true);
    // Modes do not hold the superuser, which a container's tests may run
    // as. There the listing below is the whole of the check.
    let probe = root.join("Elsewhere/new-file");
    if std::fs::write(&probe, b"x").is_ok() {
        std::fs::remove_file(&probe).unwrap();
        seal(&root, true);
    }
    let before = listing(&root);

    let summary = rig.scan();
    assert_eq!((summary.files, summary.new), (5, 5));
    assert_eq!(rig.scan().unchanged, 5);
    assert_eq!(listing(&root), before);
    assert_eq!(listing(rig.scratch.path()).len(), before.len() + 1);
    seal(&root, false);
}

/// A library of several folders, so that a scan of two files a step has
/// steps to be cut off between.
fn several(rig: &mut Rig) {
    album(rig);
    rig.put("Moss Arcade/Tin Weather/03.m4a", "a.m4a");
    rig.put(
        "Moss Arcade/Tin Weather (copy)/02 Paper Kites.flac",
        "a.flac",
    );
    rig.put("Loose/b.flac", "b.flac");
    rig.put("Loose/tone.wav", "a.wav");
    rig.put("Loose/tone.opus", "a.retagged.opus");
}

#[test]
fn a_scan_cut_off_after_marking_files_missing_still_counts_them() {
    // Cut off straight after marking two files missing, before saving that,
    // and after the resolution that follows, which saved it.
    for cut in [1, 2] {
        let mut rig = Rig::new();
        several(&mut rig);
        rig.scan();
        let root = rig.root();
        std::fs::remove_file(root.join("Loose/b.flac")).unwrap();
        std::fs::remove_file(root.join("Loose/tone.wav")).unwrap();
        let source = rig.main.clone();
        let cut_off = rig.try_scan(
            &source,
            ScanSettings {
                fail_after_step: Some(cut),
                ..ScanSettings::default()
            },
        );
        assert!(
            matches!(cut_off, Err(Error::FailPoint)),
            "{cut}: {cut_off:?}"
        );
        // Started again, its walk finds them missing already.
        let resumed = rig.resume(ScanSettings::default()).unwrap();
        assert_eq!((resumed.missing, resumed.unchanged), (2, 8), "{cut}");
        assert_eq!(
            rig.one("SELECT count(*) FROM source_item WHERE missing_since_ms IS NOT NULL"),
            "2"
        );
    }
}

#[test]
fn a_scan_cut_off_after_any_step_converges_to_what_an_uninterrupted_scan_makes() {
    let small = ScanSettings {
        batch_files: 2,
        ..ScanSettings::default()
    };
    let mut whole = Rig::new();
    several(&mut whole);
    let source = whole.main.clone();
    let uninterrupted = whole.try_scan(&source, small).unwrap();
    let steps = whole
        .rows("SELECT rev FROM library_revision WHERE method = 'scan.step'")
        .len() as i64;
    // Five batches of two files, and the resolution.
    assert_eq!(steps, 6);
    assert_eq!(uninterrupted.new, 10);

    for cut in 1..=steps {
        let mut rig = Rig::new();
        several(&mut rig);
        let source = rig.main.clone();
        // The run dies straight after the library commit of step `cut`,
        // before the step's checkpoint is saved.
        let cut_off = rig.try_scan(
            &source,
            ScanSettings {
                fail_after_step: Some(cut),
                ..small
            },
        );
        assert!(
            matches!(cut_off, Err(Error::FailPoint)),
            "{cut}: {cut_off:?}"
        );
        let left = rig.server.jobs(1).unwrap().remove(0);
        assert_eq!(
            (left.state, left.step),
            (JobState::Running, cut - 1),
            "{cut}"
        );

        // The next run finds the job claimed by a run that is gone, takes
        // it back, and carries on. Cut off after the resolution, it is
        // finished without walking the root, which may be offline by then.
        let root = rig.root();
        let away = rig.scratch.path().join("unmounted");
        if cut == steps {
            std::fs::rename(&root, &away).unwrap();
        }
        let resumed = rig.resume(small).unwrap();
        if cut == steps {
            std::fs::rename(&away, &root).unwrap();
        }
        assert_eq!(rig.shape(), whole.shape(), "{cut}");
        // No step was done twice: the files of a step that had committed
        // were found unchanged, and its command ID was found.
        let unchanged = if cut == steps {
            0
        } else {
            (2 * cut as u64).min(10)
        };
        assert_eq!(resumed.unchanged, unchanged, "{cut}");
        // And what it counted was not lost with its checkpoint.
        assert_eq!(
            ScanSummary {
                unchanged: uninterrupted.unchanged,
                ..resumed
            },
            uninterrupted,
            "{cut}"
        );
        assert_eq!(
            rig.one("SELECT count(*) FROM library_revision WHERE method = 'scan.step'"),
            steps.to_string()
        );
        assert_eq!(
            rig.one("SELECT count(*), count(DISTINCT provider_key) FROM source_item"),
            "10|10"
        );
        assert_eq!(
            rig.one("SELECT count(*) FROM assignment"),
            whole.one("SELECT count(*) FROM assignment")
        );
        let done = rig.server.jobs(1).unwrap().remove(0);
        assert_eq!(
            (done.state, done.step),
            (JobState::Succeeded, steps),
            "{cut}"
        );
        // And it is settled: the next scan writes nothing.
        let revision = rig.revision();
        assert_eq!(rig.scan().unchanged, 10);
        assert_eq!(rig.revision(), revision);
    }
}

#[test]
fn a_job_names_its_source_and_fails_without_a_path_for_its_root() {
    let mut rig = Rig::new();
    album(&mut rig);
    let request = WriteRequest {
        method: "sources.add",
        command: Some(rig.command()),
        client: None,
    };
    // A source whose root this server was never given a path for.
    rig.library
        .add_source(request, "elsewhere", |source, _| {
            source.public_id.to_string()
        })
        .unwrap();
    let elsewhere = rig.library.sources().unwrap().pop().unwrap();
    let refused = rig.try_scan(&elsewhere, ScanSettings::default());
    assert!(
        matches!(&refused, Err(Error::NoRootPath { name }) if name == "elsewhere"),
        "{refused:?}"
    );

    let request = WriteRequest {
        method: "sources.add",
        command: Some(rig.command()),
        client: None,
    };
    let twice = rig
        .library
        .add_source(request, "main", |_, _| String::new());
    assert!(matches!(twice, Err(Error::SourceExists { source }) if source == rig.main.public_id));
    assert_eq!(rig.library.source(id(0x99)).unwrap(), None);
}

#[test]
fn a_scrobble_bound_to_a_scanned_song_leaves_every_later_scan_and_the_identity_reads_as_they_were()
{
    let mut rig = Rig::new();
    rig.put("Moss Arcade/Tin Weather/a.flac", "a.flac");
    rig.scan();
    let text = rig.one(
        "SELECT ac.display_text || '|' || r.display_title FROM recording r
         JOIN artist_credit ac ON ac.id = r.display_artist_credit_id",
    );
    let (artist, title) = text.split_once('|').unwrap();
    let decisions = rig.library.stats().unwrap().decisions;
    let dry_run = |rig: &Rig| {
        let run = rig
            .library
            .dry_run(&ritornel_contract::DryRunParams {
                policy: ritornel_contract::PolicyOverrides::default(),
                detail: false,
            })
            .unwrap();
        (run.items, run.unchanged, run.would_write)
    };
    let before = dry_run(&rig);

    // A scrobble of that song, imported, binds to it.
    let account = rig
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let batch = ritornel_source::Batch {
        records: vec![ritornel_source::ImportRecord {
            source_key: b"one".to_vec(),
            played_at_ms: T0 - 60_000,
            track: ritornel_source::TrackText {
                artist: artist.to_owned(),
                album: None,
                title: title.to_owned(),
            },
            ids: ritornel_source::Identifiers::default(),
            payload_json: "{}".to_owned(),
        }],
        span: ritornel_source::Span {
            from_ms: T0 - 60_000,
            to_ms: T0,
        },
        cursor: "{}".to_owned(),
    };
    rig.library
        .ingest(crate::Observations::Imports {
            account,
            batch: &batch,
            full: false,
        })
        .unwrap();
    assert_eq!(
        rig.one("SELECT count(*) FROM identity_decision WHERE method = 'scrobble_text'"),
        "1"
    );

    // The resolver, the dry run, the review and the stats read files, as
    // they did before the scrobble.
    assert_eq!(dry_run(&rig), before);
    rig.library.review().unwrap();
    assert_eq!(rig.library.stats().unwrap().decisions, decisions);
    rig.put("Moss Arcade/Tin Weather/b.flac", "b.flac");
    rig.scan();
    rig.scan();
    assert_eq!(
        rig.one(
            "SELECT count(*) FROM assignment a JOIN identity_decision d ON d.id = a.decision_id
                 WHERE d.method = 'scrobble_text' AND a.superseded_at_ms IS NULL"
        ),
        "1"
    );
}

/// Starts a scan of the main root and carries it through its walk, as a
/// worker would, and returns it with its job's row.
fn walked(rig: &mut Rig) -> (crate::ScanRun, i64) {
    let new = NewJob {
        kind: "scan",
        dedupe_key: rig.main.name.clone(),
        target: rig.main.public_id,
        params_json: "{}".to_owned(),
        session: None,
    };
    let command = rig.command();
    rig.server.start_job(command, "scan.start", new).unwrap();
    rig.boot += 1;
    let job = rig.server.claim_job(id(rig.boot)).unwrap().unwrap();
    let row = job.id;
    let mut run =
        crate::ScanRun::open(job, &rig.server, &rig.library, ScanSettings::default()).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    assert!(run.prepare(&stop, &mut |_| {}).unwrap());
    assert_eq!(run.commit(&rig.library).unwrap(), None);
    (run, row)
}

#[test]
fn a_disk_that_goes_away_mid_scan_leaves_every_file_as_it_was_and_the_next_scan_reads_it() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let title = "SELECT r.display_title FROM source_item si
                 JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 JOIN recording r ON r.id = a.recording_id
                 WHERE si.provider_key = 'Tin Weather/01.flac'";
    assert_eq!(rig.one(title), "Paper Kites");
    // Retagged, and the disk goes away after the walk saw it and before
    // it is read: unmounted, or unmounted with its mount point left behind.
    rig.put("Tin Weather/01.flac", "a.retagged.flac");
    let (root, away) = (rig.root(), rig.scratch.path().join("unmounted"));
    for mount_point_left in [false, true] {
        let (revision, before) = (rig.revision(), rig.dump());
        let (mut run, row) = walked(&mut rig);
        std::fs::rename(&root, &away).unwrap();
        if mount_point_left {
            std::fs::create_dir(&root).unwrap();
        }
        let stop = std::sync::atomic::AtomicBool::new(false);
        let refused = run.prepare(&stop, &mut |_| {});
        assert!(
            matches!(&refused, Err(Error::RootOffline { path }) if *path == root),
            "{refused:?}"
        );
        rig.server
            .finish_job(row, JobState::Failed, Some("offline".to_owned()))
            .unwrap();
        // Nothing is unreadable, missing or changed.
        assert_eq!((rig.revision(), rig.dump()), (revision, before));
        if mount_point_left {
            std::fs::remove_dir(&root).unwrap();
        }
        std::fs::rename(&away, &root).unwrap();
    }

    // Back: the retagged file is read, with its new tags.
    let summary = rig.scan();
    assert_eq!(
        (summary.changed, summary.unreadable, summary.missing),
        (1, 0, 0)
    );
    assert_eq!(rig.one(title), "Another Name Entirely");
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 0);
}

#[test]
fn another_disk_mounted_at_the_root_after_the_walk_is_not_judged_against_it() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    let (root, away) = (rig.root(), rig.scratch.path().join("unmounted"));
    for (nothing_to_read, swapped_before_judging) in [(false, false), (true, false), (false, true)]
    {
        if swapped_before_judging {
            // The walk sees only other audio at 01, which is read from
            // the disk walked: a copy of 02's, which is there, so no file
            // moved. The swap comes before it is judged.
            rig.put("Tin Weather/02.flac", "b.flac");
            rig.scan();
            rig.put("Tin Weather/01.flac", "b.flac");
        } else if nothing_to_read {
            // The walk sees only a file gone, and the scan goes straight to
            // judging what is missing.
            rig.scan();
            rig.put("Tin Weather/03.mp3", "a.mp3");
            rig.scan();
            std::fs::remove_file(root.join("Tin Weather/03.mp3")).unwrap();
        } else {
            // The walk sees 01 changed and 02 gone.
            rig.put("Tin Weather/01.flac", "a.retagged.flac");
            std::fs::remove_file(root.join("Tin Weather/02.flac")).unwrap();
        }
        let (revision, before) = (rig.revision(), rig.dump());
        let (mut run, row) = walked(&mut rig);
        let stop = std::sync::atomic::AtomicBool::new(false);
        if swapped_before_judging {
            // The batch is read, no file moved, and nothing is missing.
            for _ in 0..3 {
                assert!(run.prepare(&stop, &mut |_| {}).unwrap());
                assert_eq!(run.commit(&rig.library).unwrap(), None);
            }
        }
        // Then the disk is swapped for another whose files all read, with
        // other audio at 01.
        std::fs::rename(&root, &away).unwrap();
        rig.put("Tin Weather/01.flac", "a.retagged.flac");
        let refused = loop {
            match run.prepare(&stop, &mut |_| {}) {
                Ok(_) => assert_eq!(run.commit(&rig.library).unwrap(), None),
                Err(error) => break error,
            }
        };
        assert!(
            matches!(&refused, Error::RootOffline { path } if *path == root),
            "{refused:?}"
        );
        rig.server
            .finish_job(row, JobState::Failed, Some("offline".to_owned()))
            .unwrap();
        // Nothing is replaced, retired, missing or changed.
        assert_eq!((rig.revision(), rig.dump()), (revision, before));
        std::fs::remove_dir_all(&root).unwrap();
        std::fs::rename(&away, &root).unwrap();
    }
}

#[test]
fn a_file_that_cannot_be_reached_when_it_is_read_keeps_what_was_last_read_of_it() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.put("Tin Weather/02.flac", "b.flac");
    rig.scan();
    rig.put("Tin Weather/01.flac", "a.retagged.flac");
    let revisions = "SELECT si.current_revision_id FROM source_item si
                     WHERE si.provider_key = 'Tin Weather/01.flac'";
    let before = rig.one(revisions);
    // The root stays, and only this file is gone by the time it is read.
    let (mut run, row) = walked(&mut rig);
    let path = rig.root().join("Tin Weather/01.flac");
    let kept = rig.scratch.path().join("kept.flac");
    std::fs::rename(&path, &kept).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let summary = loop {
        assert!(run.prepare(&stop, &mut |_| {}).unwrap());
        if let Some(summary) = run.commit(&rig.library).unwrap() {
            break summary;
        }
    };
    rig.server
        .finish_job(row, JobState::Succeeded, None)
        .unwrap();
    assert_eq!((summary.changed, summary.unreadable), (0, 0));
    assert_eq!(rig.one(revisions), before, "no revision without audio");
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 0);

    // Back, it is read; gone for good, it is missing.
    std::fs::rename(&kept, &path).unwrap();
    assert_eq!(rig.scan().changed, 1);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(rig.scan().missing, 1);
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 0);
}

#[test]
fn a_file_an_earlier_scan_recorded_as_unreadable_by_a_failed_read_is_read_again() {
    use ritornel_domain::payload::ReadError;
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.scan();
    // What a scan before the fix wrote when the disk went away mid-read:
    // the file's size and time, no audio, and the system's error.
    let path = "Tin Weather/01.flac";
    let metadata = std::fs::metadata(rig.root().join(path)).unwrap();
    let mut payload = crate::media::read_file(
        &rig.root(),
        path,
        metadata.len(),
        crate::walk::mtime_ms(&metadata),
        Vec::new(),
    );
    payload.audio = None;
    payload.error = Some(ReadError {
        kind: "io".to_owned(),
        message: "Device not configured (os error 6)".to_owned(),
    });
    let item: i64 = rig
        .one("SELECT id FROM source_item WHERE provider_key = 'Tin Weather/01.flac'")
        .parse()
        .unwrap();
    let request = WriteRequest {
        method: "scan.step",
        command: None,
        client: None,
    };
    rig.library
        .record(
            request,
            rig.main.row,
            vec![crate::scan_store::RecordOp::Update { item, payload }],
            String::new(),
        )
        .unwrap();
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 1);

    // Same size and time, and read again all the same.
    let summary = rig.scan();
    assert_eq!((summary.changed, summary.unchanged), (1, 0));
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 0);
    let revision = rig.revision();
    assert_eq!(rig.scan().unchanged, 1);
    assert_eq!(rig.revision(), revision);
}

#[test]
fn a_file_that_reads_again_after_a_failed_read_keeps_its_item() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.scan();
    let item = "SELECT si.id, a.id, a.recording_id FROM source_item si
                JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                WHERE si.provider_key = 'Tin Weather/01.flac' AND si.retired_at_ms IS NULL";
    let before = rig.one(item);
    // Its read fails: what is at the path is not audio.
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.scan().unreadable, 1);
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 1);

    // Whole again, with the audio it had: the same item, not a new one.
    rig.put("Tin Weather/01.flac", "a.flac");
    let summary = rig.scan();
    assert_eq!((summary.changed, summary.retired, summary.new), (1, 0, 0));
    assert_eq!(rig.one(item), before);
    assert_eq!(rig.one("SELECT count(*) FROM source_item"), "1");
    assert_eq!(rig.library.stats().unwrap().files_unreadable, 0);
}

#[test]
fn a_different_song_that_takes_a_path_after_a_failed_read_gets_a_new_source_item() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    rig.scan();
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.scan().unreadable, 1);

    // Audio the item has never had: judged as a replacement, not taken as
    // the item's own file read again.
    rig.put("Tin Weather/01.flac", "b.no-artist.flac");
    let summary = rig.scan();
    assert_eq!((summary.retired, summary.changed, summary.new), (1, 0, 0));
    assert_eq!(
        rig.rows(
            "SELECT si.retired_at_ms IS NOT NULL, r.display_title
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
             JOIN recording r ON r.id = a.recording_id
             WHERE a.superseded_at_ms IS NULL ORDER BY si.id"
        ),
        ["1|Paper Kites", "0|Salt Road"]
    );
}

#[test]
fn a_song_an_item_had_before_another_took_its_path_is_judged_after_a_failed_read() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "a.flac");
    // Cut off after the file is recorded and before it is resolved, and
    // unreadable by the time the scan goes on: the item is never given a
    // recording, so the next audio at its path keeps it.
    let source = rig.main.clone();
    let settings = ScanSettings {
        fail_after_step: Some(1),
        ..ScanSettings::default()
    };
    assert!(matches!(
        rig.try_scan(&source, settings),
        Err(Error::FailPoint)
    ));
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.resume(ScanSettings::default()).unwrap().unreadable, 1);
    rig.put("Tin Weather/01.flac", "b.no-artist.flac");
    assert_eq!(rig.scan().changed, 1);
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.scan().unreadable, 1);

    // The first song back is not the item's file read again: the item is
    // now the second song's, and the first is judged as a replacement.
    rig.put("Tin Weather/01.flac", "a.flac");
    let summary = rig.scan();
    assert_eq!((summary.retired, summary.changed, summary.new), (1, 0, 0));
    assert_eq!(
        rig.rows(
            "SELECT si.retired_at_ms IS NOT NULL, r.display_title
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
             JOIN recording r ON r.id = a.recording_id
             WHERE a.superseded_at_ms IS NULL ORDER BY si.id"
        ),
        ["1|Salt Road", "0|Paper Kites"]
    );
}

#[test]
fn a_file_unreadable_since_its_first_scan_keeps_its_item_when_it_reads() {
    let mut rig = Rig::new();
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.scan().unreadable, 1);
    let item = rig.one("SELECT id FROM source_item");

    rig.put("Tin Weather/01.flac", "a.flac");
    let summary = rig.scan();
    assert_eq!((summary.changed, summary.retired, summary.new), (1, 0, 0));
    assert_eq!(rig.one("SELECT id FROM source_item"), item);
    assert_eq!(
        rig.one(
            "SELECT r.display_title FROM assignment a JOIN recording r ON r.id = a.recording_id"
        ),
        "Paper Kites"
    );
}

#[test]
fn a_song_the_item_has_since_left_is_judged_after_a_failed_read() {
    let mut rig = Rig::new();
    let path = rig.root().join("Tin Weather/01.flac");
    let cut_off = ScanSettings {
        fail_after_step: Some(1),
        ..ScanSettings::default()
    };
    let source = rig.main.clone();
    // Two songs recorded at the path in turn, each scan cut off before it
    // is resolved and the file unreadable when it goes on, so the item is
    // never given a recording and keeps each.
    rig.put("Tin Weather/01.flac", "a.flac");
    let first = std::fs::metadata(&path).unwrap().modified().unwrap();
    assert!(matches!(
        rig.try_scan(&source, cut_off),
        Err(Error::FailPoint)
    ));
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    rig.resume(ScanSettings::default()).unwrap();
    rig.put("Tin Weather/01.flac", "b.no-artist.flac");
    assert!(matches!(
        rig.try_scan(&source, cut_off),
        Err(Error::FailPoint)
    ));
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    rig.resume(ScanSettings::default()).unwrap();
    // The first file put back as it was: its first revision is current
    // again, and the item is resolved to its song.
    rig.put("Tin Weather/01.flac", "a.flac");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(first)
        .unwrap();
    rig.scan();
    // Four revisions, two with audio, and the first one current again.
    assert_eq!(
        rig.one(
            "SELECT count(*), sum(sr.audio_asset_id IS NOT NULL),
                    si.current_revision_id = min(sr.id)
             FROM source_item si JOIN source_item_revision sr ON sr.source_item_id = si.id"
        ),
        "4|2|1"
    );
    assert_eq!(
        rig.one(
            "SELECT r.display_title FROM assignment a JOIN recording r ON r.id = a.recording_id
                 WHERE a.superseded_at_ms IS NULL"
        ),
        "Paper Kites"
    );
    rig.put("Tin Weather/01.flac", "not-audio.flac");
    assert_eq!(rig.scan().unreadable, 1);

    // The second song again: its revision is the item's latest with audio,
    // but the item is no longer that song.
    rig.put("Tin Weather/01.flac", "b.no-artist.flac");
    let summary = rig.scan();
    assert_eq!((summary.retired, summary.changed, summary.new), (1, 0, 0));
    assert_eq!(
        rig.rows(
            "SELECT si.retired_at_ms IS NOT NULL, r.display_title
             FROM source_item si JOIN assignment a ON a.source_item_id = si.id
             JOIN recording r ON r.id = a.recording_id
             WHERE a.superseded_at_ms IS NULL ORDER BY si.id"
        ),
        ["1|Paper Kites", "0|Salt Road"]
    );
}
