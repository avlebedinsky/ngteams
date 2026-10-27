//! A look at the app's views in the UI sandbox, on the mock (`harness = false`: GPUI runs on the process's main
//! thread): the screenshots a change of the UI is checked by, kept under `target/ui-shots/tour-*.png`.
//!
//! The test runs itself again inside the sandbox ([`Sandbox::spawn_with_fds`], only the view trace's pipe passed
//! through) as the app of lab account a, its engine on the mock chat service and its Trouter (`teams-mock`, through
//! the `test-support` mock origins, a test roster and log). Once the window is drawn and the chats are listed, the
//! child walks the views one step at a time (no chat open; the a–b chat with a message selected and its actions
//! shown; a reply begun; the account menu; every page of the settings, Calls with made-up devices; the appearance
//! and the chat again in Teams Dark) and says `shot <name>` after each; the parent screenshots the window and lets it
//! go on. The views are driven from inside the app, never by screen coordinates.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use teams_app::app::{self, Launch, LaunchAccount};
use teams_app::trace::adopt_trace;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::lab::display_name;
use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, Mri, Roster};
use teams_media::devices::catalog::{DeviceInfo, DeviceKind, DeviceList, FixedDirectory};
use teams_mock::{AB_CHAT, ABC_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock, lab_toml};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Item, MessageAction};
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
const STEPS: [Step; 13] = [
    ("no-chat", |_, _, _| {}),
    ("chat", |shell, window, cx| {
        shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
    }),
    ("chat-actions", select_last_message),
    ("chat-reply", |shell, window, cx| {
        if let Some((chat, message)) = last_message(shell, cx) {
            chat.update(cx, |chat, cx| chat.act(MessageAction::Reply, &message, window, cx));
        }
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
        // the mock's engine has no calls: made-up devices give the settings their Calls page
        let model = shell.read(cx).settings().clone();
        teams_ui::settings::attach_devices(&model, Arc::new(FixedDirectory::new(made_up_devices())), cx);
        show_page(shell, Page::Calls, cx);
    }),
    ("settings-diagnostics", |shell, _, cx| {
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
];

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
        let png = sandbox.screenshot(&file, true)?;
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
            client: Arc::new(Client::new(http()?)),
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
