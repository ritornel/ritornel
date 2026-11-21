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
    assert_eq!(run.policy.resolver_version, 2);
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
    let named = |mbid: &str| {
        library
            .artist_mbids
            .iter()
            .filter(|(_, id)| id.as_str() == mbid)
            .map(|(row, _)| {
                library
                    .artists
                    .iter()
                    .find(|(artist, _)| artist == row)
                    .map(|(_, name)| name.clone())
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        named("12121212-3434-4565-8787-909090909090"),
        ["moss arcade"]
    );
    // The artist with no ID has a row and no ID.
    assert_eq!(library.artists.len(), 2);
    assert_eq!(library.artist_mbids.len(), 1);
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
fn a_library_resolved_by_policy_1_is_resolved_by_policy_2_at_the_next_scan() {
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

    // Asked about policy 2, the dry run lists the compilation's song, and
    // nothing else: a recording of its own, marked for review, nearest the
    // album's file and its recording.
    let before = rig.dump();
    let run = rig.library.dry_run(&detail()).unwrap();
    assert!(run.policy.exact);
    assert_eq!(run.policy.resolver_version, 2);
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
    // policy 2 because the resolver's version changed.
    let summary = rig.scan();
    assert_eq!(
        (summary.unchanged, summary.assignments, summary.recordings),
        (2, 1, 1)
    );
    // Only the decision that changed is made again; the other stays policy
    // 1's, which policy 2 agrees with.
    assert_eq!(
        rig.rows(decisions),
        [
            format!("{ON_ALBUM}|new|1|0"),
            format!("{COMPILATION}|new|2|1")
        ]
    );
    assert_ne!(
        recording_of(&rig, COMPILATION),
        recording_of(&rig, ON_ALBUM)
    );
    assert_eq!(
        rig.one("SELECT value_json FROM library_setting WHERE key = 'identity.versions'"),
        format!(
            r#"{{"extractor":{},"resolver":2,"normalizer":{},"catalog":{}}}"#,
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

    // After it the dry run under policy 2 reports no change, and the scan
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
                (905, "Salt/Road"),
            ] {
                connection.execute(
                    "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                     VALUES (?1, ?2, ?1, ?3, ?3, ?4, ?5)",
                    (
                        row,
                        write.new_id()?.as_bytes(),
                        name,
                        ritornel_domain::normalize::normalize_by(1, name),
                        write.rev(),
                    ),
                )?;
            }
            // A row whose name was an edit since withdrawn: its name and
            // its key disagree.
            connection.execute(
                "INSERT INTO artist (id, public_id, canonical_id, display_name, display_sort_name, match_name, rev)
                 VALUES (906, ?1, 906, 'Salt Harbour', 'Salt Harbour', 'tinharbour', ?2)",
                (write.new_id()?.as_bytes(), write.rev()),
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
