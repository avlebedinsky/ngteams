//! A meeting's chat with its system messages, drawn on the real platform in the UI sandbox (`harness = false`: GPUI
//! runs on the process's main thread), for its screenshots: the app's window over the fake engine (`common`), its store
//! holding a made-up meeting chat with each kind of system message Teams shows in one (who was invited, the meeting's
//! name, its start and end as cards, a recording's state as lines and its card once ready, the transcript's card, a
//! visitor joining and leaving, a kind this build doesn't show) among a few messages, the meeting chat kept as the one
//! shown. The test runs itself again inside the sandbox once per theme (Teams Light, Teams Dark); the parent
//! screenshots each window into `target/ui-shots/meeting-*.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
mod common;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Window, WindowingModes};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in: the theme's key.
const CHILD_ENV: &str = "TEAMS_UI_MEETING_WINDOW_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-meeting-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(theme) => child(&theme),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("meeting window test: {error}");
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
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("teams-ui meeting window", &options)? else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        shot(&sandbox, theme, name).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    }
    say("teams-ui meeting window: ok");
    Ok(())
}

/// Runs the child in `theme` and screenshots its window as `meeting-<name>.png`.
fn shot(sandbox: &Sandbox, theme: &str, name: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, theme)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("meeting-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots_dir()?.join(&file);
    fs::copy(&png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(WAIT)?;
    if !status.success() {
        return Err(format!("the child exited {status}: {stderr}").into());
    }
    Ok(())
}

// --- the child: the app's window over the fake engine ---

const MEETING: &str = "19:meeting_bWFkZS11cC1tZWV0aW5n@thread.v2";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const VISITOR: &str = "8:teamsvisitor:00000000000000000000000000000abc";

fn message(sender: &str, kind: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: MEETING.to_owned(),
        id: arrival.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
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

/// A recording's message in `status` (with its code) holding `contents`, started by b.
fn recording(status: &str, code: &str, contents: &str, duration: &str) -> String {
    format!(
        "<URIObject format_version=\"1.1\" type=\"Video.2/CallRecording.1\" url_thumbnail=\"\" uri=\"\" version=\"1.0\">\
         <RecordingStatus status=\"{status}\" code=\"{code}\" shortChunk=\"False\"><amsErrorResult /></RecordingStatus>\
         <Title>Release review</Title><a href=\"\">Play</a>\
         <OriginalName v=\"Release review-20260102_100010-Meeting Recording.mp4\" />\
         <RecordingInitiatorId value=\"{LAB_B}\" isAutoRecording=\"False\" />\
         <RecordingContent contentTypes=\"{contents}\" duration=\"{duration}\"><item type=\"video\" uri=\"\" />\
         </RecordingContent><RequestedExports /></URIObject>"
    )
}

/// A participant of a call's end, in it for `seconds`.
fn part(mri: &str, name: &str, seconds: u32) -> String {
    format!(
        "<part identity=\"{mri}\"><name>{mri}</name><displayName>{name}</displayName><duration>{seconds}</duration></part>"
    )
}

/// A scheduled meeting's details in a call's event.
const DETAILS: &str =
    "<meetingDetails><meetingDetails><meetingType>Scheduled</meetingType></meetingDetails></meetingDetails>";

/// Members joined or left, named as the event names them; `temp`: for the meeting only.
fn members(mris: &[(&str, &str)], temp: bool) -> String {
    let kind = if temp { r#","meetingMemberType":"temp""# } else { "" };
    let list: Vec<String> = mris
        .iter()
        .map(|(mri, name)| format!(r#"{{"id":"{mri}","friendlyname":"{name}"{kind}}}"#))
        .collect();
    format!(
        r#"{{"eventtime":1,"initiator":"{LAB_B}","members":[{}]}}"#,
        list.join(",")
    )
}

/// The meeting chat's messages, the first at `start` (ms), the oldest first.
fn meeting_messages(start: i64) -> Vec<Message> {
    let mut messages = before_the_end(start);
    messages.extend(from_the_end(start));
    messages
}

/// Up to the meeting's end: invited, named, started and recorded, two messages, a note taker joined.
fn before_the_end(start: i64) -> Vec<Message> {
    let at = |minute: i64| start + minute * 60_000;
    vec![
        message(
            MEETING,
            "ThreadActivity/MemberJoined",
            &members(&[(LAB_A, ""), (LAB_C, "")], false),
            at(0),
        ),
        message(
            MEETING,
            "ThreadActivity/TopicUpdate",
            &format!(
                "<topicupdate><eventtime>1</eventtime><initiator>{LAB_B}</initiator><value>Release review</value></topicupdate>"
            ),
            at(1),
        ),
        message(
            LAB_B,
            "Event/Call",
            &format!("<partlist alt =\"\"></partlist>{DETAILS}<callEventType>callStarted</callEventType>"),
            at(60),
        ),
        message(
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Recording", "0:00:00"),
            at(61),
        ),
        message(
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Transcript", "0:00:00"),
            at(61),
        ),
        message(
            LAB_A,
            "RichText/Html",
            "<p>Agenda: the release notes first, then the open bugs</p>",
            at(62),
        ),
        message(LAB_C, "RichText/Html", "<p>Notes are in the shared folder</p>", at(63)),
        message(
            MEETING,
            "ThreadActivity/MemberJoined",
            &members(&[(VISITOR, "Note Taker")], true),
            at(64),
        ),
    ]
}

/// From the meeting's end: the recording stopped, the end, the note taker gone, the recording and transcript ready,
/// and a kind this build doesn't show.
fn from_the_end(start: i64) -> Vec<Message> {
    let at = |minute: i64| start + minute * 60_000;
    vec![
        message(
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("ChunkFinished", "202", "Recording+Transcript", "0:31:02.5"),
            at(92),
        ),
        message(
            LAB_B,
            "Event/Call",
            &format!(
                "<ended/><partlist alt=\"\" count=\"3\">{}{}{}</partlist>{DETAILS}<callEventType>callEnded</callEventType>",
                part(LAB_B, "Ben Bright", 1930),
                part(LAB_C, "Cleo Sea", 1930),
                part(LAB_A, "Ann Able", 1930)
            ),
            at(92),
        ),
        message(
            MEETING,
            "ThreadActivity/MemberLeft",
            &members(&[(VISITOR, "Note Taker")], true),
            at(92),
        ),
        message(
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Success", "200", "Recording+Transcript", "0:31:02.5"),
            at(93),
        ),
        message(
            LAB_B,
            "RichText/Media_CallTranscript",
            r#"{\"callId\":\"made-up\",\"isDeleted\":false,\"isExportedToOdsp\":true}"#,
            at(94),
        ),
        message(MEETING, "ThreadActivity/SomethingNew", "<somethingnew/>", at(95)),
    ]
}

/// The made-up meeting chat: b invites a and c and names the meeting, the meeting starts and b records it, a and c
/// write, a note taker joins for the meeting, the meeting ends after 32 minutes, its recording and transcript are
/// ready, the note taker leaves, and the thread sends a kind this build doesn't show.
fn seed(core: &FakeCore) {
    let start = chrono::Utc::now().timestamp_millis() - 3 * 60 * 60_000;
    let stored = meeting_messages(start);
    // each its own id and time, in this order
    let stored: Vec<Message> = (0_i64..)
        .zip(stored)
        .map(|(index, mut message)| {
            message.arrival_time += index;
            message.id = message.arrival_time.to_string();
            message
        })
        .collect();
    let last = stored.last().map(|message| message.arrival_time);
    locked(&core.conversations).push(Conversation {
        id: MEETING.to_owned(),
        kind: ConversationKind::Other("thread_type:meeting".to_owned()),
        topic: Some("Release review".to_owned()),
        version: 1,
        last_activity: last,
    });
    let mut newest_first = stored;
    newest_first.reverse();
    locked(&core.messages).insert(MEETING.to_owned(), newest_first);
    locked(&core.members).insert(
        MEETING.to_owned(),
        vec![
            Member::new(LAB_A, None),
            Member::new(LAB_B, None),
            Member::new(LAB_C, None),
        ],
    );
    for (mri, name) in [(LAB_A, "Ann Able"), (LAB_B, "Ben Bright"), (LAB_C, "Cleo Sea")] {
        locked(&core.names).insert(mri.to_owned(), name.to_owned());
    }
    *locked(&core.listing) = ChatListing {
        favorites: None,
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
    ui_sandbox::window_allowed()?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
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
        selected_chat: Some(MEETING.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, APP_ID, "Meeting".into(), parts)?;
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
