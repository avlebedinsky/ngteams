//! The own server's side of the accounts window test: the server the app's child signs in to, what the child tells
//! the parent of the window asking for an account and of the requests the app made, and the screen that asks an own
//! server's account for its password again.
//!
//! **The server** is the real `ngteams-server` binary when there is one (`NGTEAMS_QA_SERVER_BIN`, else the one
//! `cargo build -p ngteams-server` left in the target directory), run as an operator runs it, its users made with
//! `user add`; without one, the same server in this process.
//!
//! **The window asking for an account** is read where the app keeps it (its fields, the place chosen, the line under
//! the fields), said on standard output whenever it changes (`prompt …`) and when the parent asks (`prompt-now …`).
//!
//! **The requests**: the child installs the app's own debug log, which records every request the process sends; asked,
//! it says how many went to the own server and to which paths, and how many sign-in or authz requests went anywhere
//! else.

use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{fs, thread};

use gpui::{App, AppContext as _, AsyncApp, Entity, WindowingModes};
use ngteams::accounts;
use ngteams::accounts_file::{self, PersonalAccount, PersonalPaths};
use ngteams::setup::Setup;
use teams_auth::authz::{skypetoken_expiry_key, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{Secret, SecretStore as _};
use teams_ui::account_prompt::{self, AccountPrompt, AccountPromptEvent, OwnOffer, SignInKind};
use teams_ui::window::{KitRoot, PERSONAL_APP_ID};
use ui_sandbox::SandboxResult;

use super::own_server::OwnServer as InProcessServer;
use super::own_server_binary::{self, BinaryServer};
use super::{READY_FRAMES, after_frames, say};

pub use super::own_server_binary::PASSWORD;

/// The file in the sandbox dir the parent writes to have the window asking for an account said now.
pub const PROMPT_ASK: &str = "say-prompt";
/// The file in the sandbox dir the parent writes to have the app's requests said.
pub const REQUESTS_ASK: &str = "say-requests";
/// The file in the sandbox dir the child writes the requests' methods and URLs to.
pub const REQUESTS_FILE: &str = "requests.txt";
/// The file in the sandbox dir the child writes an origin nothing listens on to.
pub const DEAD_ORIGIN: &str = "own-dead-origin";
/// The account the password-again screen asks: an own server's, with a Microsoft 365 account of the same user name
/// beside it.
pub const AGAIN_UPN: &str = "erin@own.example.test";
const AGAIN_NAME: &str = "Erin Again";
const MICROSOFT_OID: &str = "00000000-0000-4000-8000-0000000000ee";
const TENANT: &str = "00000000-0000-4000-8000-000000000001";

/// The own server the child's app signs in to.
pub enum Own {
    Binary(BinaryServer),
    InProcess(InProcessServer),
}

impl Own {
    /// The real binary when there is one, else the server in this process; says which.
    pub fn start() -> SandboxResult<Self> {
        if let Some(binary) = own_server_binary::binary()? {
            say(&format!("own-server binary {}", binary.display()));
            return Ok(Self::Binary(BinaryServer::start(&binary)?));
        }
        say("own-server in-process");
        Ok(Self::InProcess(
            InProcessServer::start().map_err(|error| error.to_string())?,
        ))
    }

    pub fn origin(&self) -> &str {
        match self {
            Self::Binary(server) => &server.origin,
            Self::InProcess(server) => &server.origin,
        }
    }

    pub fn ca_file(&self) -> &Path {
        match self {
            Self::Binary(server) => &server.ca_file,
            Self::InProcess(server) => &server.ca_file,
        }
    }

    /// Adds the user `upn`, named `name`, with [`PASSWORD`].
    pub fn add_user(&self, upn: &str, name: &str) -> SandboxResult<String> {
        match self {
            Self::Binary(server) => server.add_user(upn, name),
            Self::InProcess(server) => Ok(server.add_user(upn, name).map_err(|error| error.to_string())?),
        }
    }

    /// The account's entry as a user of a self-signed deployment writes it: its server and its `ca_file`.
    pub fn entry(&self) -> SandboxResult<accounts_file::OwnServer> {
        Ok(accounts_file::OwnServer::parse(
            self.origin(),
            &self.ca_file().display().to_string(),
        )?)
    }
}

/// The sandbox dir, where the parent and the child leave each other files.
pub fn sandbox_dir() -> SandboxResult<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os(ui_sandbox::SANDBOX_ENV).ok_or("no sandbox dir")?,
    ))
}

/// The window asking for an account, if it is open.
fn open_prompt(cx: &App) -> Option<Entity<AccountPrompt>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<KitRoot>())
        .find_map(|window| window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok())
}

/// What the window asking for an account holds, on one line.
fn described(cx: &App) -> String {
    let Some(prompt) = open_prompt(cx) else {
        return "closed".to_owned();
    };
    let prompt = prompt.read(cx);
    let kind = match prompt.kind() {
        SignInKind::Microsoft => "microsoft",
        SignInKind::OwnServer => "own",
    };
    let (server, user) = prompt.own_fields(cx);
    let problem = prompt.problem().map_or_else(|| "-".to_owned(), ToString::to_string);
    format!(
        "kind={kind} signing={} server={server} user={user} account={} problem={problem}",
        prompt.is_signing_in(),
        prompt.account(cx),
    )
}

/// Says `prompt <state>` whenever the window asking for an account changes, and `prompt-now <state>` each time the
/// parent writes [`PROMPT_ASK`].
pub fn say_prompt(cx: &mut App) {
    let Ok(dir) = sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    let ask = dir.join(PROMPT_ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut last = "closed".to_owned();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let now = cx.update(|cx| described(cx));
            if now != last {
                say(&format!("prompt {now}"));
                last.clone_from(&now);
            }
            if fs::remove_file(&ask).is_ok() {
                say(&format!("prompt-now {now}"));
            }
        }
    })
    .detach();
}

/// Installs the app's debug log in the sandbox dir, so every request this process sends is on record.
pub fn record_requests() -> SandboxResult {
    let config = teams_log::Config {
        dir: sandbox_dir()?.join("app-log"),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::file::Rotation::default(),
    };
    teams_log::install(&config).map_err(|error| format!("the app's log: {error}"))?;
    Ok(())
}

/// The method and URL of every request the app's log holds.
fn logged_requests() -> Vec<(String, String)> {
    let Some(log) = teams_log::installed() else {
        return Vec::new();
    };
    let _ = log.flush_written();
    let text = fs::read_to_string(log.path()).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (_, request) = line.split_once(" request ")?;
            let mut words = request.split(' ');
            Some((words.next()?.to_owned(), words.next()?.to_owned()))
        })
        .collect()
}

/// Whether `url` asks a sign-in or an authz service.
fn signs_in(url: &str) -> bool {
    url.contains("/oauth2/") || url.contains("/authsvc/")
}

/// Where `requests` went: the line the child says of them (see [`say_requests_when_asked`]) and their list, one
/// `<place> <method> <url>` a line.
fn summed(requests: &[(String, String)], origin: &str, dead: &str) -> (String, String) {
    use std::fmt::Write as _;
    let mut own_paths = std::collections::BTreeSet::new();
    let (mut own, mut to_dead, mut elsewhere, mut elsewhere_auth) = (0, 0, 0, 0);
    let mut listed = String::new();
    for (method, url) in requests {
        let place = if let Some(path) = url.strip_prefix(origin) {
            own += 1;
            own_paths.insert(path.split('?').next().unwrap_or(path).to_owned());
            "own-server"
        } else if url.starts_with(dead) {
            to_dead += 1;
            "unreachable"
        } else {
            elsewhere += 1;
            elsewhere_auth += u32::from(signs_in(url));
            "microsoft-mock"
        };
        let _ = writeln!(listed, "{place} {method} {url}");
    }
    let paths = own_paths.into_iter().collect::<Vec<_>>().join(",");
    let line = format!(
        "requests own={own} own_paths={paths} dead={to_dead} elsewhere={elsewhere} elsewhere_auth={elsewhere_auth}"
    );
    (line, listed)
}

/// Once the parent writes [`REQUESTS_ASK`]: `requests own=<n> own_paths=<a,b> dead=<n> elsewhere=<n>
/// elsewhere_auth=<n>` of the requests this process sent (to the own server `origin`, to the origin nothing listens
/// on, and anywhere else), and their methods and URLs in [`REQUESTS_FILE`].
pub fn say_requests_when_asked(origin: String, dead: String, cx: &mut App) {
    let Ok(dir) = sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if fs::remove_file(dir.join(REQUESTS_ASK)).is_err() {
                continue;
            }
            let (line, listed) = summed(&logged_requests(), &origin, &dead);
            let _ = fs::write(dir.join(REQUESTS_FILE), listed);
            say(&line);
        }
    })
    .detach();
}

/// The dark theme, whatever the system says.
pub fn dark() -> teams_ui::theme::Appearance {
    teams_ui::theme::Appearance {
        follow_system: false,
        theme: teams_ui::theme::DARK_KEY,
        ..teams_ui::theme::Appearance::default()
    }
}

/// The settings file's lines for [`dark`].
pub const DARK_SETTINGS: &str = "follow_system_theme = false\ntheme = \"teams-dark\"\n";

fn block_on<T>(future: impl Future<Output = T>) -> SandboxResult<T> {
    Ok(accounts::runtime()?.block_on(future))
}

/// The child of the password-again scene: an own server's account that was signed in and is signed out now (its
/// tokens gone), beside a Microsoft 365 account of the same user name. The screen the app opens for such an account
/// (`OwnOffer::Again`, as `ngteams` opens it at startup; the binary's own startup can't run in a test build) asks
/// its password alone, and the app's own sign-in signs it in on the server (`accounts::sign_in_own`). Says
/// `again-signed-in ready=<bool>`, then forgets the account as `ngteams --forget <server>/<UPN>` does
/// (`accounts::forget`) and says `forgot own_left=<bool> microsoft_left=<bool> token_left=<bool>`, then quits.
pub fn child_again(is_dark: bool) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = tempfile::tempdir()?;
    let paths = PersonalPaths::new(dir.path().join("ngteams-config"), dir.path().join("ngteams-data"));
    let setup = Setup::new(paths)?
        .with_secrets(ngteams::secrets::AppSecrets::in_memory())
        .with_own_servers(true);
    let own = Own::start()?;
    own.add_user(AGAIN_UPN, AGAIN_NAME)?;
    PersonalAccount::new(AGAIN_UPN, MICROSOFT_OID, TENANT)?.save(&setup.paths)?;
    let account = accounts::add_own(&setup, AGAIN_UPN, &own.entry()?, &Secret::new(PASSWORD))?;
    let secrets = setup.secrets();
    for key in [
        refresh_token_key(&account.id()),
        skypetoken_key(&account.id()),
        skypetoken_expiry_key(&account.id()),
    ] {
        block_on(secrets.delete(&key))??;
    }
    if accounts::ready(&setup, &account)? {
        return Err("the signed-out account still counts as ready".into());
    }
    let offer = OwnOffer::Again {
        server: account.server.clone().unwrap_or_default(),
        user: account.upn.clone(),
    };
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        if is_dark {
            teams_ui::theme::set_appearance(&dark(), cx);
        }
        let title = ngteams::app::TITLE.into();
        match account_prompt::open_offering(cx, PERSONAL_APP_ID, title, None, &offer) {
            Ok((window, prompt)) => {
                cx.subscribe(&prompt, move |prompt, event, cx| {
                    if let AccountPromptEvent::SignInOwn(typed) = event {
                        sign_in_again(setup.clone(), account.clone(), typed.password.to_string(), prompt, cx);
                    }
                })
                .detach();
                say_prompt(cx);
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, READY_FRAMES, |_, _| say("ready"));
                });
            }
            Err(error) => {
                say(&format!("the screen can't be opened: {error:#}"));
                cx.quit();
            }
        }
    });
    drop(own);
    Ok(())
}

/// Signs `account` in again with `password` off the foreground; said on `prompt` when it fails (see [`child_again`]).
fn sign_in_again(
    setup: Setup,
    account: PersonalAccount,
    password: String,
    prompt: Entity<AccountPrompt>,
    cx: &mut App,
) {
    let signing = cx.background_spawn(async move {
        let signed = accounts::sign_in_own(&setup, &account, &Secret::new(password))?;
        let ready = accounts::ready(&setup, &signed)?;
        say(&format!("again-signed-in ready={ready}"));
        // let the parent look at the screen before the account goes
        thread::sleep(Duration::from_millis(300));
        accounts::forget(&setup, &signed.id())?;
        let left = PersonalAccount::load_all(&setup.paths).map_err(|error| error.to_string())?;
        let own_left = left.iter().any(|held| held.server.is_some());
        let microsoft_left = left.iter().any(|held| held.server.is_none() && held.upn == AGAIN_UPN);
        let token = accounts::runtime()?
            .block_on(setup.secrets().get(&refresh_token_key(&signed.id())))
            .map_err(|error| error.to_string())?;
        say(&format!(
            "forgot own_left={own_left} microsoft_left={microsoft_left} token_left={}",
            token.is_some()
        ));
        Ok::<(), String>(())
    });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(()) => cx.update(|cx| cx.quit()),
        Err(error) => cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(error, cx))),
    })
    .detach();
}
