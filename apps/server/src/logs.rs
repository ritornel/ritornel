//! Logs: one JSON object a line, in a file a day under `logs/`. Every line
//! carries the boot ID of the run that wrote it. See "Configuration and
//! logging" in docs/internals/server.md.
//!
//! ```text
//! {"ts":"2026-11-15T22:33:02.123Z","level":"info","boot_id":"019a…","target":"ritornel_server::serve","message":"ready","pid":4312}
//! ```
//!
//! Code logs through `tracing`, and this is where those events end up. A
//! secret stays out of a line because its type cannot be printed: the
//! `Debug` of a token or a key is a fixed redaction.

use std::fmt::{Debug, Write as _};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ritornel_engine::{Clock, Id};
use tracing::field::{Field, Visit};
use tracing::span;
use tracing::{Event, Level, Metadata, Subscriber};

use crate::Error;
use crate::error::io_error;
use crate::platform::{close_to_others, create_owner_only};

/// The keys every line has. A field of an event that uses one of these names
/// is written as `field.<name>`.
const RESERVED: [&str; 5] = ["ts", "level", "boot_id", "target", "message"];

/// Sends every `tracing` event in this process, at `level` or more severe,
/// to the day's file in `dir`.
pub(crate) fn init(
    dir: &Path,
    boot_id: Id,
    level: Level,
    clock: Arc<dyn Clock>,
) -> Result<(), Error> {
    let logger = JsonLines::new(dir, boot_id, level, clock)?;
    tracing::subscriber::set_global_default(logger).map_err(|_| Error::LoggerSet)
}

/// Where `tracing` events are written.
///
/// It keeps no spans: an event's own fields are all a line holds beyond the
/// fixed keys. The IDs of a command, a job and a client join them when those
/// exist.
#[derive(Debug)]
struct JsonLines {
    boot_id: String,
    level: Level,
    clock: Arc<dyn Clock>,
    file: Mutex<DailyFile>,
}

impl JsonLines {
    /// Opens the day's file, so a log that cannot be written stops the
    /// server at its start and not silently some time after it.
    fn new(dir: &Path, boot_id: Id, level: Level, clock: Arc<dyn Clock>) -> Result<Self, Error> {
        let mut file = DailyFile {
            dir: dir.to_owned(),
            open: None,
            failing: false,
        };
        let today = utc(clock.now_ms());
        let date = date_of(&today);
        let path = file.path(date);
        file.open(date).map_err(io_error("open", &path))?;
        Ok(Self {
            boot_id: boot_id.to_string(),
            level,
            clock,
            file: Mutex::new(file),
        })
    }
}

impl Subscriber for JsonLines {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // In `tracing`, a more severe level compares as less.
        *metadata.level() <= self.level
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(self.level.into())
    }

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);

        let timestamp = utc(self.clock.now_ms());
        let metadata = event.metadata();
        let line = format!(
            "{{\"ts\":{},\"level\":{},\"boot_id\":{},\"target\":{},\"message\":{}{}}}\n",
            json(&timestamp),
            json(&metadata.level().as_str().to_ascii_lowercase()),
            json(&self.boot_id),
            json(metadata.target()),
            json(&fields.message),
            fields.rest,
        );
        self.file
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .write_or_complain(date_of(&timestamp), &line);
    }

    // Spans are not kept, so every span is the same one.
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// A string as JSON, quoted and escaped.
fn json(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

/// The fields of one event, as the tail of a JSON object.
#[derive(Default)]
struct Fields {
    message: String,
    /// `,"key":value` for each field but the message.
    rest: String,
}

impl Fields {
    fn text(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.value(field, &json(value));
        }
    }

    fn value(&mut self, field: &Field, value: &str) {
        let name = field.name();
        let key = if RESERVED.contains(&name) {
            json(&format!("field.{name}"))
        } else {
            json(name)
        };
        // Writing to a String cannot fail.
        let _ = write!(self.rest, ",{key}:{value}");
    }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.text(field, &format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.text(field, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.value(field, &value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.value(field, &value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.value(field, &value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // JSON has no way to write infinity or not-a-number.
        self.value(field, &serde_json::Value::from(value).to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.text(field, &value.to_string());
    }
}

/// The log file of the current day, in UTC. A line dated another day closes
/// it and opens that day's.
#[derive(Debug)]
struct DailyFile {
    dir: PathBuf,
    open: Option<(String, File)>,
    /// The last line could not be written, and standard error was told.
    failing: bool,
}

impl DailyFile {
    fn path(&self, date: &str) -> PathBuf {
        self.dir.join(format!("server.{date}.jsonl"))
    }

    /// The file for `date`, opened if it is not the one already open.
    fn open(&mut self, date: &str) -> std::io::Result<&mut File> {
        let open = match self.open.take() {
            Some(open) if open.0 == date => open,
            // Another day's file, if there was one, is closed here.
            _ => {
                // A log names the library's folders and files, so only the
                // OS user may read it, and a file that was there already
                // is closed too: it may have been restored open to others.
                let path = self.path(date);
                let mut options = OpenOptions::new();
                options.append(true).create(true);
                create_owner_only(&mut options);
                let file = options.open(&path)?;
                close_to_others(&path)?;
                (date.to_owned(), file)
            }
        };
        Ok(&mut self.open.insert(open).1)
    }

    fn write(&mut self, date: &str, line: &str) -> std::io::Result<()> {
        // One write a line, so lines stay whole.
        self.open(date)?.write_all(line.as_bytes())
    }

    /// Writes the line. A running server has nowhere but standard error to
    /// say that its log is failing, so it says so there, once for each
    /// stretch of failures and not once a line.
    fn write_or_complain(&mut self, date: &str, line: &str) {
        match self.write(date, line) {
            Ok(()) => self.failing = false,
            Err(_) if self.failing => {}
            Err(error) => {
                self.failing = true;
                let path = self.path(date);
                // If standard error is gone too, nothing is left to tell.
                let _ = writeln!(
                    std::io::stderr(),
                    "ritornel-server: cannot write the log {}: {error}",
                    path.display()
                );
            }
        }
    }
}

/// The date of a timestamp from [`utc`]: `2026-11-15`.
fn date_of(timestamp: &str) -> &str {
    timestamp.split_once('T').map_or("", |(date, _)| date)
}

/// Unix milliseconds as UTC, to the millisecond: `2026-11-15T22:33:02.123Z`.
pub(crate) fn utc(ms: i64) -> String {
    let (days, ms_of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    let (hour, minute) = (ms_of_day / 3_600_000, ms_of_day % 3_600_000 / 60_000);
    let (second, milli) = (ms_of_day % 60_000 / 1_000, ms_of_day % 1_000);

    // Days since 1970-01-01 to a civil date, counting years from 1 March so
    // that a leap day is the last day of its year.
    let days = days + 719_468;
    let (era, day_of_era) = (days.div_euclid(146_097), days.rem_euclid(146_097));
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let (month, carry) = if shifted_month < 10 {
        (shifted_month + 3, 0)
    } else {
        (shifted_month - 9, 1)
    };
    let year = year_of_era + era * 400 + carry;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use ritornel_engine::SessionToken;
    use serde_json::{Value, json};
    use tracing::{debug, error, info, warn};

    use super::*;
    use crate::testing::Scratch;

    /// 2026-11-15T22:33:02.123Z.
    const T0: i64 = 1_794_781_982_123;
    const DAY: i64 = 86_400_000;

    #[derive(Debug)]
    struct TestClock(AtomicI64);

    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Runs `body` with its events going to `dir`, and returns the clock so
    /// the body can move it.
    fn logging(dir: &Path, level: Level, body: impl FnOnce(&TestClock)) {
        let clock = Arc::new(TestClock(AtomicI64::new(T0)));
        let logger = JsonLines::new(dir, Id::from_bytes([0xb0; 16]), level, clock.clone()).unwrap();
        tracing::subscriber::with_default(logger, || body(&clock));
    }

    fn lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_line_is_one_json_object_with_the_boot_id_and_the_events_fields() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |_| {
            info!(
                pid = 4312_u32,
                owned = true,
                name = "desk",
                ratio = 0.5,
                "ready"
            );
        });
        assert_eq!(
            lines(&scratch.path().join("server.2026-11-15.jsonl")),
            [json!({
                "ts": "2026-11-15T22:33:02.123Z",
                "level": "info",
                "boot_id": "b0b0b0b0-b0b0-b0b0-b0b0-b0b0b0b0b0b0",
                "target": "ritornel_server::logs::tests",
                "message": "ready",
                "pid": 4312,
                "owned": true,
                "name": "desk",
                "ratio": 0.5,
            })]
        );
    }

    #[test]
    fn text_is_escaped_so_a_line_stays_one_line() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |_| {
            let path = Path::new("/music/\"Live\"\n1999");
            warn!(path = %path.display(), shown = ?path, "a \"quoted\"\nmessage");
        });
        let text = std::fs::read_to_string(scratch.path().join("server.2026-11-15.jsonl")).unwrap();
        assert_eq!(text.lines().count(), 1);
        let [line] = lines(&scratch.path().join("server.2026-11-15.jsonl"))
            .try_into()
            .unwrap();
        assert_eq!(line["level"], "warn");
        assert_eq!(line["message"], "a \"quoted\"\nmessage");
        assert_eq!(line["path"], "/music/\"Live\"\n1999");
        assert_eq!(line["shown"], "\"/music/\\\"Live\\\"\\n1999\"");
    }

    #[test]
    fn a_level_less_severe_than_the_setting_is_not_written() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::WARN, |_| {
            debug!("too quiet");
            info!("still too quiet");
            warn!("heard");
            error!("heard too");
        });
        let written = lines(&scratch.path().join("server.2026-11-15.jsonl"));
        let messages: Vec<_> = written.iter().map(|line| &line["message"]).collect();
        assert_eq!(messages, ["heard", "heard too"]);
    }

    #[test]
    fn each_day_gets_a_file_of_its_own() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |clock| {
            info!("before midnight");
            clock.0.store(T0 + DAY, Ordering::SeqCst);
            info!("the next day");
            info!("and again");
        });
        let first = lines(&scratch.path().join("server.2026-11-15.jsonl"));
        let second = lines(&scratch.path().join("server.2026-11-16.jsonl"));
        assert_eq!((first.len(), second.len()), (1, 2));
        assert_eq!(second[0]["ts"], "2026-11-16T22:33:02.123Z");
        // One run, one boot ID, whichever file a line is in.
        assert_eq!(first[0]["boot_id"], second[1]["boot_id"]);
    }

    #[test]
    fn a_days_log_is_closed_to_other_users_whether_it_is_new_or_was_there() {
        use crate::platform::{open_to_others, path_is_private as private};
        let scratch = Scratch::new();
        let first = scratch.path().join("server.2026-11-15.jsonl");
        let second = scratch.path().join("server.2026-11-16.jsonl");
        // As after a restore that did not keep modes.
        std::fs::write(&first, "").unwrap();
        open_to_others(&first);
        logging(scratch.path(), Level::INFO, |clock| {
            info!("before midnight");
            clock.0.store(T0 + DAY, Ordering::SeqCst);
            info!("the next day");
        });
        assert!(private(&first));
        assert!(private(&second));
    }

    #[test]
    fn a_log_that_cannot_be_opened_stops_the_start() {
        let scratch = Scratch::new();
        // Something that is not a file sits where the day's log goes.
        std::fs::create_dir(scratch.path().join("server.2026-11-15.jsonl")).unwrap();
        let clock = Arc::new(TestClock(AtomicI64::new(T0)));

        let error = JsonLines::new(
            scratch.path(),
            Id::from_bytes([0xb0; 16]),
            Level::INFO,
            clock,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("server.2026-11-15.jsonl"),
            "{error}"
        );
    }

    #[test]
    fn a_day_whose_file_cannot_be_opened_does_not_stop_the_days_after() {
        let scratch = Scratch::new();
        std::fs::create_dir(scratch.path().join("server.2026-11-16.jsonl")).unwrap();
        logging(scratch.path(), Level::INFO, |clock| {
            info!("before midnight");
            clock.0.store(T0 + DAY, Ordering::SeqCst);
            info!("lost: the day's file cannot be opened");
            info!("lost too");
            clock.0.store(T0 + 2 * DAY, Ordering::SeqCst);
            info!("written again");
        });
        assert_eq!(
            lines(&scratch.path().join("server.2026-11-15.jsonl")).len(),
            1
        );
        let after = lines(&scratch.path().join("server.2026-11-17.jsonl"));
        assert_eq!(after[0]["message"], "written again");
    }

    #[test]
    fn a_second_run_on_the_same_day_adds_to_the_file() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |_| info!("first run"));
        logging(scratch.path(), Level::INFO, |_| info!("second run"));
        assert_eq!(
            lines(&scratch.path().join("server.2026-11-15.jsonl")).len(),
            2
        );
    }

    #[test]
    fn a_secret_cannot_be_logged_by_accident() {
        const SENTINEL: &str = "SENTINEL_SESSION_TOKEN_0001";
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |_| {
            let token = SessionToken::new(SENTINEL);
            info!(?token, "issued");
        });
        let text = std::fs::read_to_string(scratch.path().join("server.2026-11-15.jsonl")).unwrap();
        assert!(!text.contains(SENTINEL), "{text}");
        assert!(text.contains("SessionToken(<redacted>)"), "{text}");
    }

    #[test]
    fn a_field_named_like_a_fixed_key_does_not_replace_it() {
        let scratch = Scratch::new();
        logging(scratch.path(), Level::INFO, |_| {
            info!(level = "eleven", ts = 7, "loud");
        });
        let [line] = lines(&scratch.path().join("server.2026-11-15.jsonl"))
            .try_into()
            .unwrap();
        assert_eq!(line["level"], "info");
        assert_eq!(line["field.level"], "eleven");
        assert_eq!(line["field.ts"], 7);
    }

    #[test]
    fn formats_known_instants() {
        assert_eq!(utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(utc(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(utc(951_868_799_999), "2000-02-29T23:59:59.999Z");
        assert_eq!(utc(1_709_251_199_000), "2024-02-29T23:59:59.000Z");
        assert_eq!(utc(1_735_689_600_000), "2025-01-01T00:00:00.000Z");
        assert_eq!(utc(4_102_444_799_999), "2099-12-31T23:59:59.999Z");
        assert_eq!(utc(T0), "2026-11-15T22:33:02.123Z");
    }
}
