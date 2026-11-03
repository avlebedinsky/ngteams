//! "Copy diagnostics": the text the settings view and the command palette put on the clipboard: the app's version
//! and build profile, the log's level, the current log's last lines (capped) and the recent `fence.log` lines. When
//! any `debug` line is among them it says so first, plainly ([`WARNING`]), and [`Report::has_debug`] tells the view to
//! say it too.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use crate::{Level, redact};

/// What the text says first when it holds a `debug` line.
pub const WARNING: &str = "contains full requests and responses: not safe to paste";

/// The most lines of the log the text holds.
pub const MAX_LOG_LINES: usize = 200;
/// The most bytes of the log's lines the text holds (the latest lines kept).
pub const MAX_LOG_BYTES: usize = 128 * 1024;
/// The most bytes read from the end of a file.
const TAIL_BYTES: u64 = 1024 * 1024;

/// The build profile this crate was built with: the app's.
#[must_use]
pub fn build_profile() -> &'static str {
    if cfg!(debug_assertions) { "debug" } else { "release" }
}

/// Where the text comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The app's name and version (`teams-app 0.1.0`).
    pub app: String,
    /// The current log file; `None` when the log isn't written.
    pub log_file: Option<PathBuf>,
    /// Why the log isn't written, when it isn't.
    pub log_problem: Option<String>,
}

/// The text, and whether it holds a `debug` line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    pub has_debug: bool,
}

/// The level as the text names it: the one that applies, and whether [`crate::ENV`] set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LevelState {
    pub level: Level,
    pub overridden: bool,
}

impl Source {
    /// The text with the installed log's level ([`crate::installed`], flushed first so its latest lines are in);
    /// `setting` when no log is installed.
    #[must_use]
    pub fn collect(&self, setting: Level) -> Report {
        let state = match crate::installed() {
            Some(log) => {
                let _ = log.flush_written();
                LevelState {
                    level: log.level(),
                    overridden: log.overridden().is_some(),
                }
            }
            None => LevelState {
                level: setting,
                overridden: false,
            },
        };
        self.collect_with(state)
    }

    /// The text with the level `state`.
    #[must_use]
    pub fn collect_with(&self, state: LevelState) -> Report {
        let log_lines = self
            .log_file
            .as_deref()
            .map(|path| capped(last_lines(path, MAX_LOG_LINES), MAX_LOG_BYTES));
        let has_debug = log_lines
            .as_ref()
            .and_then(|lines| lines.as_ref().ok())
            .is_some_and(|lines| lines.iter().any(|line| is_debug(line)));
        let mut text = String::new();
        if has_debug {
            let _ = writeln!(text, "WARNING: {WARNING}\n");
        }
        let _ = writeln!(text, "{} ({} build)", self.app, build_profile());
        let _ = writeln!(
            text,
            "log level: {}{}",
            state.level.key(),
            if state.overridden {
                " (set by TEAMS_APP_LOG)"
            } else {
                ""
            }
        );
        let _ = match (&self.log_file, &self.log_problem) {
            (_, Some(problem)) => writeln!(text, "log: not written ({problem})"),
            (Some(path), None) => writeln!(text, "log file: {}", path.display()),
            (None, None) => writeln!(text, "log: not written"),
        };
        section(&mut text, "the log", log_lines);
        Report { text, has_debug }
    }
}

/// Appends a titled part: its lines, or why they can't be read.
fn section(text: &mut String, title: &str, lines: Option<io::Result<Vec<String>>>) {
    match lines {
        None => {}
        Some(Ok(lines)) => {
            let _ = writeln!(text, "\n== {title}: the last {} line(s) ==", lines.len());
            for line in lines {
                text.push_str(&redact::line(&line));
                text.push('\n');
            }
        }
        Some(Err(error)) => {
            let _ = writeln!(text, "\n== {title}: can't be read ({error}) ==");
        }
    }
}

/// Whether a log line was written at `debug` (`<time> DEBUG <target>: …`).
#[must_use]
pub fn is_debug(line: &str) -> bool {
    line.split_whitespace().nth(1) == Some("DEBUG")
}

/// The latest `lines` that fit in `bytes`; the newest is always kept, its start cut to fit when it alone doesn't
/// (its time, level and target are what the text and [`is_debug`] read first).
fn capped(lines: io::Result<Vec<String>>, bytes: usize) -> io::Result<Vec<String>> {
    let mut lines = lines?;
    let mut total: usize = lines.iter().map(|line| line.len() + 1).sum();
    let mut first = 0;
    while total > bytes && first + 1 < lines.len() {
        total -= lines[first].len() + 1;
        first += 1;
    }
    let mut kept = lines.split_off(first);
    if let [newest] = kept.as_mut_slice()
        && newest.len() + 1 > bytes
    {
        cut(newest, bytes);
    }
    Ok(kept)
}

/// The room [`cut`] leaves for its note.
const CUT_NOTE: usize = 64;

/// `line` cut to about `bytes` (at a char boundary), with a note of how much was left out.
fn cut(line: &mut String, bytes: usize) {
    let mut end = bytes.saturating_sub(CUT_NOTE).min(line.len());
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let left_out = line.len() - end;
    line.truncate(end);
    let _ = write!(line, " … [{left_out} more bytes cut]");
}

/// The last `count` whole lines of the file at `path` (read from its last [`TAIL_BYTES`]); none for a missing file,
/// a note in their place when the newest line alone is longer than that.
fn last_lines(path: &Path, count: usize) -> io::Result<Vec<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let length = file.metadata()?.len();
    let start = length.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<&str> = text.lines().collect();
    if start > 0 && !lines.is_empty() {
        // the first line read began before the part read
        lines.remove(0);
        if lines.is_empty() {
            // the newest line alone is longer than the part read: said, never written from its middle
            return Ok(vec![format!(
                "[the newest line is longer than {} KiB: left out]",
                TAIL_BYTES / 1024
            )]);
        }
    }
    let skip = lines.len().saturating_sub(count);
    Ok(lines[skip..].iter().map(|line| (*line).to_owned()).collect())
}
