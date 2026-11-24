//! Answers through the engine, on the invented album whose bonus disc is
//! filed apart: a yes joins it, and its undo gives everything back. See
//! "Answers" in docs/internals/corrections.md.

use std::collections::HashMap;

use ritornel_contract::Confirmation;

use crate::proposal_tests::{GROUP, album, album_with};
use crate::scan_tests::Rig;
use crate::{Command, Error, Fingerprint, Id, JobState, ScanSettings, WriteRequest};

fn command(byte: u8) -> WriteRequest {
    WriteRequest {
        method: "corrections.confirm",
        command: Some(Command {
            id: Id::from_bytes([0xd0 + byte; 16]),
            fingerprint: Fingerprint::of("answer", &[byte]),
        }),
        client: None,
    }
}

/// Every confirmation of the group's album proposals, as shown.
fn group(rig: &Rig) -> Vec<Confirmation> {
    rig.library
        .proposals()
        .unwrap()
        .proposals
        .into_iter()
        .filter(|proposal| proposal.kind == "album")
        .map(|proposal| Confirmation {
            proposal_id: proposal.proposal_id,
            release_group: GROUP.to_owned(),
            release: None,
            fingerprint: proposal.fingerprint,
        })
        .collect()
}

/// The confirmation of the group's edition, as shown, with its likeliest
/// release.
fn edition(rig: &Rig) -> Option<Confirmation> {
    let proposal = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .into_iter()
        .find(|proposal| proposal.kind == "edition")?;
    let id = proposal.proposal_id;
    let whole = rig.library.proposal(id).unwrap().1;
    Some(Confirmation {
        proposal_id: id,
        release_group: GROUP.to_owned(),
        release: whole.candidates.first()?.release.clone(),
        fingerprint: whole.fingerprint,
    })
}

fn confirm(rig: &Rig, byte: u8, confirmations: Vec<Confirmation>) -> Result<Vec<Id>, Error> {
    let written = rig
        .library
        .confirm(command(byte), confirmations, |_, answers, moved| {
            let ids: Vec<String> = answers
                .iter()
                .map(|(answer, _)| answer.to_string())
                .collect();
            serde_json::json!({ "answers": ids, "moved": moved }).to_string()
        })?;
    let result: serde_json::Value = serde_json::from_str(&written.result_json).unwrap();
    Ok(result["answers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().parse().unwrap())
        .collect())
}

fn undo(rig: &Rig, byte: u8, answers: Vec<Id>) -> Result<(), Error> {
    rig.library
        .undo(command(byte), answers, |_, _, _| String::new())
        .map(|_| ())
}

/// What a file and anything it leads to shows, with public IDs: every live
/// file's recording, appearance, release and group, what each shows, and
/// the artist keys.
fn reachable(rig: &Rig) -> Vec<String> {
    let mut shown = rig.rows(
        "SELECT si.provider_key, hex(r.public_id), hex(ta.public_id), hex(rel.public_id), hex(g.public_id),
                ta.display_title, ta.display_disc_number, ta.display_track_number, rel.display_title,
                g.display_primary_type, g.display_first_year, r.canonical_id = r.id,
                ta.canonical_id = ta.id, rel.canonical_id = rel.id
         FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN recording r ON r.id = a.recording_id
         LEFT JOIN track_appearance ta ON ta.id = a.appearance_id
         LEFT JOIN release rel ON rel.id = ta.release_id
         LEFT JOIN release_group g ON g.id = rel.release_group_id
         WHERE si.retired_at_ms IS NULL
         ORDER BY si.provider_key",
    );
    shown.extend(rig.rows(
        "SELECT key_kind, key_value, hex(a.public_id) FROM artist_key k JOIN artist a ON a.id = k.artist_id
         ORDER BY key_kind, key_value",
    ));
    shown
}

/// The rows of each kind that a file was once on, that none is on now and
/// that lead nowhere: what a queue or a rating naming them would lose.
fn stranded(rig: &Rig) -> Vec<String> {
    rig.rows(
        "SELECT 'appearance', count(*) FROM track_appearance ta
         WHERE ta.redirect_to_id IS NULL
           AND EXISTS (SELECT 1 FROM assignment a WHERE a.appearance_id = ta.id)
           AND NOT EXISTS (SELECT 1 FROM assignment a
                           WHERE a.appearance_id = ta.id AND a.superseded_at_ms IS NULL)
         UNION ALL
         SELECT 'recording', count(*) FROM recording r
         WHERE r.redirect_to_id IS NULL
           AND EXISTS (SELECT 1 FROM assignment a WHERE a.recording_id = r.id)
           AND NOT EXISTS (SELECT 1 FROM assignment a
                           WHERE a.recording_id = r.id AND a.superseded_at_ms IS NULL)",
    )
}

/// How many releases the live files are on, and each one's discs and songs.
fn releases(rig: &Rig) -> Vec<String> {
    rig.rows(
        "SELECT rel.display_title, count(DISTINCT coalesce(ta.display_disc_number, 1)), count(*)
         FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
         JOIN release rel ON rel.id = ta.release_id
         JOIN source_item si ON si.id = a.source_item_id
         WHERE a.superseded_at_ms IS NULL AND si.item_kind = 'file' AND si.retired_at_ms IS NULL
         GROUP BY rel.id ORDER BY rel.display_title",
    )
}

#[test]
fn a_yes_to_the_group_joins_the_bonus_disc_in_one_revision_and_its_undo_gives_everything_back() {
    let rig = album();
    let before = reachable(&rig);
    assert_eq!(
        releases(&rig),
        ["Tin Weather|1|3", "Tin Weather (Bonus Disc)|1|2"]
    );
    let revision = rig.revision();

    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(answers.len(), 2);
    assert_eq!(rig.revision(), revision + 1, "one revision");
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    assert_eq!(
        rig.one(
            "SELECT DISTINCT display_primary_type FROM release_group g
                 JOIN release rel ON rel.release_group_id = g.id
                 JOIN track_appearance ta ON ta.release_id = rel.id
                 JOIN assignment a ON a.appearance_id = ta.id AND a.superseded_at_ms IS NULL"
        ),
        "album"
    );
    // The answered discs are proposed for as the album no more, only as
    // one release of it.
    let open: Vec<String> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .into_iter()
        .map(|proposal| proposal.kind)
        .collect();
    assert_eq!(open, ["edition"]);
    // The bonus disc's emptied release and appearances lead to where its
    // files went.
    assert_eq!(
        rig.one("SELECT count(*) FROM release WHERE redirect_to_id IS NOT NULL"),
        "1"
    );
    assert_eq!(
        rig.one("SELECT count(*) FROM track_appearance WHERE redirect_to_id IS NOT NULL"),
        "2"
    );

    // The same key again: the stored answer, and nothing written.
    let again = confirm(&rig, 1, vec![]).unwrap();
    assert_eq!(again, answers);
    assert_eq!(rig.revision(), revision + 1);

    undo(&rig, 2, answers.clone()).unwrap();
    assert_eq!(
        reachable(&rig),
        before,
        "the library is as it was, IDs and all"
    );
    // Proposed for again: each disc as the album, and both as an edition.
    assert_eq!(rig.library.proposals().unwrap().proposals.len(), 3);
    assert_eq!(
        rig.one("SELECT count(*) FROM answer_value WHERE withdrawn_by_answer_id IS NULL"),
        "0"
    );
    // An answer undone is not undone twice, and an undo is not undone.
    assert!(matches!(
        undo(&rig, 3, vec![answers[0]]),
        Err(Error::Invalid(_))
    ));
    let ledger = rig
        .library
        .answers(ritornel_contract::AnswersParams::default())
        .unwrap()
        .answers;
    let an_undo = ledger
        .iter()
        .find(|answer| answer.undoes.is_some())
        .unwrap()
        .answer_id;
    assert!(matches!(
        undo(&rig, 4, vec![an_undo]),
        Err(Error::Invalid(_))
    ));
    assert_eq!(ledger.len(), 4);
    assert!(
        ledger
            .iter()
            .filter(|answer| answer.undoes.is_none())
            .all(|answer| answer.undone_by.is_some())
    );
}

#[test]
fn a_bonus_disc_whose_release_stays_apart_gets_its_group_back_on_undo() {
    // The main disc names the standard edition, so the bonus disc joins its
    // album but stays a release of its own, moved into the main disc's group.
    let rig = album_with(&[(
        "TXXX",
        "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000c1",
    )]);
    let before = reachable(&rig);
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(
        rig.one(
            "SELECT count(DISTINCT rel.id) || '|' || count(DISTINCT rel.release_group_id)
             FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id WHERE a.superseded_at_ms IS NULL"
        ),
        "2|1",
        "two releases in one group"
    );
    undo(&rig, 2, answers).unwrap();
    assert_eq!(
        reachable(&rig),
        before,
        "each release is back in its own group, IDs and all"
    );
}

#[test]
fn a_proposal_changed_since_it_was_shown_is_refused_and_nothing_is_written() {
    let rig = album();
    let mut shown = group(&rig);
    shown[0].fingerprint = "an older one".to_owned();
    let (revision, dump) = (rig.revision(), rig.dump());
    assert!(matches!(confirm(&rig, 1, shown), Err(Error::PlanStale)));
    assert_eq!((rig.revision(), rig.dump()), (revision, dump));
}

#[test]
fn a_proposal_a_stopped_scan_changed_the_evidence_of_is_refused_though_not_yet_worked_out() {
    let mut rig = album();
    let shown = group(&rig);
    // The bonus disc's rip log is now of another disc, and the scan stops
    // once it has kept it, before the layouts are worked out again.
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Tin Weather Bonus/rip.log",
        b"Exact Audio Copy V1.6\n Track | Start | Length | Start sector | End sector\n  1 | 0:00.00 | 0:01.00 | 0 | 99\n",
    );
    let source = rig.main.clone();
    let cut_off = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut_off, Err(Error::FailPoint)), "{cut_off:?}");
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    let (revision, dump) = (rig.revision(), rig.dump());
    let refused = confirm(&rig, 1, shown);
    assert!(matches!(refused, Err(Error::PlanStale)), "{refused:?}");
    assert_eq!((rig.revision(), rig.dump()), (revision, dump));
}

#[test]
fn a_rescan_a_re_resolve_a_rebuild_a_moved_folder_and_a_re_rip_leave_the_answer() {
    let mut rig = album();
    confirm(&rig, 1, group(&rig)).unwrap();
    let joined = reachable(&rig);
    let request = |method| WriteRequest {
        method,
        command: None,
        client: None,
    };

    // A rescan, a re-resolve and a rebuild change nothing.
    let revision = rig.revision();
    rig.scan();
    assert!(
        rig.library
            .resolve(request("test.resolve"))
            .unwrap()
            .is_none()
    );
    assert_eq!(rig.revision(), revision);
    rig.library
        .rebuild_display(request("test.rebuild"))
        .unwrap();
    assert_eq!(reachable(&rig), joined);

    // The bonus folder moves: its files keep their items and values.
    std::fs::rename(
        rig.root().join("Moss Arcade/Tin Weather Bonus"),
        rig.root().join("Moss Arcade/Tin Weather Disc 2"),
    )
    .unwrap();
    rig.scan();
    assert_eq!(
        rig.rows("SELECT rel.display_title, count(DISTINCT coalesce(ta.display_disc_number, 1)), count(*)
                  FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
                  JOIN release rel ON rel.id = ta.release_id WHERE a.superseded_at_ms IS NULL
                  GROUP BY rel.id"),
        ["Tin Weather|2|5"]
    );

    // A bonus file ripped again: other audio under its old tags, at its
    // path. Judged under its answered values, it is the same song.
    let path = "Moss Arcade/Tin Weather Disc 2/01 Glass Harbour.aiff";
    let ripped = crate::proposal_tests::aiff_for_tests(
        200,
        99,
        "Glass Harbour",
        "Tin Weather (Bonus Disc)",
        "1",
    );
    crate::proposal_tests::write(&mut rig, path, &ripped);
    rig.scan();
    assert_eq!(
        rig.one(&format!(
            "SELECT count(*) FROM source_item WHERE provider_key = '{path}' AND retired_at_ms IS NULL"
        )),
        "1"
    );
    assert_eq!(
        rig.one("SELECT count(*) FROM source_item WHERE retired_at_ms IS NOT NULL"),
        "0"
    );
    assert_eq!(
        rig.rows(
            "SELECT count(*) FROM assignment a JOIN track_appearance ta ON ta.id = a.appearance_id
                  WHERE a.superseded_at_ms IS NULL GROUP BY ta.release_id"
        ),
        ["5"]
    );
}

#[test]
fn a_queued_song_from_the_bonus_disc_still_leads_to_its_file_after_the_join_and_after_the_undo() {
    let rig = album();
    // The bonus disc's first song, by the appearance a queue would name.
    let queued = rig.one(
        "SELECT hex(ta.public_id) FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN track_appearance ta ON ta.id = a.appearance_id
         WHERE si.provider_key LIKE '%Glass Harbour%'",
    );
    // What the player reads: the appearance at the end of its redirects,
    // and the file bound to it.
    let plays = |rig: &Rig| {
        rig.one(&format!(
            "SELECT si.provider_key FROM track_appearance q
             JOIN track_appearance ta ON ta.id = q.canonical_id
             JOIN assignment a ON a.appearance_id = ta.id AND a.superseded_at_ms IS NULL
             JOIN source_item si ON si.id = a.source_item_id
             WHERE hex(q.public_id) = '{queued}'"
        ))
    };
    let file = plays(&rig);
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(plays(&rig), file);
    undo(&rig, 2, answers).unwrap();
    assert_eq!(plays(&rig), file);
}

#[test]
fn the_ledger_is_not_rewritten_by_a_replace_or_withdrawn_by_another_answer() {
    let rig = album();
    let (first, _) = one_after_another(&rig);
    undo(&rig, 3, first).unwrap();
    let kept = rig.dump();
    for sql in [
        // A value replaced through its key, under a new row ID.
        "INSERT OR REPLACE INTO answer_value
           (id, answer_id, source_item_id, field, ordinal, value, origin, rev)
         SELECT id + 1000, answer_id, source_item_id, field, ordinal, 'other', origin, rev
         FROM answer_value LIMIT 1",
        // An answer replaced through its public ID.
        "INSERT OR REPLACE INTO answer
         SELECT id + 1000, public_id, verb, kind, proposal_public_id, proposal_fingerprint,
                candidate, undoes_answer_id, actor_kind, actor_session_pid, answered_at_ms,
                layout_version, reader_version, proposer_version, evidence_json, before_json, rev
         FROM answer LIMIT 1",
        // A value written withdrawn.
        "INSERT INTO answer_value
           (answer_id, source_item_id, field, ordinal, value, origin, withdrawn_by_answer_id, rev)
         SELECT answer_id, source_item_id, 'album', 9, 'x', origin, answer_id, rev
         FROM answer_value LIMIT 1",
        // An undo of an undo.
        "INSERT INTO answer
           (public_id, verb, undoes_answer_id, actor_kind, answered_at_ms, layout_version,
            reader_version, proposer_version, evidence_json, before_json, rev)
         SELECT randomblob(16), 'undo', id, actor_kind, answered_at_ms, layout_version,
                reader_version, proposer_version, '{}', '{}', rev
         FROM answer WHERE verb = 'undo'",
        // The second answer's values, withdrawn by the undo of the first.
        "UPDATE answer_value SET withdrawn_by_answer_id = (SELECT id FROM answer WHERE verb = 'undo')
         WHERE withdrawn_by_answer_id IS NULL",
    ] {
        let request = WriteRequest {
            method: "test.ledger",
            command: None,
            client: None,
        };
        let changed = rig.library.write(request, move |write| {
            write.connection().execute(sql, [])?;
            Ok(String::new())
        });
        assert!(changed.is_err(), "{sql}");
    }
    assert_eq!(rig.dump(), kept);
}

#[test]
fn one_answer_of_a_batch_is_undone_alone_and_the_other_stands() {
    for main_first in [true, false] {
        let rig = album();
        let (before, lost) = (reachable(&rig), stranded(&rig));
        let shown = group(&rig);
        // Which answer will be the bonus disc's.
        let is_bonus: Vec<bool> = shown
            .iter()
            .map(|one| {
                rig.library
                    .proposal(one.proposal_id)
                    .unwrap()
                    .1
                    .folder
                    .ends_with("Bonus")
            })
            .collect();
        let answers = confirm(&rig, 1, shown).unwrap();
        let (bonus, main): (Vec<_>, Vec<_>) = answers
            .iter()
            .zip(&is_bonus)
            .partition(|(_, bonus)| **bonus);
        let (bonus, main) = (*bonus[0].0, *main[0].0);
        let bonus_rows = |rows: &[String]| -> Vec<String> {
            rows.iter()
                .filter(|row| row.contains("Tin Weather Bonus/"))
                .cloned()
                .collect()
        };
        if main_first {
            // The bonus disc's yes still joins it, as disc 2.
            undo(&rig, 2, vec![main]).unwrap();
            assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
            undo(&rig, 3, vec![bonus]).unwrap();
        } else {
            // The bonus disc is back as it was, IDs and all; the main
            // disc's yes stands.
            undo(&rig, 2, vec![bonus]).unwrap();
            assert_eq!(
                releases(&rig),
                ["Tin Weather|1|3", "Tin Weather (Bonus Disc)|1|2"]
            );
            assert_eq!(bonus_rows(&reachable(&rig)), bonus_rows(&before));
            undo(&rig, 3, vec![main]).unwrap();
        }
        assert_eq!(reachable(&rig), before, "main first: {main_first}");
        assert_eq!(stranded(&rig), lost, "main first: {main_first}");
    }
}

/// The bonus disc's answer and then the main disc's, each alone, and their
/// IDs, the bonus disc's first.
fn one_after_another(rig: &Rig) -> (Vec<Id>, Vec<Id>) {
    let shown = group(rig);
    let is_bonus = |confirmation: &Confirmation| {
        rig.library
            .proposal(confirmation.proposal_id)
            .unwrap()
            .1
            .folder
            .ends_with("Bonus")
    };
    let bonus = shown.iter().find(|one| is_bonus(one)).unwrap().clone();
    let main = shown.iter().find(|one| !is_bonus(one)).unwrap().proposal_id;
    let bonus_answer = confirm(rig, 1, vec![bonus]).unwrap();
    let main_now = group(rig)
        .into_iter()
        .find(|one| one.proposal_id == main)
        .unwrap();
    (bonus_answer, confirm(rig, 2, vec![main_now]).unwrap())
}

#[test]
fn an_earlier_answer_is_undone_while_a_later_one_about_the_same_files_stands() {
    for together in [false, true] {
        let rig = album();
        let (before, lost) = (reachable(&rig), stranded(&rig));
        let (first, second) = one_after_another(&rig);
        // The later answer is made to be about a file of the first as well,
        // with the value already in force on it.
        let request = WriteRequest {
            method: "test.overlap",
            command: None,
            client: None,
        };
        rig.library
            .write(request, |write| {
                write.connection().execute(
                    "INSERT INTO answer_value (answer_id, source_item_id, field, ordinal, value, origin, rev)
                     SELECT (SELECT max(id) FROM answer), source_item_id, field, ordinal, value, origin, rev
                     FROM answer_value WHERE answer_id = (SELECT min(id) FROM answer)
                       AND field = 'primary_type' LIMIT 1",
                    [],
                )?;
                Ok(String::new())
            })
            .unwrap();
        if together {
            undo(&rig, 3, [first, second].concat()).unwrap();
        } else {
            // The earlier first, while the later stands, then the later.
            undo(&rig, 3, first).unwrap();
            undo(&rig, 4, second).unwrap();
        }
        assert_eq!(reachable(&rig), before, "together: {together}");
        assert_eq!(stranded(&rig), lost, "together: {together}");
    }
}

#[test]
fn a_yes_that_took_its_title_from_another_answer_goes_with_it() {
    let rig = album();
    let (before, lost) = (reachable(&rig), stranded(&rig));
    let shown = group(&rig);
    let is_bonus = |confirmation: &Confirmation| {
        rig.library
            .proposal(confirmation.proposal_id)
            .unwrap()
            .1
            .folder
            .ends_with("Bonus")
    };
    let main = shown.iter().find(|one| !is_bonus(one)).unwrap().clone();
    let bonus_id = shown.iter().find(|one| is_bonus(one)).unwrap().proposal_id;
    let main_answer = confirm(&rig, 1, vec![main]).unwrap();
    // The main disc's yes is made to give its files their title too, as
    // it does for files that have none.
    rig.library
        .write(
            WriteRequest {
                method: "test.title",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "INSERT INTO answer_value (answer_id, source_item_id, field, ordinal, value, origin, rev)
                     SELECT answer_id, source_item_id, 'album', 0, 'Tin Weather', 'catalogue', rev
                     FROM answer_value WHERE field = 'mb_release_group'",
                    [],
                )?;
                Ok(String::new())
            },
        )
        .unwrap();
    let bonus = group(&rig)
        .into_iter()
        .find(|one| one.proposal_id == bonus_id)
        .unwrap();
    let bonus_answer = confirm(&rig, 2, vec![bonus]).unwrap();
    // The bonus disc took the title the main disc's yes gave it.
    assert_eq!(rig.one("SELECT count(*) FROM answer_dependency"), "1");
    // The undo's preview says so, and what it gives back, writing nothing.
    let (revision, dump) = (rig.revision(), rig.dump());
    let shown = rig.library.preview_undo(&main_answer).unwrap();
    assert_eq!(shown.undoes, [main_answer[0], bonus_answer[0]]);
    // Each value the undo withdraws, once.
    assert_eq!(
        shown.values.len().to_string(),
        rig.one("SELECT count(*) FROM answer_value WHERE withdrawn_by_answer_id IS NULL")
    );
    let shapes: Vec<(Option<String>, u32, u32)> = shown
        .after
        .iter()
        .map(|album| (album.title.clone(), album.discs, album.songs))
        .collect();
    assert_eq!(
        shapes,
        [
            (Some("Tin Weather".to_owned()), 1, 3),
            (Some("Tin Weather (Bonus Disc)".to_owned()), 1, 2)
        ]
    );
    assert_eq!((rig.revision(), rig.dump()), (revision, dump));
    let undone = rig
        .library
        .undo(command(3), main_answer.clone(), |_, undone, _| {
            serde_json::json!(
                undone
                    .iter()
                    .map(|(_, answer)| answer.to_string())
                    .collect::<Vec<_>>()
            )
            .to_string()
        })
        .unwrap();
    let undone: Vec<String> = serde_json::from_str(&undone.result_json).unwrap();
    assert_eq!(
        undone,
        [main_answer[0].to_string(), bonus_answer[0].to_string()],
        "the bonus disc's yes goes with the main disc's"
    );
    assert_eq!(reachable(&rig), before);
    assert_eq!(stranded(&rig), lost);
}

#[test]
fn a_release_an_answer_led_on_is_not_a_merge_its_files_are_held_to() {
    let mut rig = album();
    confirm(&rig, 1, group(&rig)).unwrap();
    // A main disc file retagged as another album leaves the joined one,
    // as it would had the files been tagged so: the bonus disc's emptied
    // release leading to it is the answer's, not a merge.
    let retagged =
        crate::proposal_tests::aiff_for_tests(90, 3, "Salt Roads", "Harbour Sessions", "3");
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Tin Weather/03 Salt Roads.aiff",
        &retagged,
    );
    rig.scan();
    assert_eq!(releases(&rig), ["Harbour Sessions|1|1", "Tin Weather|2|4"]);
}

#[test]
fn answers_given_one_after_another_are_undone_in_either_order() {
    for bonus_first in [true, false] {
        let rig = album();
        let before = reachable(&rig);
        let (bonus_answer, main_answer) = one_after_another(&rig);
        assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
        let (first, second) = if bonus_first {
            (bonus_answer, main_answer)
        } else {
            (main_answer, bonus_answer)
        };
        undo(&rig, 3, first).unwrap();
        undo(&rig, 4, second).unwrap();
        assert_eq!(reachable(&rig), before, "undone bonus first: {bonus_first}");
    }
}

#[test]
fn a_yes_joins_two_releases_that_carry_edits_of_their_own_and_its_undo_parts_them() {
    let rig = album();
    // Each release has a date a person gave it.
    rig.library
        .write(
            WriteRequest {
                method: "test.edit",
                command: None,
                client: None,
            },
            |write| {
                write.connection().execute(
                    "INSERT INTO metadata_edit
                       (entity_kind, entity_id, field, value, actor_kind, edited_at_ms, rev)
                     SELECT 'release', id, 'date', '2004', 'person', ?1, ?2 FROM release",
                    (write.now_ms(), write.rev()),
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
    let before = reachable(&rig);
    assert_eq!(
        releases(&rig),
        ["Tin Weather|1|3", "Tin Weather (Bonus Disc)|1|2"]
    );
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|2|5"], "the yes joins them");
    // Each answer keeps what it put together: both releases and both
    // groups.
    assert_eq!(
        rig.one("SELECT count(DISTINCT row_id) FROM answer_join WHERE entity = 'release'"),
        "2"
    );
    undo(&rig, 2, answers).unwrap();
    assert_eq!(
        releases(&rig),
        ["Tin Weather|1|3", "Tin Weather (Bonus Disc)|1|2"]
    );
    assert_eq!(reachable(&rig), before);
}

/// The main disc's and the bonus disc's proposals, as shown now.
fn discs(rig: &Rig) -> (Option<Confirmation>, Option<Confirmation>) {
    let shown = group(rig);
    let is_bonus = |confirmation: &Confirmation| {
        rig.library
            .proposal(confirmation.proposal_id)
            .unwrap()
            .1
            .folder
            .ends_with("Bonus")
    };
    (
        shown.iter().find(|one| !is_bonus(one)).cloned(),
        shown.iter().find(|one| is_bonus(one)).cloned(),
    )
}

#[test]
fn any_answers_undone_in_any_order_leave_what_those_still_standing_make() {
    // The albums a library makes with only these discs answered, each
    // alone, from scratch.
    let made = |main: bool, bonus: bool| {
        let rig = album();
        let (main_disc, bonus_disc) = discs(&rig);
        if main {
            confirm(&rig, 1, vec![main_disc.unwrap()]).unwrap();
        }
        if bonus {
            let bonus_now = discs(&rig).1.or(bonus_disc).unwrap();
            confirm(&rig, 2, vec![bonus_now]).unwrap();
        }
        releases(&rig)
    };
    let expected: HashMap<(bool, bool), Vec<String>> =
        [(false, false), (true, false), (false, true), (true, true)]
            .into_iter()
            .map(|answered| (answered, made(answered.0, answered.1)))
            .collect();
    // Given together, main first, or bonus first; undone one at a time in
    // either order, or together.
    for given in ["together", "main first", "bonus first"] {
        for undone in ["main first", "bonus first", "together"] {
            let case = format!("given {given}, undone {undone}");
            let rig = album();
            let (before, lost) = (reachable(&rig), stranded(&rig));
            let (main_disc, bonus_disc) = discs(&rig);
            let (main_disc, bonus_disc) = (main_disc.unwrap(), bonus_disc.unwrap());
            let (main, bonus) = match given {
                "together" => {
                    let both = confirm(&rig, 1, vec![main_disc, bonus_disc]).unwrap();
                    (both[0], both[1])
                }
                "main first" => {
                    let main = confirm(&rig, 1, vec![main_disc]).unwrap()[0];
                    let bonus_now = discs(&rig).1.unwrap();
                    (main, confirm(&rig, 2, vec![bonus_now]).unwrap()[0])
                }
                _ => {
                    let bonus = confirm(&rig, 1, vec![bonus_disc]).unwrap()[0];
                    let main_now = discs(&rig).0.unwrap();
                    (confirm(&rig, 2, vec![main_now]).unwrap()[0], bonus)
                }
            };
            assert_eq!(releases(&rig), expected[&(true, true)], "{case}");
            match undone {
                "main first" => {
                    undo(&rig, 3, vec![main]).unwrap();
                    assert_eq!(releases(&rig), expected[&(false, true)], "{case}");
                    undo(&rig, 4, vec![bonus]).unwrap();
                }
                "bonus first" => {
                    undo(&rig, 3, vec![bonus]).unwrap();
                    assert_eq!(releases(&rig), expected[&(true, false)], "{case}");
                    undo(&rig, 4, vec![main]).unwrap();
                }
                _ => undo(&rig, 3, vec![main, bonus]).unwrap(),
            }
            assert_eq!(releases(&rig), expected[&(false, false)], "{case}");
            assert_eq!(reachable(&rig), before, "{case}");
            assert_eq!(stranded(&rig), lost, "{case}");
            // No yes stands, so none of their redirects counts.
            assert_eq!(
                rig.one(
                    "SELECT count(*) FROM answer_redirect r JOIN answer a ON a.id = r.answer_id
                     WHERE NOT EXISTS (SELECT 1 FROM answer u WHERE u.undoes_answer_id = a.id)
                       AND a.verb = 'confirm'"
                ),
                "0",
                "{case}"
            );
        }
    }
}

#[test]
fn the_ledger_is_read_a_page_at_a_time_and_an_answer_opens_to_what_it_rested_on() {
    let rig = album();
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    undo(&rig, 2, vec![answers[0]]).unwrap();
    let page = |before, limit| {
        rig.library
            .answers(ritornel_contract::AnswersParams { before, limit })
            .unwrap()
    };
    // Three answers: two yeses, then an undo.
    let first = page(None, Some(2));
    assert_eq!(first.answers.len(), 2);
    assert_eq!(first.answers[0].verb, ritornel_contract::AnswerVerb::Undo);
    let rest = page(first.next, Some(2));
    assert_eq!(rest.answers.len(), 1);
    assert_eq!(rest.next, None);
    assert_eq!(page(None, None).answers.len(), 3);

    let got = rig.library.answer(answers[1]).unwrap();
    let evidence = &got.evidence;
    let candidate = evidence.candidate.as_ref().unwrap();
    assert_eq!(candidate.release_group, GROUP);
    assert_eq!(candidate.title.as_deref(), Some("Tin Weather"));
    assert!(!evidence.candidates.is_empty());
    assert!(!evidence.replies.is_empty());
    assert!(
        evidence
            .layouts
            .iter()
            .all(|layout| !layout.methods.is_empty())
    );
    assert!(
        evidence
            .values
            .iter()
            .any(|value| value.field == "mb_release_group" && value.value.as_deref() == Some(GROUP))
    );
    let undid = rig.library.answer(first.answers[0].answer_id).unwrap();
    assert_eq!(undid.answer.undoes, Some(answers[0]));
    assert!(undid.evidence.candidate.is_none());
}

/// Gives every release or group a value of `field` a person set, as
/// `metadata_edit` keeps it.
fn edit_each(rig: &Rig, kind: &'static str, field: &'static str, values: &'static [&'static str]) {
    rig.library
        .write(
            WriteRequest {
                method: "test.edit",
                command: None,
                client: None,
            },
            move |write| {
                let table = if kind == "release" {
                    "release"
                } else {
                    "release_group"
                };
                let rows: Vec<i64> = crate::resolve_store::rows(
                    write.connection(),
                    &format!("SELECT id FROM {table} ORDER BY id"),
                    |row| row.get(0),
                )?;
                for (row, value) in rows.iter().zip(values) {
                    if value.is_empty() {
                        continue;
                    }
                    write.connection().execute(
                        "INSERT INTO metadata_edit
                           (entity_kind, entity_id, field, value, actor_kind, edited_at_ms, rev)
                         VALUES (?1, ?2, ?3, ?4, 'person', ?5, ?6)",
                        (kind, row, field, value, write.now_ms(), write.rev()),
                    )?;
                }
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
}

#[test]
fn an_earlier_answer_whose_edited_release_won_the_join_is_undone_while_a_later_one_stands() {
    let rig = album();
    // Only the bonus disc's release carries an edit, so it is the one the
    // join keeps.
    edit_each(&rig, "release", "date", &["", "2004"]);
    let (before, lost) = (reachable(&rig), stranded(&rig));
    let (bonus, main) = one_after_another(&rig);
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    undo(&rig, 3, bonus).unwrap();
    undo(&rig, 4, main).unwrap();
    assert_eq!(reachable(&rig), before);
    assert_eq!(stranded(&rig), lost);
}

#[test]
fn the_preview_of_an_undo_shows_what_the_undo_gives_back_edits_and_all() {
    let rig = album();
    edit_each(&rig, "release_group", "primary_type", &["ep", "single"]);
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    let shown = rig.library.preview_undo(&answers).unwrap();
    assert_eq!(
        i64::try_from(shown.library_revision).unwrap(),
        rig.revision(),
        "the revision it was worked out at"
    );
    undo(&rig, 2, answers).unwrap();
    let kinds: Vec<Option<String>> = shown
        .after
        .iter()
        .map(|album| album.primary_type.clone())
        .collect();
    assert_eq!(kinds, [Some("ep".to_owned()), Some("single".to_owned())]);
    let now = rig.library.preview_undo(&[]).err();
    assert!(now.is_some(), "an undo of nothing is refused");
}

#[test]
fn a_yes_to_the_first_disc_alone_puts_nothing_with_it() {
    let rig = album();
    let main = discs(&rig).0.unwrap();
    confirm(&rig, 1, vec![main]).unwrap();
    // The bonus disc's yes would join it; the main disc's own does not.
    assert_eq!(
        rig.one("SELECT count(*) FROM answer_join WHERE entity = 'release'"),
        "1"
    );
    let bonus = discs(&rig).1.unwrap();
    confirm(&rig, 2, vec![bonus]).unwrap();
    assert_eq!(
        rig.one("SELECT count(DISTINCT row_id) FROM answer_join WHERE entity = 'release'"),
        "2"
    );
}

#[test]
fn a_later_answer_undone_leaves_what_the_earlier_one_made_ids_and_all() {
    let rig = album();
    let shown = group(&rig);
    let is_bonus = |confirmation: &Confirmation| {
        rig.library
            .proposal(confirmation.proposal_id)
            .unwrap()
            .1
            .folder
            .ends_with("Bonus")
    };
    let bonus = shown.iter().find(|one| is_bonus(one)).unwrap().clone();
    let main = shown.iter().find(|one| !is_bonus(one)).unwrap().proposal_id;
    confirm(&rig, 1, vec![bonus]).unwrap();
    let after_first = reachable(&rig);
    let main_now = group(&rig)
        .into_iter()
        .find(|one| one.proposal_id == main)
        .unwrap();
    let later = confirm(&rig, 2, vec![main_now]).unwrap();
    undo(&rig, 3, later).unwrap();
    assert_eq!(reachable(&rig), after_first);
}

#[test]
fn a_yes_undone_from_a_batch_takes_its_part_of_the_albums_join_and_a_retag_stays_apart() {
    let mut rig = album();
    // Each album's group carries a kind a person gave it.
    edit_each(&rig, "release_group", "primary_type", &["ep", "single"]);
    let group_of = |rig: &Rig, folder: &str| {
        rig.one(&format!(
            "SELECT hex(g.public_id) FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id
             JOIN release_group g ON g.id = rel.release_group_id
             WHERE si.provider_key LIKE '{folder}/%' LIMIT 1"
        ))
    };
    let (main, bonus) = discs(&rig);
    let answers = confirm(&rig, 1, vec![main.unwrap(), bonus.unwrap()]).unwrap();
    assert_eq!(
        group_of(&rig, "Moss Arcade/Tin Weather"),
        group_of(&rig, "Moss Arcade/Tin Weather Bonus"),
        "said yes to together, one album"
    );
    // The bonus disc's yes undone alone.
    undo(&rig, 2, vec![answers[1]]).unwrap();
    assert_ne!(
        group_of(&rig, "Moss Arcade/Tin Weather"),
        group_of(&rig, "Moss Arcade/Tin Weather Bonus")
    );
    // Its files retagged later with the album's ID: no yes standing put
    // the two groups, each with its own kind, together.
    for (seed, title, sectors, track) in
        [(4, "Glass Harbour", 200, "1"), (5, "Far Lights", 300, "2")]
    {
        let id = format!("MusicBrainz Release Group Id\u{0}{GROUP}");
        let bytes = crate::proposal_tests::aiff(
            sectors,
            seed,
            &[
                ("TIT2", title),
                ("TPE1", "Moss Arcade"),
                ("TPE2", "Moss Arcade"),
                ("TALB", "Tin Weather (Bonus Disc)"),
                ("TRCK", track),
                ("TXXX", id.as_str()),
            ],
        );
        crate::proposal_tests::write(
            &mut rig,
            &format!("Moss Arcade/Tin Weather Bonus/0{track} {title}.aiff"),
            &bytes,
        );
    }
    rig.scan();
    let read = |sql: &str| rig.one(sql).parse::<u32>().unwrap_or(0);
    assert!(
        read(
            "SELECT count(*) FROM source_item si JOIN identifier_claim c ON c.source_item_id = si.id
             WHERE si.provider_key LIKE 'Moss Arcade/Tin Weather Bonus/%' AND si.retired_at_ms IS NULL"
        ) > 0,
        "the retag is read"
    );
    assert_ne!(
        group_of(&rig, "Moss Arcade/Tin Weather"),
        group_of(&rig, "Moss Arcade/Tin Weather Bonus"),
        "a retag alone does not join two albums that carry input of their own"
    );
}

#[test]
fn the_album_and_its_edition_together_make_one_release_of_two_discs_and_each_undoes() {
    let rig = album();
    let (before, lost) = (reachable(&rig), stranded(&rig));
    let edition_yes = edition(&rig).expect("the two discs are proposed as one release");
    assert!(edition_yes.release.is_some());
    // An edition is said with or after the album of its discs.
    assert!(matches!(
        confirm(&rig, 1, vec![edition_yes.clone()]),
        Err(Error::Invalid(_))
    ));
    let mut all = group(&rig);
    all.push(edition_yes);
    let answers = confirm(&rig, 2, all).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    // Each file has its disc and track on the release.
    assert_eq!(
        rig.rows(
            "SELECT ta.display_disc_number || '-' || ta.display_track_number FROM assignment a
             JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE a.superseded_at_ms IS NULL ORDER BY 1"
        ),
        ["1-1", "1-2", "1-3", "2-1", "2-2"]
    );
    // The edition undone alone leaves the album's join.
    let edition_answer = answers[2];
    undo(&rig, 3, vec![edition_answer]).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    // The albums' yeses undone, everything is as it was.
    undo(&rig, 4, answers[..2].to_vec()).unwrap();
    assert_eq!(reachable(&rig), before);
    assert_eq!(stranded(&rig), lost);
}

#[test]
fn undoing_an_album_undoes_the_edition_that_rests_on_it() {
    let rig = album();
    let before = reachable(&rig);
    let edition_yes = edition(&rig).unwrap();
    let mut all = group(&rig);
    all.push(edition_yes);
    let answers = confirm(&rig, 1, all).unwrap();
    let shown = rig.library.preview_undo(&answers[..1]).unwrap();
    assert!(shown.undoes.contains(&answers[2]), "the edition goes too");
    undo(&rig, 2, answers[..2].to_vec()).unwrap();
    assert_eq!(
        rig.rows("SELECT count(*) FROM answer WHERE verb = 'undo'"),
        ["3"]
    );
    assert_eq!(reachable(&rig), before);
}

fn decline(rig: &Rig, byte: u8, proposal: &Confirmation, stop_asking: bool) -> Vec<Id> {
    let written = rig
        .library
        .decline(
            command(byte),
            ritornel_contract::DeclineParams {
                idempotency_key: Id::from_bytes([0xe0 + byte; 16]),
                proposal_id: proposal.proposal_id,
                fingerprint: proposal.fingerprint.clone(),
                release_group: (!stop_asking).then(|| proposal.release_group.clone()),
                release: if stop_asking {
                    None
                } else {
                    proposal.release.clone()
                },
                stop_asking,
            },
            |_, answers, _| {
                serde_json::json!(
                    answers
                        .iter()
                        .map(|(answer, _)| answer.to_string())
                        .collect::<Vec<_>>()
                )
                .to_string()
            },
        )
        .unwrap();
    serde_json::from_str::<Vec<String>>(&written.result_json)
        .unwrap()
        .iter()
        .map(|id| id.parse().unwrap())
        .collect()
}

/// The release each disc's first file is on, by its public ID.
fn release_ids(rig: &Rig) -> (Id, Id) {
    let of = |folder: &str| -> Id {
        let hex = rig.one(&format!(
            "SELECT hex(rel.public_id) FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id
             WHERE si.provider_key LIKE '{folder}/%' LIMIT 1"
        ));
        let bytes: Vec<u8> = (0..16)
            .map(|at| u8::from_str_radix(&hex[at * 2..at * 2 + 2], 16).unwrap())
            .collect();
        Id::from_bytes(bytes.try_into().unwrap())
    };
    (
        of("Moss Arcade/Tin Weather"),
        of("Moss Arcade/Tin Weather Bonus"),
    )
}

#[test]
fn a_candidate_said_no_to_is_offered_no_more_until_the_no_is_undone() {
    let mut rig = album();
    let dump = reachable(&rig);
    let bonus = discs(&rig).1.unwrap();
    let no = decline(&rig, 1, &bonus, false);
    assert_eq!(reachable(&rig), dump, "a no moves nothing");
    // Its one candidate said no to, the bonus disc is proposed for no more.
    assert!(rig.library.proposal(bonus.proposal_id).is_err());
    let said = rig.library.answer(no[0]).unwrap();
    assert_eq!(said.answer.verb, ritornel_contract::AnswerVerb::Decline);
    assert_eq!(
        said.evidence.candidate.unwrap().title.as_deref(),
        Some("Tin Weather")
    );
    // A rescan and a re-resolve keep it.
    rig.scan();
    assert!(discs(&rig).1.is_none_or(|again| {
        again.proposal_id != bonus.proposal_id
            || rig
                .library
                .proposal(again.proposal_id)
                .unwrap()
                .1
                .candidates
                .is_empty()
    }));
    undo(&rig, 2, no).unwrap();
    assert!(discs(&rig).1.is_some(), "offered again");
}

#[test]
fn files_asked_no_more_about_are_in_no_proposal_until_the_no_is_undone() {
    let rig = album();
    let main = discs(&rig).0.unwrap();
    let no = decline(&rig, 1, &main, true);
    let about_main = |rig: &Rig| {
        rig.one(
            "SELECT count(*) FROM proposal_item pi JOIN source_item si ON si.id = pi.source_item_id
             WHERE si.provider_key LIKE 'Moss Arcade/Tin Weather/%'",
        )
    };
    assert_eq!(about_main(&rig), "0");
    // Opened to what it said, a stop names no candidate.
    let said = rig.library.answer(no[0]).unwrap();
    assert!(said.evidence.candidate.is_none());
    undo(&rig, 2, no).unwrap();
    assert_ne!(about_main(&rig), "0");
}

#[test]
fn files_asked_no_more_about_as_an_edition_are_still_asked_about_as_an_album() {
    let rig = album();
    let edition_now = edition(&rig).unwrap();
    decline(&rig, 1, &edition_now, true);
    let kinds: Vec<String> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .into_iter()
        .map(|proposal| proposal.kind)
        .collect();
    assert_eq!(kinds, ["album", "album"]);
    assert_eq!(
        rig.one("SELECT about FROM answer_rule WHERE kind = 'stop_asking'"),
        "edition"
    );
}

#[test]
fn two_albums_kept_apart_are_never_joined_and_the_no_is_undone() {
    let rig = album();
    let (before, lost) = (reachable(&rig), stranded(&rig));
    let (main, bonus) = release_ids(&rig);
    let written = rig
        .library
        .keep_apart(command(1), vec![main, bonus], |_, answer, _| {
            answer.to_string()
        })
        .unwrap();
    let apart: Id = written.result_json.parse().unwrap();
    // The bonus disc is proposed as the album alone, joining nothing, and
    // the two are no one release.
    let proposals = rig.library.proposals().unwrap().proposals;
    assert!(proposals.iter().all(|proposal| proposal.kind == "album"));
    let answers = confirm(&rig, 2, group(&rig)).unwrap();
    assert_eq!(releases(&rig).len(), 2, "still two albums");
    undo(&rig, 3, answers).unwrap();
    undo(&rig, 4, vec![apart]).unwrap();
    assert_eq!(reachable(&rig), before);
    assert_eq!(stranded(&rig), lost);
    assert!(edition(&rig).is_some(), "proposed as one release again");
}

#[test]
fn two_releases_of_a_group_chosen_for_both_are_not_kept_apart() {
    let rig = album();
    let (main, bonus) = release_ids(&rig);
    rig.library
        .write(command(1), |write| {
            write.connection().execute(
                "UPDATE release SET chosen_release_group_id = (SELECT min(id) FROM release_group)",
                [],
            )?;
            Ok(String::new())
        })
        .unwrap();
    assert!(matches!(
        rig.library
            .keep_apart(command(2), vec![main, bonus], |_, _, _| String::new()),
        Err(Error::Invalid(_))
    ));
}

/// A play another service reported of the bonus disc's first song, taken
/// in and matched as an import is.
fn scrobble_the_bonus_disc(rig: &Rig) {
    let account = rig
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let batch = ritornel_source::Batch {
        records: vec![ritornel_source::ImportRecord {
            source_key: b"glass harbour".to_vec(),
            played_at_ms: 1_000,
            track: ritornel_source::TrackText {
                artist: "Moss Arcade".to_owned(),
                album: Some("Tin Weather (Bonus Disc)".to_owned()),
                title: "Glass Harbour".to_owned(),
            },
            ids: ritornel_source::Identifiers::default(),
            payload_json: "{}".to_owned(),
        }],
        span: ritornel_source::Span {
            from_ms: 0,
            to_ms: 2_000,
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
    rig.library.match_library_for(account).unwrap();
}

#[test]
fn a_spotify_stream_of_the_bonus_disc_follows_a_yes_and_its_undo() {
    let rig = album();
    let mut stream = crate::import_store::tests::stream(1_000_000, 2_000, "Glass Harbour");
    stream.album = Some("Tin Weather (Bonus Disc)".to_owned());
    rig.library
        .apply_streams(
            WriteRequest {
                method: crate::APPLY_METHOD,
                command: None,
                client: None,
            },
            Id::from_bytes([0x77; 16]),
            vec![stream],
            "ritornelctl 0.1.0".to_owned(),
            None,
        )
        .unwrap();
    // The stream's play: the song and the album it counts for, by public
    // ID, and the record it was heard from.
    let played = |rig: &Rig| {
        rig.rows(
            "SELECT hex(r.public_id), hex(ta.public_id), hex(g.public_id) FROM play_event pe
             JOIN source_item si ON si.id = pe.source_item_id AND si.item_kind = 'spotify_track'
             JOIN recording r ON r.id = pe.recording_id
             JOIN track_appearance ta ON ta.id = pe.appearance_id
             JOIN play_event_album pa ON pa.play_event_id = pe.id
             JOIN heard_album h ON h.id = pa.heard_album_id
             JOIN release_group g ON g.id = h.release_group_id",
        )
    };
    let file = |rig: &Rig, release: &str, disc: i64| {
        rig.rows(&format!(
            "SELECT hex(r.public_id), hex(ta.public_id), hex(g.public_id) FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN recording r ON r.id = a.recording_id
             JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id
             JOIN release_group g ON g.id = rel.release_group_id
             WHERE si.item_kind = 'file' AND rel.display_title = '{release}'
               AND coalesce(ta.display_disc_number, 1) = {disc} AND ta.display_track_number = 1"
        ))
    };
    let was = played(&rig);
    assert_eq!(
        was,
        file(&rig, "Tin Weather (Bonus Disc)", 1),
        "the stream is bound to the bonus disc"
    );

    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(
        played(&rig),
        file(&rig, "Tin Weather", 2),
        "its play follows the join"
    );
    undo(&rig, 2, answers).unwrap();
    assert_eq!(played(&rig), was);
}

#[test]
fn a_bonus_disc_a_scrobble_is_bound_to_is_led_to_where_its_files_went_and_back() {
    let rig = album();
    scrobble_the_bonus_disc(&rig);
    let before = reachable(&rig);
    // Where the scrobble's song leads: the appearance and recording at the
    // end of their redirects, by public ID.
    let heard = |rig: &Rig| {
        rig.rows(
            "SELECT hex(r.public_id), hex(ta.public_id), hex(rel.public_id)
             FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN recording r0 ON r0.id = a.recording_id JOIN recording r ON r.id = r0.canonical_id
             JOIN track_appearance ta0 ON ta0.id = a.appearance_id
             JOIN track_appearance ta ON ta.id = ta0.canonical_id
             JOIN release rel0 ON rel0.id = ta.release_id JOIN release rel ON rel.id = rel0.canonical_id
             WHERE si.item_kind = 'scrobble_subject'",
        )
    };
    // The song its play counts for, as the play holds it.
    let played = |rig: &Rig| {
        rig.rows(
            "SELECT hex(r.public_id), hex(ta.public_id) FROM play_event pe
             JOIN recording r ON r.id = pe.recording_id
             JOIN track_appearance ta ON ta.id = pe.appearance_id",
        )
    };
    let was = heard(&rig);
    assert_eq!(was.len(), 1, "the scrobble is bound to the bonus disc");
    let was_played = played(&rig);
    assert_eq!(was_played, [was[0].rsplit_once('|').unwrap().0]);

    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    assert_eq!(
        rig.one("SELECT count(*) FROM release WHERE redirect_to_id IS NOT NULL"),
        "1",
        "the bonus disc's release leads to the album's"
    );
    assert_eq!(
        rig.one("SELECT count(*) FROM track_appearance WHERE redirect_to_id IS NOT NULL"),
        "2"
    );
    // The scrobble's song leads to the joined album's song.
    let joined = heard(&rig);
    assert_ne!(joined, was);
    let file_now = rig.rows(
        "SELECT hex(r.public_id), hex(ta.public_id), hex(rel.public_id)
         FROM source_item si
         JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
         JOIN recording r ON r.id = a.recording_id
         JOIN track_appearance ta ON ta.id = a.appearance_id
         JOIN release rel ON rel.id = ta.release_id
         WHERE si.item_kind = 'file' AND rel.display_title = 'Tin Weather'
           AND ta.display_disc_number = 2 AND ta.display_track_number = 1",
    );
    assert_eq!(joined, file_now);
    assert_eq!(
        played(&rig),
        [joined[0].rsplit_once('|').unwrap().0],
        "its play counts for the joined album's song"
    );

    undo(&rig, 2, answers).unwrap();
    assert_eq!(reachable(&rig), before, "IDs and all");
    assert_eq!(heard(&rig), was);
    assert_eq!(played(&rig), was_played);
}

/// The album, and a live recording of it in a folder of its own whose files
/// already carry the album's group ID, more of them than the main disc has.
fn album_and_its_tagged_live_release() -> Rig {
    let mut rig = album();
    let id = format!("MusicBrainz Release Group Id\u{0}{GROUP}");
    let titles = [
        "Paper Kites",
        "Low Tide",
        "Salt Roads",
        "Glass Harbour",
        "Far Lights",
        "Encore",
    ];
    for (seed, title) in (20..).zip(titles) {
        let track = (seed - 19).to_string();
        let bytes = crate::proposal_tests::aiff(
            150 + seed,
            seed,
            &[
                ("TIT2", title),
                ("TPE1", "Moss Arcade"),
                ("TPE2", "Moss Arcade"),
                ("TALB", "Tin Weather (Live)"),
                ("TRCK", track.as_str()),
                ("TXXX", id.as_str()),
            ],
        );
        crate::proposal_tests::write(
            &mut rig,
            &format!("Moss Arcade/Tin Weather Live/0{track} {title}.aiff"),
            &bytes,
        );
    }
    rig.scan();
    rig
}

#[test]
fn undoing_one_answer_of_a_batch_leaves_the_group_the_other_and_a_tagged_release_share() {
    let group_of = |rig: &Rig, folder: &str| {
        rig.one(&format!(
            "SELECT hex(g.public_id) FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id
             JOIN release_group g ON g.id = rel.release_group_id
             WHERE si.provider_key LIKE '{folder}/%' LIMIT 1"
        ))
    };
    for undone in [0, 1] {
        let rig = album_and_its_tagged_live_release();
        let (main, bonus) = discs(&rig);
        let answers = confirm(&rig, 1, vec![main.unwrap(), bonus.unwrap()]).unwrap();
        let joined = group_of(&rig, "Moss Arcade/Tin Weather");
        assert_eq!(group_of(&rig, "Moss Arcade/Tin Weather Live"), joined);
        // One disc's yes undone alone: the other's stands, and with it the
        // group it shares with the live release, ID and all.
        undo(&rig, 2, vec![answers[undone]]).unwrap();
        let other = ["Moss Arcade/Tin Weather Bonus", "Moss Arcade/Tin Weather"][undone];
        assert_eq!(group_of(&rig, other), joined, "undone: {undone}");
        assert_eq!(
            group_of(&rig, "Moss Arcade/Tin Weather Live"),
            joined,
            "undone: {undone}"
        );
    }
}

#[test]
fn an_edition_said_again_after_a_rescan_is_undone_to_where_the_rescan_left_its_files() {
    let mut rig = album();
    let album_yes = confirm(&rig, 1, group(&rig)).unwrap();
    let first = confirm(&rig, 2, vec![edition(&rig).unwrap()]).unwrap();
    undo(&rig, 3, first).unwrap();
    let undone = reachable(&rig);
    // A retitled song moves its file to other rows while the album's yes
    // stands.
    let bytes = crate::proposal_tests::aiff(
        300,
        5,
        &[
            ("TIT2", "Far Lights (Reprise)"),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", "Tin Weather (Bonus Disc)"),
            ("TRCK", "2"),
        ],
    );
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Tin Weather Bonus/02 Far Lights.aiff",
        &bytes,
    );
    rig.scan();
    let rescanned = reachable(&rig);
    assert_ne!(rescanned, undone, "the rescan moved a file");
    let again = confirm(&rig, 4, vec![edition(&rig).unwrap()]).unwrap();
    undo(&rig, 5, again).unwrap();
    assert_eq!(reachable(&rig), rescanned);
    assert!(!album_yes.is_empty());
}

#[test]
fn a_resolution_a_stopped_scan_still_owes_is_made_before_a_yes_and_is_not_its_own() {
    let mut rig = album();
    let song = |seed: u32, title: &str, album: &str| {
        crate::proposal_tests::aiff(
            50 + seed,
            seed,
            &[
                ("TIT2", title),
                ("TPE1", "Pale Engine"),
                ("TALB", album),
                ("TRCK", "1"),
            ],
        )
    };
    crate::proposal_tests::write(
        &mut rig,
        "Pale Engine/Singles/01 Lamplight.aiff",
        &song(40, "Lamplight", "Lamplight"),
    );
    crate::proposal_tests::write(
        &mut rig,
        "Pale Engine/Singles/02 Embers.aiff",
        &song(41, "Embers", "Embers"),
    );
    rig.scan();
    let shown = group(&rig);
    // One single of the folder filed under the other, and the scan stops
    // once it has read it, before it resolves.
    crate::proposal_tests::write(
        &mut rig,
        "Pale Engine/Singles/02 Embers.aiff",
        &song(41, "Embers", "Lamplight"),
    );
    let source = rig.main.clone();
    let cut_off = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut_off, Err(Error::FailPoint)), "{cut_off:?}");
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    assert_eq!(
        rig.one("SELECT count(*) FROM library_setting WHERE key = 'identity.resolution_pending'"),
        "1",
        "a resolution is owed"
    );
    let answers = confirm(&rig, 1, shown).unwrap();
    assert_eq!(
        rig.one(
            "SELECT count(*) FROM answer a, json_each(a.before_json, '$.items') e
             JOIN source_item si ON si.id = json_extract(e.value, '$.item')
             WHERE si.provider_key LIKE 'Pale Engine/%'"
        ),
        "0",
        "the singles the owed resolution moved, not the yes"
    );
    assert_eq!(
        rig.one("SELECT rel.display_title FROM track_appearance ta JOIN assignment a ON a.appearance_id = ta.id
                 JOIN release rel ON rel.id = ta.release_id
                 WHERE a.superseded_at_ms IS NULL AND a.source_item_id = (SELECT id FROM source_item WHERE provider_key LIKE 'Pale Engine/Singles/02%')"),
        "Lamplight"
    );
    // Its undo leaves them where the resolution put them.
    let singles = |rig: &Rig| {
        rig.rows(
            "SELECT si.provider_key, a.recording_id, a.appearance_id, ta.release_id
             FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             WHERE si.provider_key LIKE 'Pale Engine/%' ORDER BY 1",
        )
    };
    let settled = singles(&rig);
    undo(&rig, 2, answers).unwrap();
    assert_eq!(singles(&rig), settled);
}

/// Writes the bonus disc's files again with these tags beside their own.
fn retag_the_bonus_disc(rig: &mut Rig, more: &[(&str, &str)]) {
    for (seed, title, sectors, track) in
        [(4, "Glass Harbour", 200, "1"), (5, "Far Lights", 300, "2")]
    {
        let mut tags = vec![
            ("TIT2", title),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", "Tin Weather (Bonus Disc)"),
            ("TRCK", track),
        ];
        tags.extend_from_slice(more);
        let bytes = crate::proposal_tests::aiff(sectors, seed, &tags);
        crate::proposal_tests::write(
            rig,
            &format!("Moss Arcade/Tin Weather Bonus/0{track} {title}.aiff"),
            &bytes,
        );
    }
    rig.scan();
}

#[test]
fn a_yes_that_left_two_releases_apart_does_not_join_them_after_a_retag() {
    let main = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e1";
    let bonus = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e2";
    let mut rig = album_with(&[("TXXX", main)]);
    retag_the_bonus_disc(&mut rig, &[("TXXX", bonus)]);
    // Each release has a date a person gave it.
    edit_each(&rig, "release", "date", &["2004", "2005"]);
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    // Their own release IDs keep them two releases of the album.
    assert_eq!(releases(&rig), ["Tin Weather|1|3", "Tin Weather|1|2"]);
    // The bonus disc retagged with the main disc's release ID: two
    // releases that each carry input of their own, which the yes did not
    // join, stay two.
    retag_the_bonus_disc(&mut rig, &[("TXXX", main)]);
    assert_eq!(releases(&rig), ["Tin Weather|1|3", "Tin Weather|1|2"]);
    assert!(!answers.is_empty());
}

#[test]
fn undoing_an_answer_of_a_batch_leaves_what_another_answer_of_it_moved() {
    let mut rig = album();
    // Another pressing of the album, filed apart and already tagged with
    // its group's ID, which a yes to the album joins.
    let group_tag = format!("MusicBrainz Release Group Id\u{0}{GROUP}");
    let pressing = "Moss Arcade/Alternate Pressing/01 Paper Kites.aiff";
    let audio = |title: &str| {
        crate::proposal_tests::aiff(
            171,
            21,
            &[
                ("TIT2", title),
                ("TPE1", "Moss Arcade"),
                ("TPE2", "Moss Arcade"),
                ("TALB", "Alternate Pressing"),
                ("TRCK", "1"),
                ("TXXX", group_tag.as_str()),
            ],
        )
    };
    crate::proposal_tests::write(&mut rig, pressing, &audio("Paper Kites"));
    // And an album of another artist, its disc looked up too.
    for (track, sectors, title) in [("1", 250, "No Signal"), ("2", 475, "Last Signal")] {
        let bytes = crate::proposal_tests::aiff(
            sectors,
            if track == "1" { 56 } else { 57 },
            &[
                ("TIT2", title),
                ("TPE1", "Pale Engine"),
                ("TPE2", "Pale Engine"),
                ("TALB", "Signals"),
                ("TRCK", track),
            ],
        );
        crate::proposal_tests::write(
            &mut rig,
            &format!("Pale Engine/Signals/0{track}.aiff"),
            &bytes,
        );
    }
    crate::proposal_tests::write(
        &mut rig,
        "Pale Engine/Signals/rip.log",
        b"Exact Audio Copy V1.6\n Track | Start | Length | Start sector | End sector\n  1 | 0:00.00 | 0:01.00 | 0 | 249\n  2 | 0:00.00 | 0:01.00 | 250 | 724\n",
    );
    rig.scan();
    let disc = rig
        .library
        .discs(&ritornel_contract::DiscsParams::default())
        .unwrap()
        .discs
        .into_iter()
        .find(|disc| disc.folder.ends_with("/Signals"))
        .unwrap();
    let signals = "00000000-0000-4000-8000-0000000001b1";
    let body = include_str!("../../../fixtures/musicbrainz/disc-tin-weather-bonus.json")
        .replace("oAA1ZY.4QOAq6tpdobDdb4M9qyc-", &disc.disc_id)
        .replace(GROUP, signals)
        .replace(
            "00000000-0000-4000-8000-0000000000c2",
            "00000000-0000-4000-8000-0000000001c2",
        )
        .replace(
            "00000000-0000-4000-8000-0000000000a1",
            "00000000-0000-4000-8000-0000000001a1",
        )
        .replace("Moss Arcade", "Pale Engine")
        .replace("Tin Weather", "Signals");
    rig.library
        .keep_replies(
            command(10),
            crate::Asked {
                catalogue: "musicbrainz",
                kind: "disc",
                includes: "artist-credits+release-groups+recordings",
            },
            vec![crate::KeptReply {
                key: disc.disc_id,
                status: 200,
                body,
            }],
            vec![],
            |_, _| String::new(),
        )
        .unwrap();
    // The main disc's yes and the other artist's album's, together.
    let batch: Vec<Confirmation> = rig
        .library
        .proposals()
        .unwrap()
        .proposals
        .into_iter()
        .filter(|proposal| {
            proposal.kind == "album" && !proposal.folder.ends_with("/Tin Weather Bonus")
        })
        .map(|proposal| Confirmation {
            proposal_id: proposal.proposal_id,
            release_group: if proposal.folder.ends_with("/Signals") {
                signals
            } else {
                GROUP
            }
            .to_owned(),
            release: None,
            fingerprint: proposal.fingerprint,
        })
        .collect();
    assert_eq!(batch.len(), 2);
    let other = batch
        .iter()
        .position(|yes| yes.release_group == signals)
        .unwrap();
    let answers = confirm(&rig, 11, batch).unwrap();
    // The pressing moved with the main disc's yes alone.
    assert_eq!(
        rig.one(
            "SELECT count(*) FROM answer a, json_each(a.before_json, '$.items') e
             JOIN source_item si ON si.id = json_extract(e.value, '$.item')
             WHERE si.provider_key LIKE 'Moss Arcade/Alternate Pressing/%'"
        ),
        "1"
    );
    // Retitled since, it has rows of its own; undoing the other artist's
    // yes, which never moved it, leaves them.
    crate::proposal_tests::write(&mut rig, pressing, &audio("Paper Route"));
    rig.scan();
    let moss = |rig: &Rig| -> Vec<String> {
        reachable(rig)
            .into_iter()
            .filter(|row| row.starts_with("Moss Arcade/"))
            .collect()
    };
    let joined = moss(&rig);
    undo(&rig, 12, vec![answers[other]]).unwrap();
    assert_eq!(moss(&rig), joined);
}

#[test]
fn an_edition_for_a_disc_added_later_goes_with_the_answers_its_other_discs_rest_on() {
    let mut rig = album();
    let shown = group(&rig);
    let bonus = shown
        .iter()
        .position(|yes| {
            rig.library
                .proposal(yes.proposal_id)
                .unwrap()
                .1
                .folder
                .ends_with("Bonus")
        })
        .unwrap();
    let albums = confirm(&rig, 1, shown).unwrap();
    confirm(&rig, 2, vec![edition(&rig).unwrap()]).unwrap();
    // A third disc of the release, added later and looked up.
    for (track, sectors, title) in [("1", 250, "No Signal"), ("2", 475, "Last Signal")] {
        let bytes = crate::proposal_tests::aiff(
            sectors,
            if track == "1" { 56 } else { 57 },
            &[
                ("TIT2", title),
                ("TPE1", "Moss Arcade"),
                ("TPE2", "Moss Arcade"),
                ("TALB", "Tin Weather"),
                ("TRCK", track),
            ],
        );
        crate::proposal_tests::write(
            &mut rig,
            &format!("Moss Arcade/Disc Three/0{track}.aiff"),
            &bytes,
        );
    }
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Disc Three/rip.log",
        b"Exact Audio Copy V1.6\n Track | Start | Length | Start sector | End sector\n  1 | 0:00.00 | 0:01.00 | 0 | 249\n  2 | 0:00.00 | 0:01.00 | 250 | 724\n",
    );
    rig.scan();
    let disc = rig
        .library
        .discs(&ritornel_contract::DiscsParams::default())
        .unwrap()
        .discs
        .into_iter()
        .find(|disc| disc.folder.ends_with("/Disc Three"))
        .unwrap();
    let mut body: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/musicbrainz/disc-tin-weather-bonus.json"
    ))
    .unwrap();
    body["id"] = serde_json::json!(disc.disc_id);
    let mut third = body["releases"][0]["media"][1].clone();
    third["position"] = serde_json::json!(3);
    third["discs"][0]["id"] = serde_json::json!(disc.disc_id);
    body["releases"][0]["media"]
        .as_array_mut()
        .unwrap()
        .push(third);
    rig.library
        .keep_replies(
            command(10),
            crate::Asked {
                catalogue: "musicbrainz",
                kind: "disc",
                includes: "artist-credits+release-groups+recordings",
            },
            vec![crate::KeptReply {
                key: disc.disc_id,
                status: 200,
                body: body.to_string(),
            }],
            vec![],
            |_, _| String::new(),
        )
        .unwrap();
    let mut third = group(&rig);
    assert_eq!(third.len(), 1);
    third.push(edition(&rig).unwrap());
    let later = confirm(&rig, 3, third).unwrap();
    // Undoing the bonus disc's album undoes the first edition, and the one
    // that put the third disc with it.
    undo(&rig, 4, vec![albums[bonus]]).unwrap();
    let continued = rig.library.answer(later[1]).unwrap();
    assert!(continued.answer.undone_by.is_some(), "{continued:?}");
}

#[test]
fn an_album_said_with_its_edition_does_not_keep_the_edition_s_join_after_it_is_undone() {
    let main = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e1";
    let bonus = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e2";
    let mut rig = album_with(&[("TXXX", main)]);
    retag_the_bonus_disc(&mut rig, &[("TXXX", bonus)]);
    // Each release has a date a person gave it.
    edit_each(&rig, "release", "date", &["2004", "2005"]);
    let mut batch = group(&rig);
    batch.push(edition(&rig).unwrap());
    let answers = confirm(&rig, 1, batch).unwrap();
    assert_eq!(
        releases(&rig),
        ["Tin Weather|2|5"],
        "the edition joins them"
    );
    undo(&rig, 2, vec![answers[2]]).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|1|3", "Tin Weather|1|2"]);
    // A retag to one release ID does not join them by the albums' yeses,
    // which never did.
    retag_the_bonus_disc(&mut rig, &[("TXXX", main)]);
    assert_eq!(releases(&rig), ["Tin Weather|1|3", "Tin Weather|1|2"]);
}

#[test]
fn a_resolution_owed_that_changes_nothing_is_settled_by_the_answer() {
    let mut rig = album();
    // A tag no rule reads, and the scan stops once it has read it.
    let bytes = crate::proposal_tests::aiff(
        75,
        1,
        &[
            ("TIT2", "Paper Kites"),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", "Tin Weather"),
            ("TRCK", "1"),
            ("TXXX", "Mood\u{0}calm"),
        ],
    );
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Tin Weather/01 Paper Kites.aiff",
        &bytes,
    );
    let source = rig.main.clone();
    let cut_off = rig.try_scan(
        &source,
        ScanSettings {
            fail_after_step: Some(1),
            ..ScanSettings::default()
        },
    );
    assert!(matches!(cut_off, Err(Error::FailPoint)), "{cut_off:?}");
    let job = rig.server.jobs(1).unwrap().remove(0);
    rig.server
        .finish_job(job.id, JobState::Cancelled, None)
        .unwrap();
    let pending = |rig: &Rig| {
        rig.one("SELECT count(*) FROM library_setting WHERE key = 'identity.resolution_pending'")
    };
    assert_eq!(pending(&rig), "1", "a resolution is owed");
    // Two releases already apart kept apart: an answer that moves nothing.
    let (main, bonus) = release_ids(&rig);
    rig.library
        .keep_apart(command(1), vec![main, bonus], |_, _, _| String::new())
        .unwrap();
    assert_eq!(pending(&rig), "0", "and made");
}

#[test]
fn a_play_never_counts_for_one_song_heard_as_another_after_a_yes_parts_their_files() {
    let mut rig = album();
    // The bonus disc's first song is titled as the main disc's first: filed
    // apart, the two are taken as one recording.
    let bytes = crate::proposal_tests::aiff(
        200,
        4,
        &[
            ("TIT2", "Paper Kites"),
            ("TPE1", "Moss Arcade"),
            ("TPE2", "Moss Arcade"),
            ("TALB", "Tin Weather (Bonus Disc)"),
            ("TRCK", "1"),
        ],
    );
    crate::proposal_tests::write(
        &mut rig,
        "Moss Arcade/Tin Weather Bonus/01 Glass Harbour.aiff",
        &bytes,
    );
    rig.scan();
    // A scrobble of the main disc's song.
    let account = rig
        .library
        .ensure_service_account("lastfm", "someone")
        .unwrap();
    let batch = ritornel_source::Batch {
        records: vec![ritornel_source::ImportRecord {
            source_key: b"main kites".to_vec(),
            played_at_ms: 1_000,
            track: ritornel_source::TrackText {
                artist: "Moss Arcade".to_owned(),
                album: Some("Tin Weather".to_owned()),
                title: "Paper Kites".to_owned(),
            },
            ids: ritornel_source::Identifiers::default(),
            payload_json: "{}".to_owned(),
        }],
        span: ritornel_source::Span {
            from_ms: 0,
            to_ms: 2_000,
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
    rig.library.match_library_for(account).unwrap();
    // Every bound play's song is the song of the appearance it was heard
    // from, or it was heard from none.
    let mismatched = |rig: &Rig| {
        rig.one(
            "SELECT count(*) FROM play_event pe JOIN track_appearance ta ON ta.id = pe.appearance_id
             JOIN recording r ON r.id = ta.recording_id JOIN recording pr ON pr.id = pe.recording_id
             WHERE r.canonical_id <> pr.canonical_id",
        )
    };
    assert_eq!(mismatched(&rig), "0");
    let played = |rig: &Rig| rig.rows("SELECT recording_id, appearance_id FROM play_event");
    let was = played(&rig);
    // A yes puts the two on one release, where they are two songs: the
    // main disc's file is on a recording of its own, and the scrobble's
    // play counts for the song it is bound to, heard from no appearance of
    // another.
    let answers = confirm(&rig, 1, group(&rig)).unwrap();
    assert_eq!(mismatched(&rig), "0", "after the yes");
    // The same one play, still bound to its recording, heard from none.
    let [before] = was.as_slice() else {
        panic!("one play: {was:?}")
    };
    let (recording, appearance) = before.split_once('|').unwrap();
    assert_ne!(appearance, "-", "heard from the main disc's song before");
    assert_eq!(played(&rig), [format!("{recording}|-")]);
    undo(&rig, 2, answers).unwrap();
    assert_eq!(played(&rig), was, "after its undo");
}

#[test]
fn an_edition_said_first_in_a_batch_and_undone_alone_gives_its_discs_their_releases_back() {
    let main = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e1";
    let bonus = "MusicBrainz Album Id\u{0}00000000-0000-4000-8000-0000000000e2";
    let mut rig = album_with(&[("TXXX", main)]);
    retag_the_bonus_disc(&mut rig, &[("TXXX", bonus)]);
    edit_each(&rig, "release", "date", &["2004", "2005"]);
    // Each file's appearance and release, by public ID.
    let ids = |rig: &Rig| {
        rig.rows(
            "SELECT si.provider_key, hex(ta.public_id), hex(rel.public_id) FROM source_item si
             JOIN assignment a ON a.source_item_id = si.id AND a.superseded_at_ms IS NULL
             JOIN track_appearance ta ON ta.id = a.appearance_id
             JOIN release rel ON rel.id = ta.release_id
             WHERE si.item_kind = 'file' ORDER BY 1",
        )
    };
    let before = ids(&rig);
    // The edition listed before its discs' albums.
    let mut batch = vec![edition(&rig).unwrap()];
    batch.extend(group(&rig));
    let answers = confirm(&rig, 1, batch).unwrap();
    assert_eq!(releases(&rig), ["Tin Weather|2|5"]);
    undo(&rig, 2, vec![answers[0]]).unwrap();
    assert_eq!(
        ids(&rig),
        before,
        "the albums' yeses keep each disc's own release"
    );
}

#[test]
fn an_edition_s_candidate_is_said_no_to_by_its_release_alone() {
    let rig = album();
    let shown = edition(&rig).unwrap();
    let release = shown.release.clone();
    rig.library
        .decline(
            command(1),
            ritornel_contract::DeclineParams {
                idempotency_key: Id::from_bytes([0xe1; 16]),
                proposal_id: shown.proposal_id,
                fingerprint: shown.fingerprint.clone(),
                release_group: None,
                release: release.clone(),
                stop_asking: false,
            },
            |_, _, _| String::new(),
        )
        .unwrap();
    assert_eq!(
        rig.one("SELECT candidate FROM answer_rule WHERE kind = 'not_candidate'"),
        release.unwrap()
    );
}
