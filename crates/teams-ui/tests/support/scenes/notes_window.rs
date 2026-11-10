//! The account's own notes opened from "Favorites", drawn on the real platform in the UI sandbox (`harness = false`:
//! GPUI runs on the process's main thread), for its screenshots: the app's window over the fake engine (`common`), its
//! store holding a few made-up notes of the account's own (`48:notes`) and one other chat, the notes the chat kept as
//! the one shown. The notes show their messages, and the compose box says they can't be written. The test runs itself
//! again inside the sandbox once per theme (Teams Light, Teams Dark); the parent screenshots each window into
//! `target/ui-shots/notes-*.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Window};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::chat_list::NOTES;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in: the theme's key.
const CHILD_ENV: &str = "TEAMS_UI_NOTES_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-notes-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(theme) => child(&theme),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("notes window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

/// Where the offline runs' screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip("teams-ui notes window")? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        shot(&sandbox, theme, name).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    }
    say("teams-ui notes window: ok");
    Ok(())
}

/// Runs the child in `theme` and screenshots its window as `notes-<name>.png`.
fn shot(sandbox: &Sandbox, theme: &str, name: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, theme)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("notes-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots_dir()?.join(&file);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the child: the app's window over the fake engine ---

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

fn text(conversation: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: arrival.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: format!("<p>{content}</p>"),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The made-up store: a's notes (a few of its own, the newest first) as its one favourite, and its chat with b.
fn seed(core: &FakeCore) {
    let now = chrono::Utc::now().timestamp_millis();
    let minute = 60_000;
    let notes = [
        "Buy milk and coffee",
        "Ideas for the demo: <b>keyboard first</b>, then the call bar",
        "Room 4.12, Thursday 10:00",
        "Ask about the new build's release notes",
    ];
    let mut stored: Vec<Message> = (0_i64..)
        .zip(notes)
        .map(|(at, said)| text(NOTES, LAB_A, said, now - (4 - at) * 90 * minute))
        .collect();
    stored.reverse();
    let last = stored.first().map(|message| message.arrival_time);
    locked(&core.conversations).push(Conversation {
        id: NOTES.to_owned(),
        kind: ConversationKind::Other("thread_type:streamofnotes".to_owned()),
        topic: None,
        version: 0,
        last_activity: last,
    });
    locked(&core.messages).insert(NOTES.to_owned(), stored);
    locked(&core.conversations).push(Conversation {
        id: AB.to_owned(),
        kind: ConversationKind::OneToOne,
        topic: None,
        version: 1,
        last_activity: Some(now - 30 * minute),
    });
    locked(&core.messages).insert(
        AB.to_owned(),
        vec![text(AB, LAB_B, "See you at the stand-up", now - 30 * minute)],
    );
    locked(&core.members).insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    for (mri, name) in [(LAB_A, "Ann Able"), (LAB_B, "Ben Bright")] {
        locked(&core.names).insert(mri.to_owned(), name.to_owned());
    }
    *locked(&core.listing) = ChatListing {
        favorites: Some(vec![NOTES.to_owned()]),
        empty: Vec::new(),
    };
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

fn child(theme: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let drained = models.clone();
    let bridge = cx.new(|cx| Bridge::start(source, shared, drained, cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        selected_chat: Some(NOTES.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "Notes".into(), parts)?;
    let _ = events.send(Event::Presence {
        mri: LAB_A.to_owned(),
        availability: Availability::Available,
    });
    // the bridge and the event channel live as long as the app
    mem::forget((bridge, events));
    window.update(cx, |_, window, _| {
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    })?;
    Ok(())
}
