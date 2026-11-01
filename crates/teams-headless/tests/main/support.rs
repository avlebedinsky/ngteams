//! The main dispatcher's test dependency: a real lab description backed exclusively by synthetic loopback peers.
//! This module is included only by the binary's unit tests; the executable has no switch to use it.

#![cfg(test)]

use std::error::Error;
use std::future::{self, Future};
use std::ops::Deref;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::{Duration, SystemTime};

use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_core::{CallMedia, CallParts, CallVideo, Identity, Outbox, Relay};
use teams_fence::lab::{Alias, LabConfig};
use teams_fence::{Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster};
use teams_headless::chat::LabNames;
use teams_headless::lab::{Lab, call_media};
use teams_media::devices::Devices;
use teams_media::relay::RelayServer;
use teams_mock::{
    ABC_CHAT, CHAT_BASE, ChatServiceMock, ECHO, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE, TENANT,
    TrouterMock,
};
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{CallStack, Options, Trouter, TrouterCredentials};
use tokio::sync::broadcast;

pub(crate) struct TestLab {
    inner: Lab,
    pub(super) mock: ChatServiceMock,
    pub(super) trouter: TrouterMock,
    relay: OnceLock<std::net::SocketAddr>,
    pub(super) fail_start: bool,
    _dir: tempfile::TempDir,
}

impl Deref for TestLab {
    type Target = Lab;

    fn deref(&self) -> &Lab {
        &self.inner
    }
}

impl TestLab {
    pub(super) async fn new() -> Self {
        diagnostics();
        let mock = ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "{}\n[[service_participants]]\nname = \"echo\"\nmri = \"{ECHO}\"\n",
            teams_mock::lab_toml(dir.path())
        );
        let config = LabConfig::parse(&text, dir.path()).unwrap();
        let inner = Lab {
            names: LabNames::from_config(&config),
            config,
            alias: Alias::A,
            abc: Some(ABC_CHAT.to_owned()),
        };
        Self {
            inner,
            mock,
            trouter,
            relay: OnceLock::new(),
            fail_start: false,
            _dir: dir,
        }
    }

    /// Unit tests must provide the synthetic fixture explicitly, never load the operator's environment.
    pub(crate) fn load(_: Alias) -> Result<Self, Box<dyn Error>> {
        Err("dispatcher tests must supply their loopback lab".into())
    }

    pub(crate) fn start_engine(&self, push: bool) -> future::Ready<Result<EngineHandle, Box<dyn Error>>> {
        future::ready(self.build(push, None))
    }

    pub(crate) fn start_calling(
        &self,
        devices: impl Devices + Send + Sync + 'static,
        push: bool,
        video: Option<CallVideo>,
    ) -> future::Ready<Result<EngineHandle, Box<dyn Error>>> {
        let relay = *self.relay.get_or_init(|| {
            self.mock
                .start_echo_media(self.paths().data_dir(), Duration::from_millis(200))
                .unwrap()
        });
        future::ready(self.build(
            push,
            Some(CallMedia {
                video,
                ..call_media(devices, Relay::Fixed(vec![RelayServer::udp(relay)]))
            }),
        ))
    }

    fn build(&self, push: bool, media: Option<CallMedia>) -> Result<EngineHandle, Box<dyn Error>> {
        if self.fail_start {
            return Err("synthetic engine startup failure".into());
        }
        let tokens = Arc::new(Tokens {
            token: self.mock.token(LAB_A).unwrap(),
            skype: self.mock.skypetoken(LAB_A).unwrap(),
            renewals: broadcast::channel(4).0,
        });
        let fence = Arc::new(
            Fence::new(
                Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), [Mri::new(ECHO)]).with_lab_mail([
                    "bot-a@lab.example.test",
                    "bot-b@lab.example.test",
                    "bot-c@lab.example.test",
                ]),
                FenceLog::from_writer(std::io::sink()),
            )
            .with_own_account(Mri::new(LAB_A)),
        );
        let http = || {
            HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &self.mock.origin())
                .unwrap()
                .with_middle_tier(MIDDLE_TIER_BASE)
                .unwrap()
                .with_presence(PRESENCE_BASE)
                .unwrap()
        };
        let transport = Arc::new(http());
        let members = LiveMembershipSource::with_mock_origin(&self.mock.origin()).unwrap();
        members.set_credential_source(transport.clone());
        let members: Arc<dyn MembershipSource + Send> = Arc::new(members);
        let socket = || {
            Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), self.trouter.origin())
                .unwrap()
                .with_options(Options {
                    backoff_base: Duration::from_millis(10),
                    backoff_cap: Duration::from_millis(10),
                    backoff_jitter_percent: 0,
                    ..Options::default()
                })
        };
        let calls = media.map(|media| {
            CallParts::new(
                socket().for_calls(CallStack::new(Arc::clone(&fence), Arc::clone(&members))),
                identity(),
                media,
            )
        });
        let push = push.then(|| Push::new(socket()));
        Ok(engine::start(Parts {
            calls,
            store: Store::open(&self.paths().store_file(Alias::A))?,
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(
                transport,
                Arc::clone(&fence),
                members,
                Author::new(Mri::new(LAB_A), "Synthetic A"),
            ),
            push,
            options: EngineOptions {
                tick: Duration::from_millis(10),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })?)
    }
}

static LOG_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOGGER: DiagnosticLog = DiagnosticLog;

struct DiagnosticLog;

impl log::Log for DiagnosticLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info && metadata.target().starts_with("teams_core")
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            LOG_LINES.lock().unwrap().push(record.args().to_string());
        }
    }

    fn flush(&self) {}
}

fn diagnostics() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        log::set_logger(&LOGGER).unwrap();
        log::set_max_level(log::LevelFilter::Info);
    });
}

pub(super) fn diagnostic_lines() -> Vec<String> {
    LOG_LINES.lock().unwrap().clone()
}

fn identity() -> Identity {
    Identity {
        mri: Mri::new(LAB_A),
        display_name: Some("Synthetic A".to_owned()),
        endpoint_id: String::new(),
        language_id: Some("en-us".to_owned()),
        tenant_id: TENANT.to_owned(),
        client: teams_core::account::web_client_info(),
        region: Some("amer".to_owned()),
        partition: Some("amer01".to_owned()),
    }
}

struct Tokens {
    token: String,
    skype: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(self.skype.clone()))))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(&self, _: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

/// The test dispatcher can stop deterministically; the executable retains its Unix-signal-only implementation.
pub(crate) struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    pub(super) stop_after: Option<Duration>,
}

impl Signals {
    pub(crate) fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            stop_after: None,
        })
    }

    pub(crate) async fn next(&mut self) {
        if let Some(after) = self.stop_after {
            tokio::time::sleep(after).await;
            return;
        }
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}
