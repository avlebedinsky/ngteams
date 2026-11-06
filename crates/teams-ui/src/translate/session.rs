//! One translating tool's process, kept running for the call: started once (so the first caption doesn't wait for a
//! cold start), fed one caption at a time on its stdin and read back from its stdout, on a thread of its own so the
//! window never waits on it.
//!
//! - **Claude Code** runs as `claude -p --input-format stream-json --output-format stream-json`: one user message per
//!   caption in one conversation, its `result` line the answer. No tools, MCP servers, slash commands, settings files
//!   (so no hooks) or saved session.
//! - **Codex** runs as `codex app-server` (JSON-RPC over stdio): `initialize`, one ephemeral read-only thread whose
//!   base instructions are ours, then one `turn/start` per caption, its last agent message the answer. Hooks, apps,
//!   plugins, MCP servers, tools and project docs are turned off for it.
//!
//! **The call's context**: the process keeps the conversation, so each caption is read against the whole call so far
//! (the call's title and people, every caption with its speaker, what was said that wasn't sent, the translations it
//! gave) without that being sent again. The conversation is bounded: after [`Limits::turns`] captions or
//! [`Limits::chars`] characters sent, the tool is asked for the call's glossary (its names, terms and abbreviations
//! with the renderings it used), and a new process starts with the call's brief, that glossary and the recent
//! captions with their translations ([`Memory`]), so a long call stays consistent and each prompt stays small.
//!
//! **Failures** end the session ([`Failure`]): the tool missing, signed out, too slow, or stopped; the model shows the
//! captions as spoken and says why. Nothing here logs a caption, a translation or the tool's output.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{Backend, LineKey, instructions};

/// How long a wait on the tool polls before it looks at whether the session was dropped.
const POLL: Duration = Duration::from_millis(100);
/// The most of the tool's stderr kept, to tell a sign-in failure: its tail.
const STDERR_TAIL: usize = 4096;
/// The longest answer taken; anything longer isn't a caption's translation.
const MAX_ANSWER: usize = 2000;
/// The longest glossary carried to the next process.
const MAX_GLOSSARY: usize = 2000;
/// The most captions the recent transcript carried to the next process holds, and its most characters.
const RECENT_LINES: usize = 30;
const RECENT_CHARS: usize = 6000;
/// The longest brief (the call's title and people) sent.
const MAX_BRIEF: usize = 1500;
/// What a tool's refusal says when it isn't signed in.
const SIGN_IN: [&str; 9] = [
    "login",
    "log in",
    "sign in",
    "signed in",
    "logged in",
    "authenticat",
    "api key",
    "unauthorized",
    "credential",
];
/// What asks the tool for the call's glossary (the instructions say what to answer).
pub const GLOSSARY_REQUEST: &str = "<glossary-request/>";

/// When a session's process is replaced, and how long the tool may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Captions sent to one process before it is replaced.
    pub turns: usize,
    /// Characters sent to one process before it is replaced.
    pub chars: usize,
    /// How long the first answer may take (the process starting and signing in).
    pub first: Duration,
    /// How long any later answer may take.
    pub turn: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            turns: 150,
            chars: 60_000,
            first: Duration::from_secs(45),
            turn: Duration::from_secs(20),
        }
    }
}

/// What a session runs: the tool, where it is, the language it translates into, and its limits.
#[derive(Clone, Debug)]
pub struct Spec {
    pub backend: Backend,
    pub program: PathBuf,
    /// The target language's English name, as the instructions name it.
    pub language: &'static str,
    pub limits: Limits,
}

/// A line said in the call that goes to the tool as context only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub speaker: Option<String>,
    pub text: String,
}

/// One caption to translate, with what goes with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    pub key: LineKey,
    /// The caption's text as spoken.
    pub source: String,
    pub final_result: bool,
    pub speaker: Option<String>,
    /// What was said since the last caption sent that isn't sent for translation (already in the target language,
    /// answered from the cache, or dropped from a full queue), in order.
    pub said: Vec<Said>,
    /// The call's title and people, when they changed since the last caption sent.
    pub brief: Option<String>,
}

/// What a session tells the model.
#[derive(Debug)]
pub enum Outcome {
    Translated { job: Job, text: String },
    Failed(Failure),
}

/// Why a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The tool asked for isn't on the `PATH`; `None`: neither is.
    NotInstalled(Option<Backend>),
    SignedOut(Backend),
    TimedOut(Backend),
    /// The tool stopped or refused, in its own words when it gave some.
    Stopped(Backend, Option<String>),
}

impl Failure {
    /// What the caption pane says of it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NotInstalled(None) => {
                "Neither Claude Code (claude) nor the Codex CLI (codex) is installed.".to_owned()
            }
            Self::NotInstalled(Some(backend)) => {
                format!("{} ({}) isn't installed.", backend.tool(), backend.program())
            }
            Self::SignedOut(backend) => format!(
                "{} isn't signed in. Run {} in a terminal, then try again.",
                backend.tool(),
                backend.sign_in()
            ),
            Self::TimedOut(backend) => format!("{} took too long to answer.", backend.tool()),
            Self::Stopped(backend, Some(why)) => format!("{} stopped: {why}", backend.tool()),
            Self::Stopped(backend, None) => format!("{} stopped unexpectedly.", backend.tool()),
        }
    }
}

/// A running session: dropping it stops its thread and its process.
pub struct Session {
    jobs: mpsc::Sender<Job>,
    stop: Arc<AtomicBool>,
    outcomes: async_channel::Receiver<Outcome>,
}

impl Session {
    /// Starts `spec`'s tool on a thread of its own.
    #[must_use]
    pub fn start(spec: Spec) -> Self {
        let (jobs, queued) = mpsc::channel();
        let (out, outcomes) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let backend = spec.backend;
        let worker = Worker {
            spec,
            jobs: queued,
            out: out.clone(),
            stop: Arc::clone(&stop),
            memory: Memory::default(),
        };
        if let Err(error) = thread::Builder::new()
            .name("caption-translate".into())
            .spawn(move || worker.run())
        {
            let _ = out.try_send(Outcome::Failed(Failure::Stopped(backend, Some(error.to_string()))));
        }
        Self { jobs, stop, outcomes }
    }

    /// Hands `job` to the tool (nothing once the session ended: its failure says so).
    pub fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Where its outcomes come.
    #[must_use]
    pub fn outcomes(&self) -> async_channel::Receiver<Outcome> {
        self.outcomes.clone()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// What a session remembers of the call to start a new process with.
#[derive(Debug, Default)]
struct Memory {
    brief: String,
    glossary: Option<String>,
    /// The recent captions: speaker, text as spoken, and its translation (none for one said only).
    recent: VecDeque<(Option<String>, String, Option<String>)>,
}

impl Memory {
    /// The message `job` goes as: on a new process the call's brief, glossary and recent captions first; else the
    /// brief only when it changed.
    fn message(&mut self, job: &Job, fresh: bool) -> String {
        let mut message = String::new();
        let changed = job.brief.as_ref().is_some_and(|brief| *brief != self.brief);
        if let Some(brief) = &job.brief {
            self.brief = cap(brief, MAX_BRIEF);
        }
        if (fresh || changed) && !self.brief.is_empty() {
            let _ = write!(message, "<call>\n{}\n</call>\n", self.brief);
        }
        if fresh {
            self.earlier(&mut message);
        }
        if !job.said.is_empty() {
            message.push_str("<said>\n");
            for said in &job.said {
                let _ = writeln!(message, "{}: {}", who(said.speaker.as_deref()), clean(&said.text));
            }
            message.push_str("</said>\n");
        }
        let unfinished = if job.final_result { "" } else { " unfinished" };
        let _ = write!(
            message,
            "<caption from=\"{}\"{unfinished}>{}</caption>",
            who(job.speaker.as_deref()).replace('"', "'"),
            clean(&job.source)
        );
        message
    }

    /// The glossary and the recent captions, for a new process.
    fn earlier(&self, message: &mut String) {
        if let Some(glossary) = &self.glossary {
            let _ = write!(message, "<glossary>\n{glossary}\n</glossary>\n");
        }
        if self.recent.is_empty() {
            return;
        }
        message.push_str("<earlier>\n");
        for (speaker, source, text) in &self.recent {
            let _ = writeln!(message, "{}: {}", who(speaker.as_deref()), clean(source));
            if let Some(text) = text {
                let _ = writeln!(message, "=> {}", clean(text));
            }
        }
        message.push_str("</earlier>\n");
    }

    /// Keeps `job` (a final caption) and its translation, and what was said with it, in the recent captions.
    fn remember(&mut self, job: &Job, text: &str) {
        for said in &job.said {
            self.recent.push_back((said.speaker.clone(), said.text.clone(), None));
        }
        if job.final_result {
            self.recent
                .push_back((job.speaker.clone(), job.source.clone(), Some(text.to_owned())));
        }
        let size = |recent: &VecDeque<(Option<String>, String, Option<String>)>| {
            recent
                .iter()
                .map(|(_, source, text)| source.len() + text.as_ref().map_or(0, String::len))
                .sum::<usize>()
        };
        while self.recent.len() > RECENT_LINES || size(&self.recent) > RECENT_CHARS {
            self.recent.pop_front();
        }
    }
}

/// A speaker's name as a message gives it.
fn who(speaker: Option<&str>) -> &str {
    speaker.filter(|name| !name.trim().is_empty()).unwrap_or("Speaker")
}

/// Text as a message carries it: no angle brackets to be read as the message's own tags.
fn clean(text: &str) -> String {
    text.replace('<', "‹").replace('>', "›")
}

/// `text` cut to at most `max` bytes, on a character's boundary.
fn cap(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// The session's thread.
struct Worker {
    spec: Spec,
    jobs: Receiver<Job>,
    out: async_channel::Sender<Outcome>,
    stop: Arc<AtomicBool>,
    memory: Memory,
}

impl Worker {
    fn run(mut self) {
        let mut tool = match Tool::start(&self.spec, &self.stop) {
            Ok(tool) => tool,
            Err(failure) => return self.fail(failure),
        };
        while let Some(job) = self.next_job() {
            if tool.turns >= self.spec.limits.turns || tool.chars >= self.spec.limits.chars {
                tool = match self.renew(tool) {
                    Ok(tool) => tool,
                    Err(failure) => return self.fail(failure),
                };
            }
            let message = self.memory.message(&job, tool.turns == 0);
            let wait = if tool.answered {
                self.spec.limits.turn
            } else {
                self.spec.limits.first
            };
            match tool.ask(&message, wait, &self.stop) {
                Ok(text) => {
                    self.memory.remember(&job, &text);
                    if self.out.try_send(Outcome::Translated { job, text }).is_err() {
                        break;
                    }
                }
                Err(failure) => {
                    tool.close();
                    return self.fail(failure);
                }
            }
        }
        tool.close();
    }

    /// The next job, or `None` once the session was dropped.
    fn next_job(&self) -> Option<Job> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return None;
            }
            match self.jobs.recv_timeout(POLL) {
                Ok(job) => return Some(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Asks the worn process for the call's glossary (kept when it answers), ends it and starts the next.
    fn renew(&mut self, mut tool: Tool) -> Result<Tool, Failure> {
        if let Ok(glossary) = tool.ask(GLOSSARY_REQUEST, self.spec.limits.turn, &self.stop) {
            let glossary = glossary.trim();
            self.memory.glossary =
                (!glossary.is_empty() && !glossary.eq_ignore_ascii_case("none")).then(|| cap(glossary, MAX_GLOSSARY));
        }
        tool.close();
        Tool::start(&self.spec, &self.stop)
    }

    /// Tells the model, unless the session was dropped (nobody listens then).
    fn fail(&self, failure: Failure) {
        if !self.stop.load(Ordering::Relaxed) {
            let _ = self.out.try_send(Outcome::Failed(failure));
        }
    }
}

/// The running process and where its conversation stands.
struct Tool {
    backend: Backend,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    /// Codex's thread.
    thread: Option<String>,
    next_id: u64,
    /// Captions sent, and their characters.
    turns: usize,
    chars: usize,
    /// It answered once: later answers get the shorter wait.
    answered: bool,
}

impl Tool {
    fn start(spec: &Spec, stop: &AtomicBool) -> Result<Self, Failure> {
        let backend = spec.backend;
        let instructions = instructions(spec.language);
        let mut command = Command::new(&spec.program);
        command
            .args(backend.args(&instructions))
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // its own group, so whatever it starts ends with it
            .process_group(0);
        let mut child = command.spawn().map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotInstalled(Some(backend)),
            _ => Failure::Stopped(backend, Some(error.to_string())),
        })?;
        let stdin = child.stdin.take();
        let lines = child.stdout.take().map_or_else(
            || mpsc::channel().1,
            |stdout| {
                let (sender, lines) = mpsc::channel();
                thread::spawn(move || read_lines(stdout, &sender));
                lines
            },
        );
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            thread::spawn(move || keep_tail(pipe, &tail));
        }
        let mut tool = Self {
            backend,
            child,
            stdin,
            lines,
            stderr,
            thread: None,
            next_id: 1,
            turns: 0,
            chars: 0,
            answered: false,
        };
        if backend == Backend::Codex
            && let Err(failure) = tool.codex_thread(&instructions, spec.limits.first, stop)
        {
            tool.close();
            return Err(failure);
        }
        Ok(tool)
    }

    /// Codex: initializes the app server and starts the thread the captions go to.
    fn codex_thread(&mut self, instructions: &str, wait: Duration, stop: &AtomicBool) -> Result<(), Failure> {
        let deadline = Instant::now() + wait;
        let initialize = self.request(
            "initialize",
            &json!({"clientInfo": {"name": "ngteams", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        self.answer_of(initialize, deadline, stop)?;
        self.write(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let start = self.request(
            "thread/start",
            &json!({
                "model": Backend::Codex.model(),
                "cwd": cwd,
                "approvalPolicy": "never",
                "sandbox": "read-only",
                "ephemeral": true,
                "baseInstructions": instructions,
            }),
        )?;
        let started = self.answer_of(start, deadline, stop)?;
        let thread = started
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or(Failure::Stopped(self.backend, None))?;
        self.thread = Some(thread.to_owned());
        Ok(())
    }

    /// Sends JSON-RPC request `method`; its id.
    fn request(&mut self, method: &str, params: &Value) -> Result<u64, Failure> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// The result of JSON-RPC request `id`, answering any request the server makes meanwhile.
    fn answer_of(&mut self, id: u64, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            let message = self.next_message(deadline, stop)?;
            self.refuse_request(&message)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(self.refusal(error.get("message").and_then(Value::as_str)));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// The server asking something of the client (an approval): refused, as nothing here may be approved.
    fn refuse_request(&mut self, message: &Value) -> Result<(), Failure> {
        if let (Some(id), Some(_)) = (message.get("id"), message.get("method")) {
            self.write(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported"}}))?;
        }
        Ok(())
    }

    /// Sends `message` (a caption's, or the glossary request) and waits up to `wait` for the answer.
    fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        let deadline = Instant::now() + wait;
        self.turns += 1;
        self.chars += message.len();
        let text = match self.backend {
            Backend::Claude => self.ask_claude(message, deadline, stop),
            Backend::Codex => self.ask_codex(message, deadline, stop),
        }?;
        self.answered = true;
        Ok(answer(&text))
    }

    fn ask_claude(&mut self, message: &str, deadline: Instant, stop: &AtomicBool) -> Result<String, Failure> {
        self.write(&json!({"type": "user", "message": {"role": "user", "content": message}}))?;
        loop {
            let line = self.next_message(deadline, stop)?;
            if line.get("type").and_then(Value::as_str) != Some("result") {
                continue;
            }
            let said = line.get("result").and_then(Value::as_str);
            if line.get("is_error").and_then(Value::as_bool) == Some(true) {
                return Err(self.refusal(said));
            }
            return Ok(said.unwrap_or_default().to_owned());
        }
    }

    fn ask_codex(&mut self, message: &str, deadline: Instant, stop: &AtomicBool) -> Result<String, Failure> {
        let thread = self.thread.clone().unwrap_or_default();
        let id = self.request(
            "turn/start",
            &json!({"threadId": thread, "input": [{"type": "text", "text": message}], "effort": "low"}),
        )?;
        let (mut text, mut error) = (None::<String>, None::<String>);
        loop {
            let line = self.next_message(deadline, stop)?;
            self.refuse_request(&line)?;
            if line.get("id").and_then(Value::as_u64) == Some(id)
                && let Some(refused) = line.get("error")
            {
                return Err(self.refusal(refused.get("message").and_then(Value::as_str)));
            }
            let params = line.get("params").unwrap_or(&Value::Null);
            match line.get("method").and_then(Value::as_str) {
                Some("item/completed") if params.pointer("/item/type") == Some(&json!("agentMessage")) => {
                    text = params.pointer("/item/text").and_then(Value::as_str).map(str::to_owned);
                }
                Some("error") if params.get("willRetry") != Some(&json!(true)) => {
                    error = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                Some("turn/completed") => {
                    let failed = params.pointer("/turn/error/message").and_then(Value::as_str);
                    return match (error.or_else(|| failed.map(str::to_owned)), text) {
                        (Some(why), _) => Err(self.refusal(Some(&why))),
                        (None, Some(text)) => Ok(text),
                        (None, None) => Err(Failure::Stopped(self.backend, Some("it gave no answer.".to_owned()))),
                    };
                }
                _ => {}
            }
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), Failure> {
        let backend = self.backend;
        let stdin = self.stdin.as_mut().ok_or(Failure::Stopped(backend, None))?;
        let mut line = message.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| self.exited())
    }

    /// The tool's next JSON line (other lines skipped), by `deadline`.
    fn next_message(&mut self, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped(self.backend, None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut(self.backend));
            }
            match self.lines.recv_timeout(left.min(POLL)) {
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(self.exited()),
            }
        }
    }

    /// Why the process ended: signed out when its last words say so, else in those words.
    fn exited(&mut self) -> Failure {
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        // the stderr reader may still be taking its last bytes
        thread::sleep(Duration::from_millis(50));
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let last = tail.lines().rev().find(|line| !line.trim().is_empty());
        self.refusal(last)
    }

    /// A refusal in the tool's words: a sign-in one as such.
    fn refusal(&self, said: Option<&str>) -> Failure {
        let Some(said) = said.map(str::trim).filter(|said| !said.is_empty()) else {
            return Failure::Stopped(self.backend, None);
        };
        let lower = said.to_lowercase();
        if SIGN_IN.iter().any(|word| lower.contains(word)) {
            return Failure::SignedOut(self.backend);
        }
        let line = said.lines().next().unwrap_or(said);
        let mut why = cap(line, 160);
        if why.len() < line.len() {
            why.push('…');
        }
        Failure::Stopped(self.backend, Some(why))
    }

    /// Ends the process: its stdin closed (both tools leave then), and after a moment its whole group killed.
    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let group = rustix::process::Pid::from_child(&self.child);
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Hands each line the tool writes to the session, until it ends.
fn read_lines(stdout: impl std::io::Read, sender: &mpsc::Sender<String>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if sender.send(line).is_err() {
            break;
        }
    }
}

/// Keeps the last [`STDERR_TAIL`] bytes the tool writes to stderr.
fn keep_tail(mut pipe: impl std::io::Read, tail: &Mutex<String>) {
    let mut chunk = [0_u8; 1024];
    while let Ok(read @ 1..) = pipe.read(&mut chunk) {
        let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        tail.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if tail.len() > STDERR_TAIL {
            let cut = tail.len() - STDERR_TAIL;
            let cut = (cut..tail.len()).find(|at| tail.is_char_boundary(*at)).unwrap_or(0);
            tail.drain(..cut);
        }
    }
}

/// An answer as shown: trimmed, without a caption tag the tool may have repeated, and capped.
fn answer(text: &str) -> String {
    let mut text = text.trim();
    if let Some(rest) = text.strip_prefix("<caption")
        && let Some((_, rest)) = rest.split_once('>')
    {
        text = rest.trim_start();
    }
    let text = text.strip_suffix("</caption>").unwrap_or(text).trim();
    cap(text, MAX_ANSWER)
}

#[cfg(test)]
#[path = "../../tests/support/translate_session_units.rs"]
mod tests;
