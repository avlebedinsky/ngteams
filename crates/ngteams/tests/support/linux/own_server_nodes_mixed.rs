//! The child of the mixed scene: one window with two Microsoft 365 accounts (the lab's a and b on `teams-mock`, its
//! chat service and Trouter through the `test-support` mock origins, as `accounts_window.rs` runs them) and, once the
//! parent added it from the rail's "+" ("Own server", typed), `srv-a`'s account on the two-node deployment.
//!
//! Asked through [`super::instance::PROBE`] it says where the requests went: `requests own=<n> own_paths=<a,b>
//! elsewhere=<n> elsewhere_auth=<n>` from the app's own debug log (every request this process sent; `own` those to
//! the deployment's origin), and `mock total=<n> auth_paths=<n> own_token=<n>` of what the mock (Microsoft's services
//! here) received: sign-in or authz requests, and requests carrying the own account's skypetoken.

use std::future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui::{App, AsyncApp, WindowingModes};
use ngteams::accounts::{Runner, Started};
use ngteams::app::{self, Launch};
use ngteams::secrets::AppSecrets;
use teams_auth::secrets::{Secret, SecretStore as _};
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallFloor, Outbox};
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, TrouterMock};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::settings::Settings;
use teams_ui::{SettingsFile, SettingsModel, ViewTrace};
use tokio::sync::broadcast;
use ui_sandbox::SandboxResult;

use super::instance::{PROBE, say};
use super::support::{self, Alias};

/// One mock account's token, for its transport and its push channel alike; it never runs out in a test.
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

/// The mock, its Trouter, and the runtime they run on.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    trouter: TrouterMock,
}

impl MockLab {
    fn start() -> SandboxResult<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        Ok(Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            trouter,
        })
    }

    /// The engine of the mock account `mri` (`upn`), its store at `store`.
    fn engine(&self, store: &Path, mri: &str, upn: &str) -> SandboxResult<EngineHandle> {
        let tokens = Arc::new(Tokens {
            token: self.mock.token(mri).ok_or("the mock's token")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = self.mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        if let Some(dir) = store.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(engine::start(Parts {
            calls: None,
            store: Store::open(store)?,
            client: Arc::new(Client::new(http()?)),
            outbox: Outbox::new(transport, Author::new(Mri::new(mri), support::display_name(upn))),
            push: Some(Push::new(trouter)),
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })?)
    }
}

/// A mock account as [`support::started`] starts it, its session (the sign-out watch's alone) over a store of the
/// app's kind, so it stands beside an account the app's runner starts.
fn started(alias: Alias, engine: EngineHandle) -> SandboxResult<Started> {
    let started = support::started(alias, engine)?;
    let session = teams_auth::session::Session::new(
        started.account.upn.clone(),
        AppSecrets::in_memory(),
        teams_auth::token::TokenClient::new(
            teams_auth::token::EntraConfig::teams_native().with_authority("http://127.0.0.1:9/"),
        )?,
        teams_auth::authz::AuthzClient::with_endpoint("http://127.0.0.1:9/authz")?,
        teams_auth::authz::Region::default(),
    );
    Ok(Started {
        account: started.account,
        engine: teams_core::account::AccountEngine {
            handle: started.engine.handle,
            session: Arc::new(session),
        },
        calls: None,
    })
}

/// The mixed scene's child (see the module docs): `dir` its files, `origin` the deployment's.
pub fn child(trace: Arc<ViewTrace>, dir: &Path, ctl: PathBuf, origin: String) -> SandboxResult {
    let lab = MockLab::start()?;
    let a = started(
        Alias::A,
        lab.engine(&support::store_file(dir, Alias::A), LAB_A, "bot-a@lab.example.test")?,
    )?;
    let b = started(
        Alias::B,
        lab.engine(&support::store_file(dir, Alias::B), LAB_B, "bot-b@lab.example.test")?,
    )?;
    let runner = support::runner(dir)?;
    let runner = Runner {
        setup: runner.setup.with_secrets(AppSecrets::in_memory()),
        floor: CallFloor::new(),
        devices: None,
    };
    for account in [&a.account, &b.account] {
        account.save(&runner.setup.paths)?;
    }
    let settings = SettingsFile::new(dir.join("settings.toml"));
    settings
        .save(&Settings::default())
        .map_err(|problem| format!("{problem:?}"))?;
    let setup = runner.setup.clone();
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first: a,
                others: vec![b],
                trace,
                ctl: Some(ctl),
                settings: SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: Rc::new(|_| Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                super::instance::quit_when_asked(cx);
                super::instance::say_prompt(cx);
                answer_probes(mock, setup, origin, cx);
                let _ = launched.window.update(cx, |_, window, _| {
                    super::instance::after_frames(window, 3, |_, _| say("ready"));
                });
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

/// Each time the parent writes [`PROBE`] (`requests` or `mock`): says where the requests went (see the module docs).
fn answer_probes(mock: Arc<ChatServiceMock>, setup: ngteams::setup::Setup, origin: String, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        return;
    };
    let trigger = PathBuf::from(dir).join(PROBE);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(line) = std::fs::read_to_string(&trigger) else {
                continue;
            };
            let _ = std::fs::remove_file(&trigger);
            let said = match line.trim() {
                "requests" => requests_said(&origin),
                "mock" => mock_said(&mock, &setup).await,
                other => format!("failed not a probe: {other}"),
            };
            say(&format!("probe {said}"));
        }
    })
    .detach();
}

/// The method and URL of every request the app's log holds.
fn logged_requests() -> Vec<(String, String)> {
    let Some(log) = teams_log::installed() else {
        return Vec::new();
    };
    let _ = log.flush_written();
    let text = std::fs::read_to_string(log.path()).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let (_, request) = line.split_once(" request ")?;
            let mut words = request.split(' ');
            Some((words.next()?.to_owned(), words.next()?.to_owned()))
        })
        .collect()
}

fn requests_said(origin: &str) -> String {
    let mut paths = std::collections::BTreeSet::new();
    let (mut own, mut elsewhere, mut elsewhere_auth) = (0, 0, 0);
    for (_, url) in logged_requests() {
        if let Some(path) = url.strip_prefix(origin) {
            own += 1;
            paths.insert(path.split('?').next().unwrap_or(path).to_owned());
        } else {
            elsewhere += 1;
            elsewhere_auth += u32::from(url.contains("/oauth2/") || url.contains("/authsvc/"));
        }
    }
    let paths = paths.into_iter().collect::<Vec<_>>().join(",");
    format!("requests own={own} own_paths={paths} elsewhere={elsewhere} elsewhere_auth={elsewhere_auth}")
}

async fn mock_said(mock: &ChatServiceMock, setup: &ngteams::setup::Setup) -> String {
    let own = setup
        .signed_in_accounts()
        .unwrap_or_default()
        .into_iter()
        .find(|account| account.server.is_some());
    let token = match own {
        Some(account) => setup
            .secrets()
            .get(&teams_auth::authz::skypetoken_key(&account.id()))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let Some(token) = token else {
        return "failed the own account holds no skypetoken".to_owned();
    };
    let requests = mock.received_requests().await;
    let auth_paths = requests
        .iter()
        .filter(|request| {
            let path = request.url.path();
            path.contains("/oauth2/") || path.contains("/authsvc/")
        })
        .count();
    let own_token = requests
        .iter()
        .filter(|request| {
            request
                .headers
                .values()
                .any(|value| value.to_str().is_ok_and(|value| value.contains(token.expose())))
        })
        .count();
    format!(
        "mock total={} auth_paths={auth_paths} own_token={own_token}",
        requests.len()
    )
}
