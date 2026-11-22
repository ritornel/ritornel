//! The dry run and the review listing against a library that a scan of the
//! audio fixtures made: the proofs of "The proof" in
//! docs/design/identity.md that the calls built so far allow. A merge, a
//! split, a confirmation and a never-link are calls a later slice adds;
//! until then the domain's tests show them pinned and blocked.
//!
//! `02 Paper Kites.flac` (`a.flac`) and `copy.mp3` (`a.mp3`) are tagged as
//! one invented song, with audio of their own, so the copy binds to the
//! flac's recording by its tags and its length. `a.retagged.mp3` is the
//! copy's audio under the tags of another song. `b.compilation.flac` is the
//! same song by its tags, with audio of its own, on a compilation: another
//! album. Its length is the flac's to the millisecond, so policy 2 binds it
//! to the flac by the exception for an all but equal length, and 64 ms
//! shorter than the AAC's (`a.m4a`), which policy 1 binds it to and policy
//! 2 does not.

use ritornel_contract::{DryRunParams, ItemOutcome, PolicyOverrides, ReviewCause};
use ritornel_domain::Policy;

use crate::scan_tests::Rig;
use crate::{Error, JobState, ScanSettings, WriteRequest};

const FLAC: &str = "Moss Arcade/Tin Weather/02 Paper Kites.flac";
const COPY: &str = "Moss Arcade/Tin Weather/copy.mp3";
const AAC: &str = "Moss Arcade/Tin Weather/copy.m4a";

fn detail() -> DryRunParams {
    DryRunParams {
        detail: true,
        ..DryRunParams::default()
    }
}

fn scanned() -> Rig {
    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.put(COPY, "a.mp3");
    rig.put("Elsewhere/broken.m4a", "no-packets.m4a");
    rig.scan();
    assert_eq!(
        rig.rows(
            "SELECT si.provider_key, d.method FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN identity_decision d ON d.id = a.decision_id ORDER BY si.provider_key"
        ),
        [format!("{FLAC}|new"), format!("{COPY}|tags_duration")]
    );
    rig
}

#[test]
fn straight_after_a_scan_the_dry_run_reports_no_change_and_writes_nothing() {
    let rig = scanned();
    let (before, revision) = (rig.dump(), rig.revision());
    let run = rig.library.dry_run(&detail()).unwrap();
    assert!(run.policy.exact);
    assert_eq!(run.policy.resolver_version, 3);
    assert!(!run.would_write);
    assert!(!run.claims_stale);
    assert_eq!(run.items, 3);
    // The file that cannot be read is held as it is, and counts as
    // unchanged.
    assert_eq!(
        (run.unchanged, run.would_change, run.pinned, run.blocked),
        (3, 0, 0, 0)
    );
    assert_eq!((run.merges, run.splits), (0, 0));
    assert_eq!((run.reviews_now, run.reviews_after), (0, 0));
    assert_eq!((run.releases_now, run.releases_after), (1, 1));
    assert!(run.changes.is_empty());
    assert_eq!(rig.library.review().unwrap().reviews, []);
    // Every row of every table is as it was, and no revision was taken.
    assert_eq!(rig.dump(), before);
    assert_eq!(rig.revision(), revision);
}

#[test]
fn after_a_retag_the_dry_run_lists_exactly_the_file_that_was_retagged() {
    let mut rig = scanned();
    let copy_recording = rig.one(&format!(
        "SELECT r.public_id FROM source_item si JOIN recording r ON r.id = si.current_recording_id
         WHERE si.provider_key = '{COPY}'"
    ));
    // The copy is retagged as another song, and a scan records its new
    // tags and is cut off before it resolves the library.
    rig.put(COPY, "a.retagged.mp3");
    let source = rig.main.clone();
    let cut = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut, Err(Error::FailPoint)), "{cut:?}");

    let run = rig.library.dry_run(&detail()).unwrap();
    assert!(run.would_write);
    assert_eq!(
        (
            run.would_change,
            run.recordings_changed,
            run.splits,
            run.merges
        ),
        (1, 1, 1, 0)
    );
    let [change] = &run.changes[..] else {
        panic!("{:#?}", run.changes)
    };
    assert_eq!(change.outcome, ItemOutcome::WouldChange);
    assert_eq!(change.file.path, COPY);
    assert_eq!(change.file.title.as_deref(), Some("Another Name Entirely"));
    let (now, after) = (change.now.as_ref().unwrap(), change.after.as_ref().unwrap());
    assert_eq!(now.method.as_deref(), Some("tags_duration"));
    assert_eq!(
        now.recording_id
            .unwrap()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        copy_recording
    );
    assert_eq!(after.method.as_deref(), Some("new"));
    assert_eq!(after.recording_id, None);
    assert!(!after.needs_review);

    // The scan, carried on, resolves exactly that, and after it the dry
    // run reports no change.
    let summary = rig.resume(ScanSettings::default()).unwrap();
    assert_eq!((summary.assignments, summary.recordings), (1, 1));
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
}

#[test]
fn with_other_policy_values_the_dry_run_lists_exactly_what_they_would_change() {
    let mut rig = scanned();
    // The AAC copy reads 64 ms longer than the flac, which policy 1's free
    // band of two seconds lets bind.
    rig.put(AAC, "a.m4a");
    rig.scan();
    let before = rig.dump();
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
    // With no free band, and nothing above a distance of 0 binding, it
    // would be a recording of its own and marked for review.
    let strict = DryRunParams {
        policy: PolicyOverrides {
            bind_max: Some(0.0),
            duration_grace_ms: Some(0),
            ..PolicyOverrides::default()
        },
        detail: true,
    };
    let run = rig.library.dry_run(&strict).unwrap();
    assert!(!run.policy.exact);
    assert_eq!(run.policy.duration_grace_ms, 0);
    assert_eq!(
        (
            run.would_change,
            run.splits,
            run.reviews_added,
            run.reviews_after
        ),
        (1, 1, 1, 1)
    );
    let [change] = &run.changes[..] else {
        panic!("{:#?}", run.changes)
    };
    assert_eq!(change.file.path, AAC);
    let after = change.after.as_ref().unwrap();
    assert_eq!(
        (after.method.as_deref(), after.needs_review),
        (Some("new"), true)
    );
    let nearest = after.nearest.as_ref().unwrap();
    assert_eq!(nearest.file.path, FLAC);
    let penalties = nearest.penalties.unwrap();
    assert!(penalties.duration > 0.0);
    assert_eq!(
        (penalties.title, penalties.artist, penalties.album),
        (0.0, 0.0, 0.0)
    );

    // Switches that find nothing to do here change nothing.
    let switched = DryRunParams {
        policy: PolicyOverrides {
            fold_accents: Some(true),
            discs_from_folders: Some(true),
            disc_from_album_title: Some(true),
            ..PolicyOverrides::default()
        },
        detail: true,
    };
    let run = rig.library.dry_run(&switched).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
    assert_eq!(rig.dump(), before);
}

#[test]
fn an_artist_row_is_known_by_the_id_its_files_claim() {
    let mut rig = Rig::new();
    // Tagged by Picard: one artist ID, credited as the track artist.
    rig.put(FLAC, "a.flac");
    rig.put("Loose/tone.ogg", "a.retagged.ogg");
    rig.scan();
    let (library, _) = rig.library.load().unwrap();
    let mbid_keys: Vec<_> = library
        .artist_keys
        .iter()
        .filter(|entry| entry.kind == ritornel_domain::library::ArtistKeyKind::Mbid)
        .collect();
    let name_keys: Vec<_> = library
        .artist_keys
        .iter()
        .filter(|entry| entry.kind == ritornel_domain::library::ArtistKeyKind::Name)
        .collect();
    assert_eq!(mbid_keys.len(), 1);
    assert_eq!(mbid_keys[0].value, "12121212-3434-4565-8787-909090909090");
    // The artist with no ID has a durable name key and no MBID key.
    assert_eq!(name_keys.len(), 1);
    // And a library resolved again keeps every row where it is.
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
}

/// Commits the owner's confirmation of a file's binding, as the call a
/// later slice adds will: a user decision and an assignment to the same
/// recording and appearance, superseding the one in force.
fn confirm(rig: &Rig, path: &str) {
    let request = WriteRequest {
        method: "test.confirm",
        command: None,
        client: None,
    };
    let path = path.to_owned();
    rig.library
        .write(request, move |write| {
            let connection = write.connection();
            let (item, old, recording, appearance): (i64, i64, i64, Option<i64>) = connection
                .query_row(
                    "SELECT a.source_item_id, a.id, a.recording_id, a.appearance_id
                     FROM assignment a JOIN source_item si ON si.id = a.source_item_id
                     WHERE si.provider_key = ?1 AND a.superseded_at_ms IS NULL",
                    [&path],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
            connection.execute(
                "INSERT INTO identity_decision
                   (action, source_item_id, method, resolver_version, normalizer_version, evidence_json,
                    actor_kind, decided_at_ms, rev)
                 VALUES ('bind', ?1, 'user', 1, 1, '{}', 'person', ?2, ?3)",
                (item, write.now_ms(), write.rev()),
            )?;
            let decision = connection.last_insert_rowid();
            connection.execute(
                "UPDATE assignment SET superseded_at_ms = ?2, rev = ?3 WHERE id = ?1",
                (old, write.now_ms(), write.rev()),
            )?;
            connection.execute(
                "INSERT INTO assignment
                   (source_item_id, recording_id, appearance_id, decision_id, effective_from_ms, rev)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (item, recording, appearance, decision, write.now_ms(), write.rev()),
            )?;
            connection.execute(
                "UPDATE assignment SET superseded_by_id = ?2 WHERE id = ?1",
                (old, connection.last_insert_rowid()),
            )?;
            Ok(String::new())
        })
        .unwrap();
}

#[test]
fn a_plan_made_before_the_owner_confirms_a_binding_never_supersedes_it() {
    let mut rig = scanned();
    // The copy is retagged, and recorded without being resolved: a
    // resolution would give it a recording of its own.
    rig.put(COPY, "a.retagged.mp3");
    let source = rig.main.clone();
    let cut = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut, Err(Error::FailPoint)), "{cut:?}");
    let plan = rig.library.plan().unwrap().unwrap();

    // Between the plan and its write, the owner confirms the binding the
    // copy has.
    confirm(&rig, COPY);
    let request = || WriteRequest {
        method: "resolve",
        command: None,
        client: None,
    };
    let applied = rig.library.apply_plan(request(), plan);
    let owners = format!(
        "SELECT d.actor_kind FROM assignment a JOIN identity_decision d ON d.id = a.decision_id
         JOIN source_item si ON si.id = a.source_item_id
         WHERE si.provider_key = '{COPY}' AND a.superseded_at_ms IS NULL"
    );
    assert_eq!(rig.one(&owners), "person", "{applied:?}");
    assert!(matches!(applied, Err(Error::PlanStale)), "{applied:?}");

    // Resolving again plans from what is there now, and leaves the
    // owner's assignment in force.
    rig.library.resolve(request()).unwrap();
    assert_eq!(rig.one(&owners), "person");
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!(
        (run.would_change, run.pinned, run.would_write),
        (0, 1, false)
    );
}

#[test]
fn a_retag_recorded_by_a_scan_that_was_cancelled_is_resolved_by_the_next_scan() {
    let mut rig = scanned();
    rig.put(COPY, "a.retagged.mp3");
    let source = rig.main.clone();
    let cut = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut, Err(Error::FailPoint)), "{cut:?}");
    // The job is cancelled after the retag was recorded and before the
    // library was resolved, and is never taken up again.
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    assert_eq!(rig.library.dry_run(&detail()).unwrap().would_change, 1);

    // The next scan finds every file unchanged, and resolves all the same.
    let summary = rig.scan();
    assert_eq!((summary.unchanged, summary.assignments), (3, 1));
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
    assert_eq!(
        rig.one(&format!(
            "SELECT ta.display_title FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id WHERE si.provider_key = '{COPY}'"
        )),
        "Another Name Entirely"
    );
    // And the scan after it writes nothing.
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
}

const COMPILATION: &str = "Various/Night Drives/07 Paper Kites.flac";
const ON_ALBUM: &str = "Moss Arcade/Tin Weather/02 Paper Kites.m4a";

/// The public ID of the recording a file is bound to, as hex.
fn recording_of(rig: &Rig, path: &str) -> String {
    rig.one(&format!(
        "SELECT hex(r.public_id) FROM source_item si JOIN recording r ON r.id = si.current_recording_id
         WHERE si.provider_key = '{path}'"
    ))
}

fn hex(id: ritornel_contract::Id) -> String {
    id.as_bytes().iter().map(|b| format!("{b:02X}")).collect()
}

#[test]
fn an_earlier_policy_is_updated_at_the_next_scan() {
    let mut rig = Rig::new();
    rig.put(ON_ALBUM, "a.m4a");
    rig.put(COMPILATION, "b.compilation.flac");
    // The files are recorded by a scan cut off before it resolves, and the
    // library is resolved by policy 1, as the version before this one did.
    let source = rig.main.clone();
    let cut = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut, Err(Error::FailPoint)), "{cut:?}");
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    let request = || WriteRequest {
        method: "resolve",
        command: None,
        client: None,
    };
    let plan = rig.library.plan_by(&Policy::v1()).unwrap().unwrap();
    rig.library.apply_plan(request(), plan).unwrap();
    let decisions = "SELECT si.provider_key, d.method, d.resolver_version, d.needs_review
         FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN identity_decision d ON d.id = a.decision_id ORDER BY si.provider_key";
    assert_eq!(
        rig.rows(decisions),
        [
            format!("{ON_ALBUM}|new|1|0"),
            format!("{COMPILATION}|tags_duration|1|0")
        ]
    );
    assert_eq!(
        recording_of(&rig, COMPILATION),
        recording_of(&rig, ON_ALBUM)
    );
    let named = |version| DryRunParams {
        policy: PolicyOverrides {
            version: Some(version),
            ..PolicyOverrides::default()
        },
        detail: true,
    };
    let run = rig.library.dry_run(&named(1)).unwrap();
    assert!(run.policy.exact);
    assert_eq!((run.would_change, run.would_write), (0, false));

    // Asked about the current policy, the dry run lists the compilation's song, and
    // nothing else: a recording of its own, marked for review, nearest the
    // album's file and its recording.
    let before = rig.dump();
    let run = rig.library.dry_run(&detail()).unwrap();
    assert!(run.policy.exact);
    assert_eq!(run.policy.resolver_version, 3);
    assert!(!run.policy.tags_bind_across_albums);
    assert!(run.would_write);
    assert_eq!(
        (
            run.would_change,
            run.recordings_changed,
            run.splits,
            run.reviews_added,
            run.reviews_after
        ),
        (1, 1, 1, 1, 1)
    );
    let [change] = &run.changes[..] else {
        panic!("{:#?}", run.changes)
    };
    assert_eq!(change.file.path, COMPILATION);
    let after = change.after.as_ref().unwrap();
    assert_eq!(
        (after.method.as_deref(), after.needs_review),
        (Some("new"), true)
    );
    assert_eq!(after.recording_id, None);
    let nearest = after.nearest.as_ref().unwrap();
    assert_eq!(nearest.file.path, ON_ALBUM);
    assert_eq!(nearest.penalties.unwrap().album, 1.0);
    assert_eq!(
        hex(nearest.recording_id.unwrap()),
        recording_of(&rig, ON_ALBUM)
    );
    // Policy 2 asked whether tags may bind across albums changes nothing.
    let across = DryRunParams {
        policy: PolicyOverrides {
            tags_bind_across_albums: Some(true),
            ..PolicyOverrides::default()
        },
        detail: true,
    };
    let run = rig.library.dry_run(&across).unwrap();
    assert!(!run.policy.exact);
    assert_eq!(run.would_change, 0);
    assert_eq!(rig.dump(), before);

    // The next scan finds every file unchanged, and resolves the library by
    // the current policy because the resolver's version changed.
    let summary = rig.scan();
    assert_eq!(
        (summary.unchanged, summary.assignments, summary.recordings),
        (2, 1, 1)
    );
    // Only the decision that changed is made again; the other stays policy
    // 1's, which the current policy agrees with.
    assert_eq!(
        rig.rows(decisions),
        [
            format!("{ON_ALBUM}|new|1|0"),
            format!("{COMPILATION}|new|3|1")
        ]
    );
    assert_ne!(
        recording_of(&rig, COMPILATION),
        recording_of(&rig, ON_ALBUM)
    );
    assert_eq!(
        rig.one("SELECT value_json FROM library_setting WHERE key = 'identity.versions'"),
        format!(
            r#"{{"extractor":{},"resolver":3,"normalizer":{},"catalog":{}}}"#,
            ritornel_domain::EXTRACTOR_VERSION,
            ritornel_domain::NORMALIZER_VERSION,
            ritornel_domain::CATALOG_VERSION
        )
    );
    // The evidence names the cause, the file it nearly matched and that
    // file's recording.
    let evidence: serde_json::Value = serde_json::from_str(&rig.one(&format!(
        "SELECT d.evidence_json FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN identity_decision d ON d.id = a.decision_id WHERE si.provider_key = '{COMPILATION}'"
    )))
    .unwrap();
    let album_item: i64 = rig
        .one(&format!(
            "SELECT id FROM source_item WHERE provider_key = '{ON_ALBUM}'"
        ))
        .parse()
        .unwrap();
    let album_recording: i64 = rig
        .one(&format!(
            "SELECT current_recording_id FROM source_item WHERE provider_key = '{ON_ALBUM}'"
        ))
        .parse()
        .unwrap();
    assert_eq!(
        evidence["withheld"],
        serde_json::json!({ "cause": "other_album", "item": album_item, "recording": album_recording })
    );
    assert_eq!(evidence["candidates"][0]["item"], album_item);

    // The review listing groups it by that cause.
    let review = rig.library.review().unwrap();
    assert_eq!(review.causes.len(), 1);
    assert_eq!(
        (review.causes[0].name.as_str(), review.causes[0].count),
        ("other_album", 1)
    );
    let listed = &review.reviews[0];
    assert_eq!(listed.cause, ReviewCause::OtherAlbum);
    assert_eq!(listed.file.path, COMPILATION);
    let nearest = listed.nearest.as_ref().unwrap();
    assert_eq!(nearest.file.path, ON_ALBUM);
    assert_eq!(
        hex(nearest.recording_id.unwrap()),
        recording_of(&rig, ON_ALBUM)
    );

    // After it the dry run under the current policy reports no change, and the scan
    // after it writes nothing. Policy 1 would bind the song again.
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
    let revision = rig.revision();
    rig.scan();
    assert_eq!(rig.revision(), revision);
    let run = rig.library.dry_run(&named(1)).unwrap();
    assert_eq!((run.would_change, run.recordings_changed), (1, 1));
}

#[test]
fn policy_2_binds_across_albums_at_an_equal_length_and_says_so() {
    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.put(COMPILATION, "b.compilation.flac");
    rig.scan();
    assert_eq!(recording_of(&rig, COMPILATION), recording_of(&rig, FLAC));
    let evidence: serde_json::Value = serde_json::from_str(&rig.one(&format!(
        "SELECT d.evidence_json FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN identity_decision d ON d.id = a.decision_id WHERE si.provider_key = '{COMPILATION}'"
    )))
    .unwrap();
    assert_eq!(
        evidence["across_albums"],
        serde_json::json!({ "length_difference_ms": 0 })
    );
    assert_eq!(rig.library.review().unwrap().reviews, []);
    // With no exception it would be a recording of its own, for review.
    let strict = DryRunParams {
        policy: PolicyOverrides {
            across_albums_within_ms: Some(0),
            ..PolicyOverrides::default()
        },
        detail: true,
    };
    let run = rig.library.dry_run(&strict).unwrap();
    assert_eq!(run.policy.across_albums_within_ms, 0);
    assert_eq!((run.would_change, run.reviews_added), (1, 1));
}

#[test]
fn an_alias_the_owner_taught_under_another_normaliser_is_not_read_as_a_key() {
    let rig = scanned();
    let request = WriteRequest {
        method: "test.alias",
        command: None,
        client: None,
    };
    rig.library
        .write(request, move |write| {
            let connection = write.connection();
            let item: i64 = connection.query_row(
                "SELECT id FROM source_item WHERE provider_key = ?1",
                [FLAC],
                |row| row.get(0),
            )?;
            connection.execute(
                "INSERT INTO identity_decision
                   (action, source_item_id, method, resolver_version, normalizer_version, evidence_json,
                    actor_kind, decided_at_ms, rev)
                 VALUES ('bind', ?1, 'user', 1, 1, '{}', 'person', ?2, ?3)",
                (item, write.now_ms(), write.rev()),
            )?;
            let decision = connection.last_insert_rowid();
            // The first key was made by normaliser 1, which joined the
            // letters round a dash: it cannot be compared with a key of
            // today's.
            for (a, b, version) in [
                ("moss arcade", "mossarcade", 1),
                ("moss arcade", "the moss arcade", ritornel_domain::NORMALIZER_VERSION),
            ] {
                connection.execute(
                    "INSERT INTO alias_observation
                       (scope, name_a, name_b, normalizer_version, decision_id, observed_at_ms, rev)
                     VALUES ('artist', ?1, ?2, ?3, ?4, ?5, ?6)",
                    (a, b, version, decision, write.now_ms(), write.rev()),
                )?;
            }
            Ok(String::new())
        })
        .unwrap();
    let (library, _) = rig.library.load().unwrap();
    assert_eq!(
        library.aliases,
        [(
            ritornel_domain::library::Kind::Artist,
            "moss arcade".to_owned(),
            "the moss arcade".to_owned()
        )]
    );
}

#[test]
fn an_artist_no_file_credits_is_keyed_again_when_the_normaliser_changes() {
    let rig = scanned();
    // Two rows no file credits, as normaliser 1 keyed them; the owner
    // renamed the second.
    let request = WriteRequest {
        method: "test.normaliser",
        command: None,
        client: None,
    };
    rig.library
        .write(request, move |write| {
            let connection = write.connection();
            // The third is the scanned artist under another spelling, which
            // today's normaliser joins to it.
            // The fourth and fifth would share a key.
            for (row, name) in [
                (901, "Lanterns-42"),
                (902, "Tin/Harbour"),
                (903, "Moss/Arcade"),
                (904, "Salt-Road"),
                (905, "Salt Road"),
            ] {
                let key = ritornel_domain::normalize::normalize_by(1, name).unwrap();
                connection.execute(
                    "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                     VALUES (?1, ?2, ?1, ?3, ?3, ?4, ?5)",
                    (row, write.new_id()?.as_bytes(), name, &key, write.rev()),
                )?;
                connection.execute(
                    "INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                     VALUES ('name', ?1, ?2, 1, NULL, ?3)",
                    (&key, row, write.rev()),
                )?;
            }
            // A row whose name was an edit since withdrawn: its name and
            // its key disagree.
            connection.execute(
                "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                 VALUES (906, ?1, 906, 'Salt Harbour', 'Salt Harbour', 'saltharbor', ?2)",
                (write.new_id()?.as_bytes(), write.rev()),
            )?;
            connection.execute(
                "INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                 VALUES ('name', 'saltharbor', 906, 1, NULL, ?1)",
                [write.rev()],
            )?;
            connection.execute(
                "INSERT INTO metadata_edit (entity_kind, entity_id, field, value, actor_kind, edited_at_ms, rev)
                 VALUES ('artist', 902, 'name', 'Tin/Harbour', 'person', ?1, ?2)",
                (write.now_ms(), write.rev()),
            )?;
            let versions: String = connection.query_row(
                "SELECT value_json FROM library_setting WHERE key = 'identity.versions'",
                [],
                |row| row.get(0),
            )?;
            let old = versions.replace(
                &format!("\"normalizer\":{}", ritornel_domain::NORMALIZER_VERSION),
                "\"normalizer\":1",
            );
            assert_ne!(old, versions);
            connection.execute(
                "UPDATE library_setting SET value_json = ?1 WHERE key = 'identity.versions'",
                [old],
            )?;
            Ok(String::new())
        })
        .unwrap();
    let (library, _) = rig.library.load().unwrap();
    assert_eq!(library.artist_keys_by, Some(1));
    rig.library
        .resolve(WriteRequest {
            method: "test.resolve",
            command: None,
            client: None,
        })
        .unwrap();
    // The first has today's key, so its files find it when they return.
    // The others have none, and are never found: the renamed one, whose
    // files' names are not known; the one whose key today is the scanned
    // artist's; the two that would share a key; and the one whose name its
    // key does not come from.
    assert_eq!(
        rig.rows(
            "SELECT id || '|' || coalesce(match_name, '-') FROM artist WHERE id > 900 ORDER BY id"
        ),
        [
            "901|lanterns 42",
            "902|-",
            "903|-",
            "904|-",
            "905|-",
            "906|-"
        ]
    );
    // A second resolution changes nothing.
    assert!(
        rig.library
            .resolve(WriteRequest {
                method: "test.resolve",
                command: None,
                client: None,
            })
            .unwrap()
            .is_none()
    );
    let (library, _) = rig.library.load().unwrap();
    assert_eq!(library.artist_keys_by, None);
}

#[test]
fn an_artist_keyed_by_normaliser_2_is_keyed_again_with_its_symbol() {
    let rig = scanned();
    rig.library
        .write(
            WriteRequest {
                method: "test.normaliser",
                command: None,
                client: None,
            },
            move |write| {
                let connection = write.connection();
                let key = ritornel_domain::normalize::normalize_by(2, "Harbour Lights +").unwrap();
                connection.execute(
                    "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                     VALUES (911, ?1, 911, 'Harbour Lights +', 'Harbour Lights +', ?2, ?3)",
                    (write.new_id()?.as_bytes(), &key, write.rev()),
                )?;
                connection.execute(
                    "INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                     VALUES ('name', ?1, 911, 2, NULL, ?2)",
                    (&key, write.rev()),
                )?;
                let versions: String = connection.query_row(
                    "SELECT value_json FROM library_setting WHERE key = 'identity.versions'",
                    [],
                    |row| row.get(0),
                )?;
                let old = versions.replace(
                    &format!("\"normalizer\":{}", ritornel_domain::NORMALIZER_VERSION),
                    "\"normalizer\":2",
                );
                assert_ne!(old, versions);
                connection.execute(
                    "UPDATE library_setting SET value_json = ?1 WHERE key = 'identity.versions'",
                    [old],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    assert_eq!(
        rig.one("SELECT match_name FROM artist WHERE id = 911"),
        "harbour lights"
    );
    let (library, _) = rig.library.load().unwrap();
    assert_eq!(library.artist_keys_by, Some(2));
    rig.library
        .resolve(WriteRequest {
            method: "test.resolve",
            command: None,
            client: None,
        })
        .unwrap();
    assert_eq!(
        rig.one("SELECT match_name FROM artist WHERE id = 911"),
        "harbour lights +"
    );
    assert!(
        rig.library
            .resolve(WriteRequest {
                method: "test.resolve",
                command: None,
                client: None,
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_merged_artist_survives_rescan_and_rebuild_display_and_undoing_gives_both_back() {
    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.put("Other/track.ogg", "a.retagged.ogg");
    rig.scan();

    assert_eq!(rig.library.stats().unwrap().artists, 2);
    let ids: Vec<(i64, String)> = rig
        .library
        .read(|connection| {
            let mut stmt = connection.prepare("SELECT id, display_name FROM artist ORDER BY id")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(ids.len(), 2);
    let (row_a, row_b) = (ids[0].0, ids[1].0);

    // Merge artist B into artist A.
    rig.library
        .write(
            WriteRequest {
                method: "test.mergeArtist",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE artist SET redirect_to_id = ?2, canonical_id = ?2 WHERE id = ?1",
                    (row_b, row_a),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    assert_eq!(rig.library.stats().unwrap().artists, 1);

    // Rescan / re-resolve changes nothing and keeps the merge intact.
    let run = rig.library.dry_run(&detail()).unwrap();
    assert_eq!((run.would_change, run.would_write), (0, false));
    assert!(
        rig.library
            .resolve(WriteRequest {
                method: "test.resolve",
                command: None,
                client: None,
            })
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.library.stats().unwrap().artists, 1);
    assert_eq!(rig.one("SELECT count(*) FROM artist"), "2");

    // rebuild_display wipes display columns and re-projects; merge still holds
    // and no duplicate artist row is created.
    rig.library
        .rebuild_display(WriteRequest {
            method: "library.rebuildDisplay",
            command: None,
            client: None,
        })
        .unwrap();
    assert_eq!(rig.library.stats().unwrap().artists, 1);
    assert_eq!(rig.one("SELECT count(*) FROM artist"), "2");
    assert_eq!(
        rig.one(&format!(
            "SELECT display_name FROM artist WHERE id = {row_a}"
        )),
        ids[0].1
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT display_name FROM artist WHERE id = {row_b}"
        )),
        ids[1].1
    );
    let after_rebuild = rig.library.dry_run(&detail()).unwrap();
    assert_eq!(
        (after_rebuild.would_change, after_rebuild.would_write),
        (0, false)
    );

    // Undo the merge: both artists are back immediately with their original
    // rows, display names, and credits.
    rig.library
        .write(
            WriteRequest {
                method: "test.unmergeArtist",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE artist SET redirect_to_id = NULL, canonical_id = id WHERE id = ?1",
                    [row_b],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    assert_eq!(rig.library.stats().unwrap().artists, 2);
    let after_undo = rig.library.dry_run(&detail()).unwrap();
    assert_eq!(
        (after_undo.would_change, after_undo.would_write),
        (0, false)
    );
}

#[test]
fn retagging_an_artists_name_and_mbid_keeps_the_old_artist_row_and_public_id() {
    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.scan();
    let (old_id, old_pid): (i64, Vec<u8>) = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT id, public_id FROM artist", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?,
            )
        })
        .unwrap();

    // Simulate retagging the file's artist name AND MusicBrainz ID.
    rig.library
        .write(
            WriteRequest {
                method: "test.retagArtist",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                c.execute(
                    "UPDATE metadata_claim SET value = 'New Band' WHERE field IN ('artist', 'album_artist')",
                    [],
                )?;
                c.execute(
                    "UPDATE identifier_claim
                     SET value = '99999999-8888-7777-6666-555555555555'
                     WHERE namespace IN ('mb_artist', 'mb_album_artist')",
                    [],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .resolve(WriteRequest {
            method: "test.resolve",
            command: None,
            client: None,
        })
        .unwrap();

    // A brand-new artist row was created for the new MBID; the old row keeps
    // its public ID, display name, and durable MBID key.
    assert_eq!(rig.one("SELECT count(*) FROM artist"), "2");
    let (kept_pid, kept_mbid): (Vec<u8>, String) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT a.public_id, ak.key_value
                 FROM artist a
                 JOIN artist_key ak ON ak.artist_id = a.id AND ak.key_kind = 'mbid'
                 WHERE a.id = ?1",
                [old_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!(kept_pid, old_pid);
    assert_eq!(kept_mbid, "12121212-3434-4565-8787-909090909090");
}

#[test]
fn a_tied_mbid_vote_on_one_credit_never_creates_a_new_artist_on_rescan() {
    let mut rig = Rig::new();
    // Two files bound to the same recording and appearance.
    rig.put(FLAC, "a.flac");
    rig.put(COPY, "a.mp3");
    rig.scan();

    // Give the two files competing artist MBIDs: the second has a
    // lexicographically smaller MBID than the first.
    rig.library
        .write(
            WriteRequest {
                method: "test.tiedMbid",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                let copy_item: i64 = c.query_row(
                    "SELECT id FROM source_item WHERE provider_key = ?1",
                    [COPY],
                    |row| row.get(0),
                )?;
                let rev_id: i64 = c.query_row(
                    "SELECT current_revision_id FROM source_item WHERE id = ?1",
                    [copy_item],
                    |row| row.get(0),
                )?;
                let src_id: i64 = c.query_row(
                    "SELECT id FROM metadata_source WHERE kind = 'file_tags'",
                    [],
                    |row| row.get(0),
                )?;
                c.execute(
                    "DELETE FROM identifier_claim
                     WHERE source_item_id = ?1 AND namespace IN ('mb_artist', 'mb_album_artist')",
                    [copy_item],
                )?;
                c.execute(
                    "INSERT INTO identifier_claim
                       (source_item_id, revision_id, extractor_version, scope, namespace, value,
                        ordinal, metadata_source_id, rev)
                     VALUES (?1, ?2, 1, 'artist', 'mb_artist',
                             '00000000-0000-0000-0000-000000000001', 0, ?3, ?4)",
                    (copy_item, rev_id, src_id, write.rev()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();

    // First resolve settles the second MBID; subsequent resolves must do
    // nothing and never mint extra artist rows.
    rig.library
        .resolve(WriteRequest {
            method: "test.resolve1",
            command: None,
            client: None,
        })
        .unwrap();
    let artists_after_first = rig.one("SELECT count(*) FROM artist");
    assert!(
        rig.library
            .resolve(WriteRequest {
                method: "test.resolve2",
                command: None,
                client: None,
            })
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.one("SELECT count(*) FROM artist"), artists_after_first);
}

#[test]
fn migrating_a_saved_schema_1_library_backfills_artist_keys() {
    let scratch = crate::testing::Scratch::new();
    let path = scratch.path().join("library.db");
    {
        let mut connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        crate::migrate::run(
            &mut connection,
            &crate::migrate::LIBRARY[..2],
            &crate::testing::FixedClock::at(1_794_780_000_000),
        )
        .unwrap();
        connection
            .execute_batch(include_str!("../saved/library/0002.sql"))
            .unwrap();
    }
    let clock = std::sync::Arc::new(crate::testing::FixedClock::at(1_794_780_000_000));
    let entropy = std::sync::Arc::new(crate::testing::CountingEntropy::default());
    let store = crate::LibraryStore::open(&path, crate::Id::from_bytes([0x01; 16]), clock, entropy)
        .unwrap();

    // Artist 1 got its MBID key, artist 2 its name key, and redirected artist
    // 3 kept its name key too.
    let keys: Vec<(String, String, i64, Option<i64>)> = store
        .read(|connection| {
            let mut stmt = connection.prepare(
                "SELECT key_kind, key_value, artist_id, normalizer_version
                 FROM artist_key ORDER BY artist_id, key_kind",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(
        keys,
        [
            (
                "mbid".to_owned(),
                "12121212-3434-4565-8787-909090909090".to_owned(),
                1,
                None,
            ),
            ("name".to_owned(), "glass harbor".to_owned(), 2, Some(1)),
            ("name".to_owned(), "moss".to_owned(), 3, Some(1)),
        ]
    );

    // Resolving after migration preserves artist rows and redirects and
    // upgrades normaliser-1 name keys cleanly.
    store
        .resolve(WriteRequest {
            method: "test.resolveAfterMigrate",
            command: None,
            client: None,
        })
        .unwrap();
    let artists_after: Vec<(i64, Option<i64>)> = store
        .read(|connection| {
            let mut stmt =
                connection.prepare("SELECT id, redirect_to_id FROM artist ORDER BY id")?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(artists_after, [(1, None), (2, None), (3, Some(1))]);
    assert!(
        store
            .resolve(WriteRequest {
                method: "test.resolveAgain",
                command: None,
                client: None,
            })
            .unwrap()
            .is_none()
    );
}

#[test]
fn rebuild_display_preserves_retired_rows_and_migrates_stale_keys_safely() {
    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.put("Other/track.ogg", "a.retagged.ogg");
    rig.scan();

    // Retire the second file so its release, appearance, and artist are only
    // referenced historically; also add an unreached v1 artist row and a
    // merge target with a stale v1 key colliding with a live artist.
    let retired_artist_id: i64 = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT a.id FROM artist a
                 WHERE a.display_name <> 'Moss Arcade'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    let retired_name = rig.one(&format!(
        "SELECT display_name FROM artist WHERE id = {retired_artist_id}"
    ));
    rig.library
        .write(
            WriteRequest {
                method: "test.setupRebuild",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                c.execute(
                    "UPDATE source_item SET retired_at_ms = ?1, rev = ?2
                     WHERE provider_key = 'Other/track.ogg'",
                    (write.now_ms(), write.rev()),
                )?;
                // An unreached artist with a stale v1 key that should safely
                // rekey to "lanterns 42" rather than being wiped to NULL.
                c.execute(
                    "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                     VALUES (950, ?1, 950, 'Lanterns-42', 'Lanterns-42', 'lanterns42', ?2)",
                    (write.new_id()?.as_bytes(), write.rev()),
                )?;
                c.execute(
                    "INSERT INTO artist_key (key_kind, key_value, artist_id, normalizer_version, decision_id, rev)
                     VALUES ('name', 'lanterns42', 950, 1, NULL, ?1)",
                    [write.rev()],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();

    rig.library
        .rebuild_display(WriteRequest {
            method: "library.rebuildDisplay",
            command: None,
            client: None,
        })
        .unwrap();

    // The retired file's artist and appearance kept their last-known display.
    assert_eq!(
        rig.one(&format!(
            "SELECT display_name FROM artist WHERE id = {retired_artist_id}"
        )),
        retired_name
    );
    assert_ne!(
        rig.one(
            "SELECT coalesce(ta.display_title, '') FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE si.provider_key = 'Other/track.ogg'"
        ),
        ""
    );
    // The unreached v1 artist row was safely migrated to v2 before the wipe.
    assert_eq!(
        rig.one("SELECT key_value FROM artist_key WHERE artist_id = 950"),
        "lanterns 42"
    );
    // Live artist display was rebuilt cleanly.
    assert_eq!(
        rig.one("SELECT display_name FROM artist WHERE display_name = 'Moss Arcade'"),
        "Moss Arcade"
    );
}

#[test]
fn metadata_edits_keep_the_named_entity_across_merge_competing_edits_withdraw_and_undo() {
    use ritornel_domain::library::Kind;

    let mut rig = Rig::new();
    rig.put(FLAC, "a.flac");
    rig.put("Other/track.ogg", "a.retagged.ogg");
    rig.scan();

    let recs: Vec<(i64, crate::Id)> = rig
        .library
        .read(|connection| {
            let mut stmt = connection.prepare("SELECT id, public_id FROM recording ORDER BY id")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, crate::Id::from_bytes(row.get(1)?)))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(recs.len(), 2);
    let ((rec_a, pid_a), (rec_b, pid_b)) = (recs[0], recs[1]);

    // Edit recording A, then merge B into A, then edit B while B redirects to A.
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Recording,
            pid_a,
            "title",
            "Title On A",
        )
        .unwrap();
    rig.library
        .write(
            WriteRequest {
                method: "test.mergeRec",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE recording SET redirect_to_id = ?2, canonical_id = ?2 WHERE id = ?1",
                    (rec_b, rec_a),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Recording,
            pid_b,
            "title",
            "Title On B",
        )
        .unwrap();

    // The stored edit row named B (never rewritten to A), and while merged
    // the newest active edit (B's) wins on A's display_title.
    assert_eq!(
        rig.one("SELECT entity_id FROM metadata_edit WHERE value = 'Title On B'"),
        rec_b.to_string()
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {rec_a}"
        )),
        "Title On B"
    );

    // Undoing the merge leaves each edit on the recording it named.
    rig.library
        .write(
            WriteRequest {
                method: "test.unmergeRec",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE recording SET redirect_to_id = NULL, canonical_id = id WHERE id = ?1",
                    [rec_b],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .resolve(WriteRequest {
            method: "test.resolve",
            command: None,
            client: None,
        })
        .unwrap();
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {rec_a}"
        )),
        "Title On A"
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {rec_b}"
        )),
        "Title On B"
    );

    // Re-merge B into A, then withdraw B's edit: A's earlier active edit stands.
    rig.library
        .write(
            WriteRequest {
                method: "test.remergeRec",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE recording SET redirect_to_id = ?2, canonical_id = ?2 WHERE id = ?1",
                    (rec_b, rec_a),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .withdraw_metadata_edit(
            WriteRequest {
                method: "edit.withdraw",
                command: None,
                client: None,
            },
            Kind::Recording,
            pid_b,
            "title",
        )
        .unwrap();
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {rec_a}"
        )),
        "Title On A"
    );

    // Invalid edits (unsupported field, unparseable date) are refused, and
    // withdrawing when no edit is active returns Ok(None) without taking a rev.
    assert!(matches!(
        rig.library.set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Recording,
            pid_a,
            "bogus_field",
            "x",
        ),
        Err(Error::Invalid(_))
    ));
    let rel_pid: crate::Id = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT public_id FROM release LIMIT 1", [], |row| {
                    Ok(crate::Id::from_bytes(row.get(0)?))
                })?,
            )
        })
        .unwrap();
    assert!(matches!(
        rig.library.set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Release,
            rel_pid,
            "date",
            "unknown",
        ),
        Err(Error::Invalid(_))
    ));
    let rev_before = rig.revision();
    assert!(
        rig.library
            .withdraw_metadata_edit(
                WriteRequest {
                    method: "edit.withdraw",
                    command: None,
                    client: None,
                },
                Kind::Recording,
                pid_b,
                "title",
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.revision(), rev_before);

    // Redirected track appearance edit updates the canonical appearance's
    // display_title even when live assignments still point at the redirected row.
    let apps: Vec<(i64, crate::Id)> = rig
        .library
        .read(|connection| {
            let mut stmt =
                connection.prepare("SELECT id, public_id FROM track_appearance ORDER BY id")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, crate::Id::from_bytes(row.get(1)?)))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .unwrap();
    assert_eq!(apps.len(), 2);
    let ((ta_a, ta_pid_a), (ta_b, ta_pid_b)) = (apps[0], apps[1]);
    rig.library
        .write(
            WriteRequest {
                method: "test.mergeApp",
                command: None,
                client: None,
            },
            move |write| {
                write.connection().execute(
                    "UPDATE track_appearance SET redirect_to_id = ?2, canonical_id = ?2 WHERE id = ?1",
                    (ta_b, ta_a),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::TrackAppearance,
            ta_pid_b,
            "title",
            "Merged Appearance Title",
        )
        .unwrap();
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM track_appearance WHERE id = {ta_a}"
        )),
        "Merged Appearance Title"
    );
    let _ = ta_pid_a;
}

#[test]
fn where_edit_rating_and_play_land_after_recording_split() {
    use ritornel_domain::library::Kind;

    let mut rig = Rig::new();
    // Two files sharing recording R1 (`a.flac` and `copy.mp3`).
    rig.put(FLAC, "a.flac");
    rig.put(COPY, "a.mp3");
    rig.scan();
    let (r1, r1_pid): (i64, crate::Id) = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT id, public_id FROM recording", [], |row| {
                    Ok((row.get(0)?, crate::Id::from_bytes(row.get(1)?)))
                })?,
            )
        })
        .unwrap();

    // Put an edit and a native star rating on R1, plus a source-item love on
    // COPY, and record two scrobbles/plays of COPY (one left unpinned, one
    // explicitly pinned to R1 by an observation recording correction).
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Recording,
            r1_pid,
            "title",
            "Edited Original",
        )
        .unwrap();
    rig.library
        .set_rating(
            WriteRequest {
                method: "ratings.set",
                command: None,
                client: None,
            },
            r1_pid,
            Some(80),
            |rev, _| rev.to_string(),
        )
        .unwrap();
    let listener = rig.library.default_listener().unwrap().unwrap();
    let (copy_item, copy_item_pid, copy_asset_pid, copy_app_pid): (
        i64,
        crate::Id,
        crate::Id,
        crate::Id,
    ) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT si.id, si.public_id, aa.public_id, ta.public_id
                     FROM source_item si
                     JOIN source_item_revision sr ON sr.id = si.current_revision_id
                     JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                     JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                     JOIN track_appearance ta ON ta.id = a.appearance_id
                     WHERE si.provider_key = ?1",
                [COPY],
                |row| {
                    Ok((
                        row.get(0)?,
                        crate::Id::from_bytes(row.get(1)?),
                        crate::Id::from_bytes(row.get(2)?),
                        crate::Id::from_bytes(row.get(3)?),
                    ))
                },
            )?)
        })
        .unwrap();
    // Source-item love on COPY (from an external endpoint).
    let account = rig
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    rig.library
        .write(
            WriteRequest {
                method: "test.love",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                let l_id: i64 =
                    c.query_row("SELECT min(id) FROM listener", [], |row| row.get(0))?;
                let ep_id: i64 = c.query_row(
                    "SELECT endpoint_id FROM service_account WHERE public_id = ?1",
                    [account.as_bytes().as_slice()],
                    |row| row.get(0),
                )?;
                c.execute(
                    "INSERT INTO listener_rating
                       (listener_id, source_item_id, endpoint_id, kind, cleared, rated_at_ms, rev)
                     VALUES (?1, ?2, ?3, 'love', 0, ?4, ?5)",
                    (l_id, copy_item, ep_id, write.now_ms(), write.rev()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    // Two native listens of COPY; pin the first listen to R1 explicitly.
    let t0 = 1_794_776_400_000_i64;
    let boot = crate::Id::from_bytes([0xb0; 16]);
    let server_id: crate::Id = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT public_id FROM server LIMIT 1", [], |row| {
                    Ok(crate::Id::from_bytes(row.get(0)?))
                })?,
            )
        })
        .unwrap();
    for (idx, key_byte) in [(0_i64, 0x71_u8), (1_i64, 0x72_u8)] {
        let start = t0 + idx * 600_000;
        let sent_delay = (2 - idx) * 600_000;
        let record = crate::ListenRecord {
            v: crate::LISTEN_RECORD_VERSION,
            listen_id: crate::Id::from_bytes([key_byte; 16]),
            listener,
            actor_session: None,
            played_on_server: server_id,
            appearance: copy_app_pid,
            asset: copy_asset_pid,
            copy_kind: "master".to_owned(),
            source_item: copy_item_pid,
            source_revision: 1,
            started_at_ms: start,
            zone: Some("UTC".to_owned()),
            started_mono_ms: 1_000_000 + idx * 600_000,
            boot,
            start_reason: "user_pick".to_owned(),
            continues: None,
            start_position_ms: 0,
            track_duration_ms: 180_000,
            ms_played: 180_000,
            heard_ranges: vec![[0, 180_000]],
            heard_ranges_approximate: false,
            max_position_ms: 180_000,
            seek_count: 0,
            pause_count: 0,
            end_position_ms: 180_000,
            end_reason: "finished".to_owned(),
            ended_at_ms: start + 180_000,
            ended_mono_ms: Some(1_180_000 + idx * 600_000),
            context_kind: "album".to_owned(),
            context_ref: None,
            shuffle: false,
        };
        rig.library
            .ingest_listen_by(
                &record,
                crate::SendStamps {
                    sent_at_ms: start + sent_delay,
                    sent_mono_ms: 1_000_000 + idx * 600_000 + sent_delay,
                    sent_boot: boot,
                },
                ritornel_domain::history::PlayRule::V1,
            )
            .unwrap();
    }
    let first_obs: (crate::Id, Vec<u8>) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT e.public_id, o.source_key FROM history_observation o
                 JOIN history_endpoint e ON e.id = o.endpoint_id
                 WHERE o.kind = 'listen'
                 ORDER BY o.id LIMIT 1",
                [],
                |row| Ok((crate::Id::from_bytes(row.get(0)?), row.get(1)?)),
            )?)
        })
        .unwrap();
    rig.library
        .set_observation_recording(
            WriteRequest {
                method: "history.setRecording",
                command: None,
                client: None,
            },
            crate::CorrectionTargets::Observations(vec![first_obs]),
            Some(r1_pid),
            None,
            None,
        )
        .unwrap();

    // Split: retag COPY to a completely different song -> moves COPY to new R2.
    rig.put(COPY, "a.retagged.mp3");
    rig.scan();

    let (r2, r2_pid): (i64, crate::Id) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT r.id, r.public_id FROM source_item si
                 JOIN recording r ON r.id = si.current_recording_id
                 WHERE si.provider_key = ?1",
                [COPY],
                |row| Ok((row.get(0)?, crate::Id::from_bytes(row.get(1)?))),
            )?)
        })
        .unwrap();
    assert_ne!(r2, r1);

    // 1. Edit: stays on R1; R2 starts unedited with its own tag title.
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {r1}"
        )),
        "Edited Original"
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT display_title FROM recording WHERE id = {r2}"
        )),
        "Another Name Entirely"
    );

    // 2. Rating: native star rating stays on R1; source-item love followed COPY to R2.
    let said = rig.library.said_of(vec![r1, r2]).unwrap();
    assert_eq!(said[&r1].rating, Some(80));
    assert!(!said[&r1].loved);
    assert_eq!(said[&r2].rating, None);
    assert!(said[&r2].loved);

    // 3. Plays: the unpinned listen followed COPY to R2; the pinned listen
    //    stayed on R1, and history rebuild agrees.
    let plays = rig.library.recent_plays(listener, 10, None).unwrap();
    assert_eq!(plays.len(), 2);
    // Newest first: second listen (unpinned -> R2), first listen (pinned -> R1).
    assert_eq!(plays[0].recording, Some(r2_pid));
    assert_eq!(plays[1].recording, Some(r1_pid));
    let dry: i64 = rig
        .library
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            true,
            |rebuilt| rebuilt.changed.to_string(),
        )
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(dry, 0);
}

#[test]
fn where_edit_rating_and_play_land_after_release_split_and_regrouping() {
    use ritornel_domain::library::Kind;

    let mut rig = Rig::new();
    // Put two distinct tracks in the same folder/album so they start on ONE
    // release L1.
    rig.put(FLAC, "a.flac");
    rig.put(
        "Moss Arcade/Tin Weather/03 Second Track.ogg",
        "a.retagged.ogg",
    );
    rig.scan();
    rig.library
        .write(
            WriteRequest {
                method: "test.joinOneRelease",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                let second: i64 = c.query_row(
                    "SELECT id FROM source_item
                     WHERE provider_key = 'Moss Arcade/Tin Weather/03 Second Track.ogg'",
                    [],
                    |row| row.get(0),
                )?;
                let second_rev: i64 = c.query_row(
                    "SELECT current_revision_id FROM source_item WHERE id = ?1",
                    [second],
                    |row| row.get(0),
                )?;
                let src_id: i64 = c.query_row(
                    "SELECT id FROM metadata_source WHERE kind = 'file_tags'",
                    [],
                    |row| row.get(0),
                )?;
                let rel_mbid: String = c.query_row(
                    "SELECT value FROM identifier_claim WHERE namespace = 'mb_release' LIMIT 1",
                    [],
                    |row| row.get(0),
                )?;
                c.execute(
                    "DELETE FROM metadata_claim WHERE source_item_id = ?1 AND field = 'album'",
                    [second],
                )?;
                c.execute(
                    "INSERT INTO metadata_claim
                       (source_item_id, revision_id, extractor_version, scope, field, value,
                        ordinal, metadata_source_id, rev)
                     VALUES (?1, ?2, 1, 'release', 'album', 'Tin Weather', 0, ?3, ?4)",
                    (second, second_rev, src_id, write.rev()),
                )?;
                c.execute(
                    "INSERT INTO identifier_claim
                       (source_item_id, revision_id, extractor_version, scope, namespace, value,
                        ordinal, metadata_source_id, rev)
                     VALUES (?1, ?2, 1, 'release', 'mb_release', ?3, 0, ?4, ?5)",
                    (second, second_rev, &rel_mbid, src_id, write.rev()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .resolve(WriteRequest {
            method: "test.resolveInitial",
            command: None,
            client: None,
        })
        .unwrap();
    assert_eq!(
        rig.one(
            "SELECT count(DISTINCT ta.release_id) FROM assignment a
             JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE a.superseded_at_ms IS NULL"
        ),
        "1"
    );

    let (rel1, rel1_pid, grp1, grp1_pid, rec2, rec2_pid, item2_pid, asset2_pid, app2_pid): (
        i64,
        crate::Id,
        i64,
        crate::Id,
        i64,
        crate::Id,
        crate::Id,
        crate::Id,
        crate::Id,
    ) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT rel.id, rel.public_id, rg.id, rg.public_id, r.id, r.public_id,
                        si.public_id, aa.public_id, ta.public_id
                 FROM source_item si
                 JOIN source_item_revision sr ON sr.id = si.current_revision_id
                 JOIN audio_asset aa ON aa.id = sr.audio_asset_id
                 JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 JOIN recording r ON r.id = a.recording_id
                 JOIN track_appearance ta ON ta.id = a.appearance_id
                 JOIN release rel ON rel.id = ta.release_id
                 JOIN release_group rg ON rg.id = rel.release_group_id
                 WHERE si.provider_key = 'Moss Arcade/Tin Weather/03 Second Track.ogg'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        crate::Id::from_bytes(row.get(1)?),
                        row.get(2)?,
                        crate::Id::from_bytes(row.get(3)?),
                        row.get(4)?,
                        crate::Id::from_bytes(row.get(5)?),
                        crate::Id::from_bytes(row.get(6)?),
                        crate::Id::from_bytes(row.get(7)?),
                        crate::Id::from_bytes(row.get(8)?),
                    ))
                },
            )?)
        })
        .unwrap();

    // Put an edit on L1, an edit on G1, a rating on track 2's recording, and a
    // listen/play on track 2.
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::Release,
            rel1_pid,
            "date",
            "2019",
        )
        .unwrap();
    rig.library
        .set_metadata_edit(
            WriteRequest {
                method: "edit.set",
                command: None,
                client: None,
            },
            Kind::ReleaseGroup,
            grp1_pid,
            "primary_type",
            "ep",
        )
        .unwrap();
    rig.library
        .set_rating(
            WriteRequest {
                method: "ratings.set",
                command: None,
                client: None,
            },
            rec2_pid,
            Some(100),
            |rev, _| rev.to_string(),
        )
        .unwrap();
    let listener = rig.library.default_listener().unwrap().unwrap();
    let server_id: crate::Id = rig
        .library
        .read(|connection| {
            Ok(
                connection.query_row("SELECT public_id FROM server LIMIT 1", [], |row| {
                    Ok(crate::Id::from_bytes(row.get(0)?))
                })?,
            )
        })
        .unwrap();
    let boot = crate::Id::from_bytes([0xb1; 16]);
    let start = 1_794_776_400_000_i64;
    let listen = crate::ListenRecord {
        v: crate::LISTEN_RECORD_VERSION,
        listen_id: crate::Id::from_bytes([0x81; 16]),
        listener,
        actor_session: None,
        played_on_server: server_id,
        appearance: app2_pid,
        asset: asset2_pid,
        copy_kind: "master".to_owned(),
        source_item: item2_pid,
        source_revision: 1,
        started_at_ms: start,
        zone: Some("UTC".to_owned()),
        started_mono_ms: 1_000_000,
        boot,
        start_reason: "user_pick".to_owned(),
        continues: None,
        start_position_ms: 0,
        track_duration_ms: 180_000,
        ms_played: 180_000,
        heard_ranges: vec![[0, 180_000]],
        heard_ranges_approximate: false,
        max_position_ms: 180_000,
        seek_count: 0,
        pause_count: 0,
        end_position_ms: 180_000,
        end_reason: "finished".to_owned(),
        ended_at_ms: start + 180_000,
        ended_mono_ms: Some(1_180_000),
        context_kind: "album".to_owned(),
        context_ref: None,
        shuffle: false,
    };
    rig.library
        .ingest_listen_by(
            &listen,
            crate::SendStamps {
                sent_at_ms: start + 240_000,
                sent_mono_ms: 1_240_000,
                sent_boot: boot,
            },
            ritornel_domain::history::PlayRule::V1,
        )
        .unwrap();

    // 1. Release split: change track 2's release MBID and album so it splits
    //    onto a new release L2 (in a separate group G2).
    rig.library
        .write(
            WriteRequest {
                method: "test.splitRelease",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                let second: i64 = c.query_row(
                    "SELECT id FROM source_item
                     WHERE provider_key = 'Moss Arcade/Tin Weather/03 Second Track.ogg'",
                    [],
                    |row| row.get(0),
                )?;
                c.execute(
                    "UPDATE identifier_claim SET value = 'rel-split-l2'
                     WHERE source_item_id = ?1 AND namespace = 'mb_release'",
                    [second],
                )?;
                c.execute(
                    "UPDATE metadata_claim SET value = 'Other Album'
                     WHERE source_item_id = ?1 AND field = 'album'",
                    [second],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .resolve(WriteRequest {
            method: "test.resolveAfterSplit",
            command: None,
            client: None,
        })
        .unwrap();

    let (rel2, grp2, new_app2_pid): (i64, i64, crate::Id) = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT rel.id, rel.release_group_id, ta.public_id
                 FROM source_item si
                 JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
                 JOIN track_appearance ta ON ta.id = a.appearance_id
                 JOIN release rel ON rel.id = ta.release_id
                 WHERE si.provider_key = 'Moss Arcade/Tin Weather/03 Second Track.ogg'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, crate::Id::from_bytes(row.get(2)?))),
            )?)
        })
        .unwrap();
    assert_ne!(rel2, rel1, "track 2 must have split onto a new release L2");
    assert_ne!(grp2, grp1, "L2 must start in a separate release group G2");
    assert_ne!(
        new_app2_pid, app2_pid,
        "L2 must give track 2 a new appearance"
    );

    // L1 kept its date edit, G1 kept its primary_type edit, rec2 kept its
    // rating, and track 2's play moved to L2's appearance while keeping rec2.
    assert_eq!(
        rig.one(&format!(
            "SELECT display_date FROM release WHERE id = {rel1}"
        )),
        "2019"
    );
    assert_eq!(
        rig.one(&format!(
            "SELECT display_primary_type FROM release_group WHERE id = {grp1}"
        )),
        "ep"
    );
    assert_eq!(
        rig.library.said_of(vec![rec2]).unwrap()[&rec2].rating,
        Some(100)
    );
    let plays = rig.library.recent_plays(listener, 10, None).unwrap();
    assert_eq!(plays.len(), 1);
    assert_eq!(plays[0].recording, Some(rec2_pid));
    assert_eq!(plays[0].appearance, Some(new_app2_pid));
    let dry: i64 = rig
        .library
        .rebuild_history(
            WriteRequest {
                method: "history.rebuild",
                command: None,
                client: None,
            },
            true,
            |rebuilt| rebuilt.changed.to_string(),
        )
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(dry, 0);

    // 2. Release regrouping: give L2 the same release_group_mbid as L1 so L2
    //    regroups automatically into edited G1. G1 survives with its edit, and
    //    rec2's rating and play stay intact.
    rig.library
        .write(
            WriteRequest {
                method: "test.regroupIntoG1",
                command: None,
                client: None,
            },
            move |write| {
                let c = write.connection();
                let second: i64 = c.query_row(
                    "SELECT id FROM source_item
                     WHERE provider_key = 'Moss Arcade/Tin Weather/03 Second Track.ogg'",
                    [],
                    |row| row.get(0),
                )?;
                let second_rev: i64 = c.query_row(
                    "SELECT current_revision_id FROM source_item WHERE id = ?1",
                    [second],
                    |row| row.get(0),
                )?;
                let src_id: i64 = c.query_row(
                    "SELECT id FROM metadata_source WHERE kind = 'file_tags'",
                    [],
                    |row| row.get(0),
                )?;
                let rg_mbid: String = c.query_row(
                    "SELECT value FROM identifier_claim WHERE namespace = 'mb_release_group' LIMIT 1",
                    [],
                    |row| row.get(0),
                )?;
                c.execute(
                    "INSERT INTO identifier_claim
                       (source_item_id, revision_id, extractor_version, scope, namespace, value,
                        ordinal, metadata_source_id, rev)
                     VALUES (?1, ?2, 1, 'release_group', 'mb_release_group', ?3, 0, ?4, ?5)",
                    (second, second_rev, &rg_mbid, src_id, write.rev()),
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    rig.library
        .resolve(WriteRequest {
            method: "test.resolveAfterRegroup",
            command: None,
            client: None,
        })
        .unwrap();
    let regrouped_grp: i64 = rig
        .library
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT release_group_id FROM release WHERE id = ?1",
                [rel2],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(regrouped_grp, grp1, "L2 must regroup into edited G1");
    assert_eq!(
        rig.one(&format!(
            "SELECT display_primary_type FROM release_group WHERE id = {grp1}"
        )),
        "ep"
    );
    assert_eq!(
        rig.library.said_of(vec![rec2]).unwrap()[&rec2].rating,
        Some(100)
    );
    let plays_after = rig.library.recent_plays(listener, 10, None).unwrap();
    assert_eq!(plays_after.len(), 1);
    assert_eq!(plays_after[0].recording, Some(rec2_pid));
    assert_eq!(plays_after[0].appearance, Some(new_app2_pid));
}
