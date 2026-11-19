//! What Settings says, worked out from what the server reports
//! (`settings/words.ts`): times as a person reads them, and one line for a
//! scan, an import and a zone. Pure, so that the tests can hold the
//! wording.
//!
//! The real page writes times through `Intl` in the Mac's own locale and
//! zone. Here the zone is the system's, through `jiff`, and the locale is
//! reduced to the two things these lines show of it: whether the clock has
//! twelve hours or twenty-four, and whether the month comes before the
//! day (`Style`).

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use ritornel_contract::{
    ImportSchedule, Job, JobState, PicturesImported, ScanSummary, WatchState, WatchedFolder,
    WatchedKind, ZonePeriod,
};

use crate::words::thousands;

/// How a locale writes a time and a day, as far as these lines show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Style {
    /// "2:02 PM" rather than "14:02".
    pub hour12: bool,
    /// "Nov 17" rather than "17 Nov".
    pub month_first: bool,
}

impl Style {
    /// British English's, as the tests write.
    #[cfg(test)]
    pub(crate) const BRITISH: Self = Self {
        hour12: false,
        month_first: false,
    };

    /// The style of a BCP 47 language tag, as `Intl` would write it: the
    /// United States put the month first and count twelve hours, as do the
    /// few other places listed; everywhere else, the day first and
    /// twenty-four hours.
    pub(crate) fn of_locale(tag: &str) -> Self {
        let mut parts = tag.split(['-', '_']);
        let language = parts.next().unwrap_or("").to_ascii_lowercase();
        let region = parts
            .find(|part| part.len() == 2 && part.chars().all(|c| c.is_ascii_alphabetic()))
            .map(str::to_ascii_uppercase);
        let region = region.as_deref();
        // A bare "en" is American English, as `Intl` takes it.
        let american = language == "en" && matches!(region, None | Some("US"));
        Self {
            hour12: american
                || matches!(
                    region,
                    Some("CA" | "AU" | "NZ" | "IN" | "PH" | "PK" | "EG" | "SA")
                ),
            month_first: american || (language == "en" && region == Some("PH")),
        }
    }
}

/// How to write times: now, the zone they are read in, and the style.
#[derive(Debug, Clone)]
pub(crate) struct Clock {
    /// Unix milliseconds.
    pub now: i64,
    pub zone: TimeZone,
    pub style: Style,
}

impl Clock {
    /// The day `ms` falls on in the clock's zone.
    fn date(&self, ms: i64) -> Option<Date> {
        Some(
            Timestamp::from_millisecond(ms)
                .ok()?
                .to_zoned(self.zone.clone())
                .date(),
        )
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month(date: Date) -> &'static str {
    MONTHS
        .get(usize::try_from(date.month() - 1).unwrap_or(0))
        .copied()
        .unwrap_or("")
}

/// A day of the month and its month, with its year or not: "17 Nov",
/// "Nov 17, 2025".
fn written_day(date: Date, year: bool, style: Style) -> String {
    match (style.month_first, year) {
        (true, true) => format!("{} {}, {}", month(date), date.day(), date.year()),
        (true, false) => format!("{} {}", month(date), date.day()),
        (false, true) => format!("{} {} {}", date.day(), month(date), date.year()),
        (false, false) => format!("{} {}", date.day(), month(date)),
    }
}

/// A moment, near now: "today at 14:02", "yesterday at 9:15", "17 Nov at
/// 14:02", "17 Nov 2025 at 14:02".
pub(crate) fn when(ms: i64, clock: &Clock) -> String {
    let Ok(at) = Timestamp::from_millisecond(ms) else {
        return String::new();
    };
    let at = at.to_zoned(clock.zone.clone());
    let time = if clock.style.hour12 {
        let hour = match at.hour() % 12 {
            0 => 12,
            hour => hour,
        };
        let half = if at.hour() < 12 { "AM" } else { "PM" };
        // `Intl` puts a narrow space before AM and PM; WebKit writes a
        // plain one, for the pages that read times back.
        format!("{hour}:{:02} {half}", at.minute())
    } else {
        format!("{}:{:02}", at.hour(), at.minute())
    };
    let day = at.date();
    let today = clock.date(clock.now).unwrap_or(day);
    let days = today.until(day).map_or(0, |span| span.get_days());
    match days {
        0 => format!("today at {time}"),
        -1 => format!("yesterday at {time}"),
        1 => format!("tomorrow at {time}"),
        _ => {
            let date = written_day(day, day.year() != today.year(), clock.style);
            format!("{date} at {time}")
        }
    }
}

/// The same, starting a sentence.
pub(crate) fn when_capital(ms: i64, clock: &Clock) -> String {
    capital(&when(ms, clock))
}

fn capital(said: &str) -> String {
    let mut chars = said.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn ended(state: JobState) -> bool {
    state.ended()
}

/// What a scan found, as one line: "4,812 files: 12 new, 3 changed".
pub(crate) fn scan_found(summary: &ScanSummary) -> String {
    let changes = [
        (summary.new, "new"),
        (summary.changed, "changed"),
        (summary.moved, "moved"),
        (summary.returned, "back"),
        (summary.missing, "missing"),
        (summary.unreadable, "unreadable"),
    ];
    let said: Vec<String> = changes
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{} {word}", thousands(*n)))
        .collect();
    let files = format!(
        "{} {}",
        thousands(summary.files),
        if summary.files == 1 { "file" } else { "files" }
    );
    if said.is_empty() {
        format!("{files}, none changed")
    } else {
        format!("{files}: {}", said.join(", "))
    }
}

/// How a scan that is running is getting on: "Reading 1,234 of 5,000 files".
pub(crate) fn scan_progress(job: &Job) -> String {
    match (job.state, job.total) {
        (JobState::Queued, _) => "Waiting to start".to_owned(),
        (JobState::Cancelling, _) => "Stopping".to_owned(),
        (_, None) => "Looking for files".to_owned(),
        (_, Some(total)) => format!(
            "Reading {} of {} files",
            thousands(job.done),
            thousands(total)
        ),
    }
}

/// How a scan that has ended came out, as one line.
pub(crate) fn scan_ended(job: &Job, clock: &Clock) -> String {
    let at = job
        .finished_at
        .map_or(String::new(), |at| format!(" {}", when(at, clock)));
    match job.state {
        JobState::Failed => format!(
            "The last scan failed{at}{}",
            job.error
                .as_ref()
                .map_or(String::new(), |error| format!(": {error}"))
        ),
        JobState::Cancelled => format!("The last scan was stopped{at}"),
        _ => format!(
            "Last scanned{at}{}",
            job.scan
                .as_ref()
                .map_or(String::new(), |scan| format!(". {}", scan_found(scan)))
        ),
    }
}

/// How changes to a folder are noticed: "Watching", "Polling (no events)", "Offline".
pub(crate) fn watch_state(folder: &WatchedFolder) -> String {
    match folder.state {
        WatchState::Watching => "Watching".to_owned(),
        WatchState::Offline => "Offline".to_owned(),
        WatchState::Polling => match &folder.detail {
            Some(detail) => format!("Polling ({detail})"),
            None => "Polling".to_owned(),
        },
    }
}

/// The last rescan or import a folder had on its own: "Rescanned on its own today at 1:40".
pub(crate) fn last_auto(folder: &WatchedFolder, clock: &Clock) -> Option<String> {
    let run = folder.last_auto.as_ref()?;
    let music = folder.kind == WatchedKind::Music;
    let did = if music { "Rescanned" } else { "Imported" };
    let at = when(run.finished_at.unwrap_or(run.started_at), clock);
    Some(match run.state {
        JobState::Failed => format!("{did} on its own {at}, and failed"),
        JobState::Queued | JobState::Running => {
            let doing = if music { "Rescanning" } else { "Importing" };
            format!("{doing} on its own")
        }
        _ => format!("{did} on its own {at}"),
    })
}

/// What an import of pictures found: "182 of 240 artists have a picture".
pub(crate) fn pictures_found(imported: &PicturesImported) -> String {
    let pictured = imported.by_mbid + imported.by_name;
    let mut said = format!(
        "{} of {} artists have a picture",
        thousands(pictured),
        thousands(imported.artists)
    );
    if imported.unreadable > 0 {
        said.push_str(&format!(
            "; {} could not be read",
            thousands(imported.unreadable)
        ));
    }
    said
}

/// When the history is next imported: "Today at 20:02", "Importing now",
/// or why only by hand.
pub(crate) fn next_import(schedule: &ImportSchedule, clock: &Clock) -> String {
    if schedule.running {
        return "Importing now".to_owned();
    }
    if let Some(off) = &schedule.off {
        return format!("Only by hand: {off}");
    }
    match schedule.next {
        Some(next) => when_capital(next, clock),
        None => "Not planned".to_owned(),
    }
}

/// How the last import of the history came out.
pub(crate) fn last_import(schedule: &ImportSchedule, clock: &Clock) -> String {
    let Some(last) = &schedule.last else {
        return "Never".to_owned();
    };
    let at = when_capital(last.ended, clock);
    match last.state {
        JobState::Failed => format!(
            "{at}, failed{}",
            last.failure
                .as_ref()
                .map_or(String::new(), |failure| format!(": {failure}"))
        ),
        JobState::Cancelled => format!("{at}, stopped"),
        _ => at,
    }
}

/// A day as `YYYY-MM-DD`, written for a person: "1 Jun 2009".
pub(crate) fn day(date: &str, style: Style) -> String {
    match date.parse::<Date>() {
        Ok(parsed) => written_day(parsed, true, style),
        Err(_) => date.to_owned(),
    }
}

/// The day before `YYYY-MM-DD`.
pub(crate) fn day_before(date: &str) -> String {
    date.parse::<Date>()
        .ok()
        .and_then(|parsed| parsed.yesterday().ok())
        .map_or_else(|| date.to_owned(), |before| before.to_string())
}

/// The days a zone holds: "Always", "From 1 Jun 2009", "Up to 31 May
/// 2009", "1 Jun 2009 to 31 May 2010".
pub(crate) fn zone_span(period: &ZonePeriod, style: Style) -> String {
    let from = period.from.as_deref().map(|from| day(from, style));
    let to = period
        .until
        .as_deref()
        .map(|until| day(&day_before(until), style));
    match (from, to) {
        (None, None) => "Always".to_owned(),
        (Some(from), None) => format!("From {from}"),
        (None, Some(to)) => format!("Up to {to}"),
        (Some(from), Some(to)) => format!("{from} to {to}"),
    }
}

/// Today in the clock's zone, as `YYYY-MM-DD`.
pub(crate) fn today(clock: &Clock) -> String {
    clock
        .date(clock.now)
        .map_or_else(String::new, |date| date.to_string())
}

/// A zone's name as the list writes it: "America/New York".
pub(crate) fn zone_name(zone: &str) -> String {
    zone.replace('_', " ")
}

/// The zone the system is in, by its IANA name.
pub(crate) fn system_zone() -> String {
    TimeZone::system()
        .iana_name()
        .map_or_else(|| "UTC".to_owned(), str::to_owned)
}

/// Every zone a person may choose, by its IANA name, sorted: the
/// canonical ones `Intl.supportedValuesOf('timeZone')` lists, without the
/// database's old aliases.
pub(crate) fn all_zones() -> Vec<String> {
    const AREAS: [&str; 10] = [
        "Africa/",
        "America/",
        "Antarctica/",
        "Arctic/",
        "Asia/",
        "Atlantic/",
        "Australia/",
        "Europe/",
        "Indian/",
        "Pacific/",
    ];
    let mut zones: Vec<String> = jiff::tz::db()
        .available()
        .map(|name| name.as_str().to_owned())
        .filter(|name| AREAS.iter().any(|area| name.starts_with(area)) || name == "UTC")
        .collect();
    zones.sort();
    zones.dedup();
    zones
}

/// The last part of a path, as a root's name.
pub(crate) fn folder_name(path: &str, otherwise: &str) -> String {
    let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    if name.is_empty() {
        otherwise.to_owned()
    } else {
        name.chars().take(100).collect()
    }
}

/// `text` broken into lines where WebKit would break it, not where GPUI's
/// rules of Unicode would: a path wraps whole rather than at its folders
/// (a word joiner holds each slash to what follows), and may start a line
/// after a space (a zero-width space lets it). Neither takes any room.
pub(crate) fn unbroken_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && out.ends_with(' ') {
            out.push('\u{200b}');
        }
        out.push(c);
        if c == '/' && chars.peek().is_some_and(|next| next.is_alphanumeric()) {
            out.push('\u{2060}');
        }
    }
    out
}

/// Whether a job has ended.
pub(crate) fn is_ended(job: &Job) -> bool {
    ended(job.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornel_contract::{AutoRun, AutoTrigger, Id, LastImport, Receipt, ReceiptStatus};

    /// 17 November 2026, 15:00 in London.
    fn clock() -> Clock {
        Clock {
            now: utc(2026, 11, 17, 15, 0),
            zone: TimeZone::get("Europe/London").unwrap_or(TimeZone::UTC),
            style: Style::BRITISH,
        }
    }

    fn utc(year: i16, month: i8, day: i8, hour: i8, minute: i8) -> i64 {
        jiff::civil::date(year, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned(TimeZone::UTC)
            .map_or(0, |zoned| zoned.timestamp().as_millisecond())
    }

    fn id(n: u8) -> Id {
        format!("00000000-0000-0000-0000-0000000000{n:02}")
            .parse()
            .unwrap_or_else(|_| Id::new_v7(0, [0; 10]))
    }

    fn job(state: JobState) -> Job {
        Job {
            job_id: id(1),
            kind: "scan".to_owned(),
            target: None,
            state,
            done: 0,
            total: None,
            bytes: 0,
            failed: 0,
            skipped: 0,
            label: None,
            error: None,
            created_at: 0,
            started_at: None,
            finished_at: None,
            scan: None,
            sync: None,
        }
    }

    #[test]
    fn says_a_time_by_the_day_it_falls_on() {
        let clock = clock();
        assert_eq!(when(utc(2026, 11, 17, 14, 2), &clock), "today at 14:02");
        assert_eq!(when(utc(2026, 11, 16, 9, 15), &clock), "yesterday at 9:15");
        assert_eq!(when(utc(2026, 11, 18, 2, 2), &clock), "tomorrow at 2:02");
        assert_eq!(when(utc(2026, 3, 3, 12, 0), &clock), "3 Mar at 12:00");
        assert_eq!(
            when(utc(2025, 11, 17, 12, 0), &clock),
            "17 Nov 2025 at 12:00"
        );
    }

    #[test]
    fn writes_an_american_time_and_day_as_intl_does() {
        let clock = Clock {
            style: Style::of_locale("en-US"),
            ..clock()
        };
        assert_eq!(when(utc(2026, 11, 17, 14, 2), &clock), "today at 2:02 PM");
        assert_eq!(
            when(utc(2025, 11, 17, 0, 5), &clock),
            "Nov 17, 2025 at 12:05 AM"
        );
        assert_eq!(Style::of_locale("en-GB"), Style::BRITISH);
        assert_eq!(Style::of_locale("de-DE"), Style::BRITISH);
        assert!(Style::of_locale("en").month_first);
    }

    #[test]
    fn says_what_a_scan_changed_and_leaves_out_what_it_did_not() {
        let found = ScanSummary {
            files: 4812,
            new: 12,
            changed: 3,
            ..ScanSummary::default()
        };
        assert_eq!(scan_found(&found), "4,812 files: 12 new, 3 changed");
        let one = ScanSummary {
            files: 1,
            unchanged: 1,
            ..ScanSummary::default()
        };
        assert_eq!(scan_found(&one), "1 file, none changed");
        let ended = Job {
            finished_at: Some(utc(2026, 11, 17, 14, 2)),
            scan: Some(ScanSummary {
                files: 20,
                missing: 2,
                ..ScanSummary::default()
            }),
            ..job(JobState::Succeeded)
        };
        assert_eq!(
            scan_ended(&ended, &clock()),
            "Last scanned today at 14:02. 20 files: 2 missing"
        );
        let failed = Job {
            error: Some("the folder is gone".to_owned()),
            ..job(JobState::Failed)
        };
        assert_eq!(
            scan_ended(&failed, &clock()),
            "The last scan failed: the folder is gone"
        );
    }

    #[test]
    fn counts_the_artists_with_a_picture() {
        let imported = PicturesImported {
            receipt: Receipt {
                command_id: id(2),
                status: ReceiptStatus::Done,
                revision: None,
            },
            artists: 240,
            by_mbid: 150,
            by_name: 32,
            without: 58,
            unreadable: 0,
            dropped: 0,
            ambiguous: 0,
        };
        assert_eq!(
            pictures_found(&imported),
            "182 of 240 artists have a picture"
        );
        let unread = PicturesImported {
            unreadable: 3,
            ..imported
        };
        assert_eq!(
            pictures_found(&unread),
            "182 of 240 artists have a picture; 3 could not be read"
        );
    }

    #[test]
    fn says_when_the_history_was_and_will_be_imported() {
        let clock = clock();
        assert_eq!(last_import(&ImportSchedule::default(), &clock), "Never");
        let failed = ImportSchedule {
            last: Some(LastImport {
                ended: utc(2026, 11, 17, 9, 0),
                state: JobState::Failed,
                failure: Some("Last.fm did not answer".to_owned()),
            }),
            ..ImportSchedule::default()
        };
        assert_eq!(
            last_import(&failed, &clock),
            "Today at 9:00, failed: Last.fm did not answer"
        );
        let running = ImportSchedule {
            running: true,
            ..ImportSchedule::default()
        };
        assert_eq!(next_import(&running, &clock), "Importing now");
        let off = ImportSchedule {
            off: Some("no zones".to_owned()),
            ..ImportSchedule::default()
        };
        assert_eq!(next_import(&off, &clock), "Only by hand: no zones");
        let next = ImportSchedule {
            next: Some(utc(2026, 11, 17, 21, 0)),
            ..ImportSchedule::default()
        };
        assert_eq!(next_import(&next, &clock), "Today at 21:00");
    }

    #[test]
    fn says_how_a_folder_is_watched_and_what_it_last_had_on_its_own() {
        let clock = clock();
        let folder = WatchedFolder {
            name: "music".to_owned(),
            kind: WatchedKind::Music,
            state: WatchState::Watching,
            detail: None,
            pending_since: None,
            last_auto: None,
        };
        assert_eq!(watch_state(&folder), "Watching");
        let polling = WatchedFolder {
            state: WatchState::Polling,
            detail: Some("no events".to_owned()),
            ..folder.clone()
        };
        assert_eq!(watch_state(&polling), "Polling (no events)");
        let offline = WatchedFolder {
            state: WatchState::Offline,
            ..folder.clone()
        };
        assert_eq!(watch_state(&offline), "Offline");
        assert_eq!(last_auto(&folder, &clock), None);
        let run = AutoRun {
            trigger: AutoTrigger::Change,
            started_at: utc(2026, 11, 17, 13, 40),
            finished_at: None,
            state: JobState::Succeeded,
            job_id: None,
        };
        let rescanned = WatchedFolder {
            last_auto: Some(AutoRun {
                finished_at: Some(utc(2026, 11, 17, 13, 41)),
                ..run.clone()
            }),
            ..folder.clone()
        };
        assert_eq!(
            last_auto(&rescanned, &clock).as_deref(),
            Some("Rescanned on its own today at 13:41")
        );
        let pictures = WatchedFolder {
            kind: WatchedKind::Pictures,
            last_auto: Some(run.clone()),
            ..folder.clone()
        };
        assert_eq!(
            last_auto(&pictures, &clock).as_deref(),
            Some("Imported on its own today at 13:40")
        );
        let running = WatchedFolder {
            last_auto: Some(AutoRun {
                state: JobState::Running,
                ..run
            }),
            ..folder
        };
        assert_eq!(
            last_auto(&running, &clock).as_deref(),
            Some("Rescanning on its own")
        );
    }

    #[test]
    fn says_the_days_a_zone_holds_the_last_of_them_included() {
        let style = Style::BRITISH;
        let period = |zone: &str, from: Option<&str>, until: Option<&str>| ZonePeriod {
            zone: zone.to_owned(),
            from: from.map(str::to_owned),
            until: until.map(str::to_owned),
        };
        assert_eq!(day_before("2009-06-01"), "2009-05-31");
        assert_eq!(
            zone_span(&period("Europe/London", None, None), style),
            "Always"
        );
        assert_eq!(
            zone_span(&period("Europe/Helsinki", Some("2009-06-01"), None), style),
            "From 1 Jun 2009"
        );
        assert_eq!(
            zone_span(&period("Europe/London", None, Some("2009-06-01")), style),
            "Up to 31 May 2009"
        );
        assert_eq!(
            zone_span(
                &period("Asia/Tokyo", Some("2009-06-01"), Some("2010-06-01")),
                style
            ),
            "1 Jun 2009 to 31 May 2010"
        );
    }

    #[test]
    fn names_a_folder_after_the_last_part_of_its_path() {
        assert_eq!(folder_name("/Users/pat/Music", "Music"), "Music");
        assert_eq!(folder_name("/Volumes/Archive/FLAC/", "Music"), "FLAC");
        assert_eq!(folder_name("/", "Music"), "Music");
        let long = format!("/x/{}", "n".repeat(140));
        assert_eq!(folder_name(&long, "Music").chars().count(), 100);
    }

    #[test]
    fn keeps_a_path_whole_where_webkit_would() {
        assert_eq!(unbroken_paths("at /a/b"), "at \u{200b}/\u{2060}a/\u{2060}b");
        assert_eq!(unbroken_paths("1/2 and / x"), "1/\u{2060}2 and \u{200b}/ x");
    }

    #[test]
    fn lists_the_zones_a_person_may_choose() {
        let zones = all_zones();
        assert!(zones.iter().any(|zone| zone == "Europe/London"));
        assert!(zones.iter().any(|zone| zone == "America/New_York"));
        assert!(!zones.iter().any(|zone| zone.starts_with("US/")));
        assert_eq!(zone_name("America/New_York"), "America/New York");
    }
}
