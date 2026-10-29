//! A look at the app's views in the UI sandbox, on the mock (`harness = false`: GPUI runs on the process's main
//! thread): the screenshots a change of the UI is checked by, kept under `target/ui-shots/tour-*.png`.
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`], only the view trace's pipe passed
//! through) as the app of lab account a, its engine on the mock chat service and its Trouter (`teams-mock`, through
//! the `test-support` mock origins, a test roster and log). Once the window is drawn and the chats are listed, the
//! child walks the views one step at a time (no chat open; the a–b chat with a message selected and its actions
//! shown; a reply begun; the account menu; every page of the settings, Calls with made-up devices; the appearance
//! and the chat again in Teams Dark; a message with files, its cards and their thumbnails, in Teams Dark and Light,
//! its first video played in its card and paused; a message of lines, small pictures and a block of code, in Teams
//! Dark and Light; the 12-hour time format and a chat's times on it; a group chat's presence; a favourite channel
//! opened from "Favorites"; forwarded messages; a call's own window, its participants' pane, beside the main window
//! with its in-call strip, and in Teams Dark) and says
//! `shot <name>` after each; the parent screenshots the window and lets it go on. The views are driven from inside
//! the app, never by screen coordinates.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Context, Entity, ListOffset, Window, WindowHandle, WindowingModes, px};
use teams_app::app::{self, Launch, LaunchAccount};
use teams_app::trace::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::VideoOutput;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::lab::display_name;
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster};
use teams_media::devices::catalog::{DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_media::devices::pattern::TestPattern;
use teams_mock::{
    AB_CHAT, ABC_CHAT, AC_CHAT, AMS_BASE, AX_CHAT, CHAT_BASE, CSA_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C,
    OUTSIDER, PRESENCE_BASE, TEAM_CHANNEL, TrouterMock, lab_toml,
};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::VideoFeed;
use teams_ui::call_stage::CallStage;
use teams_ui::chat_view::{Item, MessageAction};
use teams_ui::compose::Compose;
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "TEAMS_APP_TOUR_TEST";
/// The child's view trace descriptor.
const TRACE_FD_ENV: &str = "TEAMS_APP_TOUR_TRACE_FD";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(30);
/// Frames the window presents before the child says `ready`.
const READY_FRAMES: usize = 3;
/// The file in the sandbox dir the parent makes once it took a step's screenshot.
const NEXT: &str = "tour-next";
/// How long a step's views get to settle before their screenshot.
const SETTLE: Duration = Duration::from_millis(1500);

/// A step of the tour: its screenshot's name and what it does to the shell shown.
type Step = (&'static str, fn(&Entity<Shell>, &mut Window, &mut App));

/// The tour.
const STEPS: [Step; 40] = [
    ("no-chat", |_, _, _| {}),
    ("chat", |shell, window, cx| {
        // the mock answers no one's presence: b's as UPS would tell it, for the avatar's badge
        let models = shell.read(cx).models().clone();
        models.apply(
            teams_core::Event::Presence {
                mri: LAB_B.to_owned(),
                availability: teams_core::Availability::Away,
            },
            cx,
        );
        shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
    }),
    ("chat-hover-theirs", |shell, _, cx| hover_last(shell, false, cx)),
    ("chat-hover-mine", |shell, _, cx| hover_last(shell, true, cx)),
    ("chat-actions", |shell, window, cx| {
        hover_none(shell, cx);
        select_last_message(shell, window, cx);
    }),
    ("chat-reply", |shell, window, cx| {
        if let Some((chat, message)) = last_message(shell, cx) {
            chat.update(cx, |chat, cx| chat.act(MessageAction::Reply, &message, window, cx));
        }
    }),
    // the compose box's emoji picker, opened from its button over the box
    ("chat-emoji", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| {
            compose.cancel(window, cx);
            compose.toggle_emoji(window, cx);
        });
    }),
    // the box with the focus and text in it: its accent line, Send in the brand colour
    ("chat-compose", |shell, window, cx| {
        type_in_compose(shell, "Lunch at noon?", window, cx);
    }),
    ("account-menu", |shell, _, cx| {
        shell.update(cx, Shell::toggle_status_menu);
    }),
    ("settings-general", |shell, _, cx| {
        shell.update(cx, Shell::toggle_status_menu);
        show_page(shell, Page::General, cx);
    }),
    ("settings-appearance", |shell, _, cx| {
        show_page(shell, Page::Appearance, cx);
    }),
    ("settings-notifications", |shell, _, cx| {
        show_page(shell, Page::Notifications, cx);
    }),
    ("settings-privacy", |shell, _, cx| show_page(shell, Page::Privacy, cx)),
    ("settings-calls", |shell, _, cx| {
        // the mock's engine has no calls: made-up devices give the settings their Calls page, and calls shown with the
        // calling endpoint registered its "Make a test call"
        let model = shell.read(cx).settings().clone();
        teams_ui::settings::attach_devices(&model, Arc::new(FixedDirectory::new(made_up_devices())), cx);
        let models = shell.read(cx).models().clone();
        models
            .calls
            .update(cx, |calls, _| *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO))));
        models.apply(
            teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
            cx,
        );
        show_page(shell, Page::Calls, cx);
    }),
    ("settings-diagnostics", |shell, _, cx| {
        // the calls shown for the Calls page go again: the mock's engine has none
        let calls = shell.read(cx).models().calls.clone();
        calls.update(cx, |calls, _| *calls = teams_ui::Calls::default());
        show_page(shell, Page::Diagnostics, cx);
    }),
    ("settings-appearance-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.follow_system = false;
                    settings.appearance.theme = teams_ui::theme::DARK_KEY;
                },
                cx,
            );
        });
        show_page(shell, Page::Appearance, cx);
    }),
    ("chat-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.show_chats(window, cx));
        select_last_message(shell, window, cx);
    }),
    ("chat-dark-compose", |shell, window, cx| {
        type_in_compose(shell, "Lunch at noon?\nThe usual place, or the new one?", window, cx);
    }),
    ("chat-dark-emoji", |shell, window, cx| {
        with_compose(shell, cx, |compose, cx| compose.toggle_emoji(window, cx));
    }),
    // b's pasted picture with an emoticon in its text, b's wave alone and c's GIF from the GIF picker
    // (`seed_pictures`), in Teams Dark
    ("chat-pictures", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(ABC_CHAT.to_owned(), window, cx));
    }),
    // the same once its pictures were read and decoded: the pasted picture, and c's GIF on one of its frames
    ("chat-pictures-read", |_, _, _| {}),
    // c's message with two videos and a document (`seed_files`): its lines, then the files' cards, the videos' with
    // their thumbnails as Graph and the media host (the mock) gave them, in Teams Dark
    ("chat-files-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AC_CHAT.to_owned(), window, cx));
    }),
    // the first video played in its card, its bytes from the mock's download link (`seed_files`): loading, then
    // playing (its controls hidden while the pointer is elsewhere), then paused (or played to its end), its frame and
    // its controls with the seek bar part way
    ("chat-video-loading-dark", |_, _, cx| toggle_first_video(cx)),
    ("chat-video-playing-dark", |_, _, _| {}),
    ("chat-video-paused-dark", |_, _, cx| toggle_first_video(cx)),
    // x's message of text, two small pictures and a block of code (`seed_mixed`), in Teams Dark: the a–x chat opened
    // (at its newest, the forwards), then scrolled to its first message, each picture at its own size in the flow and
    // the code in Teams' code card
    ("chat-mixed-dark", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AX_CHAT.to_owned(), window, cx));
    }),
    ("chat-mixed-dark-read", |shell, _, cx| show_first_message(shell, cx)),
    // the 12-hour clock chosen, back in Teams Light: the setting, then the a–b chat's times
    ("settings-time-12-hour", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.appearance.theme = teams_ui::theme::LIGHT_KEY;
                    settings.time_format = teams_ui::settings::TimeFormat::Twelve;
                },
                cx,
            );
        });
        show_page(shell, Page::General, cx);
    }),
    ("chat-12-hour", |shell, window, cx| {
        shell.update(cx, |shell, cx| {
            shell.show_chats(window, cx);
            shell.open_chat(AB_CHAT.to_owned(), window, cx);
        });
    }),
    // the files' cards again, in Teams Light
    ("chat-files", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AC_CHAT.to_owned(), window, cx));
    }),
    // the a–b–c chat: its senders' presence as the view asked the engine and the mock's UPS answered (`seed_group`),
    // b's and c's badges on their avatars
    ("chat-group-presence", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(ABC_CHAT.to_owned(), window, cx));
    }),
    // `@` typed in the a–b–c chat: the mention picker over the box, the chat's members, then Everyone; above it b's
    // mention of everyone (`seed_group`), the account's own
    ("chat-mention-everyone", |shell, window, cx| {
        type_in_compose(shell, "@", window, cx)
    }),
    // the team's channel the mock's CSA lists as a's favourite, under "Favorites" with its team's mark, clicked there:
    // opened read-only as a channel, its row highlighted
    ("favorite-channel", |shell, _, cx| {
        let list = shell.read(cx).list().clone();
        list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
    }),
    // two messages b sent in the a–b–c chat, forwarded to a by the outsider (`seed_forwards`): each in Teams' card
    // under the arrow, b's avatar and name and the original's time, the second's picture drawn in it
    ("chat-forward", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AX_CHAT.to_owned(), window, cx));
    }),
    ("chat-forward-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
    // x's message of text, pictures and code again, in Teams Light, above the forwards in the same chat
    ("chat-mixed", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::LIGHT_KEY, cx);
        });
        show_first_message(shell, cx);
    }),
    // a call b made and a answered, as the engine would tell it (`fake_call`: the mock's engine has no calls): its
    // own window over the main one (niri gives it the screen), b speaking on its video, the controls in its header
    ("call-window", |shell, _, cx| fake_call(shell, cx)),
    // its participants' pane open from People
    ("call-window-people", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_roster(true, cx));
    }),
    // the main window beside it, its chats usable, its in-call strip the way back: the parent puts the two side by
    // side and shoots the screen
    ("call-side-by-side", |_, _, cx| {
        with_stage(cx, |stage, cx| stage.show_roster(false, cx));
    }),
    // the call window in Teams Dark
    ("call-window-dark", |shell, _, cx| {
        let model = shell.read(cx).settings().clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.appearance.theme = teams_ui::theme::DARK_KEY, cx);
        });
    }),
];

/// The made-up call's number.
const FAKE_CALL: u64 = 1;

/// A call from b answered, told to a's models as the engine would tell it: the invitation, its answer, its media
/// connected, the roster naming b speaking with its camera on line 0; b's picture a test pattern, fed while the tour
/// runs.
fn fake_call(shell: &Entity<Shell>, cx: &mut App) {
    let models = shell.read(cx).models().clone();
    let feed = VideoFeed::new();
    models.calls.update(cx, |calls, _| {
        *calls = teams_ui::Calls::shown(Some(Mri::new(ECHO))).with_video(feed.clone());
    });
    let call = teams_core::CallId::for_tests(FAKE_CALL);
    let events = [
        teams_core::Event::CallingRegistration(teams_core::RegistrationReport::Registered),
        teams_core::Event::IncomingCall(teams_core::IncomingCall {
            call,
            caller: Mri::new(LAB_B),
            caller_name: Some("Lab Bee".to_owned()),
            group_chat: None,
        }),
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Answering,
        },
        teams_core::Event::Call {
            call,
            status: teams_core::CallStatus::Connected,
        },
        teams_core::Event::CallParticipants {
            call,
            view: teams_core::CallView {
                participants: vec![teams_core::CallParticipant {
                    mri: LAB_B.to_owned(),
                    name: Some("Lab Bee".to_owned()),
                    video_on: true,
                    line: Some(0),
                    speaking: true,
                }],
                speaking: false,
                hearing: false,
            },
        },
    ];
    for event in events {
        models.apply(event, cx);
    }
    cx.spawn(async move |cx: &mut AsyncApp| {
        let pattern = TestPattern::new(640, 360);
        let output: &dyn VideoOutput = &feed;
        let mut sink = output.remote(0);
        for frame in 0.. {
            if sink.show(&pattern.frame(frame)).is_err() {
                break;
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
    })
    .detach();
}

/// Runs `change` on the call window's stage, when it is open.
fn with_stage(cx: &mut App, change: impl FnOnce(&mut CallStage, &mut Context<CallStage>)) {
    let stage = cx.windows().into_iter().find_map(|window| {
        let kit = window.downcast::<KitRoot>()?;
        kit.read(cx).ok()?.view().clone().downcast::<CallStage>().ok()
    });
    if let Some(stage) = stage {
        stage.update(cx, change);
    }
}

fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("window") => child_window(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ui tour: {error}");
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

// --- the parent: the sandbox and the screenshots ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("teams-app ui tour", &options)? else {
        return Ok(());
    };
    tour(&sandbox).map_err(|error| format!("{error}{}", sandbox.logs()))?;
    say("teams-app ui tour: ok");
    Ok(())
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

/// The step whose screenshot is the screen, the main window and the call window side by side.
const SIDE_BY_SIDE: &str = "call-side-by-side";

/// The main window and the call window put side by side, each half the screen, and the screen shot into `file`; the
/// call window keeps the focus.
fn side_by_side(sandbox: &Sandbox, file: &str) -> SandboxResult<PathBuf> {
    for action in [
        &["action", "set-column-width", "50%"][..],
        &["action", "focus-column-left"],
        &["action", "set-column-width", "50%"],
        &["action", "focus-column-right"],
    ] {
        sandbox.niri_msg(action)?;
        // each move done before the next is reckoned from where the columns are
        thread::sleep(Duration::from_millis(600));
    }
    // niri's columns move into place
    thread::sleep(SETTLE);
    sandbox.screenshot(file, false)
}

fn tour(sandbox: &Sandbox) -> SandboxResult {
    let (reader, writer) = io::pipe()?;
    let fd = writer.as_raw_fd().to_string();
    let child = sandbox.spawn_with_fds(
        &std::env::current_exe()?,
        &[],
        &[(CHILD_ENV, "window"), (TRACE_FD_ENV, &fd)],
        &[writer.as_fd()],
    )?;
    // the child holds the only writer from here, so the trace ends when it exits
    drop(writer);
    let mut running = Running::new(child)?;
    // the trace is read only to keep its pipe from filling
    thread::spawn(move || for _ in BufReader::new(reader).lines() {});
    running
        .line_where(STARTUP, |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
    for (name, _) in STEPS {
        running
            .line_where(WAIT, |line| line == format!("shot {name}"))
            .map_err(|error| running.fail(&format!("no shot {name}: {error:?}")))?;
        let file = format!("tour-{name}.png");
        let png = if name == SIDE_BY_SIDE {
            side_by_side(sandbox, &file)?
        } else {
            sandbox.screenshot(&file, true)?
        };
        fs::copy(&png, shots_dir()?.join(&file))?;
        fs::write(sandbox.dir().join(NEXT), "")?;
    }
    running
        .line_where(WAIT, |line| line == "toured")
        .map_err(|error| running.fail(&format!("the tour didn't end: {error:?}")))?;
    drop(running);
    Ok(())
}

// --- the child: the app on the mock, walked through its views ---

/// The shell the window shows.
fn shown(window: WindowHandle<KitRoot>, cx: &mut AsyncApp) -> Option<Entity<Shell>> {
    window
        .update(cx, |kit, _, cx| {
            let root = kit.view().clone().downcast::<Root>().ok()?;
            root.read(cx).shell_in(cx).cloned()
        })
        .ok()
        .flatten()
}

/// The chat shown and the id of its last message that offers actions (a deleted one offers none), once it has one.
fn last_message(shell: &Entity<Shell>, cx: &App) -> Option<(Entity<teams_ui::chat_view::ChatView>, String)> {
    let chat = shell.read(cx).chat()?.clone();
    let message = chat.read(cx).items().iter().rev().find_map(|item| match item {
        Item::Message(message) if !message.actions().is_empty() => Some(message.id.clone()),
        _ => None,
    })?;
    Some((chat, message))
}

/// Plays the first video card drawn, or pauses it while it plays.
fn toggle_first_video(cx: &mut App) {
    if let Some(player) = teams_ui::video_player::players(cx).first() {
        player.update(cx, teams_ui::video_player::VideoPlayer::toggle);
    }
}

/// Shows the hover bar of the last message of b's (`own` false) or a's that offers actions, as the pointer over it
/// would, nothing selected.
fn hover_last(shell: &Entity<Shell>, own: bool, cx: &mut App) {
    let Some(chat) = shell.read(cx).chat().cloned() else {
        return;
    };
    let message = chat.read(cx).items().iter().rev().find_map(|item| match item {
        Item::Message(message) if message.own == own && !message.actions().is_empty() => Some(message.id.clone()),
        _ => None,
    });
    chat.update(cx, |chat, cx| {
        chat.select(None, cx);
        chat.hover(message, cx);
    });
}

/// Scrolls the chat shown to its top: its first message (the a–x chat's mixed one, above the forwards).
fn show_first_message(shell: &Entity<Shell>, cx: &mut App) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        chat.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        });
        cx.refresh_windows();
    }
}

fn hover_none(shell: &Entity<Shell>, cx: &mut App) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        chat.update(cx, |chat, cx| chat.hover(None, cx));
    }
}

/// Runs `change` on the compose box of the chat shown.
fn with_compose(shell: &Entity<Shell>, cx: &mut App, change: impl FnOnce(&mut Compose, &mut Context<Compose>)) {
    if let Some(chat) = shell.read(cx).chat().cloned() {
        let compose = chat.read(cx).compose().clone();
        compose.update(cx, change);
    }
}

/// Closes the emoji picker if it is open, gives the compose box the focus and types `text` into it, in place of what
/// an earlier step left there.
fn type_in_compose(shell: &Entity<Shell>, text: &str, window: &mut Window, cx: &mut App) {
    with_compose(shell, cx, |compose, cx| {
        if compose.emoji_picker().is_some() {
            compose.toggle_emoji(window, cx);
        }
        compose.focus(window, cx);
        compose.input().update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.insert(text.to_owned(), window, cx);
        });
    });
}

fn select_last_message(shell: &Entity<Shell>, _: &mut Window, cx: &mut App) {
    if let Some((chat, message)) = last_message(shell, cx) {
        chat.update(cx, |chat, cx| chat.select(Some(message), cx));
    }
}

/// A microphone, a headset, speakers and a camera, the system's default marked.
fn made_up_devices() -> DeviceList {
    let device = |kind, name: &str, description: &str| DeviceInfo {
        kind,
        name: name.to_owned(),
        description: description.to_owned(),
    };
    DeviceList::new(vec![
        device(DeviceKind::Microphone, "built-in-mic", "Built-in microphone"),
        device(DeviceKind::Microphone, "usb-headset-mic", "USB headset"),
        device(DeviceKind::Speaker, "built-in-speakers", "Speakers"),
        device(DeviceKind::Camera, "webcam", "Integrated webcam"),
    ])
    .with_default(DeviceKind::Microphone, Some("built-in-mic"))
}

fn show_page(shell: &Entity<Shell>, page: Page, cx: &mut App) {
    shell.update(cx, Shell::show_settings);
    let view = shell.read(cx).settings_view().clone();
    view.update(cx, |view, cx| view.show_page(page, cx));
}

/// Walks [`STEPS`], each one's screenshot taken by the parent before the next.
fn walk(window: WindowHandle<KitRoot>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let next = PathBuf::from(dir).join(NEXT);
    cx.spawn(async move |cx: &mut AsyncApp| {
        // the catch-up lists the chats
        cx.background_executor().timer(SETTLE).await;
        for (name, step) in STEPS {
            if let Some(shell) = shown(window, cx) {
                let _ = window.update(cx, |_, window, cx| step(&shell, window, cx));
            }
            cx.background_executor().timer(SETTLE).await;
            say(&format!("shot {name}"));
            while fs::remove_file(&next).is_err() {
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
        }
        say("toured");
    })
    .detach();
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

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
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

/// The mock and a's engine on it, as `teams_core::lab::start` builds one but for the mock origins, a test roster and
/// a test log; its push channel on the Trouter mock.
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
        seed_group(&mock, &runtime)?;
        seed_pictures(&mock, &runtime)?;
        seed_files(&mock, &runtime)?;
        seed_mixed(&mock, &runtime)?;
        seed_own(&mock)?;
        seed_mentions(&mock)?;
        seed_forwards(&mock, &runtime)?;
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
        record_abc_chat(config.paths(), ABC_CHAT)?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let fence = Arc::new(Fence::new(
            Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
            FenceLog::from_writer(io::sink()),
        ));
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let membership = LiveMembershipSource::with_mock_origin(&origin)?;
        membership.set_credential_source(transport.clone());
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
            store: Store::open(&config.paths().store_file(Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?,
            )),
            outbox: Outbox::new(transport, fence, Arc::new(membership), author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
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

/// c's message in the a–b–c chat, then b's mention of everyone, before b's other messages; the mock's UPS answering b as away (as the "chat" step tells it) and c
/// as busy, so the chat's senders show two badges once the view asked about them.
fn seed_group(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    runtime.block_on(mock.serve_presence(&[(LAB_B, "Away"), (LAB_C, "Busy")]));
    mock.post_as(
        LAB_C,
        ABC_CHAT,
        "<p>Morning all, the release notes are ready for a look</p>",
    )
    .ok_or("c's post refused")?;
    // b's mention of everyone, as Teams sends one (`mention-everyone.json`): the chat's id as its MRI
    let html = "<p><span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Everyone</span> \
                standup moves to 10:30 tomorrow</p>";
    let id = mock.post_as(LAB_B, ABC_CHAT, html).ok_or("b's post refused")?;
    let mentions = serde_json::json!([{
        "@type": "http://schema.skype.com/Mention", "itemid": 0, "mri": ABC_CHAT, "mentionType": "everyone",
        "displayName": "Everyone",
    }])
    .to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    if !mock.change_message(ABC_CHAT, &client_message_id, |message| {
        message["properties"]["mentions"] = serde_json::json!(mentions);
    }) {
        return Err("b's post isn't in the mock".into());
    }
    Ok(())
}

/// A pasted picture's made-up object, and the view of it b's message names (on the media service's host, read from
/// the account's AMS base).
const PICTURE_OBJECT: &str = "0-eus-d9-00000000000000000000000000000abc";

/// An emoticon's `<img>` as Teams sends it, in its span (`docs/protocol/chat.md`, "Pictures in messages").
fn emoticon(id: &str, alt: &str, side: u32) -> String {
    format!(
        "<span title=\"{id}\" type=\"({id})\" class=\"animated-emoticon-{side}-{id}\" itemscope=\"\"><img itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Emoji\" itemid=\"{id}\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/{id}/default/\
         20_f.png\" alt=\"{alt}\" style=\"width:{side}px; height:{side}px\"></span>"
    )
}

/// b's two messages in the a–b–c chat as the ones Teams shows: a status line, a pasted picture of a dashboard (made up,
/// served by the mock as AMS would) and two lines, the last ending in an emoticon; then a wave alone.
fn seed_pictures(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut png = Vec::new();
    made_up_dashboard().write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{PICTURE_OBJECT}/views/imgo"), "image/png", png));
    let html = format!(
        "<p>Made-up board summary for the lab week:</p>\r\n\
         <p><img src=\"https://us-api.asm.skype.com/v1/objects/{PICTURE_OBJECT}/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"884\" height=\"228\" alt=\"image\" \
         id=\"x_{PICTURE_OBJECT}\" itemid=\"{PICTURE_OBJECT}\"></p>\r\n\
         <p>12 made-up cards moved</p>\r\n<p>7 made-up cards added{}</p>",
        emoticon("speechless", "\u{1f610}", 20)
    );
    mock.post_as(LAB_B, ABC_CHAT, &html).ok_or("b's picture refused")?;
    let wave = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    mock.post_as(LAB_B, ABC_CHAT, &wave).ok_or("b's wave refused")?;
    let mut gif = Vec::new();
    write_made_up_gif(&mut gif)?;
    runtime.block_on(mock.serve_bytes(&format!("/media/{GIF_ID}/giphy.gif"), "image/gif", gif));
    // as the web client sends one picked from its GIF picker: between two blank paragraphs, on a shard of Giphy's host
    let picked = format!(
        "<p>&nbsp;</p>\n<img src=\"https://media2.giphy.com/media/v1.Y2lkPW1hZGUtdXA/{GIF_ID}/giphy.gif\" width=\"250\" \
         height=\"200\" alt=\"Made Up News GIF (GIF Image)\" style=\"height:auto; margin-top:4px; max-width:100%\">\n\
         <p>&nbsp;</p>"
    );
    mock.post_as(LAB_C, ABC_CHAT, &picked).ok_or("c's GIF refused")?;
    Ok(())
}

/// The made-up pictures x's mixed message shows: their objects, and the sizes the message gives them (half their
/// pixels', as a picture pasted from a screen of twice the density is given).
const MIXED_PICTURES: [(&str, (u32, u32)); 2] = [
    ("0-eus-d9-00000000000000000000000000000ab1", (198, 53)),
    ("0-eus-d9-00000000000000000000000000000ab2", (535, 83)),
];

/// x's message in the a–x chat in the shape of one Teams shows (made up): a line, a small picture, a line, a wider
/// picture, a line, a block of code as Teams' code block editor sends one (its own paragraph, `pre` with its language,
/// lines split by `<br>`, highlight.js spans), a last line.
fn seed_mixed(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut pictures = Vec::new();
    for (object, (width, height)) in MIXED_PICTURES {
        let mut png = Vec::new();
        made_up_panel(width * 2, height * 2).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{object}/views/imgo"), "image/png", png));
        pictures.push(format!(
            "<p><img src=\"https://us-api.asm.skype.com/v1/objects/{object}/views/imgo\" \
             itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" width=\"{width}\" height=\"{height}\" \
             alt=\"image\" id=\"x_{object}\" itemid=\"{object}\"></p>"
        ));
    }
    let key = |name: &str| format!("<span class=\"hljs-function\">&quot;{name}&quot;</span>");
    let html = format!(
        "<p>The service never sends a proper error, it comes over the socket like this</p>\n{}\n\
         <p>Which is odd already. The socket's tab shows it like this</p>\n{}\n\
         <p>and through the client library it arrives as this object:</p>\n<p></p>\n\
         <p itemtype=\"http://schema.skype.com/CodeBlockEditor\" id=\"x_codeBlockEditor-made-up\">\n&nbsp;</p>\n\
         <pre class=\"language-json language-was-manually-selected\" itemid=\"codeBlockEditor-made-up\"><code>{{<br> \
         &nbsp;&nbsp; {}:&nbsp;<span class=\"hljs-string\">&quot;error&quot;</span>,<br> &nbsp;&nbsp; {}:&nbsp;\
         <span class=\"hljs-keyword\">null</span>,<br> &nbsp;&nbsp; {}:&nbsp;<span class=\"hljs-string\">\
         &quot;Made&nbsp;up&nbsp;limit&nbsp;reached.&quot;</span><br>}}</code></pre>\n<p></p>\n\
         <p>without any request id at all</p>\n<p>&nbsp;</p>",
        pictures[0],
        pictures[1],
        key("type"),
        key("detail"),
        key("error"),
    );
    mock.post_as(OUTSIDER, AX_CHAT, &html).ok_or("x's message refused")?;
    Ok(())
}

/// A made-up panel, `width` by `height`: a dark strip over a blue band with a light bar in it.
fn made_up_panel(width: u32, height: u32) -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(width, height, image::Rgba([30, 30, 30, 255]));
    for y in height / 3..height {
        for x in 0..width {
            let colour = if y < height * 2 / 3 && x > width / 10 && x < width / 2 {
                [235, 235, 235, 255]
            } else {
                [20, 80, 160, 255]
            };
            picture.put_pixel(x, y, image::Rgba(colour));
        }
    }
    picture
}

/// The made-up GIF c's message names on Giphy (served by the mock as Giphy's media host would).
const GIF_ID: &str = "MadeUpGif000";

/// A made-up animated GIF, 250 by 200: a red banner on dark blue, a light bar sweeping across it in six frames.
fn write_made_up_gif(out: &mut Vec<u8>) -> SandboxResult {
    let mut encoder = image::codecs::gif::GifEncoder::new(out);
    encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
    for at in 0u32..6 {
        let mut frame = image::RgbaImage::from_pixel(250, 200, image::Rgba([20, 30, 70, 255]));
        for y in 120..170 {
            for x in 0..250 {
                frame.put_pixel(x, y, image::Rgba([200, 30, 40, 255]));
            }
        }
        for y in 135..155 {
            for x in (at * 40)..(at * 40 + 60).min(250) {
                frame.put_pixel(x, y, image::Rgba([245, 245, 245, 255]));
            }
        }
        let delay = image::Delay::from_numer_denom_ms(150, 1);
        encoder.encode_frame(image::Frame::from_parts(frame, 0, 0, delay))?;
    }
    Ok(())
}

/// A made-up dashboard, 884 by 228: four cards on black, each with a coloured figure.
fn made_up_dashboard() -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(884, 228, image::Rgba([12, 12, 12, 255]));
    let mut fill = |x0: u32, y0: u32, width: u32, height: u32, colour: [u8; 3]| {
        for y in y0..(y0 + height).min(228) {
            for x in x0..(x0 + width).min(884) {
                picture.put_pixel(x, y, image::Rgba([colour[0], colour[1], colour[2], 255]));
            }
        }
    };
    // the title and the subtitle
    fill(16, 14, 250, 16, [235, 235, 235]);
    fill(16, 40, 520, 8, [120, 120, 120]);
    let figures = [[74, 222, 128], [59, 130, 246], [245, 245, 245], [245, 245, 245]];
    for (at, figure) in (0u32..).zip(figures) {
        let x = 16 + at * 214;
        fill(x, 72, 200, 140, [44, 44, 44]);
        fill(x + 2, 74, 196, 136, [22, 22, 22]);
        fill(x + 16, 92, 80, 6, [150, 150, 150]);
        fill(x + 16, 112, 60, 28, figure);
        fill(x + 16, 156, 150, 6, [110, 110, 110]);
        fill(x + 16, 170, 110, 6, [110, 110, 110]);
    }
    picture
}

/// A made-up file c shares in the a–c chat: its name, its extension, its sharing link's kind (`v` a video, `b` a
/// document) and id, and for a video the colours its made-up thumbnail is drawn in.
struct MadeUpFile {
    name: &'static str,
    extension: &'static str,
    kind: char,
    share: &'static str,
    frame: Option<([u8; 3], [u8; 3])>,
}

const MADE_UP_FILES: [MadeUpFile; 3] = [
    MadeUpFile {
        name: "06-made-up-rehearsal-multicam-edit-v2.mp4",
        extension: "mp4",
        kind: 'v',
        share: "MadeUpShareLinkMulticam0001",
        frame: Some(([24, 120, 40], [170, 230, 120])),
    },
    MadeUpFile {
        name: "04-made-up-rehearsal-three-cameras.mp4",
        extension: "mp4",
        kind: 'v',
        share: "MadeUpShareLinkCameras0002",
        frame: Some(([14, 18, 32], [60, 90, 200])),
    },
    MadeUpFile {
        name: "made-up-stage-plan.pdf",
        extension: "pdf",
        kind: 'b',
        share: "MadeUpShareLinkStagePlan0003",
        frame: None,
    },
];

/// A file's entry in `properties.files`, in the shape `tests/fixtures/chatsvc/files.json` has (made up).
fn file_entry(file: &MadeUpFile, at: usize) -> serde_json::Value {
    let site = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
    let object = format!("{site}Documents/Microsoft Teams Chat Files/{}", file.name);
    serde_json::json!({
        "itemid": format!("made-up-item-{at}"),
        "fileName": file.name,
        "fileType": file.extension,
        "fileInfo": {
            "fileUrl": object,
            "siteUrl": site,
            "shareUrl": share_url(file),
        },
        "@type": "http://schema.skype.com/File",
        "version": 2,
        "objectUrl": object,
        "type": file.extension,
        "title": file.name,
        "state": "active",
        "filePreview": {"previewUrl": "", "previewHeight": 0, "previewWidth": 0},
    })
}

fn share_url(file: &MadeUpFile) -> String {
    format!(
        "https://contoso-my.sharepoint.com/:{}:/g/personal/made_up_user_contoso_test/{}",
        file.kind, file.share
    )
}

/// c's message in the a–c chat as the one Teams shows with two videos: lines broken by `<br>` and a line feed each,
/// then two videos and a document. Graph's thumbnail list and each video's made-up picture are served by the mock (the
/// picture's link on the media host, as Graph hands one out).
fn seed_files(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let html = "<p>Made-up lab note: a first line long enough to wrap across the chat view a couple of times over, \
                (with a made-up aside)<br>\n<br>\nThe list below:<br>\n1. The first made-up item, which runs on a \
                little.<br>\n2. The second made-up item.<br>\n3. The third made-up item.\
                <br>\n<br>\nThe stage plan is attached too</p>";
    let id = mock.post_as(LAB_C, AC_CHAT, html).ok_or("c's post refused")?;
    let entries: Vec<serde_json::Value> = (0..)
        .zip(&MADE_UP_FILES)
        .map(|(at, file)| file_entry(file, at))
        .collect();
    let files = serde_json::Value::Array(entries).to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AC_CHAT, &client_message_id, |message| {
        message["properties"]["files"] = serde_json::json!(files);
    });
    if !changed {
        return Err("c's post isn't in the mock".into());
    }
    for (at, file) in MADE_UP_FILES.iter().enumerate() {
        let Some((dark, light)) = file.frame else { continue };
        let read = teams_proto::FileThumbnails::of(&share_url(file)).ok_or("a made-up link no read names")?;
        let token = read.share();
        let picture = format!("/transform/thumbnail/made-up-{at}");
        let list = serde_json::json!({"value": [{"id": "0", "large": {
            "height": 450, "width": 800,
            "url": format!("https://westus1-mediap.svc.ms{picture}?provider=spo&docid=made-up"),
        }}]});
        runtime.block_on(mock.serve_bytes(
            &format!("/v1.0/shares/{token}/driveItem/thumbnails"),
            "application/json",
            list.to_string().into_bytes(),
        ));
        let mut png = Vec::new();
        made_up_frame(dark, light).write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
        runtime.block_on(mock.serve_bytes(&picture, "image/png", png));
        // the file's item with its download link on the sharer's site, and the file there, read by ranges
        let download = "/personal/made_up_user_contoso_test/_layouts/15/download.aspx";
        let item = serde_json::json!({
            "id": format!("made-up-item-{at}"),
            "name": file.name,
            "size": MADE_UP_VIDEO.len(),
            "@microsoft.graph.downloadUrl": format!(
                "https://contoso-my.sharepoint.com{download}?UniqueId=made-up-{at}&tempauth=made-up"
            ),
        });
        runtime.block_on(mock.serve_bytes(
            &format!("/v1.0/shares/{token}/driveItem"),
            "application/json",
            item.to_string().into_bytes(),
        ));
        runtime.block_on(mock.serve_ranged(&download, "video/mp4", MADE_UP_VIDEO.to_vec()));
    }
    Ok(())
}

/// The made-up video every video card plays: 2 s of FFmpeg's test pattern and a tone (teams-media's test input).
const MADE_UP_VIDEO: &[u8] = include_bytes!("../../teams-media/tests/fixtures/media/made-up-test-pattern.mp4");

/// A made-up video frame, 800 by 450: a gradient from `dark` to `light` with a few bars, a figure and a caption strip.
fn made_up_frame(dark: [u8; 3], light: [u8; 3]) -> image::RgbaImage {
    let (width, height) = (800_u32, 450_u32);
    image::RgbaImage::from_fn(width, height, |x, y| {
        let mix = |from: u8, to: u8| {
            let t = (x + y) * 255 / (width + height);
            u8::try_from((u32::from(from) * (255 - t) + u32::from(to) * t) / 255).unwrap_or(to)
        };
        let figure = (300..380).contains(&x) && y > 120 && y < 420;
        let caption = y > 410 && x < 260;
        let bars = y > 330 && x % 90 < 30;
        if caption {
            image::Rgba([20, 20, 20, 255])
        } else if figure {
            image::Rgba([10, 10, 10, 255])
        } else if bars {
            image::Rgba([mix(dark[0], 40), mix(dark[1], 40), mix(dark[2], 40), 255])
        } else {
            image::Rgba([
                mix(dark[0], light[0]),
                mix(dark[1], light[1]),
                mix(dark[2], light[2]),
                255,
            ])
        }
    })
}

/// The made-up picture b sent and the outsider forwarded, its object on AMS.
const FORWARDED_OBJECT: &str = "0-eus-d9-0000000000000000000000000000f0d0";

/// Two of b's messages in the a–b–c chat, forwarded to a by the outsider in their 1:1 chat as Teams forwards them
/// (`docs/protocol/chat.md`, "Forwarded messages"): a note with a link, and a picture (a made-up phone screenshot,
/// served by the mock as AMS would) over a line.
fn seed_forwards(mock: &ChatServiceMock, runtime: &tokio::runtime::Runtime) -> SandboxResult {
    let mut png = Vec::new();
    made_up_phone().write_to(&mut io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    runtime.block_on(mock.serve_bytes(&format!("/v1/objects/{FORWARDED_OBJECT}/views/imgo"), "image/png", png));
    let link = "https://example.com/made-up-assets";
    let note = format!(
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p>Hey, here is the made-up link. It was mentioned \
         in the made-up review, as a stand-in for a forwarded note that runs over a line</p>\r\n<p><a href=\
         \"{link}\" title=\"{link}\">{link}</a></p>\r\n</blockquote>"
    );
    let picture = format!(
        "<blockquote itemtype=\"http://schema.skype.com/Forward\">\r\n<p><img src=\"https://us-api.asm.skype.com/v1/\
         objects/{FORWARDED_OBJECT}/views/imgo\" itemtype=\"http://schema.skype.com/AMSImage\" itemscope=\"png\" \
         width=\"127.40112994350282\" height=\"250\" alt=\"image\" id=\"x_x_{FORWARDED_OBJECT}\" \
         itemid=\"{FORWARDED_OBJECT}\"></p>\r\n\r\n\r\n<p>The made-up picture is here, forwarded with a note that runs a \
         little long too</p>\r\n</blockquote>"
    );
    for (html, sent, original) in [
        (note, "2026-01-01T02:49:12.345Z", 1_767_235_752_345_u64),
        (picture, "2026-01-01T17:36:47.526Z", 1_767_289_007_526),
    ] {
        let id = mock
            .post_as(OUTSIDER, AX_CHAT, &html)
            .ok_or("the outsider's forward refused")?;
        let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
        let changed = mock.change_message(AX_CHAT, &client_message_id, |message| {
            message["properties"]["forwardTemplateId"] = serde_json::json!("basic_forward_message_template");
            message["properties"]["originalMessageContext"] = serde_json::json!({
                "originalSender": LAB_B, "originalSentTime": sent, "messageId": original,
                "originalThreadId": ABC_CHAT, "threadType": "chat", "messageType": "RichText/Html",
            });
        });
        if !changed {
            return Err("the outsider's forward isn't in the mock".into());
        }
    }
    Ok(())
}

/// A made-up phone screenshot, 254 by 500: a dark frame round a white screen with a title bar, two round badges and a
/// band of sea at the bottom.
fn made_up_phone() -> image::RgbaImage {
    let mut picture = image::RgbaImage::from_pixel(254, 500, image::Rgba([250, 250, 250, 255]));
    let mut fill = |x0: u32, y0: u32, width: u32, height: u32, colour: [u8; 3]| {
        for y in y0..(y0 + height).min(500) {
            for x in x0..(x0 + width).min(254) {
                picture.put_pixel(x, y, image::Rgba([colour[0], colour[1], colour[2], 255]));
            }
        }
    };
    let frame = [24, 24, 28];
    fill(0, 0, 254, 14, frame);
    fill(0, 486, 254, 14, frame);
    fill(0, 0, 14, 500, frame);
    fill(240, 0, 14, 500, frame);
    fill(40, 70, 174, 26, [30, 30, 30]);
    fill(118, 64, 18, 38, [240, 140, 30]);
    fill(44, 170, 70, 70, [230, 120, 150]);
    fill(140, 170, 70, 70, [120, 160, 230]);
    fill(94, 300, 66, 80, [200, 200, 210]);
    fill(14, 400, 226, 50, [90, 200, 210]);
    picture
}

/// a's own message in the a–b chat, before b's, with a's heart on it: a's hover bar, and a's most used reactions
/// (the heart, and the eyes on b's) first in every hover bar.
fn seed_own(mock: &ChatServiceMock) -> SandboxResult {
    let id = mock
        .post_as(LAB_A, AB_CHAT, "<p>Sure, I can look at it this afternoon</p>")
        .ok_or("a's post refused")?;
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["emotions"] = serde_json::json!([{
            "key": "heart",
            "users": [{"mri": LAB_A, "time": 1_767_268_860_000_i64, "value": "1767268860000"}],
        }]);
    });
    if !changed {
        return Err("a's post isn't in the mock".into());
    }
    Ok(())
}

/// b's message to a in the a–b chat as the one Teams shows: a mention of a and one of c in its text, and a's own
/// reaction on it.
fn seed_mentions(mock: &ChatServiceMock) -> SandboxResult {
    let mention = |id: u32, name: &str| {
        format!("<span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"{id}\">{name}</span>")
    };
    let html = format!(
        "<p>{} can you check the made-up report with {}? It is a long message written only for this tour, so that it \
         wraps onto a second line and a third. Nothing in it is about anyone, it just fills the bubble with enough \
         words to show how a long message reads with two mentions at the start of it, ending right about here</p>",
        mention(0, "Lab A"),
        mention(1, "Lab C")
    );
    let id = mock.post_as(LAB_B, AB_CHAT, &html).ok_or("b's post refused")?;
    let entry = |id: u32, mri: &str, name: &str| {
        serde_json::json!({
            "@type": "http://schema.skype.com/Mention", "itemid": id, "mri": mri, "mentionType": "person",
            "displayName": name,
        })
    };
    let mentions = serde_json::json!([entry(0, LAB_A, "Lab A"), entry(1, LAB_C, "Lab C")]).to_string();
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["mentions"] = serde_json::json!(mentions);
        message["properties"]["emotions"] = serde_json::json!([{
            "key": "1f440_eyes",
            "users": [{"mri": LAB_A, "time": 1_767_268_800_000_i64, "value": "1767268800000"}],
        }]);
    });
    if !changed {
        return Err("b's post isn't in the mock".into());
    }
    Ok(())
}

fn child_window() -> SandboxResult {
    teams_app::guard::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let (lab, engine) = MockLab::start()?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: LaunchAccount::bare(Alias::A, engine),
                others: Vec::new(),
                trace,
                settings,
                // the sandbox's private bus, with its fake notification server and tray watcher
                bus: teams_ui::bus::address_from_env(),
                diagnostics: None,
                // the made-up video plays silently
                playback: teams_media::player::PlayerSetup::default(),
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
                walk(window, cx);
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
    drop(lab);
    Ok(())
}
