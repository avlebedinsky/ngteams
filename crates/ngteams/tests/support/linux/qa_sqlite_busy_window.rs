//! QA of task 236 (round 1), in the UI sandbox: Dana chats with Erin in the app's window on an own server that keeps
//! its data in SQLite, while the operator runs `user add`, `user disable`, `user reset-password` and `user enable`
//! against the same file. The commands and the chat must all succeed.
//!
//! The server is the real `ngteams-server` binary (`NGTEAMS_QA_SERVER_BIN`, else the one `cargo build -p
//! ngteams-server` left in the target directory), run as an operator runs it. Every account is synthetic. The real
//! app runs as a child in the sandbox; the parent types with the sandbox's virtual keyboard and reads what the window
//! holds through a file in the sandbox dir (`harness = false`: GPUI runs on the process's main thread). Erin is a
//! second engine in the child, with no window.
//!
//! `QA_SQB_EVIDENCE` names a directory that gets the screenshots, the server log and the results too. Screenshots
//! also go to `target/ui-shots/qa236-*.png`. Without the binary, weston 15, niri or `dbus-daemon` it says so and
//! passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts_file::{self, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::setup::Setup;
#[path = "../own_server_binary.rs"]
#[expect(dead_code, reason = "the shared binary harness has helpers other QA tests use")]
mod own_server_binary;
#[path = "../window.rs"]
#[expect(dead_code, reason = "the shared window harness has helpers other tests use")]
mod support;
use own_server_binary::{BinaryServer, PASSWORD};
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_core::CallFloor;
use teams_core::engine::{Action, EngineHandle};
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "QA_SQB_CHILD";
const EVIDENCE_ENV: &str = "QA_SQB_EVIDENCE";
const ORIGIN_ENV: &str = "QA_SQB_ORIGIN";
const CA_ENV: &str = "QA_SQB_CA";
const DIR_ENV: &str = "QA_SQB_DIR";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa236-ask";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
const SETTLE: Duration = Duration::from_millis(800);
const STEP: Duration = Duration::from_millis(250);

const DANA: (&str, &str) = ("dana@own.example.test", "Dana Made-Up");
const ERIN: (&str, &str) = ("erin@own.example.test", "Erin Made-Up");
/// Synthetic accounts the operator disables, enables and gives new passwords while Dana chats.
const SEEDED: usize = 8;
/// Messages each way while the operator works.
const ROUNDS: usize = 4;
/// The fewest operator commands that must run beside the chat.
const MIN_COMMANDS: usize = 12;
const NEW_PASSWORD: &str = "another horse battery";

/// The compose box of a chat on the 1280x800 screen.
const COMPOSE: (u32, u32) = (700, 750);
/// Where the pointer rests when it must not hover anything.
const PARK: (u32, u32) = (300, 23);

pub(super) fn main() -> ExitCode {
    let outcome = if std::env::var_os(CHILD_ENV).is_some() {
        child()
    } else {
        parent()
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_sqlite_busy_window: {error}");
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

fn evidence() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

// --- the parent ---

/// One operator command: what it was, whether it succeeded, what it said on failure, how long it took.
type Ran = (String, bool, String, Duration);

fn parent() -> SandboxResult {
    let Some(binary) = own_server_binary::binary()? else {
        let why = "no ngteams-server binary (cargo build -p ngteams-server)";
        if std::env::var("GPUI_SMOKE_REQUIRE").as_deref() == Ok("1") {
            return Err(why.into());
        }
        say(&format!("qa_sqlite_busy_window: skipped: {why}"));
        return Ok(());
    };
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_sqlite_busy_window", &options)? else {
        return Ok(());
    };
    let server = BinaryServer::start(&binary)?;
    for (upn, name) in [DANA, ERIN] {
        server.add_user(upn, name)?;
    }
    let seeded: Vec<String> = (0..SEEDED)
        .map(|n| format!("qa236-seed-{n}@own.example.test"))
        .collect();
    for (n, upn) in seeded.iter().enumerate() {
        server.add_user(upn, &format!("Seeded Person {n}"))?;
    }
    let dir = tempfile::tempdir()?;
    let ca = server.ca_file.to_string_lossy().into_owned();
    let data = dir.path().to_string_lossy().into_owned();
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "1"),
            (ORIGIN_ENV, &server.origin),
            (CA_ENV, &ca),
            (DIR_ENV, &data),
        ],
    )?;
    let mut user = User::start(&sandbox, Running::new(child)?)?;
    let outcome = busy_chat(&mut user, &server, &seeded);
    if outcome.is_err() {
        let _ = user.shot("failed");
    }
    let quit = user.quit();
    if let Some(dir) = evidence() {
        let _ = fs::write(dir.join("own-server.log"), server.log());
    }
    let ran = outcome?;
    let mut summary = String::new();
    for (what, done, said, took) in &ran {
        let _ = writeln!(
            summary,
            "{took:>10.3?} {} `{what}` {said}",
            if *done { "ok" } else { "FAILED" }
        );
    }
    say(&summary);
    if let Some(dir) = evidence() {
        let _ = fs::write(dir.join("operator-commands.txt"), &summary);
    }
    quit?;
    let failed: Vec<&Ran> = ran.iter().filter(|(_, done, _, _)| !done).collect();
    ensure(failed.is_empty(), || format!("operator commands failed: {failed:?}"))?;
    ensure(ran.len() >= MIN_COMMANDS, || format!("only {} commands ran", ran.len()))?;
    let users = server.users()?;
    for upn in &seeded {
        ensure(
            users.iter().any(|(name, _, enabled, _)| name == upn && *enabled),
            || format!("{upn} is not enabled again: {users:?}"),
        )?;
    }
    say(&format!(
        "qa_sqlite_busy_window: passed, {} operator commands",
        ran.len()
    ));
    Ok(())
}

/// The chat in the window, each way, while a thread runs the operator's commands against the server's file.
fn busy_chat(user: &mut User<'_>, server: &BinaryServer, seeded: &[String]) -> SandboxResult<Vec<Ran>> {
    let state = user.until("Erin's chat open", |state| state["chat"]["id"].is_string())?;
    let chat = state["chat"]["id"].as_str().unwrap_or_default().to_owned();
    user.shot("chat-before-light")?;
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let operator = scope.spawn(|| operator(server, seeded, &stop));
        let chatted = chat_rounds(user, &chat);
        stop.store(true, Ordering::Relaxed);
        let ran = operator.join().map_err(|_| "the operator's thread panicked")?;
        chatted?;
        user.shots_in_both_themes("chat-after")?;
        Ok(ran)
    })
}

fn chat_rounds(user: &mut User<'_>, chat: &str) -> SandboxResult {
    for n in 0..ROUNDS {
        let mine = format!("QA synthetic note {n} while the operator works");
        user.click(COMPOSE)?;
        user.type_text(&mine)?;
        user.press("enter")?;
        user.until(&format!("note {n} sent"), |state| {
            preview(state, chat).contains(&mine) && state["chat"]["pending"] == 0 && state["chat"]["failed"] == 0
        })?;
        let theirs = format!("QA synthetic reply {n} from Erin");
        let said = user.ask(&format!("erin-says {theirs}"))?;
        ensure(said == "ok", || format!("Erin's send: {said}"))?;
        user.until(&format!("Erin's reply {n} shown"), |state| {
            preview(state, chat).contains(&theirs) && state["chat"]["messages"].as_u64() >= Some(2 * n as u64 + 3)
        })?;
    }
    Ok(())
}

fn preview(state: &Value, chat: &str) -> String {
    state["previews"][chat].as_str().unwrap_or_default().to_owned()
}

/// The operator: `user add`, `user disable`, `user reset-password` and `user enable` in turn until `stop`, and at
/// least [`MIN_COMMANDS`].
fn operator(server: &BinaryServer, seeded: &[String], stop: &AtomicBool) -> Vec<Ran> {
    let mut ran = Vec::new();
    let mut n = 0;
    while !stop.load(Ordering::Relaxed) || ran.len() < MIN_COMMANDS {
        let target = &seeded[n % seeded.len()];
        let added = format!("qa236-added-{n}@own.example.test");
        let commands: [(Vec<&str>, Option<&str>); 4] = [
            (vec!["user", "add", &added, "--name", "Added Person"], Some(PASSWORD)),
            (vec!["user", "disable", target], None),
            (vec!["user", "reset-password", target], Some(NEW_PASSWORD)),
            (vec!["user", "enable", target], None),
        ];
        for (args, input) in commands {
            let started = Instant::now();
            let outcome = server.run(&args, input);
            let took = started.elapsed();
            let (done, said) = match outcome {
                Ok(_) => (true, String::new()),
                Err(error) => (false, error.to_string()),
            };
            ran.push((args.join(" "), done, said, took));
        }
        n += 1;
    }
    ran
}

/// The user at the sandbox: the keyboard, the screenshots, and the child it asks about the window.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
}

impl<'a> User<'a> {
    fn start(sandbox: &'a Sandbox, running: Running) -> SandboxResult<Self> {
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            let mut running = running;
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        let mut user = Self {
            sandbox,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
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

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(PARK.0, PARK.1)
    }

    /// A screenshot as `qa236-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        let file = format!("qa236-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), std::path::Path::to_owned)
            .join("ui-shots");
        fs::create_dir_all(&dir)?;
        let kept = dir.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// The same screen in Teams Light and Teams Dark (`<name>-light`, `<name>-dark`); Teams Light after.
    fn shots_in_both_themes(&mut self, name: &str) -> SandboxResult {
        self.ask("theme-light")?;
        self.shot(&format!("{name}-light"))?;
        self.ask("theme-dark")?;
        self.shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
        Ok(())
    }
}

// --- the child: Dana's app on the own server, and Erin's engine ---

fn env(name: &str) -> SandboxResult<String> {
    std::env::var(name).map_err(|_| format!("{name} unset").into())
}

/// An account of the own server signed in and its engine started, in `dir`.
fn signed_in(
    dir: &std::path::Path,
    upn: &str,
    server: &accounts_file::OwnServer,
    push: bool,
    runtime: &tokio::runtime::Runtime,
) -> SandboxResult<(ngteams::accounts::Started<MemorySecretStore>, Setup)> {
    let paths = PersonalPaths::new(dir.join("config"), dir.join("data"));
    let setup = Setup::new(paths)?;
    let store = MemorySecretStore::new();
    let account = runtime.block_on(ngteams::signin::add_own(
        &setup,
        upn,
        server,
        &Secret::new(PASSWORD),
        &store,
    ))?;
    let engine = ngteams::engine::start(&setup.paths, &account, CallFloor::new(), store, push, None)?;
    Ok((
        ngteams::accounts::Started {
            account,
            engine,
            calls: None,
        },
        setup,
    ))
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = PathBuf::from(env(DIR_ENV)?);
    let server = accounts_file::OwnServer::parse(&env(ORIGIN_ENV)?, &env(CA_ENV)?)?;
    let runtime = ngteams::accounts::runtime()?;
    let (dana, _) = signed_in(&dir.join("dana"), DANA.0, &server, true, &runtime)?;
    let (erin, _) = signed_in(&dir.join("erin"), ERIN.0, &server, false, &runtime)?;
    let erin_mri = erin
        .account
        .oid
        .clone()
        .map(|oid| format!("8:orgid:{oid}"))
        .ok_or("Erin has no object id")?;
    let made = runtime.block_on(dana.engine.handle.write(Action::CreateChat {
        others: vec![Mri::new(erin_mri)],
        topic: None,
    }))?;
    let chat = made.message_id.ok_or("no chat made")?;
    runtime.block_on(dana.engine.handle.write(Action::Send {
        conversation: chat.clone(),
        text: MessageText::plain("Hello Erin, a made-up hello")?,
    }))?;
    let erin = std::rc::Rc::new(erin);
    let runner = support::runner(&dir)?;
    let settings = teams_ui::SettingsFile::new(dir.join("settings.toml"));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: dana,
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
                let window = launched.window;
                let _ = window.update(cx, |kit, window, cx| {
                    if let Some(shell) = shell_of(kit, cx) {
                        shell.update(cx, |shell, cx| shell.open_chat(chat.clone(), window, cx));
                    }
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, erin, chat, cx);
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
    drop(runtime);
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

fn shell_of(kit: &KitRoot, cx: &App) -> Option<Entity<Shell>> {
    kit.view()
        .clone()
        .downcast::<Root>()
        .ok()?
        .read(cx)
        .shell_in(cx)
        .cloned()
}

/// Erin's account, signed in in the child with no window.
type Erin = std::rc::Rc<ngteams::accounts::Started<MemorySecretStore>>;

fn answer(window: WindowHandle<KitRoot>, erin: Erin, chat: String, cx: &mut App) {
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
            let reply = if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| cx.quit());
                return;
            } else if let Some(text) = verb.strip_prefix("erin-says ") {
                erin_says(&erin.engine.handle, &chat, text).await
            } else {
                window
                    .update(cx, |kit, window, cx| {
                        let shell = shell_of(kit, cx)?;
                        Some(on_shell(&shell, verb, window, cx))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "no shell".to_owned())
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

/// Erin's engine sends `text` to `chat`, the chat Erin has with Dana.
async fn erin_says(erin: &EngineHandle, chat: &str, text: &str) -> String {
    let Ok(message) = MessageText::plain(text) else {
        return "bad text".to_owned();
    };
    match erin.sync().await {
        Ok(report) if report.failed.is_empty() => {}
        Ok(report) => return format!("Erin's sync: {report:?}"),
        Err(error) => return format!("Erin's sync failed: {error:?}"),
    }
    match erin
        .write(Action::Send {
            conversation: chat.to_owned(),
            text: message,
        })
        .await
    {
        Ok(row) if row.state == teams_store::OutboxState::Sent => "ok".to_owned(),
        Ok(row) => format!("not sent: {:?}", row.state),
        Err(error) => format!("failed: {error:?}"),
    }
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "state" => state_of(shell, cx).to_string(),
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
            let _ = window;
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

fn state_of(shell: &Entity<Shell>, cx: &App) -> Value {
    let shell_ref = shell.read(cx);
    let chat = shell_ref.chat().map(|view| {
        let view = view.read(cx);
        json!({
            "id": view.conversation(),
            "messages": view.message_items().count(),
            "pending": view.pending_items().count(),
            "failed": view.message_items().filter(|item| !item.outbox.is_empty()).count(),
        })
    });
    let list = shell_ref.list().read(cx);
    let previews: serde_json::Map<String, Value> = list
        .rows()
        .iter()
        .map(|row| (row.id.clone(), json!(row.preview.to_string())))
        .collect();
    json!({
        "main": format!("{:?}", shell_ref.main()),
        "chat": chat,
        "previews": previews,
    })
}
