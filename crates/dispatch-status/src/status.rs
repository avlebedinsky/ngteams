//! `/status.json`: the ledger's tasks, each with what its transcript says, its phase and its estimate.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::ledger::{Ledger, Task};
use crate::phase::{Flow, MINUTE_MS, Progress, estimate, typical};
use crate::redact::shown;
use crate::transcript::Transcript;

/// At most this many agents run at once.
const SLOTS: usize = 4;

/// With a background command pending and nothing written for this long, the agent is waiting for it.
const WAITING_AFTER_MS: i64 = 30_000;

/// Where a task is listed, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Waiting,
    Failed,
    Running,
    Queued,
    Completed,
}

impl Group {
    fn of(state: &str) -> Self {
        let state = state.trim().to_ascii_lowercase();
        if state.starts_with("waiting") {
            Self::Waiting
        } else if state.starts_with("fail") {
            Self::Failed
        } else if state.starts_with("queued") {
            Self::Queued
        } else if state.starts_with("landed") || state.starts_with("done") {
            Self::Completed
        } else {
            Self::Running
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Failed => "failed",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Completed => "completed",
        }
    }
}

/// One task as this poll sees it.
#[derive(Clone)]
struct Row {
    task: Task,
    group: Group,
    transcript: Option<PathBuf>,
    flow: Flow,
    progress: Option<Progress>,
    /// The estimate of what is left, and whether the current phase runs long.
    eta: Option<(i64, bool)>,
}

/// The server's state: the ledger, the transcripts read so far, and where agents' transcripts are found by name.
pub struct Status {
    ledger: Ledger,
    agents: Agents,
    transcripts: HashMap<PathBuf, Transcript>,
    by_name: HashMap<String, PathBuf>,
    scanned: bool,
}

impl Status {
    pub fn new(ledger: PathBuf, agents: Agents) -> Self {
        Self {
            ledger: Ledger::new(ledger),
            agents,
            transcripts: HashMap::new(),
            by_name: HashMap::new(),
            scanned: false,
        }
    }

    /// The page's data at `now_ms`.
    pub fn snapshot(&mut self, now_ms: i64) -> Value {
        self.ledger.refresh();
        self.scanned = false;
        let tasks = self.ledger.tasks().to_vec();
        let mut rows: Vec<Row> = tasks.into_iter().map(|task| self.row(task)).collect();
        // Only the ledger's completed tasks: a handback may be a question, and a failed task stopped short.
        let finished: Vec<&Progress> = rows
            .iter()
            .filter(|row| row.group == Group::Completed)
            .filter_map(|row| row.progress.as_ref())
            .filter(|p| p.is_done())
            .collect();
        let lengths = [Flow::Edit, Flow::Scratch].map(|flow| typical(flow, &finished));
        for row in &mut rows {
            if row.group == Group::Running
                && let Some(progress) = row.progress.as_ref().filter(|progress| !progress.is_done())
            {
                row.eta = estimate(progress, &lengths[flow_index(row.flow)], now_ms);
            }
        }
        let queue = queue_places(&rows);
        let out: Vec<Value> = rows
            .iter()
            .map(|row| {
                let transcript = row.transcript.as_ref().and_then(|path| self.transcripts.get(path));
                task_json(
                    row,
                    transcript,
                    &lengths[flow_index(row.flow)],
                    now_ms,
                    queue.get(&row.task.num),
                )
            })
            .collect();
        json!({
            "now_ms": now_ms,
            "slots_used": rows.iter().filter(|row| row.group == Group::Running).count(),
            "slots_max": SLOTS,
            "queue_len": rows.iter().filter(|row| row.group == Group::Queued).count(),
            "ledger_error": self.ledger.error(),
            "heuristic": heuristic(&lengths),
            "tasks": out,
        })
    }

    fn row(&mut self, task: Task) -> Row {
        let group = Group::of(&task.state);
        let path = task.transcript.clone().or_else(|| {
            // A queued task's agent hasn't started: an earlier agent of the same name isn't it.
            (group != Group::Queued).then(|| self.find(&task.agent)).flatten()
        });
        let transcript = path.as_ref().map(|path| {
            let transcript = self
                .transcripts
                .entry(path.clone())
                .or_insert_with(|| Transcript::new(path.clone()));
            transcript.poll();
            &*transcript
        });
        let flow = task
            .flow
            .unwrap_or(match transcript.and_then(|transcript| transcript.meta.as_ref()) {
                Some(meta) if meta.worktree.is_none() => Flow::Scratch,
                _ => Flow::Edit,
            });
        let progress = transcript.and_then(|transcript| {
            let log = &transcript.log;
            let done_ms = match group {
                Group::Completed => log.handback_ms.or(log.last_ms),
                Group::Waiting => None,
                _ => log.handback_ms,
            };
            Some(Progress::new(flow, log.first_ms?, &log.signals, done_ms))
        });
        Row {
            task,
            group,
            transcript: path,
            flow,
            progress,
            eta: None,
        }
    }

    /// The transcript of the newest agent named `name` in the session's subagents dir.
    fn find(&mut self, name: &str) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        // Once a poll (a few dozen small files): a name given to a newer agent moves to it.
        if !self.scanned {
            self.scanned = true;
            if let Some(dir) = self.agents.dir() {
                self.by_name = scan_names(&dir);
            }
        }
        self.by_name.get(name).cloned()
    }
}

/// Where agents' transcripts are found by name.
pub enum Agents {
    /// A dir given on the command line.
    Dir(PathBuf),
    /// This Claude Code session's subagents dir, by the session's id: Claude Code makes it only with the session's
    /// first agent, so it is looked for again on every poll until it exists.
    Session { projects: PathBuf, session: String },
    /// Only where the ledger's `transcript`s say.
    Nowhere,
}

impl Agents {
    fn dir(&mut self) -> Option<PathBuf> {
        match self {
            Self::Dir(dir) => Some(dir.clone()),
            Self::Session { projects, session } => {
                let dir = session_subagents(projects, session)?;
                *self = Self::Dir(dir.clone());
                Some(dir)
            }
            Self::Nowhere => None,
        }
    }

    /// Where agents are looked for, in words.
    pub fn describe(&self) -> String {
        match self {
            Self::Dir(dir) => dir.display().to_string(),
            Self::Session { projects, session } => format!("{}/*/{session}/subagents", projects.display()),
            Self::Nowhere => "the ledger's transcript paths only".to_owned(),
        }
    }
}

/// `<projects>/<project>/<session>/subagents`, once it exists.
fn session_subagents(projects: &Path, session: &str) -> Option<PathBuf> {
    fs::read_dir(projects)
        .ok()?
        .flatten()
        .map(|project| project.path().join(session).join("subagents"))
        .find(|dir| dir.is_dir())
}

fn flow_index(flow: Flow) -> usize {
    match flow {
        Flow::Edit => 0,
        Flow::Scratch => 1,
    }
}

/// Every agent's name in `dir`'s `*.meta.json`, with its transcript (the newest one of a name).
fn scan_names(dir: &Path) -> HashMap<String, PathBuf> {
    let mut found: HashMap<String, (std::time::SystemTime, PathBuf)> = HashMap::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return HashMap::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".meta.json"))
        else {
            continue;
        };
        let Some(name) = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|meta| meta.get("name").and_then(Value::as_str).map(str::to_owned))
        else {
            continue;
        };
        let transcript = dir.join(format!("{stem}.jsonl"));
        let Ok(modified) = fs::metadata(&transcript).and_then(|metadata| metadata.modified()) else {
            continue;
        };
        if found.get(&name).is_none_or(|(newest, _)| modified > *newest) {
            found.insert(name, (modified, transcript));
        }
    }
    found.into_iter().map(|(name, (_, path))| (name, path)).collect()
}

/// A queued task's place in line and its estimated start (from `now`, in ms): the ETA of the task it waits for, or
/// the n-th smallest ETA among the running tasks once the free slots are taken, n counting only the queued tasks
/// ahead that wait for no other task (`Some(0)` when a slot is free).
fn queue_places(rows: &[Row]) -> HashMap<u64, (usize, Option<i64>)> {
    let running = rows.iter().filter(|row| row.group == Group::Running).count();
    let free = SLOTS.saturating_sub(running);
    let queued_after: Vec<u64> = rows
        .iter()
        .filter(|row| row.group == Group::Queued)
        .filter_map(|row| row.task.after)
        .collect();
    // The slot a running task frees goes to the queued task waiting for it, not to the line.
    let mut etas: Vec<i64> = rows
        .iter()
        .filter(|row| !queued_after.contains(&row.task.num))
        .filter_map(|row| row.eta.map(|(eta, _)| eta))
        .collect();
    etas.sort_unstable();
    let mut queued: Vec<&Row> = rows.iter().filter(|row| row.group == Group::Queued).collect();
    queued.sort_by_key(|row| row.task.num);
    let mut places = HashMap::new();
    // Tasks waiting for another task take no slot as it frees: only the free ones ahead count.
    let mut ahead = 0;
    for (index, row) in queued.iter().enumerate() {
        let blocker = row
            .task
            .after
            .and_then(|after| rows.iter().find(|other| other.task.num == after))
            .filter(|other| other.group != Group::Completed);
        let start = match blocker {
            Some(blocker) => blocker.eta.map(|(eta, _)| eta),
            None if ahead < free => Some(0),
            None => etas.get(ahead - free).copied(),
        };
        if blocker.is_none() {
            ahead += 1;
        }
        places.insert(row.task.num, (index + 1, start));
    }
    places
}

fn task_json(
    row: &Row,
    transcript: Option<&Transcript>,
    lengths: &[i64],
    now_ms: i64,
    queue: Option<&(usize, Option<i64>)>,
) -> Value {
    let task = &row.task;
    let mut out = json!({
        "num": task.num,
        "title": shown(&task.title, 120),
        "agent": shown(&task.agent, 40),
        "state": shown(&task.state, 60),
        "group": row.group.name(),
        "note": shown(&task.note, 240),
        "flow": row.flow.name(),
        "phases": row.flow.phases(),
        "after": task.after,
        "eta_ms": row.eta.map(|(eta, _)| eta),
        "long": row.eta.is_some_and(|(_, long)| long),
        "queue": queue.map(|(place, start)| json!({"place": place, "start_ms": start})),
    });
    if let Some(progress) = &row.progress {
        out["phase"] = json!(progress.phase);
        out["phase_since_ms"] = json!(progress.since());
        out["phase_typical_ms"] = json!(lengths.get(progress.phase));
        out["done"] = json!(progress.is_done());
    }
    if let Some(transcript) = transcript {
        activity(&mut out, row, transcript, now_ms);
    }
    out
}

/// What the transcript says the agent is doing.
fn activity(out: &mut Value, row: &Row, transcript: &Transcript, now_ms: i64) {
    let log = &transcript.log;
    out["transcript_error"] = json!(transcript.error);
    out["started_ms"] = json!(log.first_ms);
    let ended = match row.group {
        Group::Running => None,
        _ => row
            .progress
            .as_ref()
            .filter(|progress| progress.is_done())
            .and_then(|progress| progress.entered.last().copied().flatten())
            .or(log.last_ms),
    };
    out["ended_ms"] = json!(ended);
    out["written_ms"] = json!(transcript.written_ms);
    out["doing"] = json!(log.latest.as_ref().map(|call| json!({
        "text": call.words,
        "started_ms": call.started_ms,
        "finished_ms": call.finished_ms,
    })));
    let quiet_since = transcript.written_ms.map_or(0, |written| now_ms - written);
    let latest_running = log.latest.as_ref().is_some_and(|call| call.finished_ms.is_none());
    let waiting_for = log
        .background
        .last()
        .filter(|_| !latest_running && quiet_since >= WAITING_AFTER_MS);
    out["waiting_for"] =
        json!(waiting_for.map(|background| json!({"text": background.words, "started_ms": background.started_ms})));
    out["background"] = log
        .background
        .iter()
        .map(|background| json!({"text": background.words, "started_ms": background.started_ms}))
        .collect();
    out["said"] = json!(log.said.as_ref().map(|(text, at)| json!({"text": text, "ms": at})));
}

/// How the estimate is made, with the typical lengths in use now.
fn heuristic(lengths: &[Vec<i64>; 2]) -> String {
    let list = |flow: Flow, lengths: &[i64]| {
        flow.phases()
            .iter()
            .zip(lengths)
            .map(|(phase, length)| format!("{phase} {}", (length + MINUTE_MS / 2) / MINUTE_MS))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "An estimate, not a promise: what is left of the current phase's typical length (at least a fifth of it) plus \
         the typical lengths of the phases after it. A phase's typical length is the median of the finished tasks' \
         times for it (phases skipped in under 30 s left out), or its default until one exists. Typical minutes now: \
         an editing agent {}; an agent without a worktree {}.",
        list(Flow::Edit, &lengths[0]),
        list(Flow::Scratch, &lengths[1]),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{Value, json};

    use super::{Agents, Group, Row, Status, queue_places};
    use crate::ledger::Task;
    use crate::phase::Flow;

    fn row(num: u64, state: &str, after: Option<u64>, eta: Option<i64>) -> Row {
        let task = Task {
            num,
            title: String::new(),
            agent: String::new(),
            state: state.to_owned(),
            transcript: None,
            note: String::new(),
            after,
            flow: None,
        };
        let eta = eta.map(|eta| (eta, false));
        Row {
            task,
            group: Group::of(state),
            transcript: None,
            flow: Flow::Edit,
            progress: None,
            eta,
        }
    }

    #[test]
    fn a_task_waiting_for_another_takes_no_slot_in_line() {
        let full = [
            row(1, "running", None, Some(30)),
            row(2, "running", None, Some(10)),
            row(3, "running", None, Some(20)),
            row(4, "running", None, Some(40)),
            row(5, "queued", Some(1), None),
            row(6, "queued", None, None),
            row(7, "queued", None, None),
        ];
        let places = queue_places(&full);
        assert_eq!(places[&5], (1, Some(30)));
        assert_eq!(places[&6], (2, Some(10)));
        assert_eq!(places[&7], (3, Some(20)));
        // #5 takes the soonest slot (#2's): the line gets the ones after it.
        let mut soonest = full.clone();
        soonest[4].task.after = Some(2);
        let places = queue_places(&soonest);
        assert_eq!(places[&5], (1, Some(10)));
        assert_eq!(places[&6], (2, Some(20)));
        assert_eq!(places[&7], (3, Some(30)));
        let free = [
            row(1, "running", None, Some(30)),
            row(5, "queued", Some(1), None),
            row(6, "queued", None, None),
        ];
        assert_eq!(queue_places(&free)[&6], (2, Some(0)));
    }

    #[test]
    fn finds_the_session_dir_once_claude_code_makes_it() {
        let projects = std::env::temp_dir().join(format!("dispatch-status-projects-{}", std::process::id()));
        fs::create_dir_all(projects.join("p/s1")).unwrap();
        let mut agents = Agents::Session {
            projects: projects.clone(),
            session: "s1".to_owned(),
        };
        assert_eq!(agents.dir(), None);
        fs::create_dir_all(projects.join("p/s1/subagents")).unwrap();
        assert_eq!(agents.dir(), Some(projects.join("p/s1/subagents")));
        fs::remove_dir_all(&projects).unwrap();
    }

    fn write(path: &std::path::Path, lines: &[Value]) {
        let text = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
        fs::write(path, text).unwrap();
    }

    #[test]
    fn groups_estimates_and_queues() {
        let dir = std::env::temp_dir().join(format!("dispatch-status-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("agent-a1.jsonl");
        write(
            &transcript,
            &[
                json!({"type": "user", "timestamp": "2026-10-29T10:00:00Z", "message": {"content": "brief"}}),
                json!({"type": "assistant", "timestamp": "2026-10-29T10:05:00Z", "message": {"content": [
                    {"type": "tool_use", "id": "t1", "name": "Edit", "input": {"file_path": "/r/src/a.rs"}}]}}),
            ],
        );
        fs::write(
            dir.join("agent-a1.meta.json"),
            r#"{"name": "one", "worktreePath": "/r"}"#,
        )
        .unwrap();
        let ledger = dir.join("ledger.json");
        let tasks = json!({"tasks": [
            {"num": 1, "title": "One", "agent": "one", "state": "running"},
            {"num": 2, "title": "Two", "agent": "two", "state": "queued", "after": 1},
            {"num": 3, "title": "Three", "agent": "three", "state": "waiting on user"},
        ]});
        fs::write(&ledger, tasks.to_string()).unwrap();
        let mut status = Status::new(ledger.clone(), Agents::Dir(dir.clone()));
        // 10:15, ten minutes into implementing: 20 min of it left, then revmux, commit, land and gate (48 min).
        let snapshot = status.snapshot(1_793_268_900_000);
        let one = &snapshot["tasks"][0];
        assert_eq!(one["group"], "running");
        assert_eq!(one["phase"], 1);
        assert_eq!(one["eta_ms"], 68 * 60_000);
        assert_eq!(one["doing"]["text"], "Editing src/a.rs");
        let two = &snapshot["tasks"][1];
        assert_eq!(two["queue"], json!({"place": 1, "start_ms": 68 * 60_000}));
        assert_eq!(snapshot["tasks"][2]["group"], "waiting");
        assert_eq!(snapshot["slots_used"], 1);

        fs::write(&ledger, r#"{"tasks": [{"num": 1"#).unwrap();
        let kept = status.snapshot(1_793_268_900_000);
        assert_eq!(kept["tasks"].as_array().map(Vec::len), Some(3));
        assert!(kept["ledger_error"].is_string());
        fs::remove_dir_all(&dir).unwrap();
    }
}
