//! An opt-in log on stderr for a live run's diagnosis: `TEAMS_HEADLESS_LOG=debug` shows the debug lines of our own
//! crates (`teams_*`: Trouter's packets and deliveries by kind, never a payload); nothing else is shown, and without
//! the variable nothing is logged at all. `log` is capped at `debug` at compile time (`teams-trouter` takes it with
//! `max_level_debug`, and log's features unify), so no crate's `trace!` (tungstenite's frames, the authentication
//! frame's token with them) exists in this binary to log. Each line is redacted by the same final pass as the app's log, then goes through [`visible`].

use std::io::Write as _;

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::print::{now_ms, visible};

/// The variable that turns the log on: `debug` (or `info`, `warn`, `error`).
pub const VARIABLE: &str = "TEAMS_HEADLESS_LOG";

/// Whether a record is shown: only our own crates' (`teams_…` targets), at `level` or above, never a full request,
/// answer or frame (the app's diagnostic log's `teams_wire` records, `teams_log::wire::is_wire`): this log stays by kind
/// and size.
#[must_use]
pub fn shown(metadata: &Metadata<'_>, level: Level) -> bool {
    metadata.level() <= level && metadata.target().starts_with("teams_") && !teams_log::wire::is_wire(metadata.target())
}

/// One log line: `<ms> <level> <target>: <message>`, [`visible`].
#[must_use]
pub fn line(at_ms: i64, record: &Record<'_>) -> String {
    visible(&teams_log::redact::line(&format!(
        "{at_ms} {} {}: {}",
        record.level(),
        record.target(),
        record.args()
    )))
}

/// The log; its level is `log`'s max level, set once in [`init_from_env`].
struct StderrLog;

static LOG: StderrLog = StderrLog;

impl Log for StderrLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        log::max_level().to_level().is_some_and(|level| shown(metadata, level))
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(std::io::stderr().lock(), "{}", line(now_ms(), record));
        }
    }

    fn flush(&self) {}
}

/// Installs the log when [`VARIABLE`] names a level; does nothing otherwise (or when a logger is set already).
pub fn init_from_env() {
    let Some(level) = std::env::var(VARIABLE)
        .ok()
        .and_then(|value| value.parse::<Level>().ok())
    else {
        return;
    };
    if log::set_logger(&LOG).is_ok() {
        log::set_max_level(level.to_level_filter().min(LevelFilter::Debug));
    }
}
