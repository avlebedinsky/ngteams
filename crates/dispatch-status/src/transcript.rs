//! One agent's JSONL transcript, read incrementally from a remembered offset while the agent appends to it.

use std::fs::{self, File};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::describe::tool_words;
use crate::redact::shown;
use crate::signals::{Signal, tool_signals};
use crate::time::{parse_rfc3339, system_ms};

/// The longest line of the agent's own text shown.
const SAID_MAX: usize = 160;

/// A tool call: what it does, when it started, and when its result came back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub id: String,
    pub words: String,
    pub started_ms: i64,
    pub finished_ms: Option<i64>,
}

/// A command the agent left running in the background.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Background {
    pub tool_use_id: String,
    pub task_id: Option<String>,
    pub words: String,
    pub started_ms: i64,
}

/// What the transcript says so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Log {
    pub first_ms: Option<i64>,
    /// The latest user or assistant entry.
    pub last_ms: Option<i64>,
    pub signals: Vec<(i64, Signal)>,
    pub latest: Option<Call>,
    pub background: Vec<Background>,
    /// The first line of the agent's latest text, and when it was written.
    pub said: Option<(String, i64)>,
    /// When the agent handed back its report, while that is the last thing it did.
    pub handback_ms: Option<i64>,
}

/// A transcript file and how far it has been read.
pub struct Transcript {
    path: PathBuf,
    inode: Option<u64>,
    offset: u64,
    partial: Vec<u8>,
    /// The `.meta.json` beside it, if there is one.
    pub meta: Option<Meta>,
    pub error: Option<String>,
    /// When the file was last written.
    pub written_ms: Option<i64>,
    pub log: Log,
}

impl Transcript {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            inode: None,
            offset: 0,
            partial: Vec::new(),
            meta: None,
            error: None,
            written_ms: None,
            log: Log::default(),
        }
    }

    /// Reads what was appended since the last poll. A replaced or shorter file is read again from its start.
    pub fn poll(&mut self) {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.error = Some(format!("the transcript can't be read ({})", error.kind()));
                return;
            }
        };
        self.error = None;
        self.written_ms = metadata.modified().ok().and_then(system_ms);
        if self.inode != Some(metadata.ino()) || metadata.size() < self.offset {
            self.inode = Some(metadata.ino());
            self.offset = 0;
            self.partial.clear();
            self.log = Log::default();
            self.meta = read_meta(&self.path);
        }
        if metadata.size() == self.offset {
            return;
        }
        match self.read_more() {
            Ok(bytes) => self.take(&bytes),
            Err(error) => self.error = Some(format!("the transcript can't be read ({})", error.kind())),
        }
    }

    fn read_more(&mut self) -> std::io::Result<Vec<u8>> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        self.offset += bytes.len() as u64;
        Ok(bytes)
    }

    /// Takes the complete lines of `bytes` (after what was left of the last read); a last line without its newline
    /// waits for the next read.
    fn take(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        let Some(end) = self.partial.iter().rposition(|&byte| byte == b'\n') else {
            return;
        };
        let rest = self.partial.split_off(end + 1);
        let complete = std::mem::replace(&mut self.partial, rest);
        let worktree = self.meta.as_ref().and_then(|meta| meta.worktree.clone());
        for line in complete.split(|&byte| byte == b'\n') {
            if let Ok(entry) = serde_json::from_slice::<Value>(line) {
                self.log.entry(&entry, worktree.as_deref());
            }
        }
    }
}

/// What a transcript's `.meta.json` says about its agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// The agent's worktree; none for an agent working in the main checkout.
    pub worktree: Option<String>,
}

/// The `.meta.json` beside a transcript, when it exists.
fn read_meta(path: &Path) -> Option<Meta> {
    let bytes = fs::read(path.with_extension("meta.json")).ok()?;
    let meta: Value = serde_json::from_slice(&bytes).ok()?;
    let worktree = meta.get("worktreePath").and_then(Value::as_str).map(str::to_owned);
    Some(Meta { worktree })
}

impl Log {
    /// Takes one transcript entry.
    pub fn entry(&mut self, entry: &Value, worktree: Option<&str>) {
        let at = entry.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339);
        if let Some(at) = at {
            self.first_ms.get_or_insert(at);
        }
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or_default();
        if kind == "attachment" {
            if let Some(prompt) = entry.pointer("/attachment/prompt").and_then(Value::as_str) {
                self.notifications(prompt);
            }
            return;
        }
        let Some(at) = at.filter(|_| kind == "user" || kind == "assistant") else {
            return;
        };
        self.last_ms = Some(at);
        match entry.pointer("/message/content") {
            Some(Value::String(text)) if kind == "user" => self.notifications(text),
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match (kind, block.get("type").and_then(Value::as_str)) {
                        ("assistant", Some("text")) => self.text(block, at),
                        ("assistant", Some("tool_use")) => self.tool_use(block, at, worktree),
                        ("user", Some("tool_result")) => self.tool_result(block, entry, at),
                        ("user", Some("text")) => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
                            self.notifications(text);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, block: &Value, at: i64) {
        let text = block.get("text").and_then(Value::as_str).unwrap_or_default();
        let Some(line) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
            return;
        };
        let line = line.trim_start_matches(['#', '*', '-', '>', ' ']).trim();
        self.said = Some((shown(line, SAID_MAX), at));
    }

    fn tool_use(&mut self, block: &Value, at: i64, worktree: Option<&str>) {
        let name = block.get("name").and_then(Value::as_str).unwrap_or("a tool");
        let input = block.get("input").unwrap_or(&Value::Null);
        let id = block.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let words = in_worktree(&tool_words(name, input), worktree);
        if input.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            self.background.push(Background {
                tool_use_id: id.clone(),
                task_id: None,
                words: words.clone(),
                started_ms: at,
            });
        }
        if name == "TaskStop" {
            let task = input
                .get("task_id")
                .or_else(|| input.get("shell_id"))
                .and_then(Value::as_str);
            if let Some(task) = task {
                self.background
                    .retain(|background| background.task_id.as_deref() != Some(task));
            }
        }
        self.handback_ms = (name == "SubagentHandback").then_some(at);
        self.signals
            .extend(tool_signals(name, input).into_iter().map(|signal| (at, signal)));
        self.latest = Some(Call {
            id,
            words,
            started_ms: at,
            finished_ms: None,
        });
    }

    fn tool_result(&mut self, block: &Value, entry: &Value, at: i64) {
        let id = block.get("tool_use_id").and_then(Value::as_str).unwrap_or_default();
        if let Some(latest) = self.latest.as_mut().filter(|latest| latest.id == id) {
            latest.finished_ms = Some(at);
        }
        let Some(index) = self
            .background
            .iter()
            .position(|background| background.tool_use_id == id)
        else {
            return;
        };
        if block.get("is_error").and_then(Value::as_bool) == Some(true) {
            self.background.remove(index);
            return;
        }
        let task = entry
            .pointer("/toolUseResult/backgroundTaskId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| background_id(block));
        self.background[index].task_id = task;
    }

    /// Clears the background commands a `<task-notification>` in `text` reports as ended.
    fn notifications(&mut self, text: &str) {
        for notification in text.split("<task-notification>").skip(1) {
            let notification = notification.split("</task-notification>").next().unwrap_or_default();
            if tag(notification, "status") == Some("running") {
                continue;
            }
            let task = tag(notification, "task-id");
            let tool_use = tag(notification, "tool-use-id");
            let ended = |background: &Background| {
                (task.is_some() && background.task_id.as_deref() == task)
                    || (tool_use.is_some() && Some(background.tool_use_id.as_str()) == tool_use)
            };
            self.background.retain(|background| !ended(background));
        }
    }
}

/// The text of `<name>…</name>` in `text`.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{name}>"))? + name.len() + 2;
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(text[start..end].trim())
}

/// The id in a background command's "running in background with ID: …" result.
fn background_id(block: &Value) -> Option<String> {
    let content = block.get("content")?;
    let text = match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let after = text.split_once("with ID: ")?.1;
    let id: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Words naming a path in the agent's own worktree name it relative to the worktree.
fn in_worktree(words: &str, worktree: Option<&str>) -> String {
    match worktree {
        Some(worktree) if !worktree.is_empty() => words.replace(&format!("{}/", worktree.trim_end_matches('/')), ""),
        _ => words.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{Log, Transcript};
    use crate::signals::Signal;

    #[test]
    fn reads_what_was_appended_and_starts_over_on_a_new_file() {
        use std::io::Write as _;
        let path = std::env::temp_dir().join(format!("dispatch-status-transcript-{}.jsonl", std::process::id()));
        let first = r#"{"type":"user","timestamp":"2026-10-29T10:00:00Z","message":{"content":"brief"}}"#;
        let edit = r#"{"type":"assistant","timestamp":"2026-10-29T10:01:00Z","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo check"}}]}}"#;
        std::fs::write(&path, format!("{first}\n{}", &edit[..40])).unwrap();
        let mut transcript = Transcript::new(path.clone());
        transcript.poll();
        assert_eq!(transcript.log.first_ms, Some(1_793_268_000_000));
        assert!(transcript.log.signals.is_empty(), "half a line waits for the rest");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", &edit[40..]).unwrap();
        transcript.poll();
        assert_eq!(transcript.log.signals, [(1_793_268_060_000, Signal::Build)]);

        // A new file in its place (another inode, shorter) is read from its start.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, format!("{first}\n")).unwrap();
        transcript.poll();
        assert!(transcript.log.signals.is_empty());
        assert_eq!(transcript.log.first_ms, Some(1_793_268_000_000));
        std::fs::remove_file(&path).unwrap();
    }

    fn log(entries: &[Value]) -> Log {
        let mut log = Log::default();
        for entry in entries {
            log.entry(entry, None);
        }
        log
    }

    fn assistant(at: &str, content: &Value) -> Value {
        json!({"type": "assistant", "timestamp": at, "message": {"role": "assistant", "content": content}})
    }

    fn user(at: &str, content: &Value) -> Value {
        json!({"type": "user", "timestamp": at, "message": {"role": "user", "content": content}})
    }

    #[test]
    fn follows_tool_calls_and_background_commands() {
        let entries = [
            user("2026-10-29T10:00:00Z", &json!("the brief")),
            assistant(
                "2026-10-29T10:00:05Z",
                &json!([{"type": "text", "text": "\n## Let me build it.\nmore"}]),
            ),
            assistant(
                "2026-10-29T10:00:10Z",
                &json!([{"type": "tool_use", "id": "t1", "name": "Bash",
                        "input": {"command": "cargo check", "description": "Compile check", "run_in_background": true}}]),
            ),
            json!({"type": "user", "timestamp": "2026-10-29T10:00:11Z", "toolUseResult": {"backgroundTaskId": "b1"},
                   "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": "running"}]}}),
            assistant(
                "2026-10-29T10:00:20Z",
                &json!([{"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/tmp/x"}}]),
            ),
        ];
        let running = log(&entries);
        assert_eq!(running.first_ms, Some(1_793_268_000_000));
        assert_eq!(
            running.said.as_ref().map(|said| said.0.as_str()),
            Some("Let me build it.")
        );
        assert_eq!(running.background.len(), 1);
        assert_eq!(running.background[0].task_id.as_deref(), Some("b1"));
        assert_eq!(running.signals, [(1_793_268_010_000, Signal::Build)]);
        let latest = running.latest.as_ref().unwrap();
        assert_eq!((latest.words.as_str(), latest.finished_ms), ("Reading /tmp/x", None));

        let ended = json!({"type": "attachment", "attachment": {"type": "queued_command",
            "prompt": "<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n</task-notification>"}});
        let mut done = running;
        done.entry(&ended, None);
        assert!(done.background.is_empty());
    }

    #[test]
    fn a_handback_counts_while_it_is_the_last_call() {
        let handback = assistant(
            "2026-10-29T10:00:00Z",
            &json!([{"type": "tool_use", "id": "t1", "name": "SubagentHandback", "input": {"message": "a question"}}]),
        );
        let mut log = log(&[handback]);
        assert_eq!(log.handback_ms, Some(1_793_268_000_000));
        let resumed = assistant(
            "2026-10-29T10:05:00Z",
            &json!([{"type": "tool_use", "id": "t2", "name": "Read", "input": {"file_path": "/x"}}]),
        );
        log.entry(&resumed, None);
        assert_eq!(log.handback_ms, None);
    }

    #[test]
    fn a_stopped_command_is_no_longer_pending() {
        let entries = [
            assistant(
                "2026-10-29T10:00:00Z",
                &json!([{"type": "tool_use", "id": "t1", "name": "Bash",
                        "input": {"command": "sleep 99", "run_in_background": true}}]),
            ),
            user(
                "2026-10-29T10:00:01Z",
                &json!([{"type": "tool_result", "tool_use_id": "t1",
                        "content": "Command running in background with ID: bx9. Output is being written to: /tmp/o"}]),
            ),
            assistant(
                "2026-10-29T10:00:02Z",
                &json!([{"type": "tool_use", "id": "t2", "name": "TaskStop", "input": {"task_id": "bx9"}}]),
            ),
        ];
        assert!(log(&entries).background.is_empty());
    }
}
