//! The dispatcher's ledger: a JSON file it rewrites on every change, reread here whenever it changes.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;

use serde_json::Value;

use crate::phase::Flow;

/// One task of the ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub num: u64,
    pub title: String,
    pub agent: String,
    /// `queued`, `running`, `waiting on user`, `landed <hash>`, `failed…` or `done…`.
    pub state: String,
    /// The agent's JSONL transcript; without it the agent's transcript is looked up by its name.
    pub transcript: Option<PathBuf>,
    pub note: String,
    /// The task this queued one waits for.
    pub after: Option<u64>,
    pub flow: Option<Flow>,
}

/// The ledger file and its last good copy.
pub struct Ledger {
    path: PathBuf,
    /// The file's (mtime seconds, mtime nanoseconds, size, inode) when it was last read well: the nanoseconds tell
    /// two rewrites within one second apart.
    seen: Option<(i64, i64, u64, u64)>,
    tasks: Vec<Task>,
    error: Option<String>,
}

impl Ledger {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            seen: None,
            tasks: Vec::new(),
            error: None,
        }
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// Why the file couldn't be read the last time it changed (its last good copy still in use).
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Reads the file again if it changed since it was last read well. A copy caught mid-write is skipped: the last
    /// good one stays, and the next poll reads it again.
    pub fn refresh(&mut self) {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.error = Some(format!("the ledger can't be read ({})", error.kind()));
                return;
            }
        };
        let key = (metadata.mtime(), metadata.mtime_nsec(), metadata.size(), metadata.ino());
        if self.seen == Some(key) {
            return;
        }
        match fs::read(&self.path)
            .map_err(|error| error.kind().to_string())
            .and_then(|bytes| parse(&bytes))
        {
            Ok(tasks) => {
                self.tasks = tasks;
                self.seen = Some(key);
                self.error = None;
            }
            Err(error) => {
                self.error = Some(format!(
                    "the ledger is unreadable ({error}); showing its last good copy"
                ));
            }
        }
    }
}

/// The ledger's tasks: `{"tasks": [{"num": 1, "title": …, "agent": …, "state": …}, …]}`.
pub fn parse(bytes: &[u8]) -> Result<Vec<Task>, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|error| format!("not JSON: line {}", error.line()))?;
    let tasks = value.get("tasks").and_then(Value::as_array).ok_or("no tasks list")?;
    Ok(tasks.iter().filter_map(task).collect())
}

fn task(value: &Value) -> Option<Task> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let num = value.get("num").and_then(Value::as_u64)?;
    let transcript = value
        .get("transcript")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let state = text("state");
    Some(Task {
        num,
        title: text("title"),
        agent: text("agent"),
        state: if state.trim().is_empty() {
            "queued".to_owned()
        } else {
            state
        },
        transcript,
        note: text("note"),
        after: value.get("after").and_then(Value::as_u64),
        flow: value.get("flow").and_then(Value::as_str).and_then(Flow::parse),
    })
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::phase::Flow;

    #[test]
    fn reads_the_tasks() {
        let text = br#"{"tasks": [
            {"num": 1, "title": "A", "agent": "a", "state": "running", "transcript": "/t/a.jsonl", "flow": "scratch"},
            {"num": 2, "title": "B", "agent": "b", "state": "queued", "after": 1, "note": "n"},
            {"title": "no number"}
        ]}"#;
        let tasks = parse(text).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].flow, Some(Flow::Scratch));
        assert_eq!(tasks[0].transcript.as_deref(), Some(std::path::Path::new("/t/a.jsonl")));
        assert_eq!(tasks[1].after, Some(1));
        assert_eq!(tasks[1].note, "n");
    }

    #[test]
    fn refuses_a_half_written_file() {
        assert!(parse(br#"{"tasks": [{"num": 1"#).is_err());
        assert!(parse(b"{}").is_err());
    }
}
