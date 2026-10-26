//! Live, against the company tenant with the lab accounts: M2's acceptance run of the app (M2 Task 25).
//!
//! This test is an orchestrator only. It opens no fence, holds no token and sends nothing: it starts a UI sandbox per
//! lab account (a nested niri on a headless weston, a private bus with its fake notification server and tray watcher,
//! the no-op opener; `ui-sandbox`), spawns in each the app built by `cargo build -p teams-app --target-dir target/lab`
//! (its path in `TEAMS_APP_BIN`) for a, b and c, one app process each, drives them by keyboard through the sandbox's
//! virtual keyboard (the chat switcher, the compose box, the message list's keys, the search; never a screen
//! coordinate), and reads their view traces (ids and kinds, never text) and `fence.log`. The binary this test build
//! makes has `test-support` unified in and refuses to run, so a wrong path fails the test rather than writes.
//!
//! **Credentials.** For each account it starts that binary's credential broker (`teams-app secrets --lab <alias>`)
//! outside the sandbox, with this process's session bus (the user's, where the lab's tokens are), and hands the other
//! end of the broker's socket pair to the app it spawns in the sandbox (`--secrets-fd`). It never reads or writes the
//! socket itself: both ends are closed here once the two processes hold them. The app's environment is the sandbox's
//! (checked from `/proc`: its bus and its compositor, no `DISPLAY`). Every broker has exited when the run ends.
//!
//! **The steps** (each app in a sandbox of its own, so each window is focused, as a user's would be; each app started
//! has run its first catch-up, `status caught-up`, before the run goes on):
//! - a and b open their 1:1 chat, c the a–b–c group chat (`chats.toml`'s `abc`), by the switcher (Ctrl+K, Down to the
//!   chat's row as the chat list's trace orders them, Enter).
//! - a types and sends [`MESSAGES`] messages to b; each is shown in b's chat view; send (a's Enter, pressed once the
//!   app has taken in what was typed: its `compose changed` lines have gone quiet) → shown (b's `chat-view shown`
//!   naming it: the id new to b's view and to a's since before the send) has a p95 under [`P95_LIMIT_MS`] (p50, p95
//!   and max reported, with Enter → a's `compose sent` and `compose sent` → shown apart).
//! - a edits its last message (Up in the empty box), replies to it (Escape to the list, Down to the newest, `r`) and
//!   reacts to the reply (`a`, `1`): b shows the edit, the reply and the reaction.
//! - a sends to the group chat: c shows it; b, viewing the 1:1 chat, gets a notification on its sandbox's bus, whose
//!   activation opens the group chat, where b shows the message.
//! - names and presence: a manual status an earlier run left in a's settings is reset first ("Set status:
//!   Automatic"); b's header for the 1:1 chat names a from people's names with a's picture, and shows a available;
//!   a sets Busy from the switcher's "Set status: Busy", b's header shows busy; a sets Automatic again, b's header
//!   shows available.
//! - b's search (Ctrl+Shift+F) finds one of a's messages; a channel opens read-only from the switcher ("General"):
//!   no compose box is made for it (no `compose` line, `opened` included, while it is shown); nothing is typed in it.
//! - b's window closed into the tray: a sends, b notifies once, and the window opened again from the tray shows it.
//! - b's app quit from the tray (its broker exits with it): a sends [`GAP_MESSAGES`], b's app started again shows every
//!   message of the run in the 1:1 chat exactly once.
//! - Images and files only as far as M2 Tasks 21–23 got: none is sent (sending stays deferred).
//! - `fence.log` over the run: whole lines; allows only on the a–b chat and the recorded group chat (their messages,
//!   edits, reactions, read receipts, typing) and the lab accounts' own availability; no deny; requests let through
//!   because they reach nobody (logged `allowed`: the registration, profile lookups and presence reads, whoever they
//!   name); no frame (the native sink writes none to `fence.log`).
//!
//! Screenshots of each step go to `<data_dir>/screenshots/<time>/` (`0700`), and the traces, the processes' output and
//! `summary.json` to `<data_dir>/ui-live/<time>/`, never the repo. No file kept there holds a token (the apps' and the
//! brokers' stdout and stderr, the traces, `summary.json`, which holds every step this process printed); an app still
//! running when the run fails is asked to quit from its tray, killed when it doesn't, and its output kept there, never
//! printed. The harness (the apps, their traces, the run's dirs and numbers) is `tests/common`, which `call_live`
//! shares.
//!
//! Built only with `--features live`, and even then it skips unless `PROTO_LAB_LIVE=1`, so
//! `cargo test --all-features` stays offline. Run it under the lab's live lock (`flock
//! ~/.local/share/teams-lab/live.lock`) with an outer `timeout --signal=INT`, so no other lab run shares the accounts
//! or `fence.log`.

// test code throughout (its helpers unwrap and panic like the tests do)
#![cfg(test)]

#[expect(
    dead_code,
    reason = "the live harness `call_live` shares: its trace readers and process id are the calls'"
)]
mod common;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teams_fence::lab::{Alias, LabChats, LabConfig, LabLocations};
use teams_fence::one_to_one_id;
use teams_ui::settings::{ManualStatus, SettingsFile};
use teams_ui::tray::ITEM_PATH;
use ui_sandbox::input::{Modifier, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, SandboxOptions, Tools};

use common::{App, Launch, STARTUP, STEP, is, no_token_kept, now_ms, private_dir, stamp, stats, strings};

/// How many messages a sends to b while b shows their chat.
const MESSAGES: usize = 20;
/// How many messages a sends while b's app is closed.
const GAP_MESSAGES: usize = 3;
/// The acceptance target for a's Enter → b's chat view showing the message, at the 95th percentile.
const P95_LIMIT_MS: i64 = 2000;
/// How long a message, an edit, a reaction or a notification may take to show on the other side.
const ARRIVAL: Duration = Duration::from_secs(60);
/// How long a presence change may take to show (the engine reads presence once a minute).
const PRESENCE: Duration = Duration::from_secs(180);
/// How long the run waits after a notification for a second one of the same message, which mustn't come.
const QUIET: Duration = Duration::from_secs(5);
/// How long the run lets the switcher take in a query before it presses Enter (the switcher traces no keystroke): the
/// debug build takes in the virtual keyboard's keystrokes more slowly than it types them.
const SETTLE: Duration = Duration::from_secs(4);
/// The compose box has taken in what was typed once no `compose changed` came for this long: Enter is pressed only
/// then, so it doesn't wait behind keystrokes the debug build hasn't handled yet.
const TYPED: Duration = Duration::from_millis(1500);
/// The sandboxes' refresh rate, mHz: three compositor pairs repainting at 60 Hz take CPU the apps need, and the trace
/// tells what a view shows when its model changes, not when a frame is presented, so a slower output doesn't move it.
const REFRESH_MHZ: u32 = 20_000;

/// The lab as the run needs it.
struct Lab {
    binary: PathBuf,
    ab_chat: String,
    group_chat: String,
    accounts: Vec<String>,
    /// a's settings file, where its manual status is kept across runs.
    a_settings: PathBuf,
    fence_log: PathBuf,
    out: PathBuf,
    shots: PathBuf,
    tools: Tools,
}

/// The lab, or `None` (after a note) when live tests aren't asked for. Fails without `TEAMS_APP_BIN` or the sandbox's
/// tools.
fn lab() -> Option<Lab> {
    if std::env::var("PROTO_LAB_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: a live lab test; set PROTO_LAB_LIVE=1 to run it");
        return None;
    }
    let binary = std::env::var_os("TEAMS_APP_BIN").map_or_else(
        || panic!("TEAMS_APP_BIN is unset: build with `cargo build -p teams-app --target-dir target/lab`"),
        PathBuf::from,
    );
    assert!(binary.is_file(), "TEAMS_APP_BIN {} is not a file", binary.display());
    let tools = Tools::find().unwrap_or_else(|missing| panic!("the UI sandbox's tools: {missing}"));
    let config = LabConfig::load(&LabLocations::from_env().unwrap()).unwrap();
    let mri = |alias| config.account(alias).mri.clone().unwrap();
    let ab_chat = one_to_one_id(&mri(Alias::A), &mri(Alias::B)).unwrap();
    let group_chat = LabChats::load(config.paths()).unwrap().abc.unwrap();
    let stamp = stamp();
    Some(Lab {
        binary,
        ab_chat,
        group_chat,
        accounts: [Alias::A, Alias::B, Alias::C].map(mri).to_vec(),
        a_settings: config.paths().ui_settings_file(Alias::A),
        fence_log: config.paths().fence_log(),
        out: private_dir(&config.paths().data_dir().join("ui-live").join(&stamp)),
        shots: private_dir(&config.paths().data_dir().join("screenshots").join(&stamp)),
        tools,
    })
}

/// Whether `line` is a `chat-view shown` of `chat`.
fn chat_view_of(line: &Value, chat: &str) -> bool {
    is(line, "chat-view", "shown") && line["conversation"] == chat
}

/// The kind a `chat-view shown` line gives message `id`.
fn kind_of(line: &Value, id: &str) -> Option<String> {
    let at = strings(line, "messages").iter().position(|shown| shown == id)?;
    strings(line, "kinds").get(at).cloned()
}

// --- the apps ---

/// a's, b's or c's app in its sandbox, ready once its push channel connected and its first catch-up ran.
fn app(lab: &Lab, alias: &'static str) -> App {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        refresh_mhz: Some(REFRESH_MHZ),
        ..SandboxOptions::default()
    };
    let launch = Launch {
        binary: lab.binary.clone(),
        out: lab.out.clone(),
        shots: lab.shots.clone(),
        env: Vec::new(),
        ready: vec![("status", "connected"), ("status", "caught-up")],
    };
    App::new(&lab.tools, alias, &options, launch)
}

impl App {
    /// Types `text` into the compose box that has the focus, and waits until the app has taken it in: a `compose
    /// changed` came, and then none for [`TYPED`].
    fn type_into_compose(&mut self, text: &str) {
        let from = self.trace().mark();
        self.type_text(text);
        let changed = |line: &Value| is(line, "compose", "changed");
        self.trace().wait_from(from, STEP, "compose changed", changed);
        let deadline = Instant::now() + STEP;
        let mut seen = self.trace().count_from(from, changed);
        loop {
            thread::sleep(TYPED);
            let now = self.trace().count_from(from, changed);
            if now == seen {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: the compose box kept changing",
                self.alias
            );
            seen = now;
        }
    }
}

// --- the run ---

/// What the run found, step by step, for `summary.json`.
#[derive(Default)]
struct Found {
    steps: BTreeMap<&'static str, Value>,
}

impl Found {
    fn step(&mut self, name: &'static str, value: Value) {
        eprintln!("ui_live: {name}: {value}");
        self.steps.insert(name, value);
    }
}

/// The run's three apps.
struct Apps {
    a: App,
    b: App,
    c: App,
}

/// The live acceptance run, in its steps; see the module's docs.
#[test]
fn the_app_reaches_b_and_c_through_the_ui_and_the_fence_holds() {
    let Some(lab) = lab() else {
        return;
    };
    let nonce = now_ms() % 10_000_000;
    let log_start = fs::metadata(&lab.fence_log).map_or(0, |meta| meta.len());
    let started_ms = now_ms();
    let mut found = Found::default();
    let mut apps = Apps {
        a: app(&lab, "a"),
        b: app(&lab, "b"),
        c: app(&lab, "c"),
    };
    open_the_chats(&lab, &mut apps);
    let mut sent = latency(&lab, &mut apps, nonce, &mut found);
    sent.extend(edit_reply_react(&lab, &mut apps, nonce, &mut found));
    group_message(&lab, &mut apps, nonce, &mut found);
    names_and_presence(&lab, &mut apps, &mut found);
    search_and_channel(&lab, &mut apps, nonce, &sent[6], &mut found);
    sent.push(closed_into_the_tray(&lab, &mut apps, nonce, &mut found));
    sent.extend(quit_and_reopened(&lab, &mut apps, nonce, &mut found));
    every_message_once(&mut apps.b, &lab.ab_chat, &sent, &mut found);
    for app in [&mut apps.a, &mut apps.b, &mut apps.c] {
        app.shot("end");
        app.quit();
    }
    drop(apps);
    let fence = fence_log_holds(&lab, log_start);
    let summary = json!({
        "started_ms": started_ms,
        "ended_ms": now_ms(),
        "messages": MESSAGES,
        "gap_messages": GAP_MESSAGES,
        "steps": found.steps,
        "fence_log": fence,
        "screenshots": lab.shots,
    });
    fs::write(lab.out.join("summary.json"), format!("{summary:#}\n")).unwrap();
    // the summary holds every step this process printed
    no_token_kept(&lab.out);
    eprintln!("{summary:#}");
    let p95 = summary["steps"]["latency"]["send_to_shown_ms"]["p95"].as_i64().unwrap();
    assert!(
        p95 < P95_LIMIT_MS,
        "send → shown p95 {p95} ms, not under {P95_LIMIT_MS} ms"
    );
}

/// a and b open their 1:1 chat, c the group chat.
fn open_the_chats(lab: &Lab, apps: &mut Apps) {
    apps.a.open_chat(&lab.ab_chat);
    apps.b.open_chat(&lab.ab_chat);
    apps.c.open_chat(&lab.group_chat);
    for app in [&mut apps.a, &mut apps.b, &mut apps.c] {
        app.shot("opened");
    }
}

/// One message a typed and sent to a chat: its id as both sides know it, when a pressed Enter, and when a's compose
/// box took the send (its `compose sent`).
#[derive(Clone, Debug)]
struct Sent {
    id: String,
    enter_ms: i64,
    taken_ms: i64,
}

/// The ids `app`'s view of `chat` showed last.
fn shown_ids(app: &mut App, chat: &str) -> HashSet<String> {
    app.trace()
        .last(|line| chat_view_of(line, chat))
        .map(|line| strings(&line, "messages").into_iter().collect())
        .unwrap_or_default()
}

/// The message a `chat-view shown` line holds that neither side showed before the send: new to the watcher
/// (`known`) and to the sender (`sender_known`), so a message an app caught up on late can't pass for it.
fn new_to_both(line: &Value, known: &HashSet<String>, sender_known: &HashSet<String>) -> Option<String> {
    strings(line, "messages")
        .into_iter()
        .find(|id| !known.contains(id) && !sender_known.contains(id))
}

/// a types `text` in the chat it shows and presses Enter; `watcher` shows the message in `chat`. The message's id (a
/// new one in the watcher's `chat-view shown` that a's view didn't show before either, and shows once it came back),
/// when Enter was pressed and when the watcher showed it.
fn send_and_watch(a: &mut App, watcher: &mut App, chat: &str, text: &str) -> (Sent, i64) {
    let known = shown_ids(watcher, chat);
    let a_known = shown_ids(a, chat);
    let from = watcher.trace().mark();
    let a_from = a.trace().mark();
    a.type_into_compose(text);
    let enter_ms = now_ms();
    a.key(keys::ENTER);
    let sent = a
        .trace()
        .wait_from(a_from, STEP, "compose sent", |line| is(line, "compose", "sent"));
    let taken_ms = sent["at_ms"].as_i64().unwrap();
    let shown = watcher.trace().wait_from(from, ARRIVAL, "a's message shown", |line| {
        chat_view_of(line, chat) && new_to_both(line, &known, &a_known).is_some()
    });
    let id = new_to_both(&shown, &known, &a_known).unwrap();
    let shown_ms = shown["at_ms"].as_i64().unwrap();
    // the message a sent, as a's own view shows it once it came back
    a.trace().wait_from(a_from, ARRIVAL, "a's own message shown", |line| {
        chat_view_of(line, chat) && strings(line, "messages").contains(&id)
    });
    (Sent { id, enter_ms, taken_ms }, shown_ms)
}

/// a sends [`MESSAGES`] messages to b, one at a time, each shown by b before the next: send → shown per message.
fn latency(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let mut sent = Vec::new();
    let (mut times, mut taken, mut through) = (Vec::new(), Vec::new(), Vec::new());
    for n in 1..=MESSAGES {
        let text = format!("M2 live {nonce} m{n:02}");
        let (message, shown_ms) = send_and_watch(&mut apps.a, &mut apps.b, &lab.ab_chat, &text);
        times.push(shown_ms - message.enter_ms);
        taken.push(message.taken_ms - message.enter_ms);
        through.push(shown_ms - message.taken_ms);
        sent.push(message);
    }
    apps.a.shot("sent");
    apps.b.shot("shown");
    found.step(
        "latency",
        json!({
            "send_to_shown_ms": stats(times.clone()),
            "enter_to_compose_sent_ms": stats(taken),
            "compose_sent_to_shown_ms": stats(through),
            "each_ms": times,
        }),
    );
    sent
}

/// a edits its last message (Up in the empty box), replies to it and reacts to the reply (Escape to the list, Down),
/// all by the keyboard; b shows each. The reply, as sent.
fn edit_reply_react(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let chat = lab.ab_chat.clone();
    let last = apps.b.trace().last(|line| chat_view_of(line, &chat)).unwrap();
    let edited = strings(&last, "messages").last().cloned().unwrap();
    // the edit: Up in the empty box loads a's last message
    let from = apps.a.trace().mark();
    apps.a.key(keys::UP);
    apps.a
        .trace()
        .wait_from(from, STEP, "compose editing", |line| is(line, "compose", "editing"));
    apps.a.type_into_compose(" ed");
    let b_from = apps.b.trace().mark();
    let start = now_ms();
    apps.a.key(keys::ENTER);
    let shown = apps.b.trace().wait_from(b_from, ARRIVAL, "the edit shown", |line| {
        chat_view_of(line, &chat) && kind_of(line, &edited).as_deref() == Some("edited")
    });
    let edit_ms = shown["at_ms"].as_i64().unwrap() - start;
    // the reply: Escape to the list, Down to the newest (it stops there), r
    apps.a.key(keys::ESC);
    for _ in 0..3 {
        apps.a.key(keys::DOWN);
    }
    let from = apps.a.trace().mark();
    let (r, _) = apps.a.input.keymap().key_for('r').unwrap();
    apps.a.key(r);
    apps.a
        .trace()
        .wait_from(from, STEP, "compose replying", |line| is(line, "compose", "replying"));
    let (reply, reply_shown) = send_and_watch(&mut apps.a, &mut apps.b, &chat, &format!("M2 live {nonce} reply"));
    let line = apps.b.trace().last(|line| chat_view_of(line, &chat)).unwrap();
    assert!(
        strings(&line, "replies").contains(&reply.id),
        "b shows no reply: {line}"
    );
    // the reaction: Escape to the list, Down to the newest (the reply: Down stops there, whatever the reply left
    // selected), a, 1 (like)
    apps.a.key(keys::ESC);
    for _ in 0..3 {
        apps.a.key(keys::DOWN);
    }
    let (a_key, _) = apps.a.input.keymap().key_for('a').unwrap();
    let (one, _) = apps.a.input.keymap().key_for('1').unwrap();
    apps.a.key(a_key);
    let b_from = apps.b.trace().mark();
    let start = now_ms();
    apps.a.key(one);
    let id = reply.id.clone();
    let shown = apps.b.trace().wait_from(b_from, ARRIVAL, "the reaction shown", |line| {
        chat_view_of(line, &chat) && strings(line, "reacted").contains(&id)
    });
    let reaction_ms = shown["at_ms"].as_i64().unwrap() - start;
    // back to the box: Escape clears the selection, Escape again moves the focus
    apps.a.key(keys::ESC);
    apps.a.key(keys::ESC);
    apps.a.shot("edited-replied-reacted");
    apps.b.shot("edit-reply-reaction");
    found.step(
        "edit_reply_reaction",
        json!({"edit_ms": edit_ms, "reply_ms": reply_shown - reply.enter_ms, "reaction_ms": reaction_ms}),
    );
    vec![reply]
}

/// a sends to the group chat: c shows it; b, showing the 1:1 chat, notifies on its sandbox's bus, and the
/// notification activated opens the group chat, where b shows it.
fn group_message(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) {
    let group = lab.group_chat.clone();
    apps.a.open_chat(&group);
    let b_from = apps.b.trace().mark();
    let text = format!("M2 live {nonce} group");
    let (message, c_shown) = send_and_watch(&mut apps.a, &mut apps.c, &group, &text);
    let noticed = apps
        .b
        .trace()
        .wait_from(b_from, ARRIVAL, "b's notification of the group chat", |line| {
            is(line, "notification", "shown") && line["conversation"] == group.as_str()
        });
    let notification = apps
        .b
        .sandbox
        .bus()
        .notifications()
        .into_iter()
        .rfind(|notification| notification.body.contains(&text))
        .unwrap_or_else(|| panic!("no notification of the group message on b's bus"));
    assert_eq!(notification.app_name, "Teams (lab b)", "{notification:?}");
    apps.b.shot("notified");
    let from = apps.b.trace().mark();
    apps.b.sandbox.bus().invoke_action(notification.id, "default").unwrap();
    apps.b
        .trace()
        .wait_from(from, STEP, "the group chat opened from the notification", |line| {
            is(line, "chat", "opened") && line["conversation"] == group.as_str()
        });
    let id = message.id.clone();
    apps.b
        .trace()
        .wait_from(from, ARRIVAL, "the group message shown by b", |line| {
            chat_view_of(line, &group) && strings(line, "messages").contains(&id)
        });
    apps.b.shot("group");
    apps.c.shot("group");
    found.step(
        "group",
        json!({
            "c_shown_ms": c_shown - message.enter_ms,
            "b_notified_ms": noticed["at_ms"].as_i64().unwrap() - message.enter_ms,
            "notification_kind": noticed["kinds"],
        }),
    );
}

/// b's header for the 1:1 chat shown, from `from` on, with a's presence `presence`.
fn b_header_line(apps: &mut Apps, chat: &str, from: usize, presence: &str) -> Value {
    apps.b
        .trace()
        .wait_from(from, PRESENCE, &format!("b's header showing a {presence}"), |line| {
            is(line, "header", "shown")
                && line["conversation"] == chat
                && strings(line, "kinds").first().map(String::as_str) == Some(presence)
        })
}

/// [`b_header_line`]'s kinds.
fn b_header(apps: &mut Apps, chat: &str, from: usize, presence: &str) -> Value {
    b_header_line(apps, chat, from, presence)["kinds"].clone()
}

/// a runs the switcher's "Set status: …" command it finds by `query`, and its report (`reported`) goes out; when
/// Enter was pressed.
fn set_status(a: &mut App, query: &str, reported: &str) -> i64 {
    let from = a.trace().mark();
    a.ctrl('k');
    a.trace()
        .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
    a.type_text(query);
    // the palette lists the command once the query is in
    thread::sleep(SETTLE);
    let start = now_ms();
    a.key(keys::ENTER);
    a.trace()
        .wait_from(from, STEP, "status chosen", |line| is(line, "status-menu", "chosen"));
    a.trace()
        .wait_from(from, ARRIVAL, &format!("own presence {reported} sent"), |line| {
            is(line, "own-presence", "sent") && strings(line, "kinds").first().map(String::as_str) == Some(reported)
        });
    start
}

/// Whether the settings `file` keeps a manual status other than Automatic (an earlier run that stopped between its
/// Busy and its reset left Busy there).
fn saved_status_is_manual(file: &Path) -> bool {
    SettingsFile::new(file).load().settings.manual_status != ManualStatus::Automatic
}

/// b's header for the 1:1 chat names a from people's names with a's picture and shows a available; a sets Busy,
/// b's header shows busy; a sets Automatic (the reset), b's header shows available again. A manual status an earlier
/// run left in a's settings is reset first, so the run doesn't depend on how the last one ended.
fn names_and_presence(lab: &Lab, apps: &mut Apps, found: &mut Found) {
    let chat = lab.ab_chat.clone();
    let reset_first = saved_status_is_manual(&lab.a_settings);
    // b's header from before the reset doesn't count
    let from = if reset_first { apps.b.trace().mark() } else { 0 };
    if reset_first {
        set_status(&mut apps.a, "set status automatic", "reset");
    }
    apps.b.open_chat(&chat);
    let available = b_header(apps, &chat, from, "available");
    assert_eq!(available[1], "named", "b's header doesn't name a: {available}");
    assert_eq!(
        available[2], "picture",
        "b's header shows no picture of a (Graph has one): {available}"
    );
    let busy_from = apps.b.trace().mark();
    let busy_start = set_status(&mut apps.a, "set status busy", "busy");
    let busy = b_header_line(apps, &chat, busy_from, "busy");
    apps.b.shot("a-busy");
    let reset_from = apps.b.trace().mark();
    let reset_start = set_status(&mut apps.a, "set status automatic", "reset");
    let back = b_header_line(apps, &chat, reset_from, "available");
    apps.b.shot("a-available");
    found.step(
        "names_and_presence",
        json!({
            "header": available,
            "status_reset_first": reset_first,
            "busy_shown_ms": busy["at_ms"].as_i64().unwrap() - busy_start,
            "available_again_ms": back["at_ms"].as_i64().unwrap() - reset_start,
        }),
    );
}

/// b's search (Ctrl+Shift+F) finds `target`, one of a's messages, by the words it was sent with; then a channel opens
/// read-only from the switcher ("General"), and b goes back to the 1:1 chat.
fn search_and_channel(lab: &Lab, apps: &mut Apps, nonce: i64, target: &Sent, found: &mut Found) {
    let b = &mut apps.b;
    let from = b.trace().mark();
    let (f, _) = b.input.keymap().key_for('f').unwrap();
    b.input.key_with(&[Modifier::Ctrl, Modifier::Shift], f).unwrap();
    b.trace()
        .wait_from(from, STEP, "search opened", |line| is(line, "search", "opened"));
    b.type_text(&format!("{nonce} m07"));
    let id = target.id.clone();
    let hits = b
        .trace()
        .wait_from(from, STEP, "the search finding a's message", |line| {
            is(line, "search", "shown") && strings(line, "messages").contains(&id)
        });
    b.shot("search");
    b.key(keys::ESC);
    b.trace()
        .wait_from(from, STEP, "search closed", |line| is(line, "search", "closed"));
    // a channel, read-only: the switcher's channels under the chats
    let from = b.trace().mark();
    b.ctrl('k');
    b.trace()
        .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
    b.type_text("General");
    thread::sleep(SETTLE);
    b.key(keys::ENTER);
    let opened = b
        .trace()
        .wait_from(from, STEP, "a channel opened", |line| is(line, "channel", "opened"));
    let channel = opened["conversation"].as_str().unwrap().to_owned();
    let shown = b.trace().wait_from(from, ARRIVAL, "the channel's view shown", |line| {
        is(line, "channel-view", "shown") && line["conversation"] == channel.as_str()
    });
    b.shot("channel");
    // nothing to write in it: no compose box was made for it (`compose opened`) and none changed since the switcher
    // opened; the run types nothing into the channel, so no channel write is ever tried
    let composes = b.trace().count_from(from, |line| line["view"] == "compose");
    assert_eq!(composes, 0, "b: a compose line while the channel was shown");
    b.open_chat(&lab.ab_chat);
    found.step(
        "search_and_channel",
        json!({
            "search_hits": strings(&hits, "messages").len(),
            "channel_messages": strings(&shown, "messages").len(),
            "channel_kinds": shown["kinds"],
        }),
    );
}

/// a types `text` in `chat` (the chat it shows) and presses Enter: the message's id once a's own view shows it.
fn a_sends(a: &mut App, chat: &str, text: &str) -> Sent {
    let known: HashSet<String> = a
        .trace()
        .last(|line| chat_view_of(line, chat))
        .map(|line| strings(&line, "messages").into_iter().collect())
        .unwrap_or_default();
    let from = a.trace().mark();
    a.type_into_compose(text);
    let enter_ms = now_ms();
    a.key(keys::ENTER);
    let sent = a
        .trace()
        .wait_from(from, STEP, "compose sent", |line| is(line, "compose", "sent"));
    let taken_ms = sent["at_ms"].as_i64().unwrap();
    let shown = a.trace().wait_from(from, ARRIVAL, "a's own message shown", |line| {
        chat_view_of(line, chat) && strings(line, "messages").iter().any(|id| !known.contains(id))
    });
    let id = strings(&shown, "messages")
        .into_iter()
        .find(|id| !known.contains(id))
        .unwrap();
    Sent { id, enter_ms, taken_ms }
}

/// b's window closed into the tray: a sends to b, b notifies once, and the window opened again from the tray item
/// shows the message.
fn closed_into_the_tray(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Sent {
    let chat = lab.ab_chat.clone();
    apps.a.open_chat(&chat);
    let from = apps.b.trace().mark();
    apps.b.sandbox.niri_msg(&["action", "close-window"]).unwrap();
    apps.b
        .trace()
        .wait_from(from, STEP, "b's window closed", |line| is(line, "window", "closed"));
    let sent = a_sends(&mut apps.a, &chat, &format!("M2 live {nonce} tray"));
    let notice = |line: &Value| is(line, "notification", "shown") && line["conversation"] == chat.as_str();
    let noticed = apps
        .b
        .trace()
        .wait_from(from, ARRIVAL, "b's notification of the 1:1 chat", notice);
    thread::sleep(QUIET);
    let notifications = apps.b.trace().count_from(from, notice);
    assert_eq!(notifications, 1, "b notified {notifications} times");
    // the window again, from the tray item
    let (host, name) = apps.b.tray();
    let item = zbus::blocking::Proxy::new(&host, name.as_str(), ITEM_PATH, "org.kde.StatusNotifierItem").unwrap();
    let opened_from = apps.b.trace().mark();
    item.call_method("Activate", &(0_i32, 0_i32)).unwrap();
    apps.b
        .trace()
        .wait_from(opened_from, STEP, "b's window opened again", |line| {
            is(line, "window", "opened")
        });
    let id = sent.id.clone();
    apps.b.trace().wait_from(
        opened_from,
        ARRIVAL,
        "the tray message in the window opened again",
        |line| chat_view_of(line, &chat) && strings(line, "messages").contains(&id),
    );
    apps.b.shot("from-the-tray");
    found.step(
        "tray",
        json!({"notified_ms": noticed["at_ms"].as_i64().unwrap() - sent.enter_ms, "notifications": notifications}),
    );
    sent
}

/// b's app quit from the tray (its broker exits with it); a sends [`GAP_MESSAGES`]; b's app started again shows them.
fn quit_and_reopened(lab: &Lab, apps: &mut Apps, nonce: i64, found: &mut Found) -> Vec<Sent> {
    let chat = lab.ab_chat.clone();
    apps.b.quit();
    let gap: Vec<Sent> = (1..=GAP_MESSAGES)
        .map(|n| a_sends(&mut apps.a, &chat, &format!("M2 live {nonce} gap{n}")))
        .collect();
    let restarted = now_ms();
    apps.b.start();
    let ids: Vec<String> = gap.iter().map(|sent| sent.id.clone()).collect();
    let shown = apps
        .b
        .trace()
        .wait_from(0, STARTUP, "the gap messages shown after the restart", |line| {
            chat_view_of(line, &chat) && ids.iter().all(|id| strings(line, "messages").contains(id))
        });
    thread::sleep(QUIET);
    let notified = apps.b.trace().count_from(0, |line| is(line, "notification", "shown"));
    apps.b.shot("restarted");
    found.step(
        "restart",
        json!({
            "shown_after_start_ms": shown["at_ms"].as_i64().unwrap() - restarted,
            "notifications_after_start": notified,
        }),
    );
    gap
}

/// b's 1:1 chat view shows every message of the run sent there exactly once.
fn every_message_once(b: &mut App, chat: &str, sent: &[Sent], found: &mut Found) {
    let line = b.trace().last(|line| chat_view_of(line, chat)).unwrap();
    let shown = strings(&line, "messages");
    for message in sent {
        let times = shown.iter().filter(|id| **id == message.id).count();
        assert_eq!(times, 1, "{} shown {times} times in b's view", message.id);
    }
    found.step("every_message_once", json!({"messages": sent.len()}));
}

/// What `fence.log` holds for the run, counted by line kind.
#[derive(Debug, Default)]
struct FenceCounts {
    writes: BTreeMap<String, usize>,
    group_writes: BTreeMap<String, usize>,
    own_availability: usize,
    /// Mutating requests let through because they reach nobody, by method, host and path shape.
    allowed: BTreeMap<String, usize>,
}

/// `fence.log`'s lines since `start` (a byte offset): whole JSON lines; allows only on the a–b chat, the recorded
/// group chat and the lab accounts' own availability; no deny; the requests let through because they reach nobody,
/// counted by their shape; no frame (the native sink logs none).
fn fence_log_holds(lab: &Lab, start: u64) -> Value {
    let log = fs::read(&lab.fence_log).unwrap();
    let run = &log[usize::try_from(start).unwrap()..];
    assert!(run.is_empty() || run.ends_with(b"\n"), "fence.log ends in a half line");
    let mut counts = FenceCounts::default();
    let mut lines = 0;
    for line in std::str::from_utf8(run).unwrap().lines() {
        lines += 1;
        let entry: Value = serde_json::from_str(line).unwrap_or_else(|error| panic!("not a whole line: {error}"));
        assert_eq!(entry["verdict"], "allow", "a line that is no allow: {entry}");
        counts.add(lab, &entry);
    }
    assert_eq!(
        counts.writes.get("message"),
        Some(&(MESSAGES + 1 + 1 + GAP_MESSAGES)),
        "{counts:?}"
    );
    assert_eq!(counts.writes.get("edit"), Some(&1), "{counts:?}");
    assert_eq!(counts.writes.get("reaction"), Some(&1), "{counts:?}");
    assert_eq!(counts.group_writes.get("message"), Some(&1), "{counts:?}");
    assert!(counts.own_availability >= 3, "{counts:?}");
    json!({
        "lines": lines,
        "allowed_writes": counts.writes,
        "allowed_group_writes": counts.group_writes,
        "own_availability": counts.own_availability,
        "allowed": counts.allowed,
    })
}

impl FenceCounts {
    fn add(&mut self, lab: &Lab, entry: &Value) {
        if let Some(kind) = entry["kind"].as_str() {
            self.add_write(lab, kind, entry);
        } else {
            assert_eq!(entry["event"], "allowed", "a line of another kind: {entry}");
            let text = |key: &str| entry[key].as_str().unwrap_or_default().to_owned();
            let shape = format!("{} {}{}", text("method"), text("host"), text("path"));
            *self.allowed.entry(shape).or_default() += 1;
        }
    }

    fn add_write(&mut self, lab: &Lab, kind: &str, entry: &Value) {
        if let Some(mri) = entry["scope"]["account"]["mri"].as_str() {
            assert_eq!(kind, "own_availability", "a {kind} on an account: {entry}");
            assert!(
                lab.accounts.iter().any(|account| account == mri),
                "an availability off the lab: {entry}"
            );
            self.own_availability += 1;
            return;
        }
        let conversation = entry["scope"]["conversation"]["id"].as_str().unwrap_or_default();
        let writes = if conversation == lab.ab_chat {
            assert!(
                ["message", "edit", "reaction", "read_receipt", "typing"].contains(&kind),
                "a {kind} allowed on the a–b chat"
            );
            &mut self.writes
        } else {
            assert!(
                conversation == lab.group_chat,
                "a {kind} allowed off the a–b and the recorded group chat: {entry}"
            );
            assert!(
                ["message", "read_receipt", "typing"].contains(&kind),
                "a {kind} allowed on the group chat"
            );
            &mut self.group_writes
        };
        *writes.entry(kind.to_owned()).or_default() += 1;
    }
}

// --- the helpers' own tests (offline: they run without PROTO_LAB_LIVE) ---

#[test]
fn the_sent_message_is_the_one_new_to_both_sides() {
    let ids = |list: &[&str]| -> HashSet<String> { list.iter().map(|id| (*id).to_owned()).collect() };
    let line = json!({"view": "chat-view", "change": "shown", "messages": ["100", "150", "200", "300"]});
    // the watcher caught up late on 150 and 200, which a held already: the sent one is 300
    assert_eq!(
        new_to_both(&line, &ids(&["100"]), &ids(&["100", "150", "200"])).as_deref(),
        Some("300")
    );
    // only messages a caught up on came yet: none is the sent one
    let early = json!({"view": "chat-view", "change": "shown", "messages": ["100", "150", "200"]});
    assert_eq!(new_to_both(&early, &ids(&["100"]), &ids(&["100", "150", "200"])), None);
    assert_eq!(new_to_both(&line, &ids(&[]), &ids(&[])).as_deref(), Some("100"));
}

#[test]
fn only_a_status_other_than_automatic_is_reset_first() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("settings.toml");
    assert!(!saved_status_is_manual(&file), "no file: Automatic");
    for (status, manual) in [("automatic", false), ("busy", true), ("do-not-disturb", true)] {
        fs::write(&file, format!("manual_status = \"{status}\"\n")).unwrap();
        assert_eq!(saved_status_is_manual(&file), manual, "{status}");
    }
}
