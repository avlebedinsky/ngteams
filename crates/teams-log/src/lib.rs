//! `teams-log`: the app's diagnostic log (M2 Task 24a, `docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).
//!
//! One logger of our own on the `log` facade (no `tracing` subscriber), installed once by the app ([`install`])
//! before its engine starts: `<data_dir>/logs/teams-app.log` (the retained file name), private and rotated by size,
//! written from a thread of its own ([`file`]).
//!
//! - **Only our crates**: a record reaches the file only when its target is one of our crates' (`teams_…` and the
//!   app's `ngteams`, [`ours`]); a third-party crate's `log` line, and the UI kit's `tracing` events (which `tracing` forwards to `log` when no
//!   subscriber is set), never do, at any level. `log` is capped at `debug` at compile time (`max_level_debug`), so no
//!   crate's `trace!` (tungstenite's frames, the authentication's token with them) exists in the binary.
//! - **Levels** ([`Level`]): `info` by default, the settings file's `log_level`, changed live ([`apply_setting`]),
//!   [`ENV`] (`TEAMS_APP_LOG`) overriding it; `trace` is taken as `debug`. At `info` the log carries no payload: no
//!   body, query, header value, message text or display name; at `debug` our transport and auth clients add every
//!   request and answer in full, and the socket adds every Trouter frame both ways, as typed records ([`wire`]).
//! - **Redaction is the logger's** ([`redact`]): the records' formatting redacts credentials by name, and every line,
//!   any record's, passes a last pattern pass. Auth exchanges also mask their known credential values. A panic is
//!   written the same way ([`install`]'s hook): redacted, with its location, thread and a backtrace, flushed before
//!   the previous hook runs.
//! - **"Copy diagnostics"** ([`diagnostics`]): the version, the level and the log's last lines, said plainly to be
//!   unsafe to paste when a `debug` line is among them.

pub mod diagnostics;
pub mod file;
pub mod redact;
pub mod wire;

use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

pub use file::Rotation;

/// The variable that overrides the settings' level: `error`, `warn`, `info`, `debug` (`trace` is `debug`).
pub const ENV: &str = "TEAMS_APP_LOG";

/// The app's log file's name.
pub const FILE_NAME: &str = "teams-app.log";

/// The target of the logger's own lines (a panic, dropped lines).
pub const TARGET: &str = "teams_log";

/// How much the log writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Error,
    Warn,
    /// What happened, by kind and id: no payload.
    #[default]
    Info,
    /// Every request and answer in full and every frame both ways, redacted.
    Debug,
}

impl Level {
    pub const ALL: [Self; 4] = [Self::Error, Self::Warn, Self::Info, Self::Debug];

    /// Its name in the settings file and in [`ENV`].
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// What the settings view calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "Errors",
            Self::Warn => "Warnings",
            Self::Info => "Info",
            Self::Debug => "Debug (full requests and responses)",
        }
    }

    /// A level by its name, in any case, spaces around it ignored; `trace` is `debug`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        if text == "trace" {
            return Some(Self::Debug);
        }
        Self::ALL.into_iter().find(|level| level.key() == text)
    }

    /// `log`'s filter for it.
    #[must_use]
    pub fn filter(self) -> LevelFilter {
        match self {
            Self::Error => LevelFilter::Error,
            Self::Warn => LevelFilter::Warn,
            Self::Info => LevelFilter::Info,
            Self::Debug => LevelFilter::Debug,
        }
    }

    fn from_u8(value: u8) -> Self {
        Self::ALL.get(usize::from(value)).copied().unwrap_or_default()
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Error => 0,
            Self::Warn => 1,
            Self::Info => 2,
            Self::Debug => 3,
        }
    }
}

/// The level that applies: [`ENV`]'s value when it names a level, else the setting's.
#[must_use]
pub fn effective(setting: Level, env: Option<&str>) -> Level {
    env.and_then(Level::parse).unwrap_or(setting)
}

/// Whether a record is one of our crates' (`teams_…` targets, and the app's own `ngteams`): the only ones the file
/// takes.
#[must_use]
pub fn ours(metadata: &Metadata<'_>) -> bool {
    // the app crate's own lines too (its quit and account paths), never a crate merely named like it
    let target = metadata.target();
    target.starts_with("teams_") || target == "ngteams" || target.starts_with("ngteams::")
}

/// One line of the file: `<UTC time> <LEVEL> <target>: <message>`, the message passed through the last redaction
/// pass ([`redact::line`]) and written on one line ([`redact::one_line`]).
#[must_use]
pub fn format_line(at: SystemTime, level: log::Level, target: &str, message: &str) -> String {
    let message = redact::one_line(&redact::line(message));
    format!("{} {level:<5} {target}: {message}", utc(at))
}

/// `at` as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn utc(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or_default();
    let of_day = seconds % 86_400;
    // days to a civil date (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let mut text = String::with_capacity(24);
    let _ = write!(
        text,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60,
        since.subsec_millis()
    );
    text
}

/// What the log is installed with.
#[derive(Clone, Debug)]
pub struct Config {
    /// The log's directory (the lab's `<data_dir>/logs/<alias>`).
    pub dir: PathBuf,
    /// The settings file's level.
    pub setting: Level,
    /// [`ENV`]'s value, if it is set.
    pub env: Option<String>,
    pub rotation: Rotation,
}

impl Config {
    /// A log in `dir` at the settings' `setting`, [`ENV`] read from the environment, rotated by default.
    #[must_use]
    pub fn from_env(dir: PathBuf, setting: Level) -> Self {
        Self {
            dir,
            setting,
            env: std::env::var(ENV).ok(),
            rotation: Rotation::default(),
        }
    }
}

/// The logger: the file's sink and the level that applies now.
#[derive(Debug)]
pub struct FileLog {
    sink: file::Sink,
    level: AtomicU8,
    /// [`ENV`]'s level, which the settings can't change.
    overridden: Option<Level>,
}

impl FileLog {
    /// Opens the log's file in `config.dir` ([`file::Sink::open`]) at the level that applies ([`effective`]).
    ///
    /// # Errors
    /// When the file can't be opened (see [`file::Sink::open`]).
    pub fn open(config: &Config) -> io::Result<Self> {
        let sink = file::Sink::open(&config.dir, FILE_NAME, config.rotation)?;
        Ok(Self::with_sink(sink, config))
    }

    /// The logger over a sink opened already.
    #[must_use]
    pub fn with_sink(sink: file::Sink, config: &Config) -> Self {
        let overridden = config.env.as_deref().and_then(Level::parse);
        let level = overridden.unwrap_or(config.setting);
        Self {
            sink,
            level: AtomicU8::new(level.as_u8()),
            overridden,
        }
    }

    /// The level that applies now.
    #[must_use]
    pub fn level(&self) -> Level {
        Level::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// [`ENV`]'s level, when it overrides the settings'.
    #[must_use]
    pub fn overridden(&self) -> Option<Level> {
        self.overridden
    }

    /// Takes the settings' level, unless [`ENV`] overrides it; the level that applies afterwards.
    pub fn apply_setting(&self, setting: Level) -> Level {
        let level = self.overridden.unwrap_or(setting);
        self.level.store(level.as_u8(), Ordering::Relaxed);
        level
    }

    /// The file being written.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.sink.path()
    }

    /// Lines dropped so far because the writer fell behind.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.sink.dropped()
    }

    /// Waits for every line handed over so far to be written (up to [`file::FLUSH_WAIT`]); whether it was.
    #[must_use]
    pub fn flush_written(&self) -> bool {
        self.sink.flush()
    }

    /// Writes a line of the logger's own, waiting for room in the channel (a panic's).
    fn write_waiting(&self, level: log::Level, message: &str) {
        self.sink
            .push_waiting(format_line(SystemTime::now(), level, TARGET, message));
    }
}

impl Log for FileLog {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        ours(metadata) && metadata.level() <= self.level().filter()
    }

    fn log(&self, record: &Record<'_>) {
        if self.enabled(record.metadata()) {
            let message = record.args().to_string();
            self.sink.push(format_line(
                SystemTime::now(),
                record.level(),
                record.target(),
                &message,
            ));
        }
    }

    fn flush(&self) {
        let _ = self.sink.flush();
    }
}

/// Why the log wasn't installed; the app runs without it.
#[derive(Debug)]
pub enum InstallError {
    /// The log's file can't be opened.
    Open { dir: PathBuf, source: io::Error },
    /// A logger is installed already (another `install`).
    AlreadyInstalled,
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open { dir, source } => write!(
                f,
                "the diagnostic log can't be opened in {} ({source}): the app runs without it",
                dir.display()
            ),
            Self::AlreadyInstalled => f.write_str("a logger is installed already: the diagnostic log isn't"),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open { source, .. } => Some(source),
            Self::AlreadyInstalled => None,
        }
    }
}

/// The logger installed ([`install`]).
static INSTALLED: OnceLock<&'static FileLog> = OnceLock::new();

/// Installs the log for the process (see the crate docs): its file opened, `log`'s logger and its level set, and the
/// panic hook that writes a panic (redacted), its location, its thread and a backtrace, flushes, then runs the
/// previous hook. Once per process.
///
/// # Errors
/// [`InstallError::Open`] when the file can't be opened, [`InstallError::AlreadyInstalled`] when a logger is set
/// already; the app runs without the log either way.
pub fn install(config: &Config) -> Result<&'static FileLog, InstallError> {
    if INSTALLED.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    let opened = FileLog::open(config).map_err(|source| InstallError::Open {
        dir: config.dir.clone(),
        source,
    })?;
    let logger: &'static FileLog = Box::leak(Box::new(opened));
    log::set_logger(logger).map_err(|_| InstallError::AlreadyInstalled)?;
    if INSTALLED.set(logger).is_err() {
        return Err(InstallError::AlreadyInstalled);
    }
    log::set_max_level(logger.level().filter());
    install_panic_hook(logger);
    log::info!(
        target: TARGET,
        "log opened at level {} ({})",
        logger.level().key(),
        if logger.overridden().is_some() { "TEAMS_APP_LOG" } else { "the settings" }
    );
    Ok(logger)
}

/// The installed log, if [`install`] installed one.
#[must_use]
pub fn installed() -> Option<&'static FileLog> {
    INSTALLED.get().copied()
}

/// Takes the settings' level live (the settings view's change), unless [`ENV`] overrides it; the level that applies
/// afterwards, or `None` when no log is installed.
#[must_use = "the level that applies may not be the one asked for"]
pub fn apply_setting(setting: Level) -> Option<Level> {
    let logger = installed()?;
    let level = logger.apply_setting(setting);
    log::set_max_level(level.filter());
    log::info!(target: TARGET, "log level now {}", level.key());
    Some(level)
}

/// The panic's message, as text.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(a panic without a message)".to_owned())
}

fn install_panic_hook(logger: &'static FileLog) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let location = info
            .location()
            .map_or_else(|| "an unknown place".to_owned(), ToString::to_string);
        let backtrace = std::backtrace::Backtrace::force_capture();
        logger.write_waiting(
            log::Level::Error,
            &format!(
                "panic on thread {} at {location}: {}\nbacktrace:\n{backtrace}",
                thread.name().unwrap_or("(unnamed)"),
                panic_message(info)
            ),
        );
        let _ = logger.flush_written();
        previous(info);
    }));
}
