//! QA of task 407 (round 1), in the UI sandbox: a new chat not made yet, and its draft, stay after the user opens
//! another chat. The real app runs as a child in the sandbox; the parent drives it only with the sandbox's virtual
//! pointer and keyboard and reads what the window holds through a file in the sandbox dir (`harness = false`: GPUI
//! runs on the process's main thread).
//!
//! - **Mock** (`mock`): lab account a on `teams-mock` (its chat service and Trouter). c wrote a synthetic message in the
//!   a–c chat, to copy from. The checks: a 1:1 chat's "Start a group chat", a draft, another chat, a copy there and
//!   the way back by the kept row with a paste; New message with people, a group name and a draft; the row's click,
//!   Enter, "..." → "Discard" and Delete; several kept new chats; Back, Forward and the return from Settings; the first
//!   message from a kept new chat; a draft to people who have a chat; the update's wait for unsent text; long names,
//!   many people, an empty draft and the keyboard only.
//! - **Own server** (`own`): the real `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`, else the one beside the test's
//!   target directory). Dana's 1:1 chats with Erin and Finn exist; one pass of the main flow, its first message sent.
//!
//! `QA_NCK_ONLY` (a comma list of check names, or `own`) runs only those. `QA_NCK_EVIDENCE` names a directory that
//! gets the screenshots and the results too. Screenshots also go to `target/ui-shots/qa407-*.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../own_server_binary.rs"]
#[expect(dead_code, reason = "the shared binary harness has helpers other QA tests use")]
mod own_server_binary;
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AMS_BASE, AX_CHAT, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C,
    MIDDLE_TIER_BASE, OUTSIDER, PRESENCE_BASE, PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::chat::MessageText;
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "QA_NCK_CHILD";
const ONLY_ENV: &str = "QA_NCK_ONLY";
const EVIDENCE_ENV: &str = "QA_NCK_EVIDENCE";
const ORIGIN_ENV: &str = "QA_NCK_ORIGIN";
const CA_ENV: &str = "QA_NCK_CA";
const DIR_ENV: &str = "QA_NCK_DIR";
const PEOPLE_ENV: &str = "QA_NCK_PEOPLE";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa407-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(700);
/// The time between a key or click and the next, so the app sees each.
const STEP: Duration = Duration::from_millis(250);
/// The time the directory gets after the typing in a picker stops.
const PICKER: Duration = Duration::from_millis(900);

/// What c wrote in the a–c chat, which the user copies.
const COPIED: &str = "Room 4.12 is booked for Friday at 12:30";
const COPIED_HTML: &str = "<p>Room 4.12 is booked for Friday at 12:30</p>";

// --- where things are on the 1280x800 screen ---

const BACK: (u32, u32) = (405, 23);
const FORWARD: (u32, u32) = (435, 23);
const RAIL_SETTINGS: (u32, u32) = (37, 765);
/// The header's people button of a 1:1 chat.
const PEOPLE_BUTTON: (u32, u32) = (1243, 71);
/// "Start a group chat" in a 1:1 chat's people.
const START_GROUP: (u32, u32) = (1098, 258);
/// The picker's Create.
const CREATE: (u32, u32) = (1062, 375);
/// The compose box of a chat or a new chat.
const COMPOSE: (u32, u32) = (700, 750);
/// The first row under the "Chats" header, and the height of a row.
const FIRST_CHAT_ROW: u32 = 256;
const ROW: u32 = 64;
const ROW_X: u32 = 250;
/// The "..." of a hovered kept row.
const ROW_MORE_X: u32 = 433;
/// "Discard" in the row's menu, from the "..." it opens under.
const DISCARD_FROM_MORE: (i32, i32) = (-60, 36);
/// New message's group name button (the pen right of the To: line).
const GROUP_NAME_TOGGLE: (u32, u32) = (1240, 71);
/// The x of the list's focus ring's right edge.
const RING_X: u32 = 452;
/// Where the pointer rests when it must not hover anything.
const PARK: (u32, u32) = (300, 23);

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("mock") => mock_child(),
        Ok("own") => own_child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_new_chat_kept_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn shots_dir() -> io::Result<PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), std::path::Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn only() -> Option<Vec<String>> {
    std::env::var(ONLY_ENV)
        .ok()
        .map(|list| list.split(',').map(str::trim).map(str::to_owned).collect())
}

/// Whether `name` runs: each check unless `QA_NCK_ONLY` names others; the history probe only when named.
fn wanted(name: &str) -> bool {
    match only() {
        Some(list) => list.iter().any(|one| one == name),
        None => name != "history_probe",
    }
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

const CHECKS: [Check; 14] = [
    (
        "group_chat_from_1to1_kept_with_draft_copy_and_paste",
        group_chat_from_1to1,
    ),
    ("kept_row_more_menu_discard", kept_row_more_menu_discard),
    ("new_message_people_name_draft_kept_named_row", new_message_named_row),
    ("kept_row_enter_opens_and_delete_discards", kept_row_keys),
    ("several_kept_latest_first_each_own_draft", several_kept),
    ("back_forward_show_new_chat_last_left", back_forward),
    ("settings_return_shows_new_chat_last_left", settings_return),
    ("draft_to_people_with_a_chat_goes_to_that_chat", draft_to_existing_chat),
    ("update_restart_waits_for_kept_draft", update_waits),
    ("long_name_and_many_people_row", long_name_many_people),
    ("empty_drafts_what_is_kept", empty_drafts),
    ("keyboard_only_round_trip", keyboard_only),
    ("history_probe", history_probe),
    // last: the chat it makes is found by later new chats of the same people
    ("first_message_from_kept_makes_chat_row_goes", first_message_makes_chat),
];

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_new_chat_kept_window", &options)? else {
        return Ok(());
    };
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    if CHECKS.iter().any(|(name, _)| wanted(name)) {
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "mock")])?;
        let mut user = User::start(&sandbox, Running::new(child)?, "mock")?;
        for (name, check) in CHECKS {
            if !wanted(name) {
                continue;
            }
            user.run_check(name, check, &mut passed, &mut failed);
        }
        user.quit()?;
    }
    if wanted("own") {
        match own_pass(&sandbox) {
            Ok((ok, bad)) => {
                passed.extend(ok);
                failed.extend(bad);
            }
            Err(error) => failed.push(("own_server_main_flow".to_owned(), error.to_string())),
        }
    }
    let mut summary = String::new();
    for name in &passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(dir).join("results.txt"), &summary);
    }
    say(&format!(
        "qa_new_chat_kept_window: {} passed, {} failed",
        passed.len(),
        failed.len()
    ));
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} checks failed", failed.len()).into())
    }
}

/// The user at the sandbox: the pointer, the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    mode: &'static str,
}

impl<'a> User<'a> {
    fn start(sandbox: &'a Sandbox, running: Running, mode: &'static str) -> SandboxResult<Self> {
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            let mut running = running;
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        let mut user = Self {
            sandbox,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
            mode,
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(
        &mut self,
        name: &'static str,
        check: fn(&mut User<'_>) -> SandboxResult,
        passed: &mut Vec<String>,
        failed: &mut Vec<(String, String)>,
    ) {
        say(&format!("check {name}"));
        let outcome = self.reset().and_then(|()| check(self));
        match outcome {
            Ok(()) => {
                say(&format!("  pass {name}"));
                passed.push(name.to_owned());
            }
            Err(error) => {
                let error = short(&error.to_string());
                let state = short(&self.state().map(|state| state.to_string()).unwrap_or_default());
                let _ = self.shot(&format!("failed-{name}"));
                say(&format!("  FAIL {name}: {error}\n  state: {state}"));
                failed.push((name.to_owned(), error));
            }
        }
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Back to the start of a check: no kept new chats, no drafts, the a–b chat shown, Teams Light.
    fn reset(&mut self) -> SandboxResult {
        let answer = self.ask("reset")?;
        ensure(answer == "ok", || format!("reset: {answer}"))?;
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn quit(&mut self) -> SandboxResult {
        self.ask("quit")?;
        let running = self.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn hover(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)?;
        thread::sleep(Duration::from_millis(400));
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(PARK.0, PARK.1)
    }

    /// A screenshot as `qa407-<mode>-<name>.png`, under `target/ui-shots/` and the evidence dir. The sandbox's niri
    /// puts each screenshot on its clipboard: never take one between a copy and its paste.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa407-{}-{name}.png", self.mode);
        let png = self.sandbox.screenshot(&file, false)?;
        let kept = shots_dir()?.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
            let dir = PathBuf::from(dir).join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    fn settled_shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(name)
    }

    /// The same screen in Teams Light and Teams Dark (`<name>-light`, `<name>-dark`); Teams Light after.
    fn shots_in_both_themes(&mut self, name: &str) -> SandboxResult {
        self.settled_shot(&format!("{name}-light"))?;
        self.ask("theme-dark")?;
        self.settled_shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The y of the chat row `id` in the list, from the rows the child reports in their drawn order (kept rows first).
    fn row_y(&mut self, id: &str) -> SandboxResult<u32> {
        let state = self.state()?;
        let rows = state["sidebar_rows"].as_array().cloned().unwrap_or_default();
        let at = rows
            .iter()
            .position(|row| row.as_str() == Some(id))
            .ok_or_else(|| format!("no row {id} in {rows:?}"))?;
        // the Favorites section, when there is one, is a header and its rows above the "Chats" header
        let favorites = u32::try_from(state["favorites"].as_u64().unwrap_or(0))?;
        let first = if favorites == 0 {
            FIRST_CHAT_ROW - ROW - 32
        } else {
            FIRST_CHAT_ROW + (favorites - 1) * ROW
        };
        Ok(first + u32::try_from(at)? * ROW)
    }

    /// Types `person` in the To: line or the picker, waits for a suggestion, and takes the first with Enter.
    fn take(&mut self, person: &str) -> SandboxResult {
        self.type_text(person)?;
        self.until(&format!("a suggestion for {person}"), |state| {
            state["suggestions"].as_u64().is_some_and(|count| count > 0)
        })?;
        thread::sleep(PICKER);
        self.press("enter")
    }

    /// Takes the list's ring to the row `id` (in `sidebar_rows`) with Up and Down, as the user does by looking.
    fn ring_to(&mut self, id: &str) -> SandboxResult {
        let target = self.row_y(id)?;
        for _ in 0..12 {
            let png = self.sandbox.screenshot("ring.png", false)?;
            let shot = image::open(&png)?.to_rgb8();
            let at = ring_middle(&shot).ok_or("no focus ring in the list")?;
            if at.abs_diff(target) < ROW / 3 {
                return Ok(());
            }
            self.press(if at > target { "up" } else { "down" })?;
        }
        Err(format!("the ring never reached {id}").into())
    }

    /// Clicks the chat `id`'s row in the list, then waits for the chat to show.
    fn open_by_row(&mut self, id: &str) -> SandboxResult<Value> {
        let y = self.row_y(id)?;
        self.click((ROW_X, y))?;
        self.until(&format!("the chat {id} shown"), |state| state["main"] == chat_main(id))
    }

    /// Clicks the kept row at `index` (0: the latest).
    fn open_kept(&mut self, index: usize) -> SandboxResult<Value> {
        let y = self.row_y(&format!("kept-{index}"))?;
        self.click((ROW_X, y))?;
        self.until("the kept new chat shown", |state| state["main"] == "NewChat")
    }

    /// The draft of the box that has the keyboard: typed after `COMPOSE` got a click.
    fn type_in_box(&mut self, text: &str) -> SandboxResult {
        self.click(COMPOSE)?;
        self.press("ctrl+end")?;
        self.type_text(text)
    }

    /// a–b's people → "Start a group chat" → `who` typed and taken → Create: the new group chat shown.
    fn start_group_from_ab(&mut self, who: &[&str]) -> SandboxResult<Value> {
        let state = self.state()?;
        ensure(state["main"] == chat_main(AB_CHAT), || format!("not in a–b: {state}"))?;
        self.click(PEOPLE_BUTTON)?;
        self.until("the people flyout", |state| state["people_open"] == true)?;
        self.click(START_GROUP)?;
        thread::sleep(STEP);
        for person in who {
            self.take(person)?;
        }
        self.click(CREATE)?;
        self.until("the new group chat shown", |state| {
            state["main"] == "NewChat" && state["new_chat"]["group"] == true
        })
    }

    /// Ctrl+N, each of `who` typed on To: and taken with Enter.
    fn new_message_to(&mut self, who: &[&str]) -> SandboxResult<Value> {
        self.press("ctrl+n")?;
        self.until("New message shown", |state| state["main"] == "NewChat")?;
        for person in who {
            self.take(person)?;
        }
        let count = who.len();
        self.until("the people on To:", |state| {
            state["new_chat"]["people"].as_array().map(Vec::len) == Some(count)
        })
    }
}

/// `text` with the mock's chat ids named.
fn short(text: &str) -> String {
    [
        (AB_CHAT, "<a-b>"),
        (AC_CHAT, "<a-c>"),
        (ABC_CHAT, "<a-b-c>"),
        (AX_CHAT, "<a-x>"),
    ]
    .iter()
    .fold(text.to_owned(), |text, (id, name)| text.replace(id, name))
}

/// The y of the middle of the list's focus ring (its right edge, which a selected row's bar does not have), if any.
fn ring_middle(shot: &image::RgbImage) -> Option<u32> {
    let ringed: Vec<u32> = (96..790)
        .filter(|&y| {
            (RING_X - 3..=RING_X + 2).any(|x| {
                let image::Rgb([r, g, b]) = *shot.get_pixel(x, y);
                b > 150 && r < 140 && g < 140
            })
        })
        .collect();
    Some((ringed.first()? + ringed.last()?) / 2)
}

fn chat_main(id: &str) -> String {
    format!("Chat({id:?})")
}

fn text(state: &Value, path: &[&str]) -> String {
    let mut at = state;
    for key in path {
        at = &at[*key];
    }
    at.as_str().unwrap_or_default().trim().to_owned()
}

fn kept(state: &Value) -> Vec<Value> {
    state["kept"].as_array().cloned().unwrap_or_default()
}

fn kept_names(state: &Value) -> Vec<String> {
    kept(state)
        .iter()
        .map(|row| row["name"].as_str().unwrap_or_default().to_owned())
        .collect()
}

// --- the checks on the mock ---

fn group_chat_from_1to1(user: &mut User<'_>) -> SandboxResult {
    let before = user.state()?["list_ids"].as_array().map_or(0, Vec::len);
    let state = user.start_group_from_ab(&["Outsider"])?;
    let people = state["new_chat"]["people"].clone();
    ensure(people == json!([OUTSIDER, LAB_B]), || format!("people {people}"))?;
    ensure(state["new_chat"]["box_focused"] == true, || {
        format!("the box has no keyboard after Create: {state}")
    })?;
    user.type_text("Lunch on Friday? ")?;
    user.settled_shot("group-draft")?;
    // to the a–c chat, to copy c's message there
    user.open_by_row(AC_CHAT)?;
    let state = user.until("the kept row", |state| kept(state).len() == 1)?;
    let name = kept_names(&state).join("|");
    ensure(name == "Outsider and Lab", || format!("the kept row is named {name:?}"))?;
    user.shots_in_both_themes("group-kept-row")?;
    // c's message selected by the keys and copied
    user.click(COMPOSE)?;
    user.press("escape")?;
    user.press("up")?;
    user.press("ctrl+c")?;
    let clipboard = clipboard_text(user.sandbox)?;
    ensure(clipboard.contains(COPIED), || {
        format!("the clipboard holds {clipboard:?}")
    })?;
    let state = user.open_kept(0)?;
    ensure(text(&state, &["new_chat", "draft"]) == "Lunch on Friday?", || {
        format!("the draft after the return: {state}")
    })?;
    ensure(state["new_chat"]["people"] == people, || {
        format!("other people: {state}")
    })?;
    ensure(state["new_chat"]["box_focused"] == true, || {
        format!("the box has no keyboard after the row's click: {state}")
    })?;
    ensure(kept(&state).is_empty(), || {
        format!("its row is still a kept row: {state}")
    })?;
    ensure(state["sidebar_draft_row"] == "Outsider and Lab", || {
        format!("the row of the new chat shown: {state}")
    })?;
    user.press("ctrl+end")?;
    user.press("ctrl+v")?;
    let state = user.until("the paste", |state| {
        text(state, &["new_chat", "draft"]).contains(COPIED)
    })?;
    let draft = text(&state, &["new_chat", "draft"]);
    ensure(draft.starts_with("Lunch on Friday?"), || {
        format!("the draft: {draft:?}")
    })?;
    user.shots_in_both_themes("group-back-pasted")?;
    let state = user.state()?;
    ensure(state["list_ids"].as_array().map(Vec::len) == Some(before), || {
        format!("a chat was made: {state}")
    })
}

fn clipboard_text(sandbox: &Sandbox) -> SandboxResult<String> {
    let offers = sandbox.read_clipboard()?;
    Ok(offers
        .iter()
        .find(|(mime, _)| mime.starts_with("text/plain"))
        .map_or_else(
            || {
                format!(
                    "(no text: {:?})",
                    offers.iter().map(|(mime, _)| mime).collect::<Vec<_>>()
                )
            },
            |(_, bytes)| String::from_utf8_lossy(bytes).into_owned(),
        ))
}

/// A new group chat of b and the outsider from a–b with `draft`, then the a–c chat by its row.
fn leave_group_draft(user: &mut User<'_>, draft: &str) -> SandboxResult<Value> {
    user.start_group_from_ab(&["Outsider"])?;
    if !draft.is_empty() {
        user.type_text(draft)?;
    }
    user.open_by_row(AC_CHAT)?;
    user.until("the kept row", |state| !kept(state).is_empty())
}

fn kept_row_more_menu_discard(user: &mut User<'_>) -> SandboxResult {
    leave_group_draft(user, "to discard")?;
    let y = user.row_y("kept-0")?;
    user.hover((ROW_X, y))?;
    user.shot("kept-row-hover")?;
    user.click((ROW_MORE_X, y))?;
    thread::sleep(SETTLE);
    user.shot("kept-row-menu")?;
    let at = (
        ROW_MORE_X.saturating_add_signed(DISCARD_FROM_MORE.0),
        y.saturating_add_signed(DISCARD_FROM_MORE.1),
    );
    user.click(at)?;
    let state = user.until("the row discarded", |state| kept(state).is_empty())?;
    ensure(state["main"] == chat_main(AC_CHAT), || {
        format!("the place changed: {state}")
    })?;
    ensure(state["waiting"].is_null(), || format!("an update still waits: {state}"))?;
    // nothing comes back with New message
    user.press("ctrl+n")?;
    let state = user.until("New message", |state| state["main"] == "NewChat")?;
    ensure(text(&state, &["new_chat", "draft"]).is_empty(), || {
        format!("New message holds the discarded draft: {state}")
    })?;
    ensure(state["new_chat"]["people"] == json!([]), || format!("people: {state}"))
}

fn new_message_named_row(user: &mut User<'_>) -> SandboxResult {
    user.new_message_to(&["Lab C", "Outsider"])?;
    user.click(GROUP_NAME_TOGGLE)?;
    let state = user.until("the group name line", |state| state["new_chat"]["naming"] == true)?;
    let _ = state;
    user.type_text("Friday lunch crew")?;
    user.type_in_box("Who brings the cake?")?;
    let state = user.state()?;
    ensure(text(&state, &["new_chat", "draft"]) == "Who brings the cake?", || {
        format!("the draft: {state}")
    })?;
    user.settled_shot("new-message-named")?;
    user.open_by_row(AB_CHAT)?;
    let state = user.until("the kept row", |state| kept(state).len() == 1)?;
    let names = kept_names(&state);
    ensure(names == ["Friday lunch crew"], || format!("the kept rows: {names:?}"))?;
    user.shots_in_both_themes("named-kept-row")?;
    let state = user.open_kept(0)?;
    ensure(state["new_chat"]["naming"] == true, || {
        format!("the name line is gone: {state}")
    })?;
    ensure(text(&state, &["new_chat", "group_name"]) == "Friday lunch crew", || {
        format!("the name: {state}")
    })?;
    ensure(text(&state, &["new_chat", "draft"]) == "Who brings the cake?", || {
        format!("the draft: {state}")
    })?;
    ensure(state["new_chat"]["people"] == json!([LAB_C, OUTSIDER]), || {
        format!("the people: {state}")
    })
}

fn kept_row_keys(user: &mut User<'_>) -> SandboxResult {
    leave_group_draft(user, "by the keys")?;
    // Ctrl+2 gives the list the keyboard; Up and Down take its ring to the kept row
    user.press("ctrl+2")?;
    user.ring_to("kept-0")?;
    user.settled_shot("kept-row-ringed")?;
    user.press("enter")?;
    let state = user.until("the kept new chat by Enter", |state| state["main"] == "NewChat")?;
    ensure(text(&state, &["new_chat", "draft"]) == "by the keys", || {
        format!("the draft: {state}")
    })?;
    // back to a–c, then Delete on the ringed kept row
    user.open_by_row(AC_CHAT)?;
    user.until("the kept row", |state| kept(state).len() == 1)?;
    user.press("ctrl+2")?;
    user.ring_to("kept-0")?;
    user.press("delete")?;
    let state = user.until("the row discarded by Delete", |state| kept(state).is_empty())?;
    ensure(state["main"] == chat_main(AC_CHAT), || {
        format!("the place changed: {state}")
    })
}

fn several_kept(user: &mut User<'_>) -> SandboxResult {
    leave_group_draft(user, "draft one")?;
    user.new_message_to(&["Lab C", "Outsider"])?;
    user.type_in_box("draft two")?;
    user.new_message_to(&["Lab B", "Lab C", "Outsider"])?;
    user.type_in_box("draft three")?;
    user.open_by_row(AB_CHAT)?;
    let state = user.until("three kept rows", |state| kept(state).len() == 3)?;
    let drafts: Vec<String> = kept(&state)
        .iter()
        .map(|row| row["draft"].as_str().unwrap_or_default().trim().to_owned())
        .collect();
    ensure(drafts == ["draft three", "draft two", "draft one"], || {
        format!(
            "the kept rows' drafts, latest first: {drafts:?} ({:?})",
            kept_names(&state)
        )
    })?;
    user.shots_in_both_themes("three-kept-rows")?;
    // each opens with its own draft; the one shown is no kept row then
    for (index, want) in [(1, "draft two"), (0, "draft three"), (2, "draft one")] {
        user.open_by_row(AB_CHAT)?;
        user.until("three kept rows", |state| kept(state).len() == 3)?;
        // the order is the one of the last leave: find the row by its draft
        let state = user.state()?;
        let at = kept(&state)
            .iter()
            .position(|row| row["draft"].as_str().map(str::trim) == Some(want))
            .ok_or_else(|| format!("no kept row with {want}"))?;
        let _ = index;
        let state = user.open_kept(at)?;
        ensure(text(&state, &["new_chat", "draft"]) == want, || {
            format!("row {at} opened {state}")
        })?;
    }
    Ok(())
}

fn back_forward(user: &mut User<'_>) -> SandboxResult {
    let mut findings = Vec::new();
    leave_group_draft(user, "back and forth")?;
    user.click(BACK)?;
    let state = user.until("Back shows the new chat", |state| state["main"] == "NewChat")?;
    ensure(text(&state, &["new_chat", "draft"]) == "back and forth", || {
        format!("Back showed {state}")
    })?;
    user.settled_shot("back-shows-new-chat")?;
    // the user types on after Back, as in a chat that Back shows
    user.type_text(" again")?;
    let state = user.state()?;
    let draft = text(&state, &["new_chat", "draft"]);
    if draft != "back and forth again" {
        findings.push(format!(
            "after Back (arrow) shows the new chat, typed text goes nowhere: its draft is {draft:?}, its box has the \
             keyboard: {}",
            state["new_chat"]["box_focused"]
        ));
        user.shot("back-typed-text-lost")?;
    }
    user.click(FORWARD)?;
    user.until("Forward shows a–c", |state| state["main"] == chat_main(AC_CHAT))?;
    user.press("alt+left")?;
    let state = user.until("Alt+Left shows the new chat", |state| state["main"] == "NewChat")?;
    ensure(
        text(&state, &["new_chat", "draft"]).starts_with("back and forth"),
        || format!("Alt+Left showed {state}"),
    )?;
    user.press("alt+right")?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    if state["main"] != chat_main(AC_CHAT) {
        findings.push(format!(
            "Alt+Right after Alt+Left to the new chat does nothing: {} shown, history {}",
            state["main"], state["history"]
        ));
        user.click(FORWARD)?;
    }
    let state = user.until("a–c again", |state| state["main"] == chat_main(AC_CHAT))?;
    ensure(kept(&state).len() == 1, || format!("the kept rows: {state}"))?;
    ensure(findings.is_empty(), || short(&findings.join("; ")))
}

fn settings_return(user: &mut User<'_>) -> SandboxResult {
    user.start_group_from_ab(&["Outsider"])?;
    user.type_text("settings and back")?;
    user.click(RAIL_SETTINGS)?;
    user.until("Settings", |state| state["main"] == "Settings")?;
    user.settled_shot("settings-from-new-chat")?;
    user.press("ctrl+2")?;
    let state = user.until("the chats again", |state| state["main"] != "Settings")?;
    ensure(state["main"] == "NewChat", || {
        format!("Ctrl+2 from Settings showed {state}")
    })?;
    ensure(text(&state, &["new_chat", "draft"]) == "settings and back", || {
        format!("the draft: {state}")
    })?;
    ensure(state["new_chat"]["group"] == true, || {
        format!("not the group chat: {state}")
    })?;
    // a new chat left for another chat, then Settings from that chat: the chat again, the row kept
    user.open_by_row(AC_CHAT)?;
    user.click(RAIL_SETTINGS)?;
    user.until("Settings", |state| state["main"] == "Settings")?;
    user.press("ctrl+2")?;
    let state = user.until("a–c again", |state| state["main"] == chat_main(AC_CHAT))?;
    ensure(kept(&state).len() == 1, || format!("the kept rows: {state}"))
}

fn first_message_makes_chat(user: &mut User<'_>) -> SandboxResult {
    let before = user.state()?["list_ids"].clone();
    leave_group_draft(user, "first words")?;
    user.open_kept(0)?;
    user.press("enter")?;
    let state = user.until("the chat made", |state| {
        state["main"].as_str().is_some_and(|main| main.starts_with("Chat("))
    })?;
    let main = state["main"].as_str().unwrap_or_default().to_owned();
    let known = before.as_array().cloned().unwrap_or_default();
    ensure(
        !known
            .iter()
            .any(|id| chat_main(id.as_str().unwrap_or_default()) == main),
        || format!("an old chat opened: {main}"),
    )?;
    let state = user.until("the row of the new chat in the list", |state| {
        state["list_ids"].as_array().is_some_and(|ids| ids.len() > known.len())
    })?;
    ensure(kept(&state).is_empty(), || format!("the kept row stays: {state}"))?;
    ensure(state["sidebar_draft_row"].is_null(), || {
        format!("a new chat's row stays: {state}")
    })?;
    ensure(text(&state, &["chat", "draft"]).is_empty(), || {
        format!("the box: {state}")
    })?;
    let made = main.trim_start_matches("Chat(\"").trim_end_matches("\")").to_owned();
    let state = user.until("the new chat's preview", |state| {
        state["previews"][made.as_str()]
            .as_str()
            .is_some_and(|preview| preview.contains("first words"))
    })?;
    ensure(state["main"] == chat_main(&made), || format!("the chat shown: {state}"))?;
    user.shots_in_both_themes("first-message-made")
}

fn draft_to_existing_chat(user: &mut User<'_>) -> SandboxResult {
    let state = user.new_message_to(&["Lab B"])?;
    ensure(state["new_chat"]["found"] == AB_CHAT, || {
        format!("no chat found: {state}")
    })?;
    user.type_in_box("for b only")?;
    user.open_by_row(AC_CHAT)?;
    let state = user.state()?;
    ensure(kept(&state).is_empty(), || {
        format!("a kept row for a chat that exists: {state}")
    })?;
    let state = user.open_by_row(AB_CHAT)?;
    ensure(text(&state, &["chat", "draft"]) == "for b only", || {
        format!("a–b's box: {state}")
    })?;
    user.settled_shot("found-draft-in-ab")?;
    // b and c have a group chat: the draft goes there
    let state = user.new_message_to(&["Lab B", "Lab C"])?;
    ensure(state["new_chat"]["found"] == ABC_CHAT, || {
        format!("no group found: {state}")
    })?;
    user.type_in_box("for both")?;
    user.open_by_row(AC_CHAT)?;
    let state = user.open_by_row(ABC_CHAT)?;
    ensure(text(&state, &["chat", "draft"]) == "for both", || {
        format!("the group's box: {state}")
    })
}

fn update_waits(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    ensure(state["waiting"].is_null(), || {
        format!("an update waits at the start: {state}")
    })?;
    leave_group_draft(user, "do not lose me")?;
    let state = user.state()?;
    ensure(text(&state, &["chat", "draft"]).is_empty(), || {
        format!("a–c's box: {state}")
    })?;
    ensure(state["waiting"] == "Unsent", || {
        format!("the update does not wait for the kept draft: {state}")
    })?;
    // a kept group chat with no draft holds no text: nothing to wait for
    user.reset()?;
    leave_group_draft(user, "")?;
    let state = user.state()?;
    ensure(kept(&state).len() == 1, || {
        format!("the empty group chat is not kept: {state}")
    })?;
    ensure(state["waiting"].is_null(), || {
        format!("an update waits for an empty draft: {state}")
    })
}

fn long_name_many_people(user: &mut User<'_>) -> SandboxResult {
    user.new_message_to(&["Lab B", "Lab C", "Outsider"])?;
    user.click(GROUP_NAME_TOGGLE)?;
    user.until("the group name line", |state| state["new_chat"]["naming"] == true)?;
    let long = "Synthetic planning group for the Friday lunch and the quarterly offsite with a very long name";
    user.type_text(long)?;
    user.type_in_box("a long name")?;
    user.settled_shot("long-name-new-message")?;
    user.open_by_row(AB_CHAT)?;
    let state = user.until("the kept row", |state| kept(state).len() == 1)?;
    ensure(kept_names(&state) == [long], || {
        format!("the row's name: {:?}", kept_names(&state))
    })?;
    user.shots_in_both_themes("long-name-kept-row")?;
    // the people's names, with no group name: all three in a group chat from a–b
    user.reset()?;
    user.start_group_from_ab(&["Lab C", "Outsider"])?;
    user.type_text("three people")?;
    user.open_by_row(AC_CHAT)?;
    let state = user.until("the kept row", |state| kept(state).len() == 1)?;
    let names = kept_names(&state);
    ensure(names.first().is_some_and(|name| name.contains("Outsider")), || {
        format!("the row's name: {names:?}")
    })?;
    user.settled_shot("many-people-kept-row").map(drop)
}

fn empty_drafts(user: &mut User<'_>) -> SandboxResult {
    // New message with people and no draft: nothing to lose, no row
    user.new_message_to(&["Lab C", "Outsider"])?;
    user.open_by_row(AB_CHAT)?;
    let state = user.state()?;
    ensure(kept(&state).is_empty(), || {
        format!("an empty New message kept: {state}")
    })?;
    // a draft with nobody on To: has no row; Ctrl+N shows it again
    user.press("ctrl+n")?;
    user.until("New message", |state| state["main"] == "NewChat")?;
    user.type_in_box("for nobody yet")?;
    user.open_by_row(AB_CHAT)?;
    let state = user.state()?;
    ensure(kept(&state).is_empty(), || {
        format!("a row for a draft to nobody: {state}")
    })?;
    user.press("ctrl+n")?;
    let state = user.until("New message", |state| state["main"] == "NewChat")?;
    ensure(text(&state, &["new_chat", "draft"]) == "for nobody yet", || {
        format!("Ctrl+N lost the draft: {state}")
    })?;
    // a draft typed and then deleted in a new group chat: the group chat is still kept
    user.reset()?;
    user.start_group_from_ab(&["Outsider"])?;
    user.type_text("gone")?;
    for _ in 0.."gone".len() {
        user.press("backspace")?;
    }
    user.open_by_row(AC_CHAT)?;
    let state = user.state()?;
    ensure(kept(&state).len() == 1, || {
        format!("the new group chat is not kept: {state}")
    })
}

fn keyboard_only(user: &mut User<'_>) -> SandboxResult {
    // New message, two people, a draft, then Ctrl+Alt+G to a–c, then back by the list's keys
    user.press("ctrl+n")?;
    user.until("New message", |state| state["main"] == "NewChat")?;
    for person in ["Lab C", "Outsider"] {
        user.take(person)?;
    }
    user.until("two people", |state| {
        state["new_chat"]["people"].as_array().map(Vec::len) == Some(2)
    })?;
    // the box by Tab from To: (or Shift+Tab back from its toolbar)
    let mut tabs = Vec::new();
    for key in ["tab", "tab", "tab", "shift+tab", "shift+tab", "shift+tab"] {
        if user.state()?["new_chat"]["box_focused"] == true {
            break;
        }
        user.press(key)?;
        tabs.push(key);
    }
    let state = user.state()?;
    say(&format!(
        "  the keys from To: to the box: {tabs:?}, reached {}",
        state["new_chat"]["box_focused"]
    ));
    let mut findings = Vec::new();
    if state["new_chat"]["box_focused"] != true {
        findings.push("no Tab or Shift+Tab takes the keyboard from To: to the box (it goes to the toolbar)".to_owned());
        user.settled_shot("tab-from-to-misses-box")?;
        // one click, so the rest of the round trip is still checked by the keys
        user.click(COMPOSE)?;
    }
    user.type_text("typed with keys")?;
    user.press("ctrl+alt+g")?;
    user.type_text("Lab B")?;
    thread::sleep(PICKER);
    user.press("enter")?;
    let state = user.until("a–b by the keys", |state| state["main"] == chat_main(AB_CHAT))?;
    ensure(kept(&state).len() == 1, || format!("the kept rows: {state}"))?;
    user.press("ctrl+2")?;
    user.ring_to("kept-0")?;
    user.press("enter")?;
    let state = user.until("the kept new chat", |state| state["main"] == "NewChat")?;
    ensure(text(&state, &["new_chat", "draft"]) == "typed with keys", || {
        format!("the draft: {state}")
    })?;
    ensure(state["new_chat"]["box_focused"] == true, || format!("the box: {state}"))?;
    user.type_text(" and more")?;
    let state = user.state()?;
    ensure(
        text(&state, &["new_chat", "draft"]) == "typed with keys and more",
        || format!("the draft: {state}"),
    )?;
    ensure(findings.is_empty(), || findings.join("; "))
}

/// Not a check: the history and the focus after each Back and Forward, for the report.
fn history_probe(user: &mut User<'_>) -> SandboxResult {
    let mut log = String::new();
    let note = |user: &mut User<'_>, log: &mut String, step: &str| -> SandboxResult {
        let state = user.state()?;
        log.push_str(&short(&format!(
            "{step}: main {} history {} focused {} chat-box-focused {} new-chat-box-focused {}\n",
            state["main"],
            state["history"],
            state["focused"],
            state["chat"]["box_focused"],
            state["new_chat"]["box_focused"]
        )));
        Ok(())
    };
    leave_group_draft(user, "probe")?;
    note(user, &mut log, "left for a-c")?;
    user.press("alt+left")?;
    thread::sleep(SETTLE);
    user.type_text("where am i")?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    log.push_str(&short(&format!(
        "typed after alt+left: main {} new chat draft {} chat {} draft {}\n",
        state["main"], state["new_chat"]["draft"], state["chat"]["id"], state["chat"]["draft"]
    )));
    user.shot("typed-after-back")?;
    user.click(FORWARD)?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    log.push_str(&short(&format!(
        "forward: main {} chat draft {} kept {}\n",
        state["main"], state["chat"]["draft"], state["kept"]
    )));
    user.shot("a-c-after-typing")?;
    for (step, how) in [
        ("alt+left", "key"),
        ("alt+right", "key"),
        ("alt+left", "key"),
        ("alt+right", "key"),
        ("back", "click"),
        ("forward", "click"),
        ("back", "click"),
        ("alt+right", "key"),
    ] {
        match (step, how) {
            ("back", _) => user.click(BACK)?,
            ("forward", _) => user.click(FORWARD)?,
            (key, _) => user.press(key)?,
        }
        thread::sleep(SETTLE);
        note(user, &mut log, step)?;
    }
    say(&log);
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        fs::write(PathBuf::from(dir).join("history-probe.txt"), &log)?;
    }
    Ok(())
}

// --- the own server ---

const OWN_UPN: &str = "dana@own.example.test";
const OWN_NAME: &str = "Dana Made-Up";
const ERIN: (&str, &str) = ("erin@own.example.test", "Erin Synthetic");
const FINN: (&str, &str) = ("finn@own.example.test", "Finn Synthetic");
const GINA: (&str, &str) = ("gina@own.example.test", "Gina Synthetic");

type Outcome = (Vec<String>, Vec<(String, String)>);

fn own_pass(sandbox: &Sandbox) -> SandboxResult<Outcome> {
    let Some(binary) = own_server_binary::binary()? else {
        say("skipped the own server: no ngteams-server binary (cargo build -p ngteams-server)");
        return Ok((
            Vec::new(),
            vec![("own_server_main_flow".to_owned(), "skipped: no binary".to_owned())],
        ));
    };
    let server = own_server_binary::BinaryServer::start(&binary)?;
    server.add_user(OWN_UPN, OWN_NAME)?;
    let mut people = Vec::new();
    for (upn, name) in [ERIN, FINN, GINA] {
        people.push(format!("8:orgid:{}", server.add_user(upn, name)?));
    }
    let dir = tempfile::tempdir()?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let data = dir.path().to_string_lossy().into_owned();
    let people = people.join(",");
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "own"),
            (ORIGIN_ENV, &server.origin),
            (CA_ENV, &ca),
            (DIR_ENV, &data),
            (PEOPLE_ENV, &people),
        ],
    )?;
    let mut user = User::start(sandbox, Running::new(child)?, "own")?;
    let (mut passed, mut failed) = (Vec::new(), Vec::new());
    user.run_check("own_server_main_flow", own_main_flow, &mut passed, &mut failed);
    user.quit()?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let _ = fs::write(PathBuf::from(evidence).join("own-server.log"), server.log());
    }
    Ok((passed, failed))
}

fn own_main_flow(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    let chats: Vec<String> = state["list_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|id| id.as_str().map(str::to_owned))
        .collect();
    ensure(chats.len() >= 2, || format!("Dana's chats: {state}"))?;
    let erin = user.ask("own-erin")?;
    let finn = user.ask("own-finn")?;
    let opened = user.ask(&format!("open {erin}"))?;
    ensure(opened == "ok", || format!("open: {opened}"))?;
    user.until("Erin's chat", |state| state["main"] == chat_main(&erin))?;
    user.settled_shot("erin-chat")?;
    let state = user.start_group_from_ab_on(&erin, &["Gina"])?;
    ensure(state["new_chat"]["people"].as_array().map(Vec::len) == Some(2), || {
        format!("the people: {state}")
    })?;
    user.type_text("Own server draft ")?;
    user.open_by_row(&finn)?;
    let state = user.until("the kept row", |state| kept(state).len() == 1)?;
    user.shots_in_both_themes("own-kept-row")?;
    let _ = state;
    let state = user.open_kept(0)?;
    ensure(text(&state, &["new_chat", "draft"]) == "Own server draft", || {
        format!("the draft: {state}")
    })?;
    user.press("enter")?;
    let state = user.until("the chat made on the own server", |state| {
        state["main"].as_str().is_some_and(|main| main.starts_with("Chat("))
            && state["main"] != chat_main(&erin)
            && state["main"] != chat_main(&finn)
    })?;
    ensure(kept(&state).is_empty(), || format!("the kept row stays: {state}"))?;
    user.until("the new chat in the list", |state| {
        state["list_ids"].as_array().is_some_and(|ids| ids.len() > chats.len())
    })?;
    user.settled_shot("own-made-light").map(drop)
}

impl User<'_> {
    /// The 1:1 chat `id`'s people → "Start a group chat" → `who` → Create.
    fn start_group_from_ab_on(&mut self, id: &str, who: &[&str]) -> SandboxResult<Value> {
        let state = self.state()?;
        ensure(state["main"] == chat_main(id), || format!("not in {id}: {state}"))?;
        self.click(PEOPLE_BUTTON)?;
        self.until("the people flyout", |state| state["people_open"] == true)?;
        self.click(START_GROUP)?;
        thread::sleep(STEP);
        for person in who {
            self.take(person)?;
        }
        self.settled_shot("own-picker")?;
        self.click(CREATE)?;
        self.until("the new group chat shown", |state| {
            state["main"] == "NewChat" && state["new_chat"]["group"] == true
        })
    }
}

// --- the child: the app, answering the parent ---

struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock, c's message in the a–c chat, and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    _mock: ChatServiceMock,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        mock.post_as(LAB_C, AC_CHAT, COPIED_HTML).ok_or("c's post refused")?;
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: vec![
                (LAB_B.to_owned(), "Lab B".to_owned()),
                (LAB_C.to_owned(), "Lab C".to_owned()),
                (OUTSIDER.to_owned(), "Outsider X".to_owned()),
            ],
        })?;
        let lab = Self {
            _runtime: runtime,
            _mock: mock,
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }
}

/// What the child knows beyond the window: the own server's chats, and the chats seeded at the start.
#[derive(Clone, Default)]
struct Extra {
    own_chats: Vec<String>,
    seeded: Vec<String>,
}

fn mock_child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let (lab, engine) = MockLab::start()?;
    let first = support::started(Alias::A, engine)?;
    let runner = support::runner(lab.dir.path())?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("settings.toml"));
    let extra = Extra {
        own_chats: Vec::new(),
        seeded: vec![
            AB_CHAT.to_owned(),
            AC_CHAT.to_owned(),
            AX_CHAT.to_owned(),
            ABC_CHAT.to_owned(),
        ],
    };
    run_app(first, runner, settings, extra)?;
    drop(lab);
    Ok(())
}

fn env(name: &str) -> SandboxResult<String> {
    std::env::var(name).map_err(|_| format!("{name} unset").into())
}

fn own_child() -> SandboxResult {
    use ngteams::accounts_file::{self, PersonalPaths};
    use ngteams::setup::Setup;
    use teams_auth::secrets::MemorySecretStore;
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(DIR_ENV)?);
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?;
    let server = accounts_file::OwnServer::parse(&env(ORIGIN_ENV)?, &env(CA_ENV)?)?;
    let store = MemorySecretStore::new();
    let runtime = ngteams::accounts::runtime()?;
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        OWN_UPN,
        &server,
        &Secret::new(own_server_binary::PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, true, None)?;
    // Dana's 1:1 chats with Erin and Finn, each with a first message, made before the window opens
    let people = env(PEOPLE_ENV)?;
    let people: Vec<&str> = people.split(',').collect();
    let mut chats = Vec::new();
    for (mri, hello) in people
        .iter()
        .take(2)
        .zip(["Hello Erin, a made-up hello", "Hello Finn, a made-up hello"])
    {
        let made = runtime.block_on(engine.handle.write(Action::CreateChat {
            others: vec![Mri::new(*mri)],
            topic: None,
        }))?;
        let chat = made.message_id.ok_or("no chat made")?;
        runtime.block_on(engine.handle.write(Action::Send {
            conversation: chat.clone(),
            text: MessageText::plain(hello)?,
        }))?;
        chats.push(chat);
    }
    let first = ngteams::accounts::Started {
        account,
        engine,
        calls: None,
    };
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let extra = Extra {
        seeded: chats.clone(),
        own_chats: chats,
    };
    run_app(first, runner, settings, extra)?;
    drop(runtime);
    Ok(())
}

fn run_app<S: teams_auth::secrets::SecretStore + 'static>(
    first: ngteams::accounts::Started<S>,
    runner: ngteams::accounts::Runner,
    settings: teams_ui::SettingsFile,
    extra: Extra,
) -> SandboxResult {
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                // what holds an update's restart up, wired as the installed app wires it
                teams_ui::update::holds_with(ngteams::update::holds(launched.desktop.downgrade()), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, extra, cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    Ok(())
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn answer(window: WindowHandle<KitRoot>, extra: Extra, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = match verb {
                "quit" => {
                    say(&format!("re {seq} ok"));
                    cx.update(|cx| cx.quit());
                    return;
                }
                "own-erin" => extra.own_chats.first().cloned().unwrap_or_default(),
                "own-finn" => extra.own_chats.get(1).cloned().unwrap_or_default(),
                _ => window
                    .update(cx, |kit, window, cx| {
                        let shell = kit
                            .view()
                            .clone()
                            .downcast::<Root>()
                            .ok()?
                            .read(cx)
                            .shell_in(cx)
                            .cloned()?;
                        Some(on_shell(&shell, verb, &extra, window, cx))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "no shell".to_owned()),
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, extra: &Extra, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, extra, window, cx).to_string(),
        "reset" => {
            reset(shell, extra, window, cx);
            "ok".to_owned()
        }
        "theme-dark" | "theme-light" => {
            let key = if verb == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let model = shell.read(cx).settings().clone();
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
            "ok".to_owned()
        }
        other => {
            if let Some(id) = other.strip_prefix("open ") {
                shell.update(cx, |shell, cx| shell.open_chat(id.to_owned(), window, cx));
                "ok".to_owned()
            } else {
                format!("unknown {other}")
            }
        }
    }
}

/// No kept new chats, no drafts in the seeded chats, the first seeded chat shown, Teams Light.
fn reset(shell: &Entity<Shell>, extra: &Extra, window: &mut Window, cx: &mut App) {
    let first = extra.seeded.first().cloned().unwrap_or_default();
    for chat in extra.seeded.iter().rev() {
        shell.update(cx, |shell, cx| shell.open_chat(chat.clone(), window, cx));
        if let Some(view) = shell.read(cx).chat().cloned() {
            let compose = view.read(cx).compose().clone();
            compose.update(cx, |compose, cx| {
                compose.editor().update(cx, teams_ui::editor::Editor::clear);
            });
        }
    }
    shell.update(cx, |shell, cx| shell.open_chat(first, window, cx));
    let kept: Vec<u64> = shell
        .read(cx)
        .kept_new_chats(cx)
        .iter()
        .map(|view| view.entity_id().as_u64())
        .collect();
    for id in kept {
        shell.update(cx, |shell, cx| shell.discard_kept_new_chat(id, cx));
    }
    let model = shell.read(cx).settings().clone();
    model.update(cx, |model, cx| {
        model.change(
            |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = teams_ui::theme::LIGHT_KEY;
            },
            cx,
        );
    });
}

fn compose_text(compose: &Entity<teams_ui::compose::Compose>, cx: &App) -> String {
    compose.read(cx).value(cx).to_string()
}

fn state_of(shell: &Entity<Shell>, extra: &Extra, window: &Window, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = format!("{:?}", shell_ref.main());
    let chat = shell_ref.chat().map(|view| {
        let view = view.read(cx);
        json!({
            "id": view.conversation(),
            "draft": compose_text(view.compose(), cx),
            "box_focused": view.compose().read(cx).focused(window, cx),
        })
    });
    let new_chat = shell_ref
        .new_chat()
        .filter(|_| *shell_ref.main() == Main::NewChat)
        .map(|view| {
            let compose = view.read(cx).compose(cx);
            let read = view.read(cx);
            json!({
                "people": read.people(),
                "found": read.found(),
                "naming": read.naming(),
                "group": read.draft_row().is_some(),
                "group_name": read.kept_row(cx).map(|row| row.name.to_string()),
                "draft": compose_text(&compose, cx),
                "box_focused": compose.read(cx).focused(window, cx),
            })
        });
    let sidebar = shell_ref.sidebar().read(cx);
    let entities = shell_ref.kept_new_chats(cx);
    let kept: Vec<Value> = sidebar
        .kept_new_chats()
        .iter()
        .map(|(id, row)| {
            let draft = entities
                .iter()
                .find(|view| view.entity_id().as_u64() == *id)
                .map(|view| compose_text(&view.read(cx).compose(cx), cx));
            json!({"id": id, "name": row.name.to_string(), "draft": draft})
        })
        .collect();
    let list = shell_ref.list().read(cx);
    let list_ids: Vec<String> = list.rows().iter().map(|row| row.id.clone()).collect();
    let previews: serde_json::Map<String, Value> = list
        .rows()
        .iter()
        .map(|row| (row.id.clone(), json!(row.preview.to_string())))
        .collect();
    // the rows under "Chats" in their drawn order: New message's row, the kept rows, then the chats shown
    let mut sidebar_rows = Vec::new();
    if sidebar.new_message_open() {
        sidebar_rows.push("new-message".to_owned());
    }
    sidebar_rows.extend((0..kept.len()).map(|index| format!("kept-{index}")));
    sidebar_rows.extend(
        list.shown_rows()
            .into_iter()
            .filter_map(|index| list.rows().get(index))
            .map(|row| row.id.clone()),
    );
    json!({
        "main": main,
        "chat": chat,
        "new_chat": new_chat,
        "people_open": shell_ref.people().is_some(),
        "suggestions": shell_ref
            .people()
            .map(|people| people.read(cx).suggestions().len())
            .or_else(|| shell_ref.new_chat().map(|view| view.read(cx).suggestions().len())),
        "kept": kept,
        "sidebar_draft_row": sidebar.new_message_draft().map(|row| row.name.to_string()),
        "sidebar_rows": sidebar_rows,
        "list_ids": list_ids,
        "favorites": list.favorites().len(),
        "previews": previews,
        "history": format!("{:?}", shell_ref.history()),
        "focused": format!("{:?}", window.focused(cx)),
        "waiting": teams_ui::update::waiting(cx).map(|waiting| format!("{waiting:?}")),
        "seeded": extra.seeded,
    })
}
