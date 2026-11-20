//! Calls of ten people and more, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the
//! process's main thread), for their screenshots: the call window over the fake engine (`common`), a group call of
//! 10, 16, 25 and 50 made-up participants, some with their camera on (a test pattern), the rest showing their
//! initials, two with a hand up; in the gallery (also in Teams Dark), in the speaker view (alone and beside the People pane) and with someone's screen shared;
//! on a small, a laptop's and a full HD screen. On a laptop's screen also: a share of 14 whose presenter and speaker
//! come from the strip's second page to its top (also in Teams Dark), live reactions on their senders' tiles, live
//! reactions floating over Focus on content, and the Chat's unread dot with a chat bubble (also in Teams Dark). The
//! test runs itself again inside the sandbox per scene; the parent screenshots each window into
//! `target/ui-shots/call-large-<scene>.png`.
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

use common::{FakeCore, FakeEvents, LAB_A};
use gpui::{App, AppContext as _, Window};
use teams_core::{CallId, CallParticipant, CallStatus, CallView, Event, RegistrationReport, VideoOutput};
use teams_media::devices::pattern::TestPattern;
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::call_stage::{CallStage, Layout};
use teams_ui::call_window::CallWindows;
use teams_ui::calls::{self, Ask};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::ShellParts;
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::video::VideoFeed;
use teams_ui::window::KitRoot;
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "the shared fixture's unsized sandbox serves the other scenes")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sized_sandbox_or_skip};

/// Which scene the child draws: its name.
const CHILD_ENV: &str = "TEAMS_UI_CALL_LARGE_TEST";
/// The child window's Wayland app id.
const APP_ID: &str = "ngteams-call-large-test";
/// From the child's start to its `ready`.
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 30;

/// The screens the scenes are drawn on: their name and size, px.
const SCREENS: [(&str, (u32, u32)); 3] = [("small", (800, 560)), ("laptop", (1280, 800)), ("full", (1920, 1080))];

/// The scenes, each screenshotted on each screen as `call-large-<scene>-<screen>.png`: how the stage is laid out and
/// how many are in the call.
const SCENES: [&str; 10] = [
    "gallery-10",
    "gallery-16",
    "gallery+dark-16",
    "gallery-25",
    "gallery-50",
    "speaker-25",
    "speaker+people-25",
    "share-10",
    "share-25",
    "share-50",
];

/// The scenes shot on a laptop's screen alone.
const LAPTOP_SCENES: [&str; 6] = [
    "share+order-14",
    "share+order+dark-14",
    "gallery+react-10",
    "focus+float-25",
    "gallery+bubble-10",
    "gallery+bubble+dark-10",
];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(scene) => child(&scene),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("call large window test: {error}");
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
    for (screen, size) in SCREENS {
        let Some(sandbox) = sized_sandbox_or_skip("teams-ui call large window", Some(size))? else {
            return Ok(());
        };
        let laptop = if screen == "laptop" { &LAPTOP_SCENES[..] } else { &[] };
        for scene in SCENES.iter().chain(laptop) {
            shot(&sandbox, scene, screen).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    say("teams-ui call large window: ok");
    Ok(())
}

/// Runs the child drawing `scene` and screenshots its call window as `call-large-<scene>-<screen>.png`.
fn shot(sandbox: &Sandbox, scene: &str, screen: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, scene)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    let file = format!("call-large-{scene}-{screen}.png");
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

// --- the child: the call window over the fake engine ---

const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const GROUP: &str = "19:0000000000000000000000000000crew@thread.v2";

/// Made-up first and last names the participants are named from.
const FIRST: [&str; 10] = ["Ann", "Ben", "Cleo", "Dan", "Eva", "Finn", "Gus", "Hana", "Ivo", "Jun"];
const LAST: [&str; 5] = ["Able", "Bright", "Sea", "Dee", "Eastwood"];

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The `n`th participant's MRI, from 1.
fn mri(n: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-0000000002{n:02}")
}

/// What the scene's name says: its layout and how many others are in the call.
fn parse(scene: &str) -> (&str, usize) {
    let (layout, count) = scene.rsplit_once('-').unwrap_or((scene, "10"));
    (layout, count.parse().unwrap_or(10))
}

/// The group chat and its members.
fn seed(core: &FakeCore, count: usize) {
    locked(&core.conversations).push(Conversation {
        id: GROUP.to_owned(),
        kind: ConversationKind::Group,
        topic: Some("All hands".to_owned()),
        version: 1,
        last_activity: Some(1),
    });
    locked(&core.members).insert(
        GROUP.to_owned(),
        std::iter::once(LAB_A.to_owned())
            .chain((1..=count).map(mri))
            .map(|mri| Member::new(&mri, None))
            .collect(),
    );
}

/// The roster of `count` others: the first nine with their camera on, each on a line of its own, one speaking (the
/// third, or `speaker`), a few muted, two with a hand up (one with its camera on, one without), `sharer` sharing.
fn roster(count: usize, (sharer, speaker): (Option<usize>, Option<usize>)) -> CallView {
    CallView {
        participants: (1..=count)
            .map(|n| CallParticipant {
                mri: mri(n),
                name: Some(format!(
                    "{} {}",
                    FIRST[n % FIRST.len()],
                    LAST[n / FIRST.len() % LAST.len()]
                )),
                video_on: n % 2 == 1 && n < 19,
                line: (n % 2 == 1 && n < 19).then_some(n / 2 + 1),
                speaking: speaker.map_or(n == 3, |speaker| n == speaker),
                muted: n % 3 == 0,
                hand_raised: n == 5 || n == 8,
                organizer: n == 1,
                joined: None,
                sharing: sharer == Some(n),
            })
            .collect(),
        speaking: false,
        hearing: true,
    }
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        #[cfg(windows)]
        if !fixture::scene_activated(window) {
            after_frames(window, frames, done);
            window.refresh();
            return;
        }
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(scene: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let scene = scene.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        teams_ui::window::init(cx);
        open(&scene, cx);
    });
    Ok(())
}

/// The cameras' pictures on their lines, again and again so they never go stale.
fn film(feed: VideoFeed, cx: &mut App) {
    let pattern = TestPattern::new(320, 180);
    cx.spawn(async move |cx| {
        for frame in 1..u32::MAX {
            let output: &dyn VideoOutput = &feed;
            for line in 0..=teams_core::GROUP_SLOTS {
                let _ = output.remote(line).show(&pattern.frame(frame));
            }
            let _ = output
                .remote(teams_media::session::SHARING_LINE)
                .show(&TestPattern::new(1280, 720).frame(frame));
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    })
    .detach();
}

/// The call window of `scene`'s call, once it is up as the scene has it.
fn open(scene: &str, cx: &mut App) {
    let (layout, count) = parse(scene);
    let sharing = layout.starts_with("share") || layout.starts_with("focus");
    let layout = layout.to_owned();
    let core = Arc::new(FakeCore::default());
    seed(&core, count);
    let fake = Arc::clone(&core);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let feed = VideoFeed::new();
    let models = Models::with_video_calls(Some(teams_proto::Mri::new(ECHO)), feed.clone(), cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let theme = if layout.ends_with("+dark") { DARK_KEY } else { LIGHT_KEY };
    let appearance = Appearance {
        follow_system: false,
        theme,
        ..Appearance::default()
    };
    teams_ui::theme::set_appearance(&appearance, cx);
    let settings = Settings {
        appearance,
        ..Settings::default()
    };
    let trace = Arc::new(ViewTrace::off());
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models: models.clone(),
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::clone(&trace),
    };
    let windows = cx.new(|cx| CallWindows::new(APP_ID, cx));
    windows.update(cx, |windows, cx| windows.add(parts.clone(), cx));
    let _ = events.send(Event::CallingRegistration(RegistrationReport::Registered));
    let core = parts.core.clone();
    let calls_model = models.calls.clone();
    let send = events.clone();
    film(feed, cx);
    // the registration drained first, then the call asked for and told up
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(300)).await;
        cx.update(|cx| {
            calls::ask(&calls_model, &core, &trace, Ask::Call { chat: GROUP.to_owned() }, cx);
        });
        cx.background_executor().timer(Duration::from_millis(300)).await;
        let call = CallId::for_tests(1);
        let _ = send.send(Event::Call {
            call,
            status: CallStatus::Connected,
        });
        tell(&layout, (count, sharing), (&fake, &send), cx).await;
        cx.update(|cx| ready(&windows, &layout, cx));
        // the bridge, the windows and the event channel live as long as the app
        mem::forget((bridge, send));
    })
    .detach();
}

/// Tells the scene's call as `layout` has it (`count` others, someone `sharing`): its roster and share, and the
/// scene's speaker, reactions or chat messages.
async fn tell(
    layout: &str,
    (count, sharing): (usize, bool),
    (fake, send): (&FakeCore, &mpsc::UnboundedSender<Event>),
    cx: &mut gpui::AsyncApp,
) {
    let call = CallId::for_tests(1);
    let order = layout.contains("+order");
    let presenter = if order { Some(12) } else { sharing.then_some(2) };
    let speaker = order.then_some(0);
    let _ = send.send(Event::CallParticipants {
        call,
        view: roster(count, (presenter, speaker)),
    });
    if sharing {
        let _ = send.send(Event::CallRemoteShare { call, on: true });
    }
    cx.background_executor().timer(Duration::from_millis(500)).await;
    if order {
        // the 13th, on the strip's second page, speaks long enough to come up
        let _ = send.send(Event::CallParticipants {
            call,
            view: roster(count, (presenter, Some(13))),
        });
        cx.background_executor().timer(Duration::from_millis(1_800)).await;
    }
    if layout.contains("+react") {
        for (n, reaction) in [(1, "heart"), (4, "like"), (6, "laugh"), (0, "applause")] {
            let from = if n == 0 { LAB_A.to_owned() } else { mri(n) };
            let _ = send.send(react(call, &from, reaction));
            cx.background_executor().timer(Duration::from_millis(150)).await;
        }
        cx.background_executor().timer(Duration::from_millis(1_400)).await;
    }
    if layout.contains("+float") {
        // a reaction now and then from people whose tiles the share hides, so several float at once
        let floating = send.clone();
        cx.spawn(async move |cx| {
            for (index, reaction) in ["heart", "like", "applause", "laugh", "surprised"]
                .iter()
                .cycle()
                .enumerate()
            {
                let _ = floating.send(react(call, &mri(index % 20 + 1), reaction));
                cx.background_executor().timer(Duration::from_millis(450)).await;
            }
        })
        .detach();
        cx.background_executor().timer(Duration::from_millis(1_500)).await;
    }
    if layout.contains("+bubble") {
        chat_message(fake, send, (2, "Can everyone see the deck? I can share it again.", 1));
        chat_message(fake, send, (3, "Yes, it looks fine here", 2));
        cx.background_executor().timer(Duration::from_millis(400)).await;
    }
}

/// A live reaction `reaction` from `from` in `call`.
fn react(call: CallId, from: &str, reaction: &str) -> Event {
    Event::CallReaction {
        call,
        from: teams_proto::Mri::new(from),
        name: None,
        reaction: reaction.to_owned(),
    }
}

/// The `n`th participant wrote `text` in the call's chat (its `seq`th message), the store counting it unread.
fn chat_message(core: &FakeCore, send: &mpsc::UnboundedSender<Event>, (n, text, seq): (usize, &str, u64)) {
    let arrival = 1_760_000_000_000 + i64::try_from(seq).unwrap_or(0);
    locked(&core.messages)
        .entry(GROUP.to_owned())
        .or_default()
        .push(Message {
            conversation_id: GROUP.to_owned(),
            id: arrival.to_string(),
            version: 1,
            client_message_id: None,
            sender: Some(mri(n)),
            sender_name: Some(format!(
                "{} {}",
                FIRST[n % FIRST.len()],
                LAST[n / FIRST.len() % LAST.len()]
            )),
            message_type: "RichText/Html".to_owned(),
            content: format!("<p>{text}</p>"),
            properties: None,
            arrival_time: arrival,
            compose_time: Some(arrival),
            edit_time: None,
            delete_time: None,
        });
    locked(&core.unread).insert(GROUP.to_owned(), seq);
    let _ = send.send(Event::ConversationChanged {
        conversation: GROUP.to_owned(),
    });
}

/// The call window laid out as `layout` has it, then `ready` once it settled.
fn ready(windows: &gpui::Entity<CallWindows>, layout: &str, cx: &mut App) {
    let Some(window) = windows.read(cx).window() else {
        eprintln!("no call window opened");
        cx.quit();
        return;
    };
    let speaker = layout.starts_with("speaker");
    let focus = layout.starts_with("focus");
    let people = layout.ends_with("+people");
    let _ = window.update(cx, move |kit: &mut KitRoot, window, cx| {
        if let Ok(stage) = kit.view().clone().downcast::<CallStage>() {
            stage.update(cx, |stage, cx| {
                if speaker {
                    stage.set_layout(Layout::Speaker, cx);
                }
                if focus {
                    stage.set_layout(Layout::Focus, cx);
                }
                stage.show_roster(people, cx);
            });
        }
        window.refresh();
        after_frames(window, READY_FRAMES, |_, _| say("ready"));
    });
}
