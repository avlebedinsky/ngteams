//! The child of the account picture QA: the real app (`ngteams::app::launch`) in the sandbox with mock accounts a, b
//! and c, each on the mock's chat service and Trouter. The mock's Graph gives each account named in the mode the same
//! profile picture ([`super::portal::PROFILE`]). The settings file is in the parent's directory, so the pictures the
//! user chose stay for the next run of the app there.
//!
//! The child answers the parent's questions through a file in the sandbox dir (`<n> <verb>`; the answer is
//! `re <n> <answer>` on stdout): the state of the accounts and of the shown account's shell, and a few things a test
//! can't do with the pointer (a theme, a font size, a message that b posts on the mock).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use std::{fs, future, io};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use ngteams::setup::Setup;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::{AccessToken, Renewal, Session};
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::accounts::Accounts;
use teams_ui::desktop::Desktop;
use teams_ui::settings_view::Page;
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::SandboxResult;

/// The child's mode: the accounts (`abc`, `ac`, …), then `:` and the accounts the mock gives the profile picture.
pub const CHILD_ENV: &str = "QA_AP_CHILD";
/// The directory of the app's files, kept by the parent from one run to the next.
pub const ROOT_ENV: &str = "QA_AP_ROOT";
/// The file in the sandbox dir the parent writes a question to.
pub const ASK: &str = "qa-ap-ask";

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn mri_of(letter: char) -> Option<&'static str> {
    match letter {
        'a' => Some(LAB_A),
        'b' => Some(LAB_B),
        'c' => Some(LAB_C),
        _ => None,
    }
}

/// The mock account `letter` (its MRI `mri`) as the app starts an account, its engine `handle`. Its session serves only
/// the sign-out watch: it reaches only loopback, with an empty secret store in memory.
fn started(letter: char, mri: &str, handle: EngineHandle) -> SandboxResult<Started<MemorySecretStore>> {
    let upn = format!("bot-{letter}@lab.example.test");
    let mut account = PersonalAccount::new(
        &upn,
        mri.strip_prefix("8:orgid:").ok_or("mock orgid")?,
        teams_mock::TENANT,
    )?;
    account.name = Some(format!("bot-{letter}"));
    let session = Session::new(
        upn,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"))?,
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        Region::default(),
    );
    Ok(Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    })
}

/// The letter of the mock account `mri`.
pub fn letter_of(mri: &str) -> &'static str {
    match mri {
        LAB_A => "a",
        LAB_B => "b",
        LAB_C => "c",
        _ => "?",
    }
}

/// The settings file of the app in `root`; the pictures are beside it.
pub fn settings_file(root: &Path) -> PathBuf {
    root.join("ui").join("settings.toml")
}

pub fn child(mode: &str) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let (accounts, pictured) = mode.split_once(':').unwrap_or((mode, ""));
    let root = PathBuf::from(std::env::var(ROOT_ENV)?);
    let lab = MockLab::start(&root, pictured)?;
    let mut started = Vec::new();
    for letter in accounts.chars() {
        let mri = mri_of(letter).ok_or("unknown account")?;
        started.push(self::started(letter, mri, lab.engine(letter, mri)?)?);
    }
    if started.is_empty() {
        return Err("no account".into());
    }
    let first = started.remove(0);
    run_app(&lab, first, started)
}

fn run_app(lab: &MockLab, first: Started<MemorySecretStore>, others: Vec<Started<MemorySecretStore>>) -> SandboxResult {
    let paths = PersonalPaths::new(lab.root.join("ngteams-config"), lab.root.join("ngteams-data"));
    let runner = Runner {
        setup: Setup::new(paths)?.with_secrets(AppSecrets::in_memory()),
        floor: CallFloor::new(),
        devices: None,
    };
    let settings = teams_ui::SettingsFile::new(settings_file(&lab.root));
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
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
                let _ = launched.window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(launched.window, launched.desktop, mock, cx);
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
    say("quit");
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

fn answer(window: WindowHandle<KitRoot>, desktop: Entity<Desktop>, mock: Arc<ChatServiceMock>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(20)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            if verb == "quit" {
                say(&format!("re {seq} ok"));
                cx.update(|cx| desktop.update(cx, Desktop::quit));
                continue;
            }
            let reply = window
                .update(cx, |kit, window, cx| on_window(kit, &desktop, &mock, verb, window, cx))
                .unwrap_or_else(|_| "no window".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_window(
    kit: &KitRoot,
    desktop: &Entity<Desktop>,
    mock: &ChatServiceMock,
    verb: &str,
    window: &mut Window,
    cx: &mut App,
) -> String {
    let (head, rest) = verb.split_once(' ').unwrap_or((verb, ""));
    match head {
        "state" => state_of(kit, desktop, cx).to_string(),
        "theme" => {
            let key = if rest == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            change_settings(desktop, cx, move |settings| {
                settings.appearance.follow_system = false;
                settings.appearance.theme = key;
            });
            "ok".to_owned()
        }
        "font" => {
            let size: u16 = rest.parse().unwrap_or(teams_ui::theme::FONT_SIZE);
            change_settings(desktop, cx, move |settings| settings.appearance.font_size = size);
            "ok".to_owned()
        }
        "settings-accounts" => {
            let Some(shell) = shell_of(kit, cx) else {
                return "no shell".to_owned();
            };
            shell.update(cx, |shell, cx| shell.open_settings_page(Page::Accounts, window, cx));
            "ok".to_owned()
        }
        "open-chat" => {
            let Some(shell) = shell_of(kit, cx) else {
                return "no shell".to_owned();
            };
            shell.update(cx, |shell, cx| shell.open_chat(rest.to_owned(), window, cx));
            "ok".to_owned()
        }
        "b-posts" => match mock.post_as(LAB_B, teams_mock::AB_CHAT, &format!("<p>{rest}</p>")) {
            Some(id) => format!("posted {id}"),
            None => "refused".to_owned(),
        },
        other => format!("unknown {other}"),
    }
}

fn change_settings(desktop: &Entity<Desktop>, cx: &mut App, change: impl FnOnce(&mut teams_ui::Settings) + 'static) {
    let model = desktop.read(cx).settings().clone();
    model.update(cx, |model, cx| model.change(change, cx));
}

fn state_of(kit: &KitRoot, desktop: &Entity<Desktop>, cx: &App) -> Value {
    let accounts: &Entity<Accounts> = desktop.read(cx).accounts();
    let accounts = accounts.read(cx);
    let tiles: Vec<Value> = accounts
        .tiles()
        .iter()
        .map(|tile| {
            json!({
                "account": letter_of(&tile.key),
                "chosen": tile.chosen_picture.is_some(),
                "profile": tile.picture.is_some(),
                "problem": accounts.picture_problem(&tile.key).map(ToString::to_string),
            })
        })
        .collect();
    let shell = shell_of(kit, cx);
    let shell = shell.as_ref().map(|shell| shell.read(cx));
    json!({
        "shown": accounts.active_key().map(letter_of),
        "can_choose": accounts.can_choose_pictures(),
        "tiles": tiles,
        "menu": shell.is_some_and(Shell::status_menu_open),
        "toast": shell.and_then(|shell| shell.toast().map(ToString::to_string)),
        "main": shell.map(|shell| format!("{:?}", shell.main())),
        "chat": shell.and_then(|shell| shell.chat().map(|chat| chat.read(cx).conversation().to_owned())),
        "page": shell.map(|shell| format!("{:?}", shell.settings_view().read(cx).page())),
    })
}

// --- the mock ---

struct MockLab {
    runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
    root: PathBuf,
}

impl MockLab {
    /// The mock, its Graph giving each account of `pictured` ([`super::portal::profile_png`]) as its profile picture.
    fn start(root: &Path, pictured: &str) -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let png = super::portal::profile_png()?;
        for letter in pictured.chars() {
            let mri = mri_of(letter).ok_or("unknown account")?;
            let guid = mri.strip_prefix("8:orgid:").ok_or("mock orgid")?;
            for side in ["48x48", "64x64", "96x96", "120x120", "240x240"] {
                runtime.block_on(mock.serve_bytes(
                    &format!("/v1.0/users/{guid}/photos/{side}/$value"),
                    "image/png",
                    png.clone(),
                ));
            }
        }
        Ok(Self {
            runtime,
            mock: Arc::new(mock),
            trouter,
            root: root.to_owned(),
        })
    }

    fn engine(&self, letter: char, mri: &str) -> SandboxResult<EngineHandle> {
        let _entered = self.runtime.enter();
        let tokens = Arc::new(Tokens {
            token: self.mock.token(mri).ok_or("the mock's token")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = self.mock.origin();
        let transport = Arc::new(HttpTransport::with_mock_origin(
            Arc::clone(&tokens),
            CHAT_BASE,
            &origin,
        )?);
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let store = self.root.join("store").join(format!("{letter}.sqlite"));
        if let Some(dir) = store.parent() {
            fs::create_dir_all(dir)?;
        }
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(&store)?,
            client: Arc::new(Client::new(HttpTransport::with_mock_origin(
                Arc::clone(&tokens),
                CHAT_BASE,
                &origin,
            )?)),
            outbox: Outbox::new(transport, Author::new(Mri::new(mri), format!("bot-{letter}"))),
            push: Some(Push::new(trouter)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }
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
