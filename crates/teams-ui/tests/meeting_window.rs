//! A meeting's chat with its system messages, drawn on the real platform in the UI sandbox (`harness = false`: GPUI
//! runs on the process's main thread), for its screenshots: the app's window over the fake engine (`common`), its store
//! holding a made-up meeting chat with each kind of system message Teams shows in one (who was invited, the meeting's
//! name, its start, a recording started, stopped and saved with its expiry, an anonymous note taker invited and gone,
//! a kind this build doesn't show, each a line after its icon), the meeting's end with its recap card (View recap, the
//! transcript, the recording's thumbnail with its length and expiry) among a few messages, the meeting chat kept as
//! the one shown. The test runs itself again inside the sandbox once per theme (Teams Light, Teams Dark); the parent
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
use gpui::{App, AppContext as _, ListOffset, Window, WindowingModes};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in: the theme's key.
const CHILD_ENV: &str = "TEAMS_UI_MEETING_WINDOW_TEST";
/// After the theme's key in [`CHILD_ENV`]: the chat scrolled to its top before the shot.
const TOP: &str = ":top";
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
    // the chat at its newest, then scrolled to its top
    for (mode, name) in [
        (LIGHT_KEY.to_owned(), "light"),
        (DARK_KEY.to_owned(), "dark"),
        (format!("{LIGHT_KEY}{TOP}"), "light-top"),
        (format!("{DARK_KEY}{TOP}"), "dark-top"),
    ] {
        shot(&sandbox, &mode, name).map_err(|error| format!("{error}{}", sandbox.logs()))?;
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
const CALL: &str = "00000000-0000-4000-8000-0000000000c1";
const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_b/MadeUpRecording";
const DAY: i64 = 24 * 60 * 60_000;

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

/// A recording's message of the meeting's call in `status` (with its code) holding `contents`, begun at `started`
/// (ms) by a in b's meeting; once ready its link and its file in b's OneDrive, as a lab meeting's were.
fn recording(status: &str, code: &str, contents: &str, started: i64, duration: &str) -> String {
    let ready = status == "Success";
    let link = if ready { LINK } else { "" };
    let file = if ready {
        format!("<item type=\"onedriveForBusinessVideo\" uri=\"{LINK}\" driveId=\"b!drive1\" driveItemId=\"ITEM1\" />")
    } else {
        String::new()
    };
    let timestamp = chrono::DateTime::from_timestamp_millis(started)
        .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string())
        .unwrap_or_default();
    format!(
        "<URIObject format_version=\"1.1\" type=\"Video.2/CallRecording.1\" url_thumbnail=\"\" uri=\"\" version=\"1.0\">\
         <RecordingStatus status=\"{status}\" code=\"{code}\" shortChunk=\"False\"><amsErrorResult /></RecordingStatus>\
         <Title>Release review</Title><a href=\"{link}\">Play</a>\
         <OriginalName v=\"Release review-20260102_100010-Meeting Recording.mp4\" />\
         <MeetingOrganizerId value=\"{LAB_B}\" /><RecordingInitiatorId value=\"{LAB_A}\" isAutoRecording=\"False\" />\
         <Identifiers><Id type=\"callId\" value=\"{CALL}\" /></Identifiers>\
         <RecordingContent contentTypes=\"{contents}\" timestamp=\"{timestamp}\" duration=\"{duration}\">\
         <item type=\"video\" uri=\"\" />{file}</RecordingContent><RequestedExports /></URIObject>"
    )
}

/// A participant of a call's end, in it for `seconds`.
fn part(mri: &str, name: &str, seconds: u32) -> String {
    format!(
        "<part identity=\"{mri}\"><name>{mri}</name><displayName>{name}</displayName><duration>{seconds}</duration></part>"
    )
}

/// A scheduled meeting's details in a call's event: 30 minutes from `start` (ms), as Teams writes them (UTC).
fn details(start: i64) -> String {
    let at = |ms: i64| {
        chrono::DateTime::from_timestamp_millis(ms)
            .map(|at| at.format("%m/%d/%Y %H:%M:%S").to_string())
            .unwrap_or_default()
    };
    format!(
        "<meetingDetails><meetingDetails><meetingType>Scheduled</meetingType><startTime>{}</startTime>\
         <endTime>{}</endTime></meetingDetails></meetingDetails>",
        at(start),
        at(start + 30 * 60_000)
    )
}

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

/// Up to the meeting's end: invited, named, started and recorded, two messages, a note taker invited.
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
            &format!(
                "<partlist alt =\"\"></partlist>{}<callEventType>callStarted</callEventType><callId>{CALL}</callId>",
                details(at(60))
            ),
            at(60),
        ),
        message(
            LAB_A,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Recording", at(61), "0:00:00"),
            at(61),
        ),
        message(
            LAB_A,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Transcript", at(61), "0:00:00"),
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
            &members(&[(VISITOR, "Note Taker")], false),
            at(64),
        ),
    ]
}

/// From the meeting's end: the end, the recording stopped, the note taker gone, the recording and transcript ready,
/// and a kind this build doesn't show.
fn from_the_end(start: i64) -> Vec<Message> {
    let at = |minute: i64| start + minute * 60_000;
    vec![
        message(
            LAB_B,
            "Event/Call",
            &format!(
                "<ended/><partlist alt=\"\" count=\"3\">{}{}{}</partlist>{}<callEventType>callEnded</callEventType>\
                 <callId>{CALL}</callId>",
                part(LAB_B, "Ben Bright", 3697),
                part(LAB_C, "Cleo Sea", 3697),
                part(LAB_A, "Ann Able", 3697),
                details(at(60))
            ),
            at(89),
        ),
        message(
            LAB_A,
            "RichText/Media_CallRecording",
            &recording("ChunkFinished", "202", "Recording", at(61), "0:28:38.5"),
            at(89),
        ),
        message(
            MEETING,
            "ThreadActivity/MemberLeft",
            &members(&[(VISITOR, "Note Taker")], false),
            at(89),
        ),
        message(
            LAB_A,
            "RichText/Media_CallRecording",
            &recording("Success", "200", "Recording+Transcript", at(61), "0:28:38.5"),
            at(90),
        ),
        message(
            LAB_A,
            "RichText/Media_CallTranscript",
            &format!(r#"{{\"callId\":\"{CALL}\",\"isDeleted\":false,\"isExportedToOdsp\":true}}"#),
            at(91),
        ),
        message(MEETING, "ThreadActivity/SomethingNew", "<somethingnew/>", at(92)),
    ]
}

/// The made-up meeting chat: b invites a and c and names the meeting, the meeting starts and a records it, a and c
/// write, a note taker is invited, the meeting ends after an hour, its recording stops and is saved with its
/// transcript, the note taker leaves, and the thread sends a kind this build doesn't show.
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
    // the recording's thumbnail and its expiry, 120 days after it was saved (as Teams sets it)
    locked(&core.thumbnails).insert(LINK.to_owned(), poster());
    locked(&core.expirations).insert("ITEM1".to_owned(), start + 90 * 60_000 + 120 * DAY);
    *locked(&core.listing) = ChatListing {
        favorites: None,
        empty: Vec::new(),
    };
}

/// A recording's poster: a made-up frame, as Graph's thumbnail of the recording would be.
fn poster() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    let frame = image::RgbaImage::from_fn(640, 360, |x, y| {
        let shade = u8::try_from((x / 4 + y / 4) % 200).unwrap_or(0);
        image::Rgba([30 + shade / 3, 40 + shade / 4, 70 + shade / 2, 255])
    });
    if frame
        .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .is_err()
    {
        bytes.clear();
    }
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
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

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, top) = mode.strip_suffix(TOP).map_or((mode, false), |theme| (theme, true));
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, top, cx) {
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

/// The chat shown in `window` scrolled to its top.
fn scroll_to_top(window: &Window, cx: &App) {
    let chat = window
        .root::<KitRoot>()
        .flatten()
        .and_then(|kit| kit.read(cx).view().clone().downcast::<Root>().ok())
        .and_then(|root| root.read(cx).shell_in(cx).cloned())
        .and_then(|shell| shell.read(cx).chat().cloned());
    if let Some(chat) = chat {
        chat.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: gpui::px(0.0),
        });
    }
}

fn open(theme: &'static str, top: bool, cx: &mut App) -> gpui::Result<()> {
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
    window.update(cx, move |_, window, _| {
        after_frames(window, READY_FRAMES, move |window, cx| {
            if !top {
                say("ready");
                return;
            }
            scroll_to_top(window, cx);
            window.refresh();
            after_frames(window, READY_FRAMES, |_, _| say("ready"));
        });
    })?;
    Ok(())
}
