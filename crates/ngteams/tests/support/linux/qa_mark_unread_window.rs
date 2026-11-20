//! "Mark as unread" as a user does it in the app's window in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread). It is the QA test of task 660.
//!
//! The parent runs the mock (`teams-mock`) with made-up messages and runs the app as a child in the sandbox, its store
//! and settings in one directory, so a later run is a restart of the same app. The child reaches the chat service
//! through a small TCP relay of the parent, which the parent can cut, as a network that goes down. The parent moves
//! the sandbox's pointer and presses its keys; it finds the hover bar, the menus and the chat rows' "..." on the
//! screen, reads the views (the divider, the chat list's counts, the menus open) from the child, and reads the writes
//! on the mock.
//!
//! - **The message menu**: "Mark as unread" in a 1:1 chat, by its key U in a group chat, in a meeting chat, on the
//!   account's own message, on a channel post (the post's thread's newest message is named). The divider moves before
//!   the message, the list counts the chat unread, the bookmark goes out, and no read receipt while the chat shows.
//! - **The mark stays and ends**: new messages after the mark, the account's own message sent from the box, the
//!   window's focus given to another window and back, then the chat shown again after the calendar, a marked chat
//!   opened again, the same with read receipts off.
//! - **The chat row's "More chat options"**: by the pointer ("..." on hover), Shift+F10 and the menu key; "Mark as
//!   unread" and "Mark as read", also on a chat with unread messages from others.
//! - **Another client of the account**: a mark pushed over Trouter; a mark in the conversation list at a restart
//!   (made while the app was closed).
//! - **The outbox**: a mark made while the network is down, then the network back.
//! - **A restart** keeps a mark made in the app. **Two accounts** in one window: a's mark leaves b alone.
//! - **The look**: Teams Light and Dark, a larger font, a narrow window.
//!
//! Every name, id and text is made up. Screenshots are kept under `target/ui-shots/qa660-*.png`. `QA660_ONLY`
//! (`walk`, `restart` or `two`, a comma list) runs only those runs; `QA660_EVIDENCE` names a directory that gets a
//! copy of each screenshot. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Read as _, Write as _};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AppContext as _, AsyncApp, Entity, Window, WindowHandle, WindowOptions, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, ABC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE,
    PRESENCE_BASE, PROVISIONING_BASE, TEAM_CHANNEL, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{ChatView, Item};
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{Modifier, VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "QA660_CHILD";
const ORIGIN_ENV: &str = "QA660_ORIGIN";
const TROUTER_ENV: &str = "QA660_TROUTER";
const TOKEN_A_ENV: &str = "QA660_TOKEN_A";
const TOKEN_B_ENV: &str = "QA660_TOKEN_B";
const DIR_ENV: &str = "QA660_DIR";
/// `a` (a's account alone) or `ab` (a and b in one window).
const MODE_ENV: &str = "QA660_MODE";
const ONLY_ENV: &str = "QA660_ONLY";
const EVIDENCE_ENV: &str = "QA660_EVIDENCE";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa660-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(40);
const POLL: Duration = Duration::from_millis(100);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(3000);
const SCREEN: (u32, u32) = (1280, 800);

/// Teams Light's surface and its borders.
const SURFACE: [u8; 3] = [255, 255, 255];
const BORDER: [u8; 3] = [224, 224, 224];
/// The message box of a chat on the 1280×800 screen, a one-line box.
const COMPOSE: (u32, u32) = (700, 749);
/// The evdev codes of F10, the context-menu key, the 2 and 4 keys.
const KEY_F10: u32 = 68;
const KEY_MENU: u32 = 127;
const KEY_2: u32 = 3;
const KEY_4: u32 = 5;
const KEY_EQUAL: u32 = 13;
const KEY_0: u32 = 11;
/// The chat list's rows: their height, a point inside the list's column, and a first guess of where the first row of
/// "Chats" is (the walk finds it on the screen).
const ROW: u32 = 64;
const ROW_X: u32 = 200;
const FIRST_ROW_GUESS: u32 = 160;
/// The mock's team channel's post with replies under it.
const CHANNEL_POST: &str = "1760000300000";
/// The made-up meeting chat.
const MEETING: &str = "19:meeting_made-up-qa660@thread.v2";

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_mark_unread_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

/// Notes `finding` unless `ok`.
fn expect(ok: bool, finding: impl FnOnce() -> String) {
    if !ok {
        note(finding());
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn wanted(run: &str) -> bool {
    let only = std::env::var(ONLY_ENV).unwrap_or_default();
    only.is_empty() || only.split(',').any(|name| name.trim() == run)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

// --- the parent ---

fn parent() -> SandboxResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams mark as unread", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = runs(&sandbox, &runtime, &service, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams mark as unread: ok");
    Ok(())
}

fn runs(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    if wanted("walk") {
        step("--- run walk ---");
        let mut run = AppRun::launch(sandbox, &service.env("a"), dir)?;
        let outcome = (|| -> SandboxResult {
            let mut walk = Walk::new(&mut run, sandbox, runtime, service);
            walk.one_to_one_menu()?;
            walk.new_messages_after_the_mark()?;
            walk.focus_elsewhere_keeps_it()?;
            walk.shown_again_ends_it()?;
            walk.own_message()?;
            walk.group_by_key()?;
            walk.opened_again_ends_it()?;
            walk.meeting_chat()?;
            walk.channel_post()?;
            walk.row_menu_by_pointer()?;
            walk.row_menu_mark_read_of_others()?;
            walk.row_menu_by_keys()?;
            walk.pushed_from_another_client()?;
            walk.receipts_off()?;
            walk.looks()?;
            walk.offline()?;
            walk.kept_for_the_restart()
        })();
        if let Err(error) = outcome {
            return Err(run.fail(sandbox, &*error));
        }
        run.quit()?;
    }
    if wanted("restart") {
        step("--- run restart ---");
        // a's other client marks the meeting chat unread from its last message while this app is closed: the
        // walk's own mark there counted 2, the listed one counts 1
        let listed = service.ids.meet2.parse::<i64>()? - 1;
        service.bookmark_as(runtime, LAB_A, MEETING, &format!("{listed};{};1", now_ms()))?;
        let mut run = AppRun::launch(sandbox, &service.env("a"), dir)?;
        let outcome = Walk::new(&mut run, sandbox, runtime, service).after_restart();
        if let Err(error) = outcome {
            return Err(run.fail(sandbox, &*error));
        }
        run.quit()?;
    }
    if wanted("two") {
        step("--- run two ---");
        let mut run = AppRun::launch(sandbox, &service.env("ab"), dir)?;
        let outcome = Walk::new(&mut run, sandbox, runtime, service).two_accounts();
        if let Err(error) = outcome {
            return Err(run.fail(sandbox, &*error));
        }
        run.quit()?;
    }
    Ok(())
}

/// What one step of the walk works with.
struct Walk<'a> {
    run: &'a mut AppRun,
    sandbox: &'a Sandbox,
    runtime: &'a tokio::runtime::Runtime,
    service: &'a Service,
    /// Where the first row of "Chats" is drawn, once found.
    first_row: Option<u32>,
    /// The centre of the "..." clicked last on a chat row.
    dots: (u32, u32),
}

impl<'a> Walk<'a> {
    fn new(
        run: &'a mut AppRun,
        sandbox: &'a Sandbox,
        runtime: &'a tokio::runtime::Runtime,
        service: &'a Service,
    ) -> Self {
        Self {
            run,
            sandbox,
            runtime,
            service,
            first_row: None,
            dots: (0, 0),
        }
    }
}

/// Where the parts of a screen are.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    const fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    const fn centre(&self) -> (u32, u32) {
        (self.x0 + (self.x1 - self.x0) / 2, self.y0 + (self.y1 - self.y0) / 2)
    }
}

impl Walk<'_> {
    fn ids(&self) -> &Ids {
        &self.service.ids
    }

    /// The shots of `name` in Teams Light, then Teams Dark, then back to Light.
    fn shots(&mut self, name: &str) -> SandboxResult<PathBuf> {
        let light = self.run.shot(self.sandbox, &format!("qa660-{name}-light.png"))?;
        self.run.theme(true)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, &format!("qa660-{name}-dark.png"))?;
        self.run.theme(false)?;
        thread::sleep(SETTLE);
        Ok(light)
    }

    /// Opens `chat` as a click on its row does, once its message `id` shows.
    fn open_chat(&mut self, chat: &str, id: &str) -> SandboxResult {
        self.run.question(&format!("open {chat}"))?;
        self.run.until("chat", &format!("the chat {chat} with {id}"), |state| {
            state["conversation"] == chat && item(state, id).is_some()
        })?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// The chat list's count of `chat`.
    fn count(&mut self, chat: &str) -> SandboxResult<u64> {
        let state = self.run.state("chat")?;
        Ok(state["counts"][chat].as_u64().unwrap_or(0))
    }

    /// Waits until the chat list counts `chat` as `wanted` takes it; the count.
    fn until_count(&mut self, chat: &str, what: &str, wanted: impl Fn(u64) -> bool) -> SandboxResult<u64> {
        let state = self.run.until("chat", what, |state| {
            wanted(state["counts"][chat].as_u64().unwrap_or(0))
        })?;
        Ok(state["counts"][chat].as_u64().unwrap_or(0))
    }

    /// Like [`Walk::until_count`], but a count that never comes is a finding, not the end of the walk.
    fn expect_count(&mut self, chat: &str, what: &str, wanted: impl Fn(u64) -> bool) -> SandboxResult<u64> {
        match self.until_count(chat, what, wanted) {
            Ok(count) => Ok(count),
            Err(error) => {
                note(error.to_string());
                self.count(chat)
            }
        }
    }

    /// The row of `id` in `view` inside the list's viewport: the list is scrolled with the wheel first when it is not.
    fn row(&mut self, view: &str, id: &str) -> SandboxResult<Rect> {
        let mut last = Value::Null;
        for _ in 0..40 {
            let state = self.run.state(view)?;
            last = json!({"viewport": state["viewport"], "row": item(&state, id)});
            let index = state["items"]
                .as_array()
                .and_then(|items| items.iter().position(|row| row["id"] == id))
                .ok_or_else(|| format!("no message {id} in the {view}: {state}"))?;
            let viewport = rect_of(&state["viewport"]).ok_or("no viewport")?;
            let rect = rect_of(&state["items"][index]["rect"]);
            if let Some(rect) = rect
                && rect.y0 >= viewport.y0 + 40
                && rect.y1 <= viewport.y1 + 2
            {
                return Ok(rect);
            }
            let placed = state["items"]
                .as_array()
                .is_some_and(|items| items.iter().any(|other| !other["rect"].is_null()));
            if !placed && let Some(found) = self.hover_scan(view, id, viewport)? {
                return Ok(found);
            }
            let below = rect.map_or_else(
                || {
                    state["items"].as_array().is_some_and(|items| {
                        items[..index]
                            .iter()
                            .filter_map(|other| rect_of(&other["rect"]))
                            .any(|other| other.y0 >= viewport.y1)
                    })
                },
                |rect| rect.y1 > viewport.y1 + 2,
            );
            let (x, y) = viewport.centre();
            self.run.input.scroll(x, y, if below { 3 } else { -3 })?;
            thread::sleep(Duration::from_millis(300));
        }
        Err(format!("the message {id} never came into the {view}'s view: {last}").into())
    }

    /// Where the message `id` is in the view, found as the pointer finds it: moved up the view, the rows it hovers.
    fn hover_scan(&mut self, view: &str, id: &str, viewport: Rect) -> SandboxResult<Option<Rect>> {
        let mut found: Option<Rect> = None;
        let mut y = viewport.y1.saturating_sub(3);
        while y > viewport.y0 + 2 {
            self.run.input.move_to(viewport.x0 + 6, y)?;
            thread::sleep(Duration::from_millis(40));
            let hovered = self.run.state(view)?["hovered"] == id;
            if hovered {
                found = Some(grow(found, viewport.x0, y));
            } else if found.is_some() {
                break;
            }
            y -= 4;
        }
        self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
        Ok(found.map(|rect| Rect {
            x1: viewport.x1,
            y1: rect.y1 + 3,
            ..rect
        }))
    }

    /// Points at the message `id` and finds its hover bar's "More options": its centre.
    fn more_button(&mut self, view: &str, id: &str) -> SandboxResult<(u32, u32)> {
        let row = self.row(view, id)?;
        let still = load(&self.run.shot(self.sandbox, "qa660-still.png")?)?;
        // in the row's left margin: never right of the bar, never on a face (its profile card would open)
        self.run
            .input
            .move_to(row.x0 + 6, row.y0 + (row.height() / 2).min(14))?;
        thread::sleep(SETTLE);
        let hover = load(&self.sandbox.screenshot("qa660-hover.png", false)?)?;
        let region = Rect {
            x0: row.x0,
            y0: row.y0.saturating_sub(48),
            x1: row.x1,
            y1: row.y0 + 36,
        };
        let bar = changed_box(&still, &hover, region).ok_or_else(|| format!("no hover bar over the message {id}"))?;
        let band = Rect {
            x0: bar.x1.saturating_sub(30),
            ..bar
        };
        let button = changed_box(&still, &hover, band).unwrap_or(band);
        Ok((bar.x1.saturating_sub(17), button.y0 + 17))
    }

    /// Opens the More options menu of `id` with a click on its hover bar's button; the button's centre.
    fn open_menu(&mut self, view: &str, id: &str) -> SandboxResult<(u32, u32)> {
        let mut last = String::new();
        for attempt in 0..2 {
            let found = self.more_button(view, id);
            let (x, y) = match found {
                Ok(at) => at,
                Err(error) => {
                    last = error.to_string();
                    step(&format!("attempt {attempt}: {last}"));
                    continue;
                }
            };
            for (dx, dy) in [(0_i32, 0_i32), (-6, -4), (-10, 4)] {
                let at = (x.saturating_add_signed(dx), y.saturating_add_signed(dy));
                self.run.click(at.0, at.1)?;
                thread::sleep(SETTLE);
                let state = self.run.state(view)?;
                if state["options"] == id {
                    return Ok(at);
                }
                if !state["options"].is_null() {
                    self.close_menu(view)?;
                }
            }
            last = format!("the More options menu of {id} never opened");
        }
        Err(last.into())
    }

    fn close_menu(&mut self, view: &str) -> SandboxResult {
        for _ in 0..2 {
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
            if self.run.state(view)?["options"].is_null() {
                return Ok(());
            }
        }
        self.run.click(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Opens the menu of `id` and chooses "Mark as unread" with a click, its row found under the menu's top edge (Teams
    /// Light). `own`: the menu also offers Delete before it. `shots`: the open menu's shots are kept under that name.
    fn mark_by_menu(&mut self, view: &str, id: &str, own: bool, shots: Option<&str>) -> SandboxResult {
        let (x, y) = self.open_menu(view, id)?;
        let menu = load(&self.run.shot(self.sandbox, "qa660-menu.png")?)?;
        let column = x.saturating_sub(100);
        // a menu with no room under its button opens over it: its own hairlines are not its top
        let top = panel_top(&menu, column, y + 12..menu.height())
            .or_else(|| panel_top(&menu, column, y.saturating_sub(320).max(60)..y))
            .ok_or("no menu's top edge under or over its button")?;
        if let Some(name) = shots {
            self.shots(name)?;
        }
        // the rows of Copy, Delete (the account's own): 32 px with 2 px between, under 1 + 4 px; a hairline (1 px with
        // 4 px over and under it); then Mark as unread
        let index = u32::from(own) + 1;
        let row_y = top + 1 + 4 + 34 * index + 11 + 16;
        self.run.click(column, row_y)?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// Selects the message `id` of the chat as the keys do: a click in the box, Escape, then Up until it is selected.
    fn select_by_keys(&mut self, id: &str) -> SandboxResult {
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(Duration::from_millis(300));
        for _ in 0..40 {
            self.run.input.key(keys::UP)?;
            thread::sleep(Duration::from_millis(150));
            if self.run.state("chat")?["selected"] == id {
                return Ok(());
            }
        }
        Err(format!("Up never selected the message {id}").into())
    }

    /// Waits until the divider of `view` stands before `id`; a divider elsewhere is a finding.
    fn expect_divider(&mut self, view: &str, id: &str, what: &str) {
        let outcome = self
            .run
            .until(view, &format!("{what}: the divider before {id}"), |state| {
                state["divider_before"] == id
            });
        if let Err(error) = outcome {
            note(error.to_string());
        }
    }

    /// The account `mri`'s bookmarks put on `chat` the mock got after `since` of them.
    fn bookmarks(&self, chat: &str, mri: &str, since: usize) -> SandboxResult<Vec<String>> {
        Ok(self
            .service
            .puts(self.runtime, chat, "consumptionHorizonBookmark", mri)?
            .into_iter()
            .skip(since)
            .collect())
    }

    /// Waits until a's bookmark puts on `chat` are more than `since`; the new ones.
    fn until_bookmark(&self, chat: &str, since: usize, what: &str) -> SandboxResult<Vec<String>> {
        let found = wait_until(WAIT, || {
            self.bookmarks(chat, LAB_A, since).ok().filter(|new| !new.is_empty())
        });
        found.ok_or_else(|| format!("{what}: no bookmark on {chat} reached the service").into())
    }

    /// Like [`Walk::until_bookmark`], but none is a finding; the new ones (maybe none).
    fn expect_bookmark(&self, chat: &str, since: usize, what: &str) -> Vec<String> {
        match self.until_bookmark(chat, since, what) {
            Ok(new) => new,
            Err(error) => {
                note(error.to_string());
                Vec::new()
            }
        }
    }

    fn bookmark_count(&self, chat: &str) -> SandboxResult<usize> {
        Ok(self.bookmarks(chat, LAB_A, 0)?.len())
    }

    fn receipt_count(&self, chat: &str) -> SandboxResult<usize> {
        Ok(self
            .service
            .puts(self.runtime, chat, "consumptionhorizon", LAB_A)?
            .len())
    }

    /// Checks a mark's bookmark: `<message id - 1>;<ms>;<clientmessageid>`, for the message `id`.
    fn expect_mark_value(value: &str, id: &str, what: &str) {
        let fields: Vec<&str> = value.split(';').collect();
        let before = id.parse::<i64>().map(|id| (id - 1).to_string()).unwrap_or_default();
        expect(
            fields.len() == 3 && fields[0] == before && fields[1].parse::<i64>().is_ok() && fields[2] != "0",
            || format!("{what}: the bookmark {value:?} does not name the message {id} (want {before};<ms>;<id>)"),
        );
    }

    /// Checks the end of a mark: `0;<ms>;0`.
    fn expect_end_value(value: &str, what: &str) {
        let fields: Vec<&str> = value.split(';').collect();
        expect(
            fields.len() == 3 && fields[0] == "0" && fields[2] == "0" && fields[1].parse::<i64>().is_ok(),
            || format!("{what}: the bookmark {value:?} is not the end of a mark (0;<ms>;0)"),
        );
    }

    /// No read receipt on `chat` for [`QUIET`] after `since` of them.
    fn expect_no_receipt(&self, chat: &str, since: usize, what: &str) -> SandboxResult {
        thread::sleep(QUIET);
        let now = self.receipt_count(chat)?;
        expect(now == since, || {
            format!(
                "{what}: {} read receipt(s) on {chat} went while the chat stayed shown",
                now - since
            )
        });
        Ok(())
    }

    /// Waits until a read receipt on `chat` reached the service, after `since` of them.
    fn wait_receipt(&self, chat: &str, since: usize) -> bool {
        wait_until(WAIT, || {
            self.receipt_count(chat).ok().filter(|count| *count > since).map(|_| ())
        })
        .is_some()
    }

    // --- the chat list ---

    /// The y of the chat row `chat` (its middle), from the rows the child reports in their drawn order.
    fn row_y(&mut self, chat: &str) -> SandboxResult<u32> {
        let state = self.run.state("chat")?;
        let rows = state["shown"].as_array().cloned().unwrap_or_default();
        let at = rows
            .iter()
            .position(|row| row.as_str() == Some(chat))
            .ok_or_else(|| format!("no row {chat} in {rows:?}"))?;
        // the Favorites section, when there is one, is a header and its rows above the "Chats" header
        let favorites = u32::try_from(state["favorites"].as_u64().unwrap_or(0))?;
        let guess = if favorites == 0 {
            FIRST_ROW_GUESS
        } else {
            FIRST_ROW_GUESS + favorites * ROW + ROW / 2
        };
        Ok(self.first_row.unwrap_or(guess) + u32::try_from(at)? * ROW)
    }

    /// Points at the row of `chat` and clicks its "...", found on the screen; the menu open, checked by the child.
    /// A wrong first guess of the rows' place is corrected from the row whose menu opened.
    fn open_row_menu(&mut self, chat: &str, shot: Option<&str>) -> SandboxResult {
        for _ in 0..4 {
            let y = self.row_y(chat)?;
            self.run.input.move_to(self.run.rest.0, self.run.rest.1)?;
            thread::sleep(Duration::from_millis(400));
            let still = load(&self.sandbox.screenshot("qa660-list-still.png", false)?)?;
            self.run.input.move_to(ROW_X, y)?;
            thread::sleep(SETTLE);
            let hover_png = self.sandbox.screenshot("qa660-list-hover.png", false)?;
            let hover = load(&hover_png)?;
            if let Some(name) = shot {
                keep(&hover_png, &format!("qa660-{name}.png"))?;
            }
            let band = Rect {
                x0: 60,
                y0: y.saturating_sub(ROW / 2 - 4),
                x1: SCREEN.0 / 2,
                y1: y + ROW / 2 - 4,
            };
            let row = changed_box(&still, &hover, band).ok_or_else(|| format!("no hovered row at y {y}"))?;
            // the "..." is the dark glyph in the row's right end, over the time it covers
            let end = Rect {
                x0: row.x1.saturating_sub(72),
                ..row
            };
            let dots = pixels_box(&hover, end, |pixel| pixel.iter().all(|&channel| channel < 170))
                .ok_or_else(|| format!("no \"...\" in the hovered row of {chat}"))?;
            let (x, dy) = dots.centre();
            self.dots = (x, dy);
            self.run.click(x, dy)?;
            thread::sleep(SETTLE);
            let state = self.run.state("chat")?;
            let open = state["row_menu"].as_str().map(str::to_owned);
            if open.as_deref() == Some(chat) {
                return Ok(());
            }
            let Some(other) = open else {
                return Err(format!("the \"...\" of {chat} opened no menu").into());
            };
            // another row's menu: the rows are drawn elsewhere than guessed
            self.run.input.key(keys::ESC)?;
            thread::sleep(SETTLE);
            let shown = state["shown"].as_array().cloned().unwrap_or_default();
            let index = |id: &str| shown.iter().position(|row| row.as_str() == Some(id));
            let (Some(want), Some(got)) = (index(chat), index(&other)) else {
                return Err(format!("the menu of {other} opened for {chat}").into());
            };
            let first = y - u32::try_from(want)? * ROW;
            let shift = (i64::try_from(want)? - i64::try_from(got)?) * i64::from(ROW);
            self.first_row = Some(u32::try_from(i64::from(first) + shift)?);
            step(&format!("the first row is at {:?}", self.first_row));
        }
        Err(format!("the row menu of {chat} never opened").into())
    }

    /// Chooses the open row menu's one entry with a click: the menu opens under the "...", its left edge the
    /// button's; its top edge is found in Teams Light (a panel's border over its surface, not a focus line).
    fn choose_row_entry(&mut self, chat: &str, entry: &str) -> SandboxResult {
        let png = self.run.shot(self.sandbox, "qa660-row-menu.png")?;
        let shot = load(&png)?;
        let (x, y) = (self.dots.0 + 60, self.dots.1);
        let top = panel_top(&shot, x, y + 6..(y + 60).min(shot.height()));
        let menu_y = top.map_or(y + 26, |top| top + 18);
        step(&format!(
            "{entry}: the row menu's entry at ({x}, {menu_y}), its top {top:?}"
        ));
        self.run.click(x, menu_y)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        expect(state["row_menu"].is_null(), || {
            format!("the row menu of {chat} stayed open after {entry}")
        });
        Ok(())
    }

    // --- the walk ---

    /// b's message in the 1:1 chat: "Mark as unread" from its menu. The divider moves before it, the list counts the
    /// chat unread from it on, the bookmark names it, and no read receipt goes while the chat is shown.
    fn one_to_one_menu(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.m4)?;
        self.expect_count(AB_CHAT, "the 1:1 chat read once shown", |count| count == 0)?;
        thread::sleep(SETTLE);
        self.run.shot(self.sandbox, "qa660-1to1-read-light.png")?;
        let (marks, receipts) = (self.bookmark_count(AB_CHAT)?, self.receipt_count(AB_CHAT)?);
        self.mark_by_menu("chat", &ids.m2, false, Some("message-menu"))?;
        self.expect_divider("chat", &ids.m2, "Mark as unread on b's message");
        // m2, m3 and m4 are b's; a's own message between them does not count
        let count = self.expect_count(AB_CHAT, "the 1:1 chat counted unread from the mark", |count| count == 3)?;
        step(&format!("the 1:1 chat's count after the mark: {count}"));
        let new = self.expect_bookmark(AB_CHAT, marks, "Mark as unread on b's message");
        if let Some(value) = new.first() {
            Self::expect_mark_value(value, &ids.m2, "Mark as unread on b's message");
        }
        self.expect_no_receipt(AB_CHAT, receipts, "after Mark as unread")?;
        self.shots("marked")?;
        let state = self.run.state("chat")?;
        expect(state["options"].is_null(), || {
            "the menu stayed open after Mark as unread".to_owned()
        });
        step("1:1 chat: Mark as unread from b's message's menu");
        Ok(())
    }

    /// b writes once more while the marked chat shows: the count grows from the mark; a's own message sent from the
    /// box keeps the chat unread; still no read receipt.
    fn new_messages_after_the_mark(&mut self) -> SandboxResult {
        let receipts = self.receipt_count(AB_CHAT)?;
        let m5 = self
            .service
            .post(LAB_B, AB_CHAT, "One more made-up line after the mark.")?;
        self.run
            .until("chat", "b's new message shown", |state| item(state, &m5).is_some())?;
        self.expect_count(AB_CHAT, "the count grows from the mark", |count| count == 4)?;
        // a writes in the marked chat: typed in the box, sent with Enter
        let owns = |state: &Value| {
            state["items"]
                .as_array()
                .map_or(0, |items| items.iter().filter(|row| row["own"] == true).count())
        };
        let before = owns(&self.run.state("chat")?);
        self.run.click(COMPOSE.0, COMPOSE.1)?;
        self.run.input.type_text("A made-up line a sends in the marked chat")?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.key(keys::ENTER)?;
        self.run
            .until("chat", "a's own new message shown", |state| owns(state) > before)?;
        thread::sleep(QUIET);
        let count = self.count(AB_CHAT)?;
        expect(count >= 1, || {
            "a's own new message made the marked chat read".to_owned()
        });
        expect(count == 4, || {
            format!("a's own new message changed the marked chat's count from 4 to {count}")
        });
        let ids = self.ids().m2.clone();
        self.expect_divider("chat", &ids, "new messages after the mark");
        self.expect_no_receipt(AB_CHAT, receipts, "new messages after the mark")?;
        self.run.shot(self.sandbox, "qa660-after-new-light.png")?;
        step("new messages after the mark");
        Ok(())
    }

    /// The window gives the keyboard to another window and gets it back: the mark stays.
    fn focus_elsewhere_keeps_it(&mut self) -> SandboxResult {
        let (marks, receipts) = (self.bookmark_count(AB_CHAT)?, self.receipt_count(AB_CHAT)?);
        let before = self.count(AB_CHAT)?;
        self.run.question("blank-open")?;
        thread::sleep(Duration::from_secs(2));
        self.run.shot(self.sandbox, "qa660-focus-away-light.png")?;
        self.run.question("blank-close")?;
        thread::sleep(Duration::from_secs(2));
        self.run.click(self.run.rest.0, self.run.rest.1)?;
        thread::sleep(QUIET);
        let state = self.run.state("chat")?;
        expect(state["conversation"] == AB_CHAT, || {
            format!("the chat is not shown after the focus came back: {}", state["main"])
        });
        expect(self.bookmark_count(AB_CHAT)? == marks, || {
            "a focus change of the window ended the mark".to_owned()
        });
        expect(self.count(AB_CHAT)? == before, || {
            "a focus change of the window changed the marked chat's count".to_owned()
        });
        expect(self.receipt_count(AB_CHAT)? == receipts, || {
            "a focus change of the window sent a read receipt on the marked chat".to_owned()
        });
        step("the focus away and back keeps the mark");
        Ok(())
    }

    /// The calendar (Ctrl+4), then the chats again (Ctrl+2): the chat is shown again, so the mark ends.
    fn shown_again_ends_it(&mut self) -> SandboxResult {
        let (marks, receipts) = (self.bookmark_count(AB_CHAT)?, self.receipt_count(AB_CHAT)?);
        self.run.input.key_with(&[Modifier::Ctrl], KEY_4)?;
        self.run
            .until("chat", "the calendar shown", |state| state["main"] == "Calendar")?;
        thread::sleep(SETTLE);
        self.run.input.key_with(&[Modifier::Ctrl], KEY_2)?;
        self.run
            .until("chat", "the chat shown again", |state| state["conversation"] == AB_CHAT)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "the chat shown again");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the chat shown again");
        }
        self.expect_count(AB_CHAT, "the chat read once shown again", |count| count == 0)?;
        expect(self.wait_receipt(AB_CHAT, receipts), || {
            "no read receipt after the marked chat was shown again".to_owned()
        });
        self.run.shot(self.sandbox, "qa660-shown-again-light.png")?;
        step("the calendar and back: the mark ended");
        Ok(())
    }

    /// a's own message: Mark as unread from its menu; the divider before it, the count from b's messages after it.
    fn own_message(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let marks = self.bookmark_count(AB_CHAT)?;
        self.mark_by_menu("chat", &ids.own, true, Some("own-menu"))?;
        self.expect_divider("chat", &ids.own, "Mark as unread on a's own message");
        // b's m4 and m5 after it
        self.expect_count(AB_CHAT, "the chat counted from a's own message", |count| count == 2)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "Mark as unread on a's own message");
        if let Some(value) = new.first() {
            Self::expect_mark_value(value, &ids.own, "Mark as unread on a's own message");
        }
        self.shots("own-marked")?;
        step("a's own message marked unread");
        Ok(())
    }

    /// The group chat: b's first message selected with the keys, then U.
    fn group_by_key(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.expect_count(ABC_CHAT, "the group chat read once shown", |count| count == 0)?;
        // leaving the 1:1 chat keeps its mark: it ends when the chat is opened again
        let count = self.count(AB_CHAT)?;
        expect(count >= 1, || "leaving the marked 1:1 chat made it read".to_owned());
        let (marks, receipts) = (self.bookmark_count(ABC_CHAT)?, self.receipt_count(ABC_CHAT)?);
        self.select_by_keys(&ids.g1)?;
        let key = self.run.letter('u')?;
        self.run.input.key(key)?;
        thread::sleep(SETTLE);
        self.expect_divider("chat", &ids.g1, "U on b's group message");
        self.expect_count(ABC_CHAT, "the group chat counted from the mark", |count| count == 2)?;
        let new = self.expect_bookmark(ABC_CHAT, marks, "U on b's group message");
        if let Some(value) = new.first() {
            Self::expect_mark_value(value, &ids.g1, "U on b's group message");
        }
        self.expect_no_receipt(ABC_CHAT, receipts, "after U")?;
        self.shots("group-key-u")?;
        step("group chat: U on a message selected with the keys");
        Ok(())
    }

    /// The marked 1:1 chat opened again: the mark ends.
    fn opened_again_ends_it(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        let marks = self.bookmark_count(AB_CHAT)?;
        self.open_chat(AB_CHAT, &ids.m4)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "the marked chat opened again");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the marked chat opened again");
        }
        self.expect_count(AB_CHAT, "the chat read once opened again", |count| count == 0)?;
        // the divider stands where the mark put it while the chat stays open
        self.expect_divider("chat", &ids.own, "the marked chat opened again");
        self.run.shot(self.sandbox, "qa660-opened-again-light.png")?;
        step("the marked chat opened again: the mark ended");
        Ok(())
    }

    /// The meeting chat: b's message marked from its menu.
    fn meeting_chat(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(MEETING, &ids.meet2)?;
        self.expect_count(MEETING, "the meeting chat read once shown", |count| count == 0)?;
        let marks = self.bookmark_count(MEETING)?;
        self.mark_by_menu("chat", &ids.meet1, false, None)?;
        self.expect_divider("chat", &ids.meet1, "Mark as unread in the meeting chat");
        self.expect_count(MEETING, "the meeting chat counted from the mark", |count| count == 2)?;
        let new = self.expect_bookmark(MEETING, marks, "Mark as unread in the meeting chat");
        if let Some(value) = new.first() {
            Self::expect_mark_value(value, &ids.meet1, "Mark as unread in the meeting chat");
        }
        self.shots("meeting-marked")?;
        step("meeting chat marked unread");
        Ok(())
    }

    /// The channel's post with replies: Mark as unread from its menu names the thread's newest reply; the channel's
    /// row is unread.
    fn channel_post(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.run.question("open-channel")?;
        self.run.until("posts", "the channel's posts", |state| {
            item(state, CHANNEL_POST).is_some()
        })?;
        thread::sleep(SETTLE);
        let marks = self.bookmark_count(TEAM_CHANNEL)?;
        let before = self.run.state("chat")?["channel_unread"].clone();
        step(&format!("the channel's unread before the mark: {before}"));
        self.mark_by_menu("posts", CHANNEL_POST, false, Some("channel-menu"))?;
        let new = self.expect_bookmark(TEAM_CHANNEL, marks, "Mark as unread on a channel post");
        if let Some(value) = new.first() {
            Self::expect_mark_value(
                value,
                &ids.reply,
                "Mark as unread on a channel post (the thread's newest reply)",
            );
        }
        let state = self.run.state("posts")?;
        expect(state["divider_before"].is_null(), || {
            format!("a channel's posts show a divider: {}", state["divider_before"])
        });
        // the channel shows unread already (a channel's posts send no read receipt): its row is only logged
        let after = self.run.state("chat")?["channel_unread"].clone();
        step(&format!("the channel's unread after the mark: {after}"));
        self.shots("channel-marked")?;
        step("channel post marked unread");
        Ok(())
    }

    /// The 1:1 chat's row "..." (it is read now): "Mark as unread", then "..." again: "Mark as read".
    fn row_menu_by_pointer(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        // the group chat is still marked from the walk: opened again, it ends
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.expect_count(ABC_CHAT, "the group chat read once opened again", |count| count == 0)?;
        self.expect_count(AB_CHAT, "the 1:1 chat read", |count| count == 0)?;
        let marks = self.bookmark_count(AB_CHAT)?;
        self.open_row_menu(AB_CHAT, Some("row-hover-light"))?;
        let state = self.run.state("chat")?;
        expect(state["row_menu_entry"] == "Mark as unread", || {
            format!("the row menu of a read chat offers {}", state["row_menu_entry"])
        });
        self.shots("row-menu-unread")?;
        self.choose_row_entry(AB_CHAT, "Mark as unread")?;
        self.expect_count(AB_CHAT, "the row's Mark as unread", |count| count >= 1)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "the row's Mark as unread");
        step(&format!("the row's Mark as unread put {new:?}"));
        self.shots("row-marked-unread")?;
        let (marks, receipts) = (self.bookmark_count(AB_CHAT)?, self.receipt_count(AB_CHAT)?);
        self.open_row_menu(AB_CHAT, None)?;
        let state = self.run.state("chat")?;
        expect(state["row_menu_entry"] == "Mark as read", || {
            format!("the row menu of a marked chat offers {}", state["row_menu_entry"])
        });
        self.shots("row-menu-read")?;
        self.choose_row_entry(AB_CHAT, "Mark as read")?;
        self.expect_count(AB_CHAT, "the row's Mark as read", |count| count == 0)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "the row's Mark as read");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the row's Mark as read");
        }
        thread::sleep(SETTLE);
        step(&format!(
            "the row's Mark as read: {} new read receipt(s)",
            self.receipt_count(AB_CHAT)? - receipts
        ));
        self.run.shot(self.sandbox, "qa660-row-marked-read-light.png")?;
        step("the row menu by the pointer");
        Ok(())
    }

    /// b writes in the group chat while another chat shows: the row's "..." offers "Mark as read", which reads it.
    fn row_menu_mark_read_of_others(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.m4)?;
        let g3 = self
            .service
            .post(LAB_B, ABC_CHAT, "A made-up group line nobody read yet.")?;
        self.expect_count(ABC_CHAT, "b's new group message unread", |count| count == 1)?;
        let (marks, receipts) = (self.bookmark_count(ABC_CHAT)?, self.receipt_count(ABC_CHAT)?);
        self.open_row_menu(ABC_CHAT, None)?;
        let state = self.run.state("chat")?;
        expect(state["row_menu_entry"] == "Mark as read", || {
            format!("the row menu of an unread chat offers {}", state["row_menu_entry"])
        });
        self.choose_row_entry(ABC_CHAT, "Mark as read")?;
        self.expect_count(ABC_CHAT, "Mark as read on b's unread message", |count| count == 0)?;
        let receipt = wait_until(WAIT, || {
            let all = self
                .service
                .puts(self.runtime, ABC_CHAT, "consumptionhorizon", LAB_A)
                .ok()?;
            all.get(receipts).cloned()
        });
        match receipt {
            Some(value) => expect(value.starts_with(&format!("{g3};")), || {
                format!("Mark as read sent the horizon {value:?}, not at b's newest message {g3}")
            }),
            None => note("Mark as read on an unread chat sent no read receipt".to_owned()),
        }
        thread::sleep(SETTLE);
        let bookmarks = self.bookmark_count(ABC_CHAT)? - marks;
        step(&format!("Mark as read of b's message: {bookmarks} bookmark put(s)"));
        step("the row menu's Mark as read on b's unread message");
        Ok(())
    }

    /// The keys: Ctrl+2 takes the list; Shift+F10 opens the menu of the row with the ring; Down and the menu key open
    /// the next row's; Down and Enter choose its entry.
    fn row_menu_by_keys(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.run.input.key_with(&[Modifier::Ctrl], KEY_2)?;
        thread::sleep(SETTLE);
        self.run.input.key_with(&[Modifier::Shift], KEY_F10)?;
        thread::sleep(SETTLE);
        // the list keeps the row its ring was on before, not always the open chat's
        let state = self.run.state("chat")?;
        let first = state["row_menu"].clone();
        expect(!first.is_null(), || {
            "Shift+F10 on the list opened no row menu".to_owned()
        });
        step(&format!("Shift+F10 on the list opened the menu of {first}"));
        self.run.shot(self.sandbox, "qa660-row-menu-shift-f10-light.png")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        expect(state["row_menu"].is_null(), || {
            "Escape left the row menu open".to_owned()
        });
        // the next row down, by the menu key
        self.run.input.key(keys::DOWN)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.key(KEY_MENU)?;
        thread::sleep(SETTLE);
        let state = self.run.state("chat")?;
        let shown = state["shown"].as_array().cloned().unwrap_or_default();
        let at = shown.iter().position(|row| *row == first);
        let next = at.and_then(|at| shown.get(at + 1)).cloned().unwrap_or(Value::Null);
        expect(!state["row_menu"].is_null() && state["row_menu"] == next, || {
            format!(
                "Down, then the menu key opened the menu of {} (the next row is {next})",
                state["row_menu"]
            )
        });
        self.run.shot(self.sandbox, "qa660-row-menu-key-light.png")?;
        let Some(chat) = state["row_menu"].as_str().map(str::to_owned) else {
            return Ok(());
        };
        let before = self.count(&chat)?;
        let read = state["row_menu_entry"] == "Mark as read";
        self.run.input.key(keys::DOWN)?;
        thread::sleep(Duration::from_millis(300));
        self.run.input.key(keys::ENTER)?;
        thread::sleep(SETTLE);
        let after = self.until_count(&chat, "the row menu's entry by Enter", |count| {
            if read { count == 0 } else { count >= 1 }
        });
        if let Err(error) = after {
            note(format!(
                "Down and Enter in the row menu of {chat} (count {before}): {error}"
            ));
        }
        // the chat ends read again for the next steps
        if !read {
            self.open_row_menu(&chat, None)?;
            self.choose_row_entry(&chat, "Mark as read")?;
            self.expect_count(&chat, "the row's Mark as read", |count| count == 0)?;
        }
        step(&format!("the row menu by the keys on {chat}"));
        Ok(())
    }

    /// a's other client marks the 1:1 chat unread over the push channel while another chat shows: the list counts it;
    /// opened, the divider stands before the message and the mark ends.
    fn pushed_from_another_client(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.expect_count(AB_CHAT, "the 1:1 chat read before the push", |count| count == 0)?;
        let before = ids.m3.parse::<i64>()? - 1;
        let resource = json!({
            "id": AB_CHAT,
            "properties": {"consumptionHorizonBookmark": format!("{before};{};1", now_ms())},
            "propertiesUpdated": ["consumptionHorizonBookmark"],
        });
        self.service.trouter.deliver_chat(
            LAB_A,
            &json!({"type": "EventMessage", "resourceType": "ConversationUpdate", "resource": resource}),
        );
        // m3, m4, m5 are b's after it
        self.expect_count(AB_CHAT, "the mark pushed from another client", |count| count == 3)?;
        self.run.shot(self.sandbox, "qa660-pushed-light.png")?;
        let marks = self.bookmark_count(AB_CHAT)?;
        self.open_chat(AB_CHAT, &ids.m4)?;
        self.expect_divider("chat", &ids.m3, "the pushed mark opened: the divider");
        let new = self.expect_bookmark(AB_CHAT, marks, "the pushed mark opened: its end");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the pushed mark opened: its end");
        }
        self.expect_count(AB_CHAT, "the pushed mark opened: the count", |count| count == 0)?;
        self.run.shot(self.sandbox, "qa660-pushed-opened-light.png")?;
        step("a mark pushed from another client");
        Ok(())
    }

    /// Read receipts off: a mark from the row menu, the chat opened: the mark still ends, no read receipt goes.
    fn receipts_off(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.run.question("receipts off")?;
        self.open_row_menu(AB_CHAT, None)?;
        self.choose_row_entry(AB_CHAT, "Mark as unread")?;
        self.expect_count(AB_CHAT, "the row's Mark as unread, receipts off", |count| count >= 1)?;
        let (marks, receipts) = (self.bookmark_count(AB_CHAT)?, self.receipt_count(AB_CHAT)?);
        self.open_chat(AB_CHAT, &ids.m4)?;
        let new = self.expect_bookmark(AB_CHAT, marks, "the marked chat opened, receipts off");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the marked chat opened, receipts off");
        }
        thread::sleep(QUIET);
        expect(self.receipt_count(AB_CHAT)? == receipts, || {
            "a read receipt went with read receipts off".to_owned()
        });
        let count = self.count(AB_CHAT)?;
        step(&format!("receipts off: the opened chat counts {count}"));
        self.run.shot(self.sandbox, "qa660-receipts-off-light.png")?;
        self.run.question("receipts on")?;
        step("read receipts off: opening still ends the mark");
        Ok(())
    }

    /// The menu, the divider and the row menu with a larger font, then in a narrow window.
    fn looks(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(AB_CHAT, &ids.m4)?;
        for _ in 0..3 {
            self.run.input.key_with(&[Modifier::Ctrl], KEY_EQUAL)?;
            thread::sleep(Duration::from_millis(300));
        }
        thread::sleep(SETTLE);
        let size = self.run.question("font")?;
        step(&format!("the font size after Ctrl+= three times: {size}"));
        expect(size != "14", || "Ctrl+= did not change the font size".to_owned());
        self.open_menu("chat", &ids.m3)?;
        self.shots("large-font-menu")?;
        self.close_menu("chat")?;
        let marks = self.bookmark_count(AB_CHAT)?;
        self.select_by_keys(&ids.m3)?;
        let key = self.run.letter('u')?;
        self.run.input.key(key)?;
        self.expect_divider("chat", &ids.m3, "U with a larger font");
        self.until_bookmark(AB_CHAT, marks, "U with a larger font")?;
        self.shots("large-font-divider")?;
        self.open_row_menu(AB_CHAT, None)?;
        self.shots("large-font-row-menu")?;
        self.run.input.key(keys::ESC)?;
        thread::sleep(SETTLE);
        self.run.input.key_with(&[Modifier::Ctrl], KEY_0)?;
        thread::sleep(SETTLE);
        // a narrow window
        self.sandbox.niri_msg(&["action", "set-column-width", "720"])?;
        thread::sleep(Duration::from_secs(2));
        self.open_menu("chat", &ids.m2)?;
        self.shots("narrow-menu")?;
        self.close_menu("chat")?;
        self.shots("narrow-divider")?;
        self.sandbox.niri_msg(&["action", "set-column-width", "100%"])?;
        thread::sleep(Duration::from_secs(2));
        // the mark made here ends when the chat shows again
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.open_chat(AB_CHAT, &ids.m4)?;
        self.expect_count(AB_CHAT, "the chat read after the looks", |count| count == 0)?;
        step("the looks: a larger font, a narrow window");
        Ok(())
    }

    /// The network goes down; U on b's group message; the network comes back: the mark reaches the service.
    fn offline(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        let marks = self.bookmark_count(ABC_CHAT)?;
        self.service.relay.down();
        step("the network is down");
        self.select_by_keys(&ids.g2)?;
        let key = self.run.letter('u')?;
        self.run.input.key(key)?;
        thread::sleep(SETTLE);
        self.expect_divider("chat", &ids.g2, "U with the network down");
        let count = self.count(ABC_CHAT)?;
        expect(count >= 1, || {
            "U with the network down: the list does not count the chat unread".to_owned()
        });
        thread::sleep(Duration::from_secs(4));
        self.shots("offline-marked")?;
        self.service.relay.up();
        step("the network is back");
        let new = self.bookmarks(ABC_CHAT, LAB_A, marks)?;
        if new.is_empty() {
            let arrived = wait_until(Duration::from_secs(90), || {
                self.bookmarks(ABC_CHAT, LAB_A, marks)
                    .ok()
                    .filter(|new| !new.is_empty())
            });
            match arrived {
                Some(new) => Self::expect_mark_value(&new[0], &ids.g2, "the mark made offline"),
                None => note(
                    "a mark made while the network was down never reached the service after the network came back \
                     (90 s)"
                        .to_owned(),
                ),
            }
        }
        self.run.shot(self.sandbox, "qa660-offline-back-light.png")?;
        let state = self.run.state("chat")?;
        step(&format!(
            "after the network came back: count {}",
            state["counts"][ABC_CHAT]
        ));
        Ok(())
    }

    /// The 1:1 chat marked from its row before the app quits, for the restart.
    fn kept_for_the_restart(&mut self) -> SandboxResult {
        let ids = self.ids().clone();
        self.open_chat(ABC_CHAT, &ids.g2)?;
        self.open_row_menu(AB_CHAT, None)?;
        self.choose_row_entry(AB_CHAT, "Mark as unread")?;
        self.until_count(AB_CHAT, "the 1:1 chat marked before the restart", |count| count >= 1)?;
        let marks = self.bookmark_count(AB_CHAT)?;
        step(&format!(
            "the 1:1 chat marked before the restart ({marks} bookmark puts)"
        ));
        Ok(())
    }

    /// After the restart: the 1:1 chat's mark is kept; the meeting chat's mark from a's other client came with the
    /// conversation list.
    fn after_restart(&mut self) -> SandboxResult {
        self.run.until("chat", "the list after the restart", |state| {
            state["shown"].as_array().is_some_and(|rows| !rows.is_empty())
        })?;
        thread::sleep(Duration::from_secs(3));
        let state = self.run.state("chat")?;
        step(&format!(
            "after the restart: the chat {}, counts {}",
            state["conversation"], state["counts"]
        ));
        // the app opens the chat shown last (the group chat); the marked chats are not it
        expect(
            state["conversation"] != AB_CHAT && state["conversation"] != MEETING,
            || format!("the app opened {} at the start", state["conversation"]),
        );
        self.expect_count(AB_CHAT, "the 1:1 chat's mark kept over the restart", |count| count >= 1)?;
        self.expect_count(MEETING, "the meeting chat's mark from a's other client", |count| {
            count == 1
        })?;
        self.shots("after-restart")?;
        let ids = self.ids().clone();
        let marks = self.bookmark_count(MEETING)?;
        self.open_chat(MEETING, &ids.meet2)?;
        self.expect_divider("chat", &ids.meet2, "the meeting chat's listed mark opened");
        let new = self.expect_bookmark(MEETING, marks, "the meeting chat's listed mark opened");
        if let Some(value) = new.first() {
            Self::expect_end_value(value, "the meeting chat's listed mark opened");
        }
        self.expect_count(MEETING, "the meeting chat read once opened", |count| count == 0)?;
        self.run.shot(self.sandbox, "qa660-after-restart-opened-light.png")?;
        step("restart: the marks are kept and listed");
        Ok(())
    }

    /// a and b in one window: a marks the 1:1 chat from its row; b's row, b's writes and b's own properties stay.
    fn two_accounts(&mut self) -> SandboxResult {
        self.run.until("chat", "both accounts' lists", |state| {
            state["accounts"]
                .as_array()
                .is_some_and(|accounts| accounts.len() == 2 && accounts[1]["shown"].as_u64().unwrap_or(0) > 0)
        })?;
        thread::sleep(Duration::from_secs(2));
        let ids = self.ids().clone();
        // the 1:1 chat's mark from before the restart: opened, it ends
        self.open_chat(AB_CHAT, &ids.m4)?;
        self.open_chat(ABC_CHAT, &ids.g2)?;
        let state = self.run.state("chat")?;
        let b_before = state["accounts"][1]["counts"][AB_CHAT].clone();
        let b_marks = self.bookmarks(AB_CHAT, LAB_B, 0)?.len();
        let marks = self.bookmark_count(AB_CHAT)?;
        self.open_row_menu(AB_CHAT, None)?;
        self.choose_row_entry(AB_CHAT, "Mark as unread")?;
        self.expect_count(AB_CHAT, "a's mark with two accounts", |count| count >= 1)?;
        self.expect_bookmark(AB_CHAT, marks, "a's mark with two accounts");
        thread::sleep(QUIET);
        let state = self.run.state("chat")?;
        expect(state["accounts"][1]["counts"][AB_CHAT] == b_before, || {
            format!(
                "a's mark changed b's count of the 1:1 chat from {b_before} to {}",
                state["accounts"][1]["counts"][AB_CHAT]
            )
        });
        expect(self.bookmarks(AB_CHAT, LAB_B, 0)?.len() == b_marks, || {
            "a's mark sent a bookmark as b".to_owned()
        });
        let own = self.service.own_properties(self.runtime, LAB_B, AB_CHAT)?;
        expect(own.get("consumptionHorizonBookmark").is_none(), || {
            format!("b's own properties of the 1:1 chat hold a bookmark after a's mark: {own}")
        });
        self.run.shot(self.sandbox, "qa660-two-a-light.png")?;
        // b's account shown: Ctrl+Alt+Down
        self.run.input.key_with(&[Modifier::Ctrl, Modifier::Alt], keys::DOWN)?;
        thread::sleep(Duration::from_secs(2));
        let state = self.run.state("chat")?;
        expect(state["account"] != state["accounts"][0]["account"], || {
            "Ctrl+Alt+Down did not show b's account".to_owned()
        });
        expect(state["counts"][AB_CHAT] == b_before, || {
            format!(
                "b's list shows the 1:1 chat with {} (before {b_before})",
                state["counts"][AB_CHAT]
            )
        });
        self.shots("two-b")?;
        self.run.input.key_with(&[Modifier::Ctrl, Modifier::Alt], keys::UP)?;
        thread::sleep(SETTLE);
        step("two accounts: a's mark leaves b alone");
        Ok(())
    }
}

fn item<'v>(state: &'v Value, id: &str) -> Option<&'v Value> {
    state["items"].as_array()?.iter().find(|row| row["id"] == id)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a place on the screen: small, and clamped at 0"
)]
fn rect_of(value: &Value) -> Option<Rect> {
    let list = value.as_array()?;
    let number = |at: usize| list.get(at).and_then(Value::as_f64).map(|n| n.max(0.0).round() as u32);
    let (x, y, w, h) = (number(0)?, number(1)?, number(2)?, number(3)?);
    Some(Rect {
        x0: x,
        y0: y,
        x1: x + w,
        y1: y + h,
    })
}

fn load(shot: &Path) -> SandboxResult<image::RgbImage> {
    Ok(image::open(shot)?.to_rgb8())
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn at(shot: &image::RgbImage, x: u32, y: u32) -> [u8; 3] {
    if x < shot.width() && y < shot.height() {
        shot.get_pixel(x, y).0
    } else {
        [0, 0, 0]
    }
}

/// The first `y` in `rows` of column `x` that is a panel's top border: [`BORDER`] over two rows of [`SURFACE`].
fn border_top(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter()
        .find(|&y| near(at(shot, x, y), BORDER, 4) && at(shot, x, y + 1) == SURFACE && at(shot, x, y + 2) == SURFACE)
}

/// The first `y` in `rows` of column `x` that is a panel's outer top edge: [`border_top`], not under [`SURFACE`].
fn panel_top(shot: &image::RgbImage, x: u32, rows: std::ops::Range<u32>) -> Option<u32> {
    rows.into_iter()
        .find(|&y| y > 0 && at(shot, x, y - 1) != SURFACE && border_top(shot, x, y..y + 1).is_some())
}

/// The bounds of the pixels in `region` that `taken` takes.
fn pixels_box(shot: &image::RgbImage, region: Rect, taken: impl Fn([u8; 3]) -> bool) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(shot.height()) {
        for x in region.x0..region.x1.min(shot.width()) {
            if taken(at(shot, x, y)) {
                bounds = Some(grow(bounds, x, y));
            }
        }
    }
    bounds
}

fn grow(bounds: Option<Rect>, x: u32, y: u32) -> Rect {
    bounds.map_or(
        Rect {
            x0: x,
            y0: y,
            x1: x + 1,
            y1: y + 1,
        },
        |known| Rect {
            x0: known.x0.min(x),
            y0: known.y0.min(y),
            x1: known.x1.max(x + 1),
            y1: known.y1.max(y + 1),
        },
    )
}

/// The bounds of the pixels that changed in `region`.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for y in region.y0..region.y1.min(before.height()) {
        for x in region.x0..region.x1.min(before.width()) {
            if !near(at(before, x, y), at(after, x, y), 8) {
                bounds = Some(grow(bounds, x, y));
            }
        }
    }
    bounds
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Copies the shot `png` to `target/ui-shots/<name>`, and to the evidence directory when one is named.
fn keep(png: &Path, name: &str) -> SandboxResult {
    fs::copy(png, shots_dir()?.join(name))?;
    if let Some(evidence) = std::env::var_os(EVIDENCE_ENV) {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence)?;
        fs::copy(png, evidence.join(name))?;
    }
    Ok(())
}

/// A path as the chat service routes it, its `%3A` and `%40` read back.
fn decoded(path: &str) -> String {
    path.replace("%3A", ":").replace("%3a", ":").replace("%40", "@")
}

// --- the service ---

/// The made-up messages' ids.
#[derive(Clone, Debug, Default)]
struct Ids {
    m2: String,
    m3: String,
    own: String,
    m4: String,
    g1: String,
    g2: String,
    meet1: String,
    meet2: String,
    reply: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    relay: Relay,
    ids: Ids,
}

impl Service {
    /// The mock with the made-up messages: b's and a's in the a–b chat, b's in the a–b–c chat and in a meeting chat,
    /// b's reply under the team channel's post.
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        if !mock.add_meeting_chat(MEETING, "Made-up planning review", &[LAB_B, LAB_A]) {
            return Err("no meeting chat on the mock".into());
        }
        let post = |from: &str, chat: &str, text: &str| -> SandboxResult<String> {
            mock.post_as(from, chat, &format!("<p>{text}</p>"))
                .ok_or_else(|| format!("the post {text:?} refused").into())
        };
        post(LAB_B, AB_CHAT, "Made-up line one: the plan is ready.")?;
        let ids = Ids {
            m2: post(LAB_B, AB_CHAT, "Made-up line two: please read it today.")?,
            m3: post(LAB_B, AB_CHAT, "Made-up line three: the numbers are in the sheet.")?,
            own: post(LAB_A, AB_CHAT, "Made-up own line: I will look at it.")?,
            m4: post(LAB_B, AB_CHAT, "Made-up line four: thanks!")?,
            g1: post(LAB_B, ABC_CHAT, "Made-up group line one: the review is at two.")?,
            g2: post(LAB_B, ABC_CHAT, "Made-up group line two: bring the notes.")?,
            meet1: post(LAB_B, MEETING, "Made-up meeting line one: the agenda is posted.")?,
            meet2: post(LAB_B, MEETING, "Made-up meeting line two: see you there.")?,
            reply: mock
                .add_message(
                    &format!("{TEAM_CHANNEL};messageid={CHANNEL_POST}"),
                    (LAB_B, "Lab B"),
                    "RichText/Html",
                    "<p>Made-up reply under the post.</p>",
                )
                .ok_or("b's reply refused")?,
        };
        let _ = mock.take_events(LAB_A);
        let _ = mock.take_events(LAB_B);
        let relay = Relay::start(&mock.origin())?;
        Ok(Self {
            mock,
            trouter,
            relay,
            ids,
        })
    }

    fn env(&self, mode: &str) -> Vec<(&'static str, String)> {
        vec![
            (ORIGIN_ENV, self.relay.origin.clone()),
            (TROUTER_ENV, self.trouter.origin().to_owned()),
            (TOKEN_A_ENV, self.mock.token(LAB_A).unwrap_or_default()),
            (TOKEN_B_ENV, self.mock.token(LAB_B).unwrap_or_default()),
            (MODE_ENV, mode.to_owned()),
        ]
    }

    /// `from` posts `text`, as one of its clients does; its id.
    fn post(&self, from: &str, chat: &str, text: &str) -> SandboxResult<String> {
        self.mock
            .post_as(from, chat, &format!("<p>{text}</p>"))
            .ok_or_else(|| format!("the post {text:?} refused").into())
    }

    /// The values of the `name` property puts on `chat` the mock got from `mri`, oldest first.
    fn puts(&self, runtime: &tokio::runtime::Runtime, chat: &str, name: &str, mri: &str) -> SandboxResult<Vec<String>> {
        let token = self.mock.token(mri).ok_or("no token")?;
        let bearer = format!("Bearer {token}");
        let path = format!("/api/chatsvc/amer/v1/users/ME/conversations/{chat}/properties");
        let requests = runtime.block_on(self.mock.received_requests());
        Ok(requests
            .iter()
            .filter(|request| {
                request.method.as_str() == "PUT"
                    && decoded(request.url.path()) == path
                    && request
                        .url
                        .query_pairs()
                        .any(|(key, value)| key == "name" && value == name)
                    && request
                        .headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        == Some(bearer.as_str())
            })
            .filter_map(|request| {
                let body: Value = serde_json::from_slice(&request.body).ok()?;
                body[name].as_str().map(str::to_owned)
            })
            .collect())
    }

    /// `mri`'s other client puts the bookmark `value` on `chat`, as Teams on another device does.
    fn bookmark_as(&self, runtime: &tokio::runtime::Runtime, mri: &str, chat: &str, value: &str) -> SandboxResult {
        let url = format!(
            "{}/api/chatsvc/amer/v1/users/ME/conversations/{chat}/properties?name=consumptionHorizonBookmark",
            self.mock.origin()
        );
        let token = self.mock.token(mri).ok_or("no token")?;
        let status = runtime.block_on(async {
            reqwest::Client::new()
                .put(url)
                .bearer_auth(token)
                .header("X-MS-Migration", "True")
                .header("BehaviorOverride", "redirectAs404")
                .header("content-type", "application/json")
                .body(json!({"consumptionHorizonBookmark": value}).to_string())
                .send()
                .await
                .map(|answer| answer.status().as_u16())
        })?;
        if !(200..300).contains(&status) {
            return Err(format!("the other client's bookmark answered {status}").into());
        }
        Ok(())
    }

    /// `mri`'s own properties of `chat` as the service gives them back.
    fn own_properties(&self, runtime: &tokio::runtime::Runtime, mri: &str, chat: &str) -> SandboxResult<Value> {
        let url = format!(
            "{}/api/chatsvc/amer/v1/users/ME/conversations/{chat}",
            self.mock.origin()
        );
        let token = self.mock.token(mri).ok_or("no token")?;
        let body = runtime.block_on(async {
            let answer = reqwest::Client::new()
                .get(url)
                .bearer_auth(token)
                .header("X-MS-Migration", "True")
                .header("BehaviorOverride", "redirectAs404")
                .send()
                .await?;
            answer.text().await
        })?;
        let read: Value = serde_json::from_str(&body)?;
        Ok(read["properties"].clone())
    }
}

/// A TCP relay to the mock: the child's way to the chat service, which the parent cuts as a network that goes down
/// (each new connection closed at once, and those open closed).
struct Relay {
    origin: String,
    up: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
}

impl Relay {
    fn start(target: &str) -> SandboxResult<Self> {
        let address = target
            .strip_prefix("http://")
            .ok_or("the mock's origin is not http")?
            .trim_end_matches('/')
            .to_owned();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let origin = format!("http://{}", listener.local_addr()?);
        let up = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(0));
        let (up_in, generation_in) = (Arc::clone(&up), Arc::clone(&generation));
        thread::spawn(move || {
            for client in listener.incoming() {
                let Ok(client) = client else { continue };
                if !up_in.load(Ordering::SeqCst) {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                }
                let Ok(server) = TcpStream::connect(&address) else {
                    continue;
                };
                let born = generation_in.load(Ordering::SeqCst);
                if let (Ok(client_in), Ok(server_in)) = (client.try_clone(), server.try_clone()) {
                    pipe(client_in, server, Arc::clone(&generation_in), born);
                    pipe(server_in, client, Arc::clone(&generation_in), born);
                }
            }
        });
        Ok(Self { origin, up, generation })
    }

    fn down(&self) {
        self.up.store(false, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn up(&self) {
        self.up.store(true, Ordering::SeqCst);
    }
}

/// Copies `from` to `to` until either ends or the relay is cut (`generation` past `born`).
fn pipe(mut from: TcpStream, mut to: TcpStream, generation: Arc<AtomicU64>, born: u64) {
    thread::spawn(move || {
        let _ = from.set_read_timeout(Some(Duration::from_millis(100)));
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            if generation.load(Ordering::SeqCst) != born {
                let _ = from.shutdown(Shutdown::Both);
                let _ = to.shutdown(Shutdown::Both);
                return;
            }
            match from.read(&mut buffer) {
                Ok(0) => {
                    let _ = to.shutdown(Shutdown::Write);
                    return;
                }
                Ok(read) => {
                    if to.write_all(&buffer[..read]).is_err() {
                        return;
                    }
                }
                Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    });
}

// --- the app in the sandbox ---

/// The app running in the sandbox: its process, its directory and the user's pointer and keys.
struct AppRun {
    running: Running,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app with `env` and its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, env: &[(&'static str, String)], dir: &Path) -> SandboxResult<Self> {
        let dir_text = dir.to_string_lossy().into_owned();
        let mut vars: Vec<(&str, &str)> = vec![(CHILD_ENV, "app"), (DIR_ENV, &dir_text)];
        vars.extend(env.iter().map(|(name, value)| (*name, value.as_str())));
        let child = sandbox.spawn(&std::env::current_exe()?, &[], &vars)?;
        let running = Running::new(child)?;
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut run = Self {
            running,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    /// The state of the window, with the rows of `view` (`chat` or `posts`).
    fn state(&mut self, view: &str) -> SandboxResult<Value> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of `view` that `wanted` takes, named `what` in the error.
    fn until(&mut self, view: &str, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state(view)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, the pointer at rest, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        keep(&png, name)?;
        Ok(png)
    }

    /// Clicks (`x`, `y`) as a user does.
    fn click(&mut self, x: u32, y: u32) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(Duration::from_millis(250));
        Ok(())
    }

    /// The key of letter `letter`.
    fn letter(&self, letter: char) -> SandboxResult<u32> {
        self.input
            .keymap()
            .key_for(letter)
            .map(|(key, _)| key)
            .ok_or_else(|| format!("no key for {letter}").into())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa660-failed-run.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits the app; once it ended.
    fn quit(mut self) -> SandboxResult {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} quit", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
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

/// The engine of `alias` (`mri`) on the parent's mock (through its relay), its store in `dir`.
fn engine_on_the_mock(dir: &Path, alias: Alias, mri: &str, token_env: &str) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(token_env)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?.with_middle_tier(MIDDLE_TIER_BASE)?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let upn = format!("bot-{alias}@lab.example.test");
    let author = Author::new(Mri::new(mri), display_name(&upn));
    let known_names = [(LAB_A, "Lab A"), (LAB_B, "Lab B"), (LAB_C, "Lab C")]
        .into_iter()
        .filter(|(other, _)| *other != mri)
        .map(|(other, name)| (other.to_owned(), name.to_owned()))
        .collect();
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, alias))?,
        client: Arc::new(ChatClient::new(
            http()?
                .with_ams(AMS_BASE)?
                .with_presence(PRESENCE_BASE)?
                .with_csa(CSA_BASE)?
                .with_user_profile(USER_PROFILE_BASE)?
                .with_provisioning(PROVISIONING_BASE)?
                .with_middle_tier(MIDDLE_TIER_BASE)?,
        )),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names,
    })?)
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked; `false` once the settings are gone.
fn show_theme(settings: &gpui::WeakEntity<teams_ui::SettingsModel>, key: &'static str, cx: &mut App) -> bool {
    settings
        .update(cx, |model, cx| {
            model.change(
                |chosen| {
                    chosen.appearance.follow_system = false;
                    chosen.appearance.theme = key;
                },
                cx,
            );
        })
        .is_ok()
}

/// Read receipts on or off (`on`), as the Privacy page's switch sets them.
fn send_receipts(settings: &Entity<teams_ui::SettingsModel>, on: bool, cx: &mut App) {
    settings.update(cx, |model, cx| {
        model.change(|chosen| chosen.send_read_receipts = on, cx);
    });
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, and says `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed = cx.update(|cx| show_theme(&settings, key, cx));
            if !changed {
                return;
            }
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let two = std::env::var(MODE_ENV).as_deref() == Ok("ab");
    let first = support::started(Alias::A, engine_on_the_mock(&dir, Alias::A, LAB_A, TOKEN_A_ENV)?)?;
    let mut others = Vec::new();
    if two {
        others.push(support::started(
            Alias::B,
            engine_on_the_mock(&dir, Alias::B, LAB_B, TOKEN_B_ENV)?,
        )?);
    }
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let theme_file = dir.join(THEME_FILE);
    let _ = fs::remove_file(&theme_file);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others,
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
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, settings, ask.clone(), cx);
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

/// A window with nothing in it: another app's window that takes the keyboard.
struct Blank;

impl gpui::Render for Blank {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::Styled as _;
        gpui::div().size_full().bg(gpui::rgb(0x0080_8080))
    }
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, settings: Entity<teams_ui::SettingsModel>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut blank: Option<WindowHandle<Blank>> = None;
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
                "blank-open" => {
                    let opened = cx.update(|cx| cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Blank)));
                    match opened {
                        Ok(handle) => {
                            blank = Some(handle);
                            "ok".to_owned()
                        }
                        _ => "no window".to_owned(),
                    }
                }
                "blank-close" => {
                    if let Some(handle) = blank.take() {
                        let _ = handle.update(cx, |_, window, _| window.remove_window());
                    }
                    let _ = window.update(cx, |_, window, _| window.activate_window());
                    "ok".to_owned()
                }
                "receipts on" | "receipts off" => {
                    let on = verb == "receipts on";
                    cx.update(|cx| send_receipts(&settings, on, cx));
                    "ok".to_owned()
                }
                "font" => cx.update(|cx| settings.read(cx).settings().appearance.font_size.to_string()),
                _ => window
                    .update(cx, |kit, window, cx| {
                        let root = kit.view().clone().downcast::<Root>().ok()?;
                        Some(on_root(&root, verb, window, cx))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "no root".to_owned()),
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

/// The chat view of `view`: the chat's, or the channel's posts.
fn view_of(shell: &Entity<Shell>, view: &str, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    match view {
        "chat" => shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))).cloned(),
        "posts" => shell.channel().map(|channel| channel.read(cx).posts().clone()),
        _ => None,
    }
}

fn on_root(root: &Entity<Root>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let Some(shell) = root.read(cx).shell_in(cx).cloned() else {
        return "no shell".to_owned();
    };
    let (verb, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => state_of(root, &shell, rest, cx).to_string(),
        "open" => {
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "open-channel" => {
            let list = shell.read(cx).list().clone();
            list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// The chat list of a shell: the rows of "Chats" in their drawn order, and each chat's count.
fn list_json(shell: &Entity<Shell>, cx: &App) -> Value {
    let list = shell.read(cx).list().read(cx);
    let shown: Vec<String> = list
        .shown_rows()
        .into_iter()
        .filter_map(|index| list.rows().get(index))
        .map(|row| row.id.clone())
        .collect();
    let counts: serde_json::Map<String, Value> = list
        .rows()
        .iter()
        .map(|row| (row.id.clone(), json!(row.unread)))
        .collect();
    // a channel is listed in Favorites only
    let channel_unread = list
        .rows()
        .iter()
        .chain(list.favorites())
        .find(|row| row.id == TEAM_CHANNEL)
        .map(|row| row.unread);
    json!({
        "account": shell.read(cx).account(),
        "shown": shown,
        "counts": counts,
        "favorites": list.favorites().len(),
        "row_menu": list.row_menu_open(),
        "row_menu_entry": list.row_menu_open().and_then(|id| list.row(id)).map(|row| {
            if row.unread > 0 { teams_ui::chat_list::MARK_READ } else { teams_ui::chat_list::MARK_UNREAD }
        }),
        "channel_unread": channel_unread,
    })
}

/// The window's state: the main view, the shown account's list, every account's list, and the rows of `view`.
fn state_of(root: &Entity<Root>, shell: &Entity<Shell>, view: &str, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let main = match shell_ref.main() {
        Main::Chat(_) => "chat".to_owned(),
        Main::Channel(_) => "channel".to_owned(),
        other => format!("{other:?}"),
    };
    let mut state = list_json(shell, cx);
    state["main"] = json!(main);
    state["accounts"] = Value::Array(
        root.read(cx)
            .shells()
            .iter()
            .map(|shell| {
                let list = list_json(shell, cx);
                json!({
                    "account": list["account"],
                    "shown": list["shown"].as_array().map_or(0, Vec::len),
                    "counts": list["counts"],
                })
            })
            .collect(),
    );
    let Some(chat) = view_of(shell, view, cx) else {
        state["items"] = json!([]);
        return state;
    };
    let chat = chat.read(cx);
    let list = chat.list_state();
    let pixels = |value: gpui::Pixels| f64::from(value);
    let viewport = list.viewport_bounds();
    let mut divider_before = Value::Null;
    let mut after_divider = false;
    let mut items = Vec::new();
    for (index, item) in chat.items().iter().enumerate() {
        match item {
            Item::NewMessages => after_divider = true,
            Item::Message(message) => {
                if after_divider && divider_before.is_null() {
                    divider_before = json!(message.id);
                }
                let rect = list.bounds_for_item(index).map(|bounds| {
                    json!([
                        pixels(bounds.origin.x),
                        pixels(bounds.origin.y),
                        pixels(bounds.size.width),
                        pixels(bounds.size.height)
                    ])
                });
                items.push(json!({"id": message.id, "own": message.own, "rect": rect}));
            }
            _ => {}
        }
    }
    state["conversation"] = json!(chat.conversation());
    state["selected"] = json!(chat.selected());
    state["hovered"] = json!(chat.hovered());
    state["options"] = json!(chat.options());
    state["divider_before"] = divider_before;
    state["viewport"] = json!([
        pixels(viewport.origin.x),
        pixels(viewport.origin.y),
        pixels(viewport.size.width),
        pixels(viewport.size.height)
    ]);
    state["items"] = Value::Array(items);
    state
}
