//! Live, against the company tenant with the lab accounts: M1's acceptance run of the native client (M1 Task 21).
//!
//! This test is an orchestrator only. It opens no fence, holds no token and sends nothing: it spawns the lab binary
//! built by `cargo build -p teams-headless --target-dir target/lab` (its path in `TEAMS_HEADLESS_BIN`), one process
//! per account at a time, reads what the processes print, and parses `fence.log`. The binary this test build makes
//! has `test-support` unified in and refuses to run, so a wrong path fails the test rather than writes.
//!
//! - b watches the push channel (`watch --json`); a sends [`MESSAGES`] messages to b in their 1:1 chat, one `send`
//!   each; every one reaches b over Trouter, and the time from a's send start to b's receive has a p95 under
//!   [`P95_LIMIT_MS`] (p50, p95 and max reported, with the post and the delivery apart).
//! - a edits its first message and reacts to it; both reach b.
//! - The a–b–c group chat (`chats.toml`'s `abc`): c watches too, in a process of its own; a sends
//!   [`GROUP_MESSAGES`] messages to the group chat, and each reaches both b's and c's watch (latencies reported per
//!   receiver, not held to a limit). The fence lets these writes out only because it reads this one recorded chat as
//!   complete when both of its member reads agree on exactly a, b and c: an assumption the user accepted on
//!   2026-10-21, not evidence (`CLAUDE.md`, `docs/protocol/fence-targets.md`, "The lab's group chat").
//! - Catch-up: b's watch stops (its socket closes), a sends [`GAP_MESSAGES`] more, b watches again, and b's store then
//!   has every message of the run exactly once (each catch-up must sync every conversation); then b reads the chat up
//!   to the run's last message: a read receipt, which moves b's stored read horizon (the consumption horizon) but not
//!   b's web client's "last read" mark (`ab_chat::wait_for_newest` deals with that).
//! - b's store holds every group message exactly once, and so does c's.
//! - `fence.log` over the run: whole lines; allows only on the a–b chat (message, edit, reaction, read receipt) and
//!   messages on the recorded group chat; no deny at all, so none of a mapped write; only reviewed own-state requests
//!   and own Trouter frames; nothing else (no write on any other chat, no breach, no delivery response).
//!
//! Built only with `--features live`, and even then it skips unless `PROTO_LAB_LIVE=1`, so
//! `cargo test --all-features` stays offline. Run it under the lab's live lock (`flock
//! ~/.local/share/teams-lab/live.lock`), so no other lab run shares the accounts or `fence.log`. What the processes
//! print goes to `<data_dir>/headless/<time>-chat-live/` (outside the repo), with `summary.json`.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use teams_fence::lab::{Alias, LabChats, LabConfig, LabLocations, LabPaths};
use teams_fence::one_to_one_id;
use teams_proto::{Authenticate, Disconnect, EndpointRegistration, MessageLossAck, OwnFrame, OwnState, Ping};

/// How many messages a sends to b while b watches.
const MESSAGES: usize = 50;
/// How many messages a sends while b's watch is down.
const GAP_MESSAGES: usize = 3;
/// How many messages a sends to the a–b–c group chat while b and c watch.
const GROUP_MESSAGES: usize = 10;
/// The acceptance target for a's send start → b's receive, at the 95th percentile.
const P95_LIMIT_MS: i64 = 2000;
/// How long one one-shot command may take.
const COMMAND_LIMIT: Duration = Duration::from_secs(180);
/// How long a watch may take to register and catch up.
const READY_LIMIT: Duration = Duration::from_secs(180);
/// How long a message, an edit or a reaction may take to reach a watch after its send returned.
const ARRIVAL_LIMIT: Duration = Duration::from_secs(60);
/// How long b's second watch stays up after it caught up.
const SETTLE: Duration = Duration::from_secs(10);
/// How many messages `show` reads back: every message of the run, with room to spare.
const SHOW_LIMIT: usize = 200;

/// The lab as the run needs it: the binary, the chats it writes to, `fence.log` and where the output goes.
struct Live {
    binary: PathBuf,
    ab_chat: String,
    group_chat: String,
    fence_log: PathBuf,
    out: PathBuf,
}

/// The lab, or `None` (after a note) when live tests aren't asked for. Fails without `TEAMS_HEADLESS_BIN`.
fn live() -> Option<Live> {
    if std::env::var("PROTO_LAB_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: a live lab test; set PROTO_LAB_LIVE=1 to run it");
        return None;
    }
    let binary = std::env::var_os("TEAMS_HEADLESS_BIN").map_or_else(
        || panic!("TEAMS_HEADLESS_BIN is unset: build with `cargo build -p teams-headless --target-dir target/lab`"),
        PathBuf::from,
    );
    assert!(
        binary.is_file(),
        "TEAMS_HEADLESS_BIN {} is not a file",
        binary.display()
    );
    let config = LabConfig::load(&LabLocations::from_env().unwrap()).unwrap();
    let mri = |alias| config.account(alias).mri.clone().unwrap();
    let ab_chat = one_to_one_id(&mri(Alias::A), &mri(Alias::B)).unwrap();
    let group_chat = LabChats::load(config.paths()).unwrap().abc.unwrap();
    Some(Live {
        binary,
        ab_chat,
        group_chat,
        fence_log: config.paths().fence_log(),
        out: output_dir(config.paths()),
    })
}

fn now_ms() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_millis()).unwrap()
}

/// `<data_dir>/headless/<UTC time>-chat-live/`, `0700`.
fn output_dir(paths: &LabPaths) -> PathBuf {
    let stamp = Command::new("date").args(["-u", "+%Y%m%dT%H%M%SZ"]).output().unwrap();
    let stamp = String::from_utf8(stamp.stdout).unwrap();
    let dir = paths
        .data_dir()
        .join("headless")
        .join(format!("{}-chat-live", stamp.trim()));
    DirBuilder::new().recursive(true).mode(0o700).create(&dir).unwrap();
    dir
}

/// Sends a signal to a child process by name (`TERM`, `INT`).
fn signal(child: &Child, name: &str) {
    let _ = Command::new("kill")
        .args([format!("-{name}"), child.id().to_string()])
        .status();
}

/// `child`'s exit, if it comes within `limit`.
fn exit_within(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Waits for `child` up to `limit`, then interrupts it (SIGINT), and kills it 30 s later.
fn wait_bounded(child: &mut Child, limit: Duration) -> ExitStatus {
    if let Some(status) = exit_within(child, limit) {
        return status;
    }
    eprintln!("a command ran over its limit of {limit:?}: interrupting it");
    signal(child, "INT");
    if let Some(status) = exit_within(child, Duration::from_secs(30)) {
        return status;
    }
    let _ = child.kill();
    child.wait().unwrap()
}

/// What a one-shot command printed, and how it exited.
struct Run {
    status: ExitStatus,
    stdout: String,
}

impl Run {
    /// The value of a `<key> <value>` line.
    fn field(&self, key: &str) -> Option<&str> {
        self.stdout
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
    }
}

impl Live {
    /// Spawns `teams-headless <args>` with its output in files named `name` under the output dir.
    fn spawn(&self, name: &str, args: &[&str]) -> Child {
        let file = |suffix: &str| fs::File::create(self.out.join(format!("{name}.{suffix}"))).unwrap();
        Command::new(&self.binary)
            .args(args)
            .stdin(Stdio::null())
            .stdout(file("out"))
            .stderr(file("err"))
            .spawn()
            .unwrap()
    }

    /// Runs a one-shot command, bounded by [`COMMAND_LIMIT`].
    fn run(&self, name: &str, args: &[&str]) -> Run {
        let mut child = self.spawn(name, args);
        let status = wait_bounded(&mut child, COMMAND_LIMIT);
        let stdout = fs::read_to_string(self.out.join(format!("{name}.out"))).unwrap();
        Run { status, stdout }
    }
}

/// A `watch --json` running in the background; stopped (SIGTERM) when dropped.
struct Watch {
    child: Option<Child>,
    path: PathBuf,
    name: String,
}

impl Watch {
    fn start(live: &Live, alias: &str, name: &str) -> Self {
        let child = live.spawn(name, &[alias, "watch", "--json"]);
        let mut watch = Self {
            child: Some(child),
            path: live.out.join(format!("{name}.out")),
            name: name.to_owned(),
        };
        let failed = watch.until(READY_LIMIT, "registered and caught up", |events| {
            let registered = events
                .iter()
                .any(|event| event["event"] == "registration" && event["registered"] == true);
            let caught_up = events
                .iter()
                .find(|event| event["event"] == "caught_up" && event["cause"] == "connected");
            caught_up.filter(|_| registered).map(|event| event["failed"].clone())
        });
        // A conversation the catch-up couldn't sync fails the run: the service's replay of recent events could
        // still fill b's store and hide it.
        assert_eq!(failed, 0, "{name}: its catch-up failed on some conversations");
        watch
    }

    /// The events printed so far (a line still being written left out).
    fn events(&self) -> Vec<Value> {
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        text.split_inclusive('\n')
            .filter(|line| line.ends_with('\n'))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits until `found` finds something in the events, up to `limit`; fails if the watch ended first.
    fn until<T>(&mut self, limit: Duration, what: &str, found: impl Fn(&[Value]) -> Option<T>) -> T {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(value) = found(&self.events()) {
                return value;
            }
            assert!(self.running(), "{}: ended before it {what}", self.name);
            assert!(Instant::now() < deadline, "{}: not {what} within {limit:?}", self.name);
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Stops the watch (SIGTERM) and gives its exit.
    fn stop(&mut self) -> ExitStatus {
        let mut child = self.child.take().unwrap();
        signal(&child, "TERM");
        wait_bounded(&mut child, Duration::from_secs(60))
    }

    fn running(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| child.try_wait().unwrap().is_none())
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            signal(&child, "TERM");
            wait_bounded(&mut child, Duration::from_secs(60));
        }
    }
}

/// One of a's posts as a printed it, and as b received it.
#[derive(Clone, Debug)]
struct Sent {
    text: String,
    id: String,
    start_ms: i64,
    arrival_ms: Option<i64>,
}

/// a sends `text` to b; fails unless it went through.
fn send(live: &Live, name: &str, text: &str) -> Sent {
    send_to(live, name, "b", text)
}

/// a sends `text` to `chat` (as `teams-headless` names a chat: `b`, `abc`); fails unless it went through.
fn send_to(live: &Live, name: &str, chat: &str, text: &str) -> Sent {
    let run = live.run(name, &["a", "send", chat, text]);
    assert!(run.status.success(), "{name}: {:?}\n{}", run.status, run.stdout);
    assert_eq!(run.field("state"), Some("sent"), "{name}");
    Sent {
        text: text.to_owned(),
        id: run.field("message_id").unwrap().to_owned(),
        start_ms: run.field("start_ms").unwrap().parse().unwrap(),
        arrival_ms: run.field("original_arrival_time").and_then(|time| time.parse().ok()),
    }
}

/// When a watch received a message event of `change` for `id` in `chat` over the push channel (the first one): a
/// read's event for the same message (a catch-up after a drop or a message-loss report) is no delivery, and a
/// delivery it stood in for may have been lost.
fn received(events: &[Value], chat: &str, id: &str, change: &str) -> Option<i64> {
    events
        .iter()
        .find(|event| {
            event["event"] == "message"
                && event["source"] == "delivery"
                && event["conversation"] == chat
                && event["id"] == id
                && event["change"] == change
        })
        .and_then(|event| event["received_ms"].as_i64())
}

/// The nearest-rank percentile of sorted values.
fn percentile(sorted: &[i64], percent: usize) -> i64 {
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn stats(mut values: Vec<i64>) -> Value {
    values.sort_unstable();
    if values.is_empty() {
        return Value::Null;
    }
    json!({
        "n": values.len(),
        "min": values[0],
        "p50": percentile(&values, 50),
        "p95": percentile(&values, 95),
        "max": values[values.len() - 1],
    })
}

/// Latency per message in `chat`: send start → the watch's receive, and its two parts (start →
/// `OriginalArrivalTime`, the post; and `OriginalArrivalTime` → the receive, the delivery; the service's clock against
/// ours, so either part may be off by the clocks' skew).
fn latency(sent: &[Sent], watch: &Watch, chat: &str) -> Value {
    let events = watch.events();
    let mut total = Vec::new();
    let mut post = Vec::new();
    let mut delivery = Vec::new();
    for message in sent {
        let receive = received(&events, chat, &message.id, "new").unwrap();
        total.push(receive - message.start_ms);
        if let Some(arrival) = message.arrival_ms {
            post.push(arrival - message.start_ms);
            delivery.push(receive - arrival);
        }
    }
    json!({"send_to_receive": stats(total), "post": stats(post), "delivery": stats(delivery)})
}

/// The live acceptance run, in its steps; see the module's docs.
#[test]
fn a_reaches_b_natively_and_the_fence_holds() {
    let Some(live) = live() else {
        return;
    };
    let nonce = now_ms();
    let log_start = fs::metadata(&live.fence_log).map_or(0, |meta| meta.len());
    let started_ms = now_ms();
    let mut watch = Watch::start(&live, "b", "b-watch-1");
    let sent = send_all(&live, &mut watch, nonce);
    let latency = latency(&sent, &watch, &live.ab_chat);
    let edit_and_reaction = edit_and_react(&live, &mut watch, &sent[0]);
    let (group, group_latency) = group_reaches_b_and_c(&live, &mut watch, nonce);
    assert!(watch.stop().success(), "b's first watch didn't exit cleanly");
    let gap = catch_up(&live, nonce);
    let mut texts: Vec<String> = sent
        .iter()
        .skip(1)
        .chain(&gap)
        .map(|message| message.text.clone())
        .collect();
    texts.push(edited_text(&sent[0]));
    every_message_once(&live, "b", "a", &texts);
    let group_texts: Vec<String> = group.iter().map(|message| message.text.clone()).collect();
    every_message_once(&live, "b", "abc", &group_texts);
    every_message_once(&live, "c", "abc", &group_texts);
    b_reads_up_to(&live, gap.last().unwrap());
    let fence = fence_log_holds(&live, log_start);
    no_token_printed(&live.out);
    let summary = json!({
        "started_ms": started_ms,
        "ended_ms": now_ms(),
        "messages": MESSAGES,
        "gap_messages": GAP_MESSAGES,
        "group_messages": GROUP_MESSAGES,
        "latency_ms": latency,
        "edit_and_reaction_ms": edit_and_reaction,
        "group_latency_ms": group_latency,
        "fence_log": fence,
    });
    fs::write(live.out.join("summary.json"), format!("{summary:#}\n")).unwrap();
    eprintln!("{summary:#}");
    let p95 = latency["send_to_receive"]["p95"].as_i64().unwrap();
    assert!(
        p95 < P95_LIMIT_MS,
        "send → receive p95 {p95} ms, not under {P95_LIMIT_MS} ms"
    );
}

/// a's [`MESSAGES`] posts, one `send` each, then each of them on b's watch.
fn send_all(live: &Live, watch: &mut Watch, nonce: i64) -> Vec<Sent> {
    let sent: Vec<Sent> = (1..=MESSAGES)
        .map(|n| {
            let text = format!("M1 Task 21 live {nonce} m{n:02}");
            send(live, &format!("a-send-{n:02}"), &text)
        })
        .collect();
    for message in &sent {
        watch.until(ARRIVAL_LIMIT, &format!("received {}", message.text), |events| {
            received(events, &live.ab_chat, &message.id, "new")
        });
    }
    sent
}

fn edited_text(message: &Sent) -> String {
    format!("{} edited", message.text)
}

/// a edits its first message and reacts to it; b receives both (their times from the write's start, after the
/// command's catch-up).
fn edit_and_react(live: &Live, watch: &mut Watch, first: &Sent) -> Value {
    let text = edited_text(first);
    let edit = live.run("a-edit", &["a", "edit", "b", &first.id, &text]);
    assert!(
        edit.status.success() && edit.field("state") == Some("sent"),
        "edit: {}",
        edit.stdout
    );
    let edit_start: i64 = edit.field("start_ms").unwrap().parse().unwrap();
    let edited = watch.until(ARRIVAL_LIMIT, "received the edit", |events| {
        events.iter().find_map(|event| {
            (event["event"] == "message"
                && event["source"] == "delivery"
                && event["id"] == first.id.as_str()
                && event["change"] == "edit"
                && event["text"] == text.as_str())
            .then(|| event["received_ms"].as_i64())
            .flatten()
        })
    });
    let react = live.run("a-react", &["a", "react", "b", &first.id, "like"]);
    assert!(
        react.status.success() && react.field("state") == Some("sent"),
        "react: {}",
        react.stdout
    );
    let react_start: i64 = react.field("start_ms").unwrap().parse().unwrap();
    let reacted = watch.until(ARRIVAL_LIMIT, "received the reaction", |events| {
        received(events, &live.ab_chat, &first.id, "reaction")
    });
    json!({"edit": edited - edit_start, "reaction": reacted - react_start})
}

/// c watches too (its own process, ready as b's was); a sends [`GROUP_MESSAGES`] messages to the a–b–c group chat, one
/// `send` each, and each of them reaches both b's watch (`b_watch`, still up) and c's. Gives a's posts and the
/// latencies per receiver; c's watch is stopped before it returns.
fn group_reaches_b_and_c(live: &Live, b_watch: &mut Watch, nonce: i64) -> (Vec<Sent>, Value) {
    let mut c_watch = Watch::start(live, "c", "c-watch");
    let sent: Vec<Sent> = (1..=GROUP_MESSAGES)
        .map(|n| {
            let text = format!("M1 Task 21 live {nonce} group g{n:02}");
            send_to(live, &format!("a-send-abc-{n:02}"), "abc", &text)
        })
        .collect();
    for message in &sent {
        for watch in [&mut *b_watch, &mut c_watch] {
            watch.until(ARRIVAL_LIMIT, &format!("received {}", message.text), |events| {
                received(events, &live.group_chat, &message.id, "new")
            });
        }
    }
    let latency = json!({
        "b": latency(&sent, b_watch, &live.group_chat),
        "c": latency(&sent, &c_watch, &live.group_chat),
    });
    assert!(c_watch.stop().success(), "c's watch didn't exit cleanly");
    (sent, latency)
}

/// b's socket closed: a sends [`GAP_MESSAGES`], then b watches again, catching up on connect.
fn catch_up(live: &Live, nonce: i64) -> Vec<Sent> {
    let gap: Vec<Sent> = (1..=GAP_MESSAGES)
        .map(|n| {
            send(
                live,
                &format!("a-send-gap-{n}"),
                &format!("M1 Task 21 live {nonce} gap{n}"),
            )
        })
        .collect();
    let mut watch = Watch::start(live, "b", "b-watch-2");
    std::thread::sleep(SETTLE);
    assert!(watch.stop().success(), "b's second watch didn't exit cleanly");
    gap
}

/// The text of a `show` line: after `<arrival> <id> <sender>[ (edited)]: `, without the reactions.
fn shown_text(line: &str) -> Option<&str> {
    let (_, text) = line.split_once(": ")?;
    Some(match text.rfind(" [") {
        Some(at) if text.ends_with(']') => &text[..at],
        _ => text,
    })
}

/// `alias`'s store has each of `texts` exactly once in `chat` (as `teams-headless` names a chat: `a`, `abc`).
fn every_message_once(live: &Live, alias: &str, chat: &str, texts: &[String]) {
    let name = format!("{alias}-show-{chat}");
    let show = live.run(&name, &[alias, "show", chat, "--limit", &SHOW_LIMIT.to_string()]);
    assert!(show.status.success(), "{name}: {:?}", show.status);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for text in show.stdout.lines().filter_map(shown_text) {
        *counts.entry(text).or_default() += 1;
    }
    for text in texts {
        assert_eq!(counts.get(text.as_str()), Some(&1), "{text} in {alias}'s store");
    }
}

/// b reads the a–b chat up to `last`: the live check of the read-receipt write, which moves b's stored read horizon
/// (the consumption horizon). It doesn't move b's web client's "last read" mark, so that client still opens the chat
/// there (`proto-lab run ab-chat` clicks "Jump to newest" or "New messages", `ab_chat::wait_for_newest`).
fn b_reads_up_to(live: &Live, last: &Sent) {
    let read = live.run("b-read", &["b", "read", "a", &last.id]);
    assert!(
        read.status.success() && read.field("state") == Some("sent"),
        "read: {}",
        read.stdout
    );
}

/// What `fence.log` holds for the run, counted by line kind.
#[derive(Debug, Default)]
struct FenceCounts {
    writes: BTreeMap<String, usize>,
    group_writes: BTreeMap<String, usize>,
    own_requests: BTreeMap<String, usize>,
    own_frames: BTreeMap<String, usize>,
}

/// `fence.log`'s lines since `start` (a byte offset): whole JSON lines; allows only for the a–b chat's message,
/// edit, reaction and read receipt and the recorded group chat's messages; no deny; own-state requests and frames only
/// of the reviewed kinds.
fn fence_log_holds(live: &Live, start: u64) -> Value {
    let log = fs::read(&live.fence_log).unwrap();
    let run = &log[usize::try_from(start).unwrap()..];
    assert!(run.is_empty() || run.ends_with(b"\n"), "fence.log ends in a half line");
    let mut counts = FenceCounts::default();
    let mut lines = 0;
    for line in std::str::from_utf8(run).unwrap().lines() {
        lines += 1;
        let entry: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("not a whole line: {error}"));
        counts.add(live, &entry);
    }
    let messages = counts.writes.get("message").copied();
    assert_eq!(messages, Some(MESSAGES + GAP_MESSAGES), "{counts:?}");
    assert_eq!(counts.writes.get("edit"), Some(&1), "{counts:?}");
    assert_eq!(counts.writes.get("reaction"), Some(&1), "{counts:?}");
    assert_eq!(counts.writes.get("read_receipt"), Some(&1), "{counts:?}");
    assert_eq!(counts.writes.len(), 4, "{counts:?}");
    let group = BTreeMap::from([("message".to_owned(), GROUP_MESSAGES)]);
    assert_eq!(counts.group_writes, group, "{counts:?}");
    json!({
        "lines": lines,
        "allowed_writes": counts.writes,
        "allowed_group_writes": counts.group_writes,
        "own_requests": counts.own_requests,
        "own_frames": counts.own_frames,
    })
}

impl FenceCounts {
    fn add(&mut self, live: &Live, entry: &Value) {
        let verdict = entry["verdict"].as_str().unwrap_or_default();
        if let Some(kind) = entry["kind"].as_str() {
            let conversation = entry["scope"]["conversation"]["id"].as_str().unwrap_or_default();
            // The run's writes are all allowed: a deny is a mapped write the fence refused (a group chat's read
            // that didn't come out complete among them).
            assert_eq!(verdict, "allow", "a {kind} denied: {}", entry["reason"]);
            let writes = if conversation == live.ab_chat {
                assert!(
                    ["message", "edit", "reaction", "read_receipt"].contains(&kind),
                    "a {kind} allowed on the a–b chat"
                );
                &mut self.writes
            } else {
                // Only the recorded group chat, and only a's messages: a write allowed on any other chat (another
                // group chat among them) fails the run.
                assert!(
                    conversation == live.group_chat,
                    "a {kind} allowed off the a–b and the recorded group chat"
                );
                assert_eq!(kind, "message", "a {kind} allowed on the group chat");
                &mut self.group_writes
            };
            *writes.entry(kind.to_owned()).or_default() += 1;
            return;
        }
        assert_eq!(verdict, "allow", "a line that is no allow: {}", entry["event"]);
        let own = entry["own"].as_str().unwrap_or_default().to_owned();
        if entry["event"] == "frame" {
            let frames = [Authenticate::KIND, Ping::KIND, MessageLossAck::KIND, Disconnect::KIND];
            assert!(frames.contains(&own.as_str()), "a frame off the list: {own}");
            *self.own_frames.entry(own).or_default() += 1;
        } else {
            assert_eq!(entry["channel"], "http", "a line of another kind");
            assert_eq!(own, EndpointRegistration::KIND, "a request off the list");
            *self.own_requests.entry(own).or_default() += 1;
        }
    }
}

/// No output of the run holds a token: no JWT (`eyJ…`), no bearer or skypetoken header.
fn no_token_printed(out: &Path) {
    for entry in fs::read_dir(out).unwrap() {
        let path = entry.unwrap().path();
        let text = fs::read_to_string(&path).unwrap();
        for marker in ["eyJ", "Bearer ", "skypetoken="] {
            assert!(!text.contains(marker), "{} holds {marker}", path.display());
        }
    }
}
