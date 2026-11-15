//! The own server's side of the accounts window test: the server the app signs in to, what the child tells
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
/// The file in the sandbox dir the parent writes to have the own account's credentials handed to the app's next run.
pub const SECRETS_ASK: &str = "hand-own-secrets";
/// The file in the sandbox dir they are handed over in: owner-only, deleted by the run that takes them.
const SECRETS_FILE: &str = "own-secrets";
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

    /// Adds the administrator `upn`, named `name`, with [`PASSWORD`].
    pub fn add_admin(&self, upn: &str, name: &str) -> SandboxResult<String> {
        match self {
            Self::Binary(server) => server.add_admin(upn, name),
            Self::InProcess(server) => Ok(server.add_admin(upn, name).map_err(|error| error.to_string())?),
        }
    }

    /// A new one-time setup code, as the operator reads it off the server.
    pub fn setup_code(&self) -> SandboxResult<String> {
        match self {
            Self::Binary(server) => server.setup_code(),
            Self::InProcess(server) => Ok(server.setup_code().map_err(|error| error.to_string())?),
        }
    }

    /// The server's accounts: `(user name, id, enabled, admin)`.
    pub fn users(&self) -> SandboxResult<Vec<(String, String, bool, bool)>> {
        match self {
            Self::Binary(server) => server.users(),
            Self::InProcess(server) => Ok(server.users().map_err(|error| error.to_string())?),
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

/// The window asking for an account, if it is open, and whether it is the active one (the keyboard's).
fn open_prompt(cx: &mut App) -> Option<(Entity<AccountPrompt>, bool)> {
    let (window, prompt) = cx
        .windows()
        .into_iter()
        .filter_map(|window| window.downcast::<KitRoot>())
        .find_map(|window| {
            let prompt = window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok()?;
            Some((window, prompt))
        })?;
    let active = window.is_active(cx).unwrap_or(false);
    Some((prompt, active))
}

/// What the window asking for an account holds, on one line.
fn described(cx: &mut App) -> String {
    let Some((prompt, active)) = open_prompt(cx) else {
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
        "active={active} stage={} kind={kind} signing={} server={server} user={user} account={} asks={} \
         problem={problem}",
        prompt.stage().key(),
        prompt.is_signing_in(),
        prompt.account(cx),
        prompt.asks(),
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
            let now = cx.update(described);
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

/// Installs the app's debug log in the sandbox dir (a directory of this run's alone, named with `run`), so every
/// request this process sends is on record.
pub fn record_requests(run: &str) -> SandboxResult {
    let config = teams_log::Config {
        dir: sandbox_dir()?.join(format!("app-log-{run}")),
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

/// The keys an own server's account's credentials are kept under.
fn own_keys(account: &PersonalAccount) -> [teams_auth::secrets::SecretKey; 3] {
    [
        refresh_token_key(&account.id()),
        skypetoken_key(&account.id()),
        skypetoken_expiry_key(&account.id()),
    ]
}

/// Once the parent writes [`SECRETS_ASK`]: the own server's account's credentials go to [`SECRETS_FILE`], one a
/// line in [`own_keys`]' order, for the app's next run in this sandbox (the sandbox has no Secret Service to keep
/// them across runs); says `own-secrets handed=<n>`, never a value.
pub fn hand_over_secrets_when_asked(setup: Setup, cx: &mut App) {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let Ok(dir) = sandbox_dir() else {
        say("no sandbox dir");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if fs::remove_file(dir.join(SECRETS_ASK)).is_err() {
                continue;
            }
            let own = setup
                .signed_in_accounts()
                .unwrap_or_default()
                .into_iter()
                .find(|account| account.server.is_some());
            let Some(account) = own else {
                say("own-secrets: no own server's account");
                continue;
            };
            let secrets = setup.secrets();
            let (mut lines, mut handed) = (String::new(), 0);
            for key in own_keys(&account) {
                if let Ok(Some(secret)) = secrets.get(&key).await {
                    lines.push_str(secret.expose());
                    handed += 1;
                }
                lines.push('\n');
            }
            let written = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(dir.join(SECRETS_FILE))
                .and_then(|mut file| file.write_all(lines.as_bytes()));
            match written {
                Ok(()) => say(&format!("own-secrets handed={handed}")),
                Err(error) => say(&format!("own-secrets: not written: {error}")),
            }
        }
    })
    .detach();
}

/// The app's second run: takes the credentials the first run handed over ([`hand_over_secrets_when_asked`]) into this
/// run's store, deleting the file, and starts the own server's account from its `accounts.toml` entry as `ngteams`
/// starts an account signed in before. Says `came-back entries=<n> started=<n>` of the file's accounts.
pub fn come_back(runner: &accounts::Runner) -> SandboxResult<accounts::Started> {
    let setup = &runner.setup;
    let file = sandbox_dir()?.join(SECRETS_FILE);
    let handed = fs::read_to_string(&file)?;
    fs::remove_file(&file)?;
    let signed_in = setup.signed_in_accounts()?;
    let account = signed_in
        .iter()
        .find(|account| account.server.is_some())
        .ok_or("no own server's account came back")?;
    let secrets = setup.secrets();
    for (key, value) in own_keys(account).iter().zip(handed.lines()) {
        if !value.is_empty() {
            block_on(secrets.put(key, &Secret::new(value)))??;
        }
    }
    if !accounts::ready(setup, account)? {
        return Err("the own server's account can't start without a sign-in".into());
    }
    say(&format!(
        "came-back entries={} started={}",
        setup.accounts()?.len(),
        signed_in.len()
    ));
    Ok(runner.start(account).map_err(String::from)?)
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

/// The child of the said-reasons scene: an own server's account signed in on a server that serves no chats (the
/// region its sign-in saved names no chat service, as an older server leaves it). The app's own check
/// (`accounts::unserved`) gives what its sign-in screen says at the start, as `ngteams` opens that screen when the
/// account can't start. The server is gone by then: the first password typed is the entry's as it is, with its
/// `ca_file`; before the second the `ca_file` line is taken out of the entry in `accounts.toml`, the screen still
/// holding the entry as it read it; each sign-in's reason is the app's own, of the entry as the file has it then. A
/// third quits.
pub fn child_said(is_dark: bool) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = tempfile::tempdir()?;
    let paths = PersonalPaths::new(dir.path().join("ngteams-config"), dir.path().join("ngteams-data"));
    let setup = Setup::new(paths)?
        .with_secrets(ngteams::secrets::AppSecrets::in_memory())
        .with_own_servers(true);
    let own = Own::start()?;
    own.add_user(AGAIN_UPN, AGAIN_NAME)?;
    let account = accounts::add_own(&setup, AGAIN_UPN, &own.entry()?, &Secret::new(PASSWORD))?;
    // the certificate authority's file outlives the server, as a deployment's does
    let ca_file = dir.path().join("ca.pem");
    fs::copy(own.ca_file(), &ca_file)?;
    let account = PersonalAccount {
        ca_file: Some(ca_file),
        ..account
    };
    account.save(&setup.paths)?;
    let region = setup.paths.region_file(&account).ok_or("the account's region file")?;
    fs::write(region, r#"{"region":"own","partition":null,"regionGtms":{}}"#)?;
    let said = accounts::unserved(&setup, &account).ok_or("a server without a chat service isn't told")?;
    drop(own);
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
                prompt.update(cx, |prompt, cx| prompt.failed(said, cx));
                let tries = std::rc::Rc::new(std::cell::Cell::new(0_u8));
                cx.subscribe(&prompt, move |prompt, event, cx| {
                    let AccountPromptEvent::SignInOwn(typed) = event else {
                        return;
                    };
                    tries.set(tries.get() + 1);
                    match tries.get() {
                        1 => {}
                        2 => without_ca_file(&setup, &account),
                        _ => return cx.quit(),
                    }
                    let account = account.clone();
                    let (setup, password) = (setup.clone(), Secret::new(typed.password.as_str()));
                    let signing =
                        cx.background_spawn(async move { accounts::sign_in_own(&setup, &account, &password) });
                    cx.spawn(async move |cx: &mut AsyncApp| {
                        let reason = signing.await.err().unwrap_or_else(|| "signed in".to_owned());
                        cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(reason, cx)));
                    })
                    .detach();
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
    Ok(())
}

/// Takes the `ca_file` line out of `account`'s entry in `accounts.toml`, as the user would by hand.
fn without_ca_file(setup: &Setup, account: &PersonalAccount) {
    let without = PersonalAccount {
        ca_file: None,
        ..account.clone()
    };
    if let Err(error) = without.save(&setup.paths) {
        say(&format!("the entry can't be written: {error}"));
    }
}

/// The child of the first-sign-in scene: an own server's entry written by hand that never signed in, without the
/// server's certificate authority. Its screen (`OwnOffer::First`) asks its password with no "again". The first
/// sign-in says where the CA file goes; the child then writes `ca_file` into the entry as the user would, the screen
/// still open, and says `ca-file-written`. The next sign-in on the same screen (the app's own
/// `accounts::sign_in_own`, handed the entry as the screen read it) reads the entry again and goes through: says
/// `first-signed-in ready=<bool> ca_file=<bool>`, then quits.
pub fn child_first(is_dark: bool) -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let dir = tempfile::tempdir()?;
    let paths = PersonalPaths::new(dir.path().join("ngteams-config"), dir.path().join("ngteams-data"));
    let setup = Setup::new(paths)?
        .with_secrets(ngteams::secrets::AppSecrets::in_memory())
        .with_own_servers(true);
    let own = Own::start()?;
    own.add_user(AGAIN_UPN, AGAIN_NAME)?;
    let account = PersonalAccount::pending_on(AGAIN_UPN, &accounts_file::OwnServer::parse(own.origin(), "")?)?;
    account.save(&setup.paths)?;
    let written = PersonalAccount {
        ca_file: Some(own.ca_file().to_path_buf()),
        ..account.clone()
    };
    let offer = OwnOffer::First {
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
                        let entries = (account.clone(), written.clone());
                        sign_in_first(setup.clone(), entries, typed.password.to_string(), prompt, cx);
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

/// Signs `shown` (the entry as the screen read it) in with `password` off the foreground; one that fails is said on
/// `prompt`, and `written` (the entry with its `ca_file`) is saved over it (see [`child_first`]).
fn sign_in_first(
    setup: Setup,
    (shown, written): (PersonalAccount, PersonalAccount),
    password: String,
    prompt: Entity<AccountPrompt>,
    cx: &mut App,
) {
    let signing = cx.background_spawn(async move {
        let signed = accounts::sign_in_own(&setup, &shown, &Secret::new(password)).inspect_err(|_| {
            if written.save(&setup.paths).is_ok() {
                say("ca-file-written");
            }
        })?;
        let ready = accounts::ready(&setup, &signed)?;
        say(&format!(
            "first-signed-in ready={ready} ca_file={}",
            signed.ca_file.is_some()
        ));
        Ok::<(), String>(())
    });
    cx.spawn(async move |cx: &mut AsyncApp| match signing.await {
        Ok(()) => cx.update(|cx| cx.quit()),
        Err(error) => cx.update(|cx| prompt.update(cx, |prompt, cx| prompt.failed(error, cx))),
    })
    .detach();
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
