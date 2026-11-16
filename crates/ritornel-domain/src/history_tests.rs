//! The history derivation's fixtures: listens made up for the purpose, a
//! few fields each. See "From observations to plays" and "Time" in
//! docs/design/history.md.

use std::collections::HashMap;

use proptest::prelude::*;

use crate::history::*;

/// 2026-11-15 21:00:00 UTC, a Sunday: 16:00 in New York.
const T0: i64 = 1_794_776_400_000;
const BOOT: [u8; 16] = [0xb0; 16];
const MINUTE: i64 = 60_000;

/// A listen of a 4:00 track on endpoint 1, sent in the boot it was heard
/// in and taken in by a hub whose clock agrees with the sender's.
fn listen(observation: i64, key: u8, ms_played: i64) -> Listen {
    let started_at_ms = T0 + observation * 10 * MINUTE;
    let ended_at_ms = started_at_ms + ms_played;
    Listen {
        observation,
        endpoint: 1,
        source_key: vec![key; 16],
        listener: 1,
        source_item: 100,
        received: Received {
            received_at_ms: ended_at_ms,
            sent_mono_ms: Some(5_000_000 + (ended_at_ms - T0)),
            sent_boot: Some(BOOT),
            clock_skew_ms: Some(0),
        },
        zone: Some("America/New_York".to_owned()),
        started_at_ms,
        ended_at_ms,
        started_mono_ms: 5_000_000 + (started_at_ms - T0),
        ended_mono_ms: Some(5_000_000 + (ended_at_ms - T0)),
        boot: BOOT,
        continues: None,
        ms_played,
        track_duration_ms: 240_000,
        end_reason: EndReason::Finished,
        context_kind: ContextKind::Album,
        context_ref: Some([0xa1; 16]),
        shuffle: false,
    }
}

fn bound() -> HashMap<i64, Binding> {
    HashMap::from([(
        100,
        Binding {
            recording: 7,
            appearance: Some(70),
        },
    )])
}

fn derive_with(
    listens: &[Listen],
    corrections: &[Correction],
    periods: &[ZonePeriod],
) -> Vec<Play> {
    derive(&Inputs {
        listens,
        bindings: &bound(),
        corrections,
        zone_periods: periods,
        rule: PlayRule::V1,
    })
}

fn plays(listens: &[Listen]) -> Vec<Play> {
    derive_with(listens, &[], &[])
}

#[test]
fn a_listen_of_40_percent_is_stored_and_does_not_count_and_one_of_60_percent_counts() {
    let mut forty = listen(1, 1, 96_000);
    forty.end_reason = EndReason::Next;
    let sixty = listen(2, 2, 144_000);
    let derived = plays(&[forty, sixty]);
    assert_eq!(derived.len(), 2);
    assert_eq!(
        (derived[0].counts, derived[0].outcome, derived[0].ms_played),
        (false, Outcome::Skipped, 96_000)
    );
    assert_eq!(
        (derived[1].counts, derived[1].outcome, derived[1].ms_played),
        (true, Outcome::Listened, 144_000)
    );
    for play in &derived {
        assert_eq!(play.play_rule_version, 1);
        assert_eq!(play.recording, Some(7));
        assert_eq!(play.appearance, Some(70));
    }
}

#[test]
fn play_rule_1_is_over_30_seconds_and_half_the_track_or_four_minutes() {
    let rule = PlayRule::V1;
    // Thirty seconds or less never counts, played whole or more.
    assert!(!rule.counts(30_000, 30_000));
    assert!(!rule.counts(60_000, 30_000));
    assert!(!rule.counts(25_000, 25_000));
    // Just over thirty seconds counts at half.
    assert!(rule.counts(15_501, 31_000));
    assert!(rule.counts(15_500, 31_000));
    assert!(!rule.counts(15_499, 31_000));
    // Half of a long track is more than four minutes, which is enough.
    assert!(rule.counts(240_000, 1_200_000));
    assert!(!rule.counts(239_999, 1_200_000));
    // A track whose length is not known does not count.
    assert!(!rule.counts(100_000, 0));
}

#[test]
fn a_track_of_30_seconds_or_less_played_whole_does_not_count() {
    let mut short = listen(1, 1, 28_000);
    short.track_duration_ms = 28_000;
    let [play] = plays(&[short]).try_into().unwrap();
    // It finished, and the listener did not move on: judged by "Judge",
    // that is partial.
    assert_eq!((play.counts, play.outcome), (false, Outcome::Partial));
}

#[test]
fn how_a_play_that_does_not_count_ended_says_whether_it_was_a_skip() {
    for (reason, outcome) in [
        (EndReason::Next, Outcome::Skipped),
        (EndReason::Previous, Outcome::Skipped),
        (EndReason::Jump, Outcome::Skipped),
        (EndReason::Replaced, Outcome::Skipped),
        (EndReason::Removed, Outcome::Skipped),
        (EndReason::Stopped, Outcome::Partial),
        (EndReason::Finished, Outcome::Partial),
        (EndReason::Error, Outcome::Partial),
        (EndReason::Shutdown, Outcome::Partial),
        (EndReason::Crash, Outcome::Partial),
    ] {
        let mut short = listen(1, 1, 10_000);
        short.end_reason = reason;
        let [play] = plays(&[short]).try_into().unwrap();
        assert_eq!(play.outcome, outcome, "{reason:?}");
        // One that counts is listened, however it ended.
        let mut long = listen(1, 1, 200_000);
        long.end_reason = reason;
        let [play] = plays(&[long]).try_into().unwrap();
        assert_eq!(play.outcome, Outcome::Listened, "{reason:?}");
    }
}

#[test]
fn the_crash_example_is_one_play_that_counts_dated_at_the_first_listens_start() {
    // history.md, "What a listen contains": a 5:00 track crashes at 2:40
    // with a checkpoint at 2:35. Listen 1 is 155 seconds, ended by crash.
    let mut first = listen(1, 1, 155_000);
    first.track_duration_ms = 300_000;
    first.end_reason = EndReason::Crash;
    // The next run closed it and sent it: another boot, so its time rests
    // on the wall clock.
    first.received = Received {
        received_at_ms: first.ended_at_ms + 30_000,
        sent_mono_ms: Some(1_000),
        sent_boot: Some([0xb1; 16]),
        clock_skew_ms: Some(0),
    };
    // After the restart the track resumes at 2:35 and finishes: listen 2
    // is 145 seconds and continues listen 1.
    let mut second = listen(2, 2, 145_000);
    second.started_at_ms = first.ended_at_ms + 40_000;
    second.ended_at_ms = second.started_at_ms + 145_000;
    second.boot = [0xb1; 16];
    second.started_mono_ms = 2_000;
    second.received = Received {
        received_at_ms: second.ended_at_ms,
        sent_mono_ms: Some(2_000 + 145_000),
        sent_boot: Some([0xb1; 16]),
        clock_skew_ms: Some(0),
    };
    second.track_duration_ms = 300_000;
    second.continues = Some(first.source_key.clone());

    let [play] = plays(&[first.clone(), second.clone()]).try_into().unwrap();
    // The chain has 300 seconds against a threshold of 150.
    assert_eq!(play.ms_played, 300_000);
    assert_eq!((play.counts, play.outcome), (true, Outcome::Listened));
    assert_eq!(
        play.observations,
        [(1, Role::Primary), (2, Role::Corroborating)]
    );
    assert_eq!(play.time.event_at_ms, Some(first.started_at_ms));
    assert_eq!(play.time.clock_basis, ClockBasis::ServerWall);
    // The second listen alone would not have counted.
    assert!(!plays(&[second.clone()])[0].counts);
    // The order the two arrive in changes nothing.
    assert_eq!(plays(&[second, first]), [play]);
}

/// The listen made `by_ms` later, on every clock it was stamped with.
fn later(listen: &Listen, by_ms: i64) -> Listen {
    let mut later = listen.clone();
    later.started_at_ms += by_ms;
    later.ended_at_ms += by_ms;
    later.started_mono_ms += by_ms;
    later.ended_mono_ms = later.ended_mono_ms.map(|end| end + by_ms);
    later.received.received_at_ms += by_ms;
    later.received.sent_mono_ms = later.received.sent_mono_ms.map(|sent| sent + by_ms);
    later
}

#[test]
fn a_continuation_more_than_a_day_later_is_a_play_of_its_own() {
    let mut first = listen(1, 1, 100_000);
    first.end_reason = EndReason::Crash;
    let mut second = listen(2, 2, 100_000);
    second.continues = Some(first.source_key.clone());
    // Moved so that it begins a day and a millisecond after the first ended,
    // and then a day exactly.
    let gap = second.started_at_ms - first.ended_at_ms;
    let day_and_more = later(&second, CHAIN_WINDOW_MS + 1 - gap);
    assert_eq!(plays(&[first.clone(), day_and_more.clone()]).len(), 2);
    // Across a restart of the machine the dates decide, and they do too.
    let mut rebooted = day_and_more;
    rebooted.boot = [0xbb; 16];
    rebooted.received.sent_boot = Some([0xbb; 16]);
    assert_eq!(plays(&[first.clone(), rebooted]).len(), 2);
    let a_day = later(&second, CHAIN_WINDOW_MS - gap);
    assert_eq!(plays(&[first, a_day]).len(), 1);
}

#[test]
fn listens_that_continue_one_another_round_a_loop_are_cut_before_the_earliest() {
    // Two short listens a second apart on two boots, each naming the other
    // as the one it continues: only bad keys make that, and neither may be
    // lost.
    let mut first = listen(1, 1, 500);
    first.boot = [0xc1; 16];
    first.received.sent_boot = Some([0xc1; 16]);
    let mut second = later(&listen(2, 2, 500), -(10 * MINUTE) + 1_000);
    second.boot = [0xc2; 16];
    second.received.sent_boot = Some([0xc2; 16]);
    first.continues = Some(second.source_key.clone());
    second.continues = Some(first.source_key.clone());
    let [play] = plays(&[second.clone(), first.clone()]).try_into().unwrap();
    assert_eq!(
        play.observations,
        [(1, Role::Primary), (2, Role::Corroborating)]
    );
    assert_eq!(plays(&[first, second]), [play]);
}

/// The sender's wall clock is `by_ms` off for this listen and for its send.
fn on_a_wrong_wall_clock(listen: &mut Listen, by_ms: i64) {
    listen.started_at_ms += by_ms;
    listen.ended_at_ms += by_ms;
    listen.received.clock_skew_ms = Some(by_ms);
}

#[test]
fn a_resume_stays_one_play_when_its_senders_wall_clock_was_set_back_an_hour() {
    let mut first = listen(1, 1, 100_000);
    first.end_reason = EndReason::Crash;
    let mut second = listen(2, 2, 140_000);
    second.continues = Some(first.source_key.clone());
    // Sent in the boot it was heard in, so dated by its age: the hour does
    // not move its date, and must not split the chain either.
    on_a_wrong_wall_clock(&mut second, -60 * MINUTE);
    let [play] = plays(&[first.clone(), second.clone()]).try_into().unwrap();
    assert_eq!(play.ms_played, 240_000);
    assert_eq!(play.time.event_at_ms, Some(first.started_at_ms));
    assert_eq!(plays(&[second, first]), [play]);
}

#[test]
fn a_resume_stays_in_the_window_when_its_senders_wall_clock_jumped_two_days_ahead() {
    let mut first = listen(1, 1, 100_000);
    first.end_reason = EndReason::Crash;
    let mut second = listen(2, 2, 140_000);
    second.continues = Some(first.source_key.clone());
    on_a_wrong_wall_clock(&mut second, 2 * CHAIN_WINDOW_MS);
    assert_eq!(plays(&[first, second]).len(), 1);
}

#[test]
fn a_continuation_from_another_endpoint_or_of_a_listen_not_there_joins_nothing() {
    let first = listen(1, 1, 100_000);
    let mut elsewhere = listen(2, 2, 100_000);
    elsewhere.endpoint = 2;
    elsewhere.continues = Some(first.source_key.clone());
    let mut orphan = listen(3, 3, 100_000);
    orphan.continues = Some(vec![0xee; 16]);
    assert_eq!(plays(&[first, elsewhere, orphan]).len(), 3);
}

#[test]
fn of_two_listens_that_continue_the_same_one_the_earlier_joins() {
    let mut first = listen(1, 1, 100_000);
    first.end_reason = EndReason::Crash;
    let mut later = listen(3, 3, 30_000);
    later.continues = Some(first.source_key.clone());
    let mut earlier = listen(2, 2, 30_000);
    earlier.continues = Some(first.source_key.clone());
    let derived = plays(&[later.clone(), first.clone(), earlier.clone()]);
    assert_eq!(derived.len(), 2);
    assert_eq!(
        derived[0].observations,
        [(1, Role::Primary), (2, Role::Corroborating)]
    );
    assert_eq!(derived[1].observations, [(3, Role::Primary)]);
    assert_eq!(derived, plays(&[earlier, first, later]));
}

#[test]
fn three_passes_by_hand_are_three_listens_and_three_plays() {
    let passes: Vec<Listen> = (1..=3)
        .map(|n| listen(n, u8::try_from(n).unwrap(), 240_000))
        .collect();
    let derived = plays(&passes);
    assert_eq!(derived.len(), 3);
    assert!(derived.iter().all(|play| play.counts));
}

#[test]
fn a_listen_whose_source_item_has_no_assignment_gives_an_unbound_play() {
    let mut loose = listen(1, 1, 200_000);
    loose.source_item = 999;
    let [play] = plays(&[loose]).try_into().unwrap();
    assert_eq!((play.recording, play.appearance), (None, None));
    assert_eq!(play.source_item, 999);
    // It still counts, and still has a date.
    assert!(play.counts);
    assert!(play.time.event_at_ms.is_some());
}

#[test]
fn a_sender_whose_wall_clock_is_an_hour_fast_is_dated_by_age_in_the_same_boot() {
    const HOUR: i64 = 60 * MINUTE;
    // The truth: the listen started at T0 and was sent four minutes later,
    // when the hub took it in.
    let truth = T0;
    let mut fast = listen(1, 1, 240_000);
    fast.started_at_ms = truth + HOUR;
    fast.ended_at_ms = truth + HOUR + 240_000;
    fast.started_mono_ms = 9_000;
    fast.received = Received {
        received_at_ms: truth + 240_000,
        sent_mono_ms: Some(9_000 + 240_000),
        sent_boot: Some(BOOT),
        clock_skew_ms: Some(HOUR),
    };
    let [play] = plays(&[fast.clone()]).try_into().unwrap();
    assert_eq!(play.time.event_at_ms, Some(truth));
    assert_eq!(play.time.clock_basis, ClockBasis::HubAge);
    assert_eq!(play.time.precision_ms, Some(HUB_AGE_PRECISION_MS));
    // 16:00 in New York, not 17:00.
    assert_eq!(play.time.local.unwrap().hour, 16);

    // Sent after a restart, the age is not known. The wall clock is used,
    // less the skew measured at the send.
    fast.received.sent_boot = Some([0xb1; 16]);
    let [play] = plays(&[fast.clone()]).try_into().unwrap();
    assert_eq!(play.time.event_at_ms, Some(truth));
    assert_eq!(play.time.clock_basis, ClockBasis::ServerWall);
    // A skew of five seconds or less is let stand.
    fast.received.clock_skew_ms = Some(SKEW_TOLERANCE_MS);
    let [play] = plays(&[fast.clone()]).try_into().unwrap();
    assert_eq!(play.time.event_at_ms, Some(truth + HOUR));
    // A monotonic clock that went back is no age either.
    fast.received.sent_boot = Some(BOOT);
    fast.received.sent_mono_ms = Some(8_999);
    assert_eq!(plays(&[fast])[0].time.clock_basis, ClockBasis::ServerWall);
}

#[test]
fn a_play_that_would_fall_on_the_wrong_day_by_a_fast_clock_does_not() {
    // 23:30 in New York on 15 November. A clock an hour fast says 00:30
    // on the 16th, which dated by age does not matter.
    let truth = T0 + 7 * 60 * MINUTE + 30 * MINUTE;
    let mut late = listen(1, 1, 200_000);
    late.started_at_ms = truth + 60 * MINUTE;
    late.ended_at_ms = late.started_at_ms + 200_000;
    late.started_mono_ms = 1_000;
    late.received = Received {
        received_at_ms: truth + 200_000,
        sent_mono_ms: Some(1_000 + 200_000),
        sent_boot: Some(BOOT),
        clock_skew_ms: Some(60 * MINUTE),
    };
    let [play] = plays(&[late]).try_into().unwrap();
    let local = play.time.local.unwrap();
    assert_eq!(
        (local.date, local.month_day, local.hour),
        (20_261_115, 1115, 23)
    );
}

#[test]
fn a_captured_zone_gives_the_local_date_hour_weekday_and_offset_across_dst() {
    let [play] = plays(&[listen(0, 1, 200_000)]).try_into().unwrap();
    assert_eq!(play.time.zone.as_deref(), Some("America/New_York"));
    assert_eq!(play.time.zone_basis, ZoneBasis::Captured);
    assert_eq!(play.time.calendar_basis, CalendarBasis::Zone);
    assert_eq!(
        play.time.local,
        Some(LocalTime {
            date: 20_261_115,
            month_day: 1115,
            hour: 16,
            weekday: 0,
            offset_minutes: -300,
        })
    );
    // In July the same zone is four hours behind.
    let july = local_time(1_784_000_000_000, "America/New_York").unwrap();
    assert_eq!(july.offset_minutes, -240);
    assert_eq!(local_time(T0, "Not/AZone"), None);
}

#[test]
fn a_zone_that_says_nothing_gives_way_to_the_listeners_zone_for_that_date() {
    let mut in_utc = listen(1, 1, 200_000);
    in_utc.zone = Some("UTC".to_owned());
    let mut no_zone = listen(2, 2, 200_000);
    no_zone.zone = None;
    let periods = [
        ZonePeriod {
            listener: 1,
            zone: "Europe/Lisbon".to_owned(),
            from_ms: None,
            to_ms: Some(T0 + 15 * MINUTE),
        },
        ZonePeriod {
            listener: 1,
            zone: "Asia/Tokyo".to_owned(),
            from_ms: Some(T0 + 15 * MINUTE),
            to_ms: None,
        },
    ];
    let derived = derive_with(&[in_utc.clone(), no_zone.clone()], &[], &periods);
    assert_eq!(derived[0].time.zone.as_deref(), Some("Europe/Lisbon"));
    assert_eq!(derived[0].time.zone_basis, ZoneBasis::Assumed);
    assert_eq!(derived[1].time.zone.as_deref(), Some("Asia/Tokyo"));
    // 21:20 UTC is 06:20 the next morning in Tokyo.
    let local = derived[1].time.local.unwrap();
    assert_eq!((local.date, local.hour), (20_261_116, 6));

    // With no period, a captured UTC stands, and no zone at all gives no
    // local date.
    let derived = plays(&[in_utc, no_zone]);
    assert_eq!(derived[0].time.zone_basis, ZoneBasis::Captured);
    assert_eq!(derived[0].time.local.unwrap().hour, 21);
    assert_eq!(derived[1].time.zone_basis, ZoneBasis::None);
    assert_eq!(derived[1].time.calendar_basis, CalendarBasis::None);
    assert_eq!(derived[1].time.local, None);
    assert!(derived[1].time.event_at_ms.is_some());
    assert!(says_nothing("Etc/UTC") && says_nothing("UTC") && !says_nothing("Europe/London"));
}

#[test]
fn an_exclude_marks_its_play_excluded_and_a_revoke_undoes_it_and_can_be_undone() {
    let mut first = listen(1, 1, 100_000);
    first.end_reason = EndReason::Crash;
    let mut second = listen(2, 2, 100_000);
    second.continues = Some(first.source_key.clone());
    let other = listen(3, 3, 200_000);
    let listens = [first, second, other];
    let exclude = Correction {
        observation: 10,
        verb: Verb::Exclude,
        targets: vec![2],
    };
    let revoke = Correction {
        observation: 11,
        verb: Verb::Revoke,
        targets: vec![10],
    };
    let revoke_the_revoke = Correction {
        observation: 12,
        verb: Verb::Revoke,
        targets: vec![11],
    };
    let flags = |corrections: &[Correction]| -> Vec<bool> {
        derive_with(&listens, corrections, &[])
            .iter()
            .map(|play| play.excluded)
            .collect()
    };
    // The chain holds the target, so the whole play is excluded.
    assert_eq!(flags(std::slice::from_ref(&exclude)), [true, false]);
    assert_eq!(flags(&[exclude.clone(), revoke.clone()]), [false, false]);
    assert_eq!(flags(&[exclude, revoke, revoke_the_revoke]), [true, false]);
}

#[test]
fn values_are_written_as_the_schema_lists_them() {
    let all = |values: Vec<&str>| values.join(",");
    assert_eq!(
        all(EndReason::ALL.iter().map(|value| value.as_str()).collect()),
        "finished,next,previous,jump,replaced,removed,stopped,error,output_lost,shutdown,crash"
    );
    assert_eq!(
        all(StartReason::ALL
            .iter()
            .map(|value| value.as_str())
            .collect()),
        "user_pick,auto_advance,repeat,resume"
    );
    assert_eq!(
        all(Outcome::ALL.iter().map(|value| value.as_str()).collect()),
        "listened,skipped,partial,unknown"
    );
    for reason in EndReason::ALL {
        assert_eq!(EndReason::parse(reason.as_str()), Some(*reason));
    }
    assert_eq!(
        all(ContextKind::ALL
            .iter()
            .map(|value| value.as_str())
            .collect()),
        "manual,album,artist,playlist,query,agent"
    );
    assert_eq!(ContextKind::parse("artist"), Some(ContextKind::Artist));
    assert_eq!(ContextKind::parse("radio"), None);
}

/// Listens on one endpoint, some of them continuing earlier ones.
fn arbitrary_listens() -> impl Strategy<Value = Vec<Listen>> {
    let one = (
        0..400_000_i64,
        proptest::option::of(0..12_u8),
        0..3_i64,
        proptest::bool::ANY,
    );
    proptest::collection::vec(one, 1..12).prop_map(|rows| {
        rows.into_iter()
            .enumerate()
            .map(|(at, (played, continues, item, crashed))| {
                let n = i64::try_from(at).unwrap() + 1;
                let mut made = listen(n, u8::try_from(n).unwrap(), played);
                made.source_item = 100 + item;
                made.continues = continues.map(|key| vec![key; 16]);
                if crashed {
                    made.end_reason = EndReason::Crash;
                }
                made
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn deriving_does_not_depend_on_the_order_listens_arrive_in(
        listens in arbitrary_listens(),
        seed in 0..1000_usize,
    ) {
        let mut shuffled = listens.clone();
        let len = shuffled.len();
        for at in 0..len {
            shuffled.swap(at, (at * 7 + seed) % len);
        }
        prop_assert_eq!(plays(&listens), plays(&shuffled));
    }

    #[test]
    fn every_listen_is_in_exactly_one_play(listens in arbitrary_listens()) {
        let derived = plays(&listens);
        let mut seen: Vec<i64> = derived
            .iter()
            .flat_map(|play| play.observations.iter().map(|(observation, _)| *observation))
            .collect();
        seen.sort_unstable();
        let mut all: Vec<i64> = listens.iter().map(|listen| listen.observation).collect();
        all.sort_unstable();
        prop_assert_eq!(seen, all);
        for play in &derived {
            prop_assert_eq!(play.observations[0].1, Role::Primary);
        }
    }

    #[test]
    fn deriving_a_play_alone_gives_what_deriving_everything_gave_for_it(
        listens in arbitrary_listens(),
    ) {
        let everything = plays(&listens);
        for play in &everything {
            let own: Vec<Listen> = listens
                .iter()
                .filter(|listen| play.observations.iter().any(|(id, _)| *id == listen.observation))
                .cloned()
                .collect();
            prop_assert_eq!(plays(&own), vec![play.clone()]);
        }
    }
}
