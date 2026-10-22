//! The app's settings, kept in a TOML file of the account's own: the lab build's at `<data_dir>/ui/<alias>.toml`
//! (`teams_fence::lab::LabPaths::ui_settings_file`), the personal build's in its own data dir.
//!
//! - **What**: the chat shown last ([`Settings::selected_chat`]), whether typing and read receipts are sent, which
//!   messages notify ([`Notifications`]), the idle time before the account shows away, the account's manual status
//!   ([`ManualStatus`]). Tasks 13 and 14 and Tasks 18–20 act on the ones this task only keeps.
//! - **Forgiving to read**: a missing file gives the defaults; a file that can't be read or isn't TOML gives the
//!   defaults and is reported, and is never written over: changes hold in memory until the app quits, so the file
//!   is there to mend by hand; a value that is bad (of the wrong type, out of range, a name this build doesn't know)
//!   is reported and that one setting keeps its default, the others are taken; a key that names no setting is
//!   reported and ignored. Every problem is a [`Problem`], shown by the settings view.
//! - **Private to write**: the file is `0600` and its directory `0700`, written whole to a temporary file beside it
//!   and renamed over it, so a crash leaves the old file or the new one. A file found readable by others is tightened
//!   when it is read.
//!
//! [`SettingsModel`] is the GPUI entity the views share: a change goes to the file at once (unless it couldn't be
//! read).

use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::io::{self, Read as _, Write as _};
use std::ops::RangeInclusive;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use gpui::{ClipboardItem, Context};
pub use teams_log::Level as LogLevel;
use teams_log::diagnostics::{Report, Source as DiagnosticsSource};
use toml::{Table, Value};

/// The settings file's mode.
pub const FILE_MODE: u32 = 0o600;
/// Its directory's mode.
pub const DIR_MODE: u32 = 0o700;
/// The idle times the settings take, in minutes.
pub const IDLE_MINUTES: RangeInclusive<u32> = 1..=240;
/// The idle time by default, in minutes.
pub const DEFAULT_IDLE_MINUTES: u32 = 5;
/// The longest chat id kept as the selected chat.
pub const MAX_CHAT_ID: usize = 512;
/// The largest settings file read; a larger one is unreadable.
pub const MAX_FILE: u64 = 64 * 1024;

/// Which messages notify.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Notifications {
    /// Every message from someone else in a chat that isn't shown.
    #[default]
    All,
    /// Only the messages that mention the account.
    Mentions,
    Off,
}

impl Notifications {
    pub const ALL: [Self; 3] = [Self::All, Self::Mentions, Self::Off];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Off => "off",
        }
    }

    /// What the settings view calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All messages",
            Self::Mentions => "Mentions only",
            Self::Off => "Off",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.key() == key)
    }
}

/// The account's status as the user sets it; [`ManualStatus::Automatic`] follows the app (Tasks 18–20 decide which
/// of these can be set, and how).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ManualStatus {
    /// Available while the app is used, away after the idle time.
    #[default]
    Automatic,
    Available,
    Busy,
    DoNotDisturb,
    BeRightBack,
    Away,
    AppearOffline,
}

impl ManualStatus {
    pub const ALL: [Self; 7] = [
        Self::Automatic,
        Self::Available,
        Self::Busy,
        Self::DoNotDisturb,
        Self::BeRightBack,
        Self::Away,
        Self::AppearOffline,
    ];

    /// Its name in the file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Available => "available",
            Self::Busy => "busy",
            Self::DoNotDisturb => "do-not-disturb",
            Self::BeRightBack => "be-right-back",
            Self::Away => "away",
            Self::AppearOffline => "appear-offline",
        }
    }

    /// What the settings view calls it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Automatic => "Automatic",
            Self::Available => "Available",
            Self::Busy => "Busy",
            Self::DoNotDisturb => "Do not disturb",
            Self::BeRightBack => "Be right back",
            Self::Away => "Appear away",
            Self::AppearOffline => "Appear offline",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.key() == key)
    }
}

/// The settings (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The chat shown last, opened again at the start.
    pub selected_chat: Option<String>,
    /// Typing is sent while the user types (Task 13).
    pub send_typing: bool,
    /// Read receipts are sent as a chat is read (Task 13).
    pub send_read_receipts: bool,
    pub notifications: Notifications,
    /// Minutes without use before the account shows away (Task 20), within [`IDLE_MINUTES`].
    pub idle_minutes: u32,
    pub manual_status: ManualStatus,
    /// The diagnostic log's level (`teams-log`), changed live; `TEAMS_APP_LOG` overrides it.
    pub log_level: LogLevel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            selected_chat: None,
            send_typing: true,
            send_read_receipts: true,
            notifications: Notifications::default(),
            idle_minutes: DEFAULT_IDLE_MINUTES,
            manual_status: ManualStatus::default(),
            log_level: LogLevel::default(),
        }
    }
}

/// The settings' keys in the file.
pub mod keys {
    pub const SELECTED_CHAT: &str = "selected_chat";
    pub const SEND_TYPING: &str = "send_typing";
    pub const SEND_READ_RECEIPTS: &str = "send_read_receipts";
    pub const NOTIFICATIONS: &str = "notifications";
    pub const IDLE_MINUTES: &str = "idle_minutes";
    pub const MANUAL_STATUS: &str = "manual_status";
    pub const LOG_LEVEL: &str = "log_level";
}

/// What went wrong with the settings file: the defaults are used, or the one setting's default.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Problem {
    #[error("the settings file {path} can't be read ({why}): the defaults are used")]
    Unreadable { path: String, why: String },
    #[error("the settings file's `{key}` is {why}: its default is kept")]
    BadValue { key: &'static str, why: &'static str },
    #[error("the settings file's `{key}` isn't a setting: it is ignored")]
    UnknownKey { key: String },
    #[error("the settings can't be saved to {path} ({why}): they hold until the app quits")]
    Unsaved { path: String, why: String },
    /// The diagnostic log couldn't be opened: the app runs without it.
    #[error("{why}")]
    LogUnavailable { why: String },
}

/// Reads settings from a file's text (see the module docs): every problem found, each one's setting at its default.
///
/// # Errors
/// The text isn't TOML: why, in a few words.
pub fn parse(text: &str) -> Result<(Settings, Vec<Problem>), &'static str> {
    let table: Table = text.parse().map_err(|_| "it isn't valid TOML")?;
    let mut settings = Settings::default();
    let mut problems = Vec::new();
    for (key, value) in &table {
        if let Err(problem) = take(&mut settings, key, value) {
            problems.push(problem);
        }
    }
    Ok((settings, problems))
}

/// Takes one key's value into `settings`.
fn take(settings: &mut Settings, key: &str, value: &Value) -> Result<(), Problem> {
    let bad = |key: &'static str, why: &'static str| Problem::BadValue { key, why };
    match key {
        keys::SELECTED_CHAT => {
            settings.selected_chat = Some(chat_id(value).ok_or(bad(keys::SELECTED_CHAT, "not a chat id"))?);
        }
        keys::SEND_TYPING => {
            settings.send_typing = value.as_bool().ok_or(bad(keys::SEND_TYPING, "not true or false"))?;
        }
        keys::SEND_READ_RECEIPTS => {
            settings.send_read_receipts = value
                .as_bool()
                .ok_or(bad(keys::SEND_READ_RECEIPTS, "not true or false"))?;
        }
        keys::NOTIFICATIONS => {
            settings.notifications = value
                .as_str()
                .and_then(Notifications::from_key)
                .ok_or(bad(keys::NOTIFICATIONS, "not one of all, mentions, off"))?;
        }
        keys::IDLE_MINUTES => {
            settings.idle_minutes = value
                .as_integer()
                .and_then(|minutes| u32::try_from(minutes).ok())
                .filter(|minutes| IDLE_MINUTES.contains(minutes))
                .ok_or(bad(keys::IDLE_MINUTES, "not a whole number of minutes from 1 to 240"))?;
        }
        keys::MANUAL_STATUS => {
            settings.manual_status = value
                .as_str()
                .and_then(ManualStatus::from_key)
                .ok_or(bad(keys::MANUAL_STATUS, "not a status this app knows"))?;
        }
        keys::LOG_LEVEL => {
            settings.log_level = value
                .as_str()
                .and_then(LogLevel::parse)
                .ok_or(bad(keys::LOG_LEVEL, "not one of error, warn, info, debug"))?;
        }
        other => {
            return Err(Problem::UnknownKey { key: other.to_owned() });
        }
    }
    Ok(())
}

/// A chat id as the settings keep one: a string of visible ASCII, at most [`MAX_CHAT_ID`] bytes.
fn chat_id(value: &Value) -> Option<String> {
    let id = value.as_str()?;
    let shaped = !id.is_empty() && id.len() <= MAX_CHAT_ID && id.bytes().all(|byte| byte.is_ascii_graphic());
    shaped.then(|| id.to_owned())
}

/// The file's text for `settings`.
#[must_use]
pub fn render(settings: &Settings) -> String {
    let mut table = Table::new();
    if let Some(chat) = &settings.selected_chat {
        table.insert(keys::SELECTED_CHAT.to_owned(), Value::String(chat.clone()));
    }
    table.insert(keys::SEND_TYPING.to_owned(), Value::Boolean(settings.send_typing));
    table.insert(
        keys::SEND_READ_RECEIPTS.to_owned(),
        Value::Boolean(settings.send_read_receipts),
    );
    table.insert(
        keys::NOTIFICATIONS.to_owned(),
        Value::String(settings.notifications.key().to_owned()),
    );
    table.insert(
        keys::IDLE_MINUTES.to_owned(),
        Value::Integer(i64::from(settings.idle_minutes)),
    );
    table.insert(
        keys::MANUAL_STATUS.to_owned(),
        Value::String(settings.manual_status.key().to_owned()),
    );
    table.insert(
        keys::LOG_LEVEL.to_owned(),
        Value::String(settings.log_level.key().to_owned()),
    );
    table.to_string()
}

/// The settings as read from their file, and what was wrong with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub settings: Settings,
    pub problems: Vec<Problem>,
}

/// The settings' file (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsFile {
    path: PathBuf,
}

impl SettingsFile {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn shown(&self) -> String {
        self.path.display().to_string()
    }

    /// The settings in the file; the defaults when there is none, or with every problem found.
    #[must_use]
    pub fn load(&self) -> Loaded {
        let unreadable = |why: String| Loaded {
            settings: Settings::default(),
            problems: vec![Problem::Unreadable {
                path: self.shown(),
                why,
            }],
        };
        let text = match read_private(&self.path) {
            Ok(Some(text)) => text,
            Ok(None) => {
                return Loaded {
                    settings: Settings::default(),
                    problems: Vec::new(),
                };
            }
            Err(why) => return unreadable(why),
        };
        match parse(&text) {
            Ok((settings, problems)) => Loaded { settings, problems },
            Err(why) => unreadable(why.to_owned()),
        }
    }

    /// Writes `settings` to the file (see the module docs).
    ///
    /// # Errors
    /// [`Problem::Unsaved`] when the directory or the file can't be written.
    pub fn save(&self, settings: &Settings) -> Result<(), Problem> {
        write_private(&self.path, render(settings).as_bytes()).map_err(|error| Problem::Unsaved {
            path: self.shown(),
            why: error.to_string(),
        })
    }
}

/// The file's text; `None` when there is no file. A file readable by others is tightened to [`FILE_MODE`] first.
fn read_private(path: &Path) -> Result<Option<String>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("it isn't a file".to_owned());
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        file.set_permissions(Permissions::from_mode(FILE_MODE))
            .map_err(|error| format!("it is readable by others and can't be made private: {error}"))?;
    }
    let mut text = String::new();
    file.take(MAX_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    if text.len() as u64 > MAX_FILE {
        return Err("it is too large".to_owned());
    }
    Ok(Some(text))
}

/// Writes `bytes` to `path` whole: the directory made (or kept) `0700`, a temporary file beside it `0600`, then
/// renamed over it.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the settings file has no directory"))?;
    DirBuilder::new().recursive(true).mode(DIR_MODE).create(dir)?;
    fs::set_permissions(dir, Permissions::from_mode(DIR_MODE))?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(FILE_MODE)
            .open(&temporary)?;
        file.set_permissions(Permissions::from_mode(FILE_MODE))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        // a temporary file left behind would only be overwritten by the next save
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// What the last "copy diagnostics" put on the clipboard, as the settings view tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Copied {
    /// It holds a `debug` line: full requests and responses, not safe to paste.
    pub has_debug: bool,
}

/// The settings as the views share them: a GPUI entity. A change goes to the file at once ([`SettingsModel::change`]);
/// a change of the log's level goes to the log at once too (`teams_log::apply_setting`). It also keeps where the
/// "copy diagnostics" text comes from ([`SettingsModel::copy_diagnostics`]).
#[derive(Debug)]
pub struct SettingsModel {
    settings: Settings,
    file: Option<SettingsFile>,
    problems: Vec<Problem>,
    /// The file couldn't be read: it is left as it is, and changes hold in memory.
    held: bool,
    diagnostics: Option<DiagnosticsSource>,
    copied: Option<Copied>,
}

impl SettingsModel {
    /// The settings in `file`, with what was wrong with it.
    #[must_use]
    pub fn load(file: SettingsFile) -> Self {
        let Loaded { settings, problems } = file.load();
        let held = problems
            .iter()
            .any(|problem| matches!(problem, Problem::Unreadable { .. }));
        Self {
            settings,
            file: Some(file),
            problems,
            held,
            diagnostics: None,
            copied: None,
        }
    }

    /// Settings kept in memory only: nothing is read or written.
    #[must_use]
    pub fn in_memory(settings: Settings) -> Self {
        Self {
            settings,
            file: None,
            problems: Vec::new(),
            held: false,
            diagnostics: None,
            copied: None,
        }
    }

    /// The settings with where "copy diagnostics" reads from; a log that couldn't be opened
    /// ([`DiagnosticsSource::log_problem`]) is shown as a problem.
    #[must_use]
    pub fn with_diagnostics(mut self, source: DiagnosticsSource) -> Self {
        if let Some(why) = &source.log_problem {
            self.problems.push(Problem::LogUnavailable { why: why.clone() });
        }
        self.diagnostics = Some(source);
        self
    }

    /// The "copy diagnostics" text as it would be copied now (see `teams_log::diagnostics`).
    #[must_use]
    pub fn diagnostics(&self) -> Report {
        let source = self.diagnostics.clone().unwrap_or_else(|| DiagnosticsSource {
            app: format!("teams {}", env!("CARGO_PKG_VERSION")),
            log_file: None,
            log_problem: None,
            fence_log: None,
        });
        source.collect(self.settings.log_level)
    }

    /// Puts the "copy diagnostics" text on the clipboard and keeps what it was for the settings view to tell
    /// ([`SettingsModel::copied`]).
    pub fn copy_diagnostics(&mut self, cx: &mut Context<Self>) -> Report {
        let report = self.diagnostics();
        cx.write_to_clipboard(ClipboardItem::new_string(report.text.clone()));
        self.copied = Some(Copied {
            has_debug: report.has_debug,
        });
        cx.notify();
        report
    }

    /// What the last "copy diagnostics" copied; `None` before one.
    #[must_use]
    pub fn copied(&self) -> Option<Copied> {
        self.copied
    }

    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// What was wrong with the file when it was read, and a save that failed since.
    #[must_use]
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The file the settings are kept in; `None` in memory.
    #[must_use]
    pub fn file(&self) -> Option<&SettingsFile> {
        self.file.as_ref()
    }

    /// Changes the settings and, when they changed, writes them to the file and tells the views. A save that fails, or
    /// one held back because the file couldn't be read (it would replace what the user wrote), is kept as a problem
    /// (the latest one only) and the change holds in memory.
    pub fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let before = self.settings.clone();
        change(&mut self.settings);
        if self.settings == before {
            return;
        }
        if self.settings.log_level != before.log_level {
            // no log installed (a view test): nothing to tell
            let _ = teams_log::apply_setting(self.settings.log_level);
        }
        self.problems
            .retain(|problem| !matches!(problem, Problem::Unsaved { .. }));
        if let Some(file) = &self.file {
            let saved = if self.held {
                Err(Problem::Unsaved {
                    path: file.shown(),
                    why: "it couldn't be read, so it is left as it is".to_owned(),
                })
            } else {
                file.save(&self.settings)
            };
            if let Err(problem) = saved {
                self.problems.push(problem);
            }
        }
        cx.notify();
    }
}
