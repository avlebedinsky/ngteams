//! Teams and channels: the real own-cloud engine and the client's typed reads against the server's CSA, middle
//! tier, provisioning and chat service.

mod create;

use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::account::{CSA_GTM, MIDDLE_TIER_GTM, PROVISIONING_GTM};
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts};
use teams_proto::{Author, Client, Cloud, CsaTeams, HttpTransport, Mri, NewTeam, TeamWrite};
use teams_store::Store;

use crate::support::{Backend, TestServer};

pub(crate) const ORIGIN: &str = "https://own.example.test";

pub(crate) type TeamClient = Client<HttpTransport<Session<MemorySecretStore>>>;

pub(crate) async fn server(backend: Backend) -> TestServer {
    TestServer::start_with(backend, |config| {
        config.public_origin = url::Url::parse(ORIGIN).unwrap();
    })
    .await
}

/// The transport as the account bring-up builds it: every base from authz's GTMs.
async fn transport(
    session: &Arc<Session<MemorySecretStore>>,
    server: &TestServer,
    id: &str,
) -> HttpTransport<Session<MemorySecretStore>> {
    session.skypetoken().await.unwrap();
    let base = |gtm: &str| {
        session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"))
    };
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &base(teams_auth::authz::CHAT_BASE_GTM),
        Cloud::own(ORIGIN).unwrap(),
        &server.origin,
    )
    .unwrap()
    .for_account(Mri::new(format!("8:orgid:{id}")))
    .with_middle_tier(&base(MIDDLE_TIER_GTM))
    .unwrap()
    .with_csa(&base(CSA_GTM))
    .unwrap()
    .with_provisioning(&base(PROVISIONING_GTM))
    .unwrap()
}

/// An account with its engine and a typed client of its own.
pub(crate) struct Seat {
    pub id: String,
    pub mri: String,
    pub engine: EngineHandle,
    pub client: Arc<TeamClient>,
    _dir: tempfile::TempDir,
}

impl Seat {
    pub(crate) async fn new(server: &TestServer, upn: &str, name: &str) -> Self {
        let id = server.add_user(upn, name).await;
        Self::of(server, upn, id).await
    }

    pub(crate) async fn of(server: &TestServer, upn: &str, id: String) -> Self {
        let session = Arc::new(server.session(upn).await);
        let client = Arc::new(Client::new(transport(&session, server, &id).await));
        let writes = Arc::new(transport(&session, server, &id).await);
        let dir = tempfile::tempdir().unwrap();
        let mri = format!("8:orgid:{id}");
        let engine = engine::start(Parts {
            store: Store::open(&dir.path().join("account.sqlite")).unwrap(),
            client: Arc::clone(&client),
            outbox: Outbox::new(writes, Author::new(Mri::new(&mri), upn)),
            calls: None,
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(20),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap();
        Self {
            id,
            mri,
            engine,
            client,
            _dir: dir,
        }
    }

    /// The teams CSA lists for the account now.
    pub(crate) async fn listed(&self) -> Vec<teams_proto::Team> {
        let answer = self.client.read(&CsaTeams).await.unwrap();
        CsaTeams::read_answer(answer.body()).unwrap().teams
    }

    pub(crate) async fn team(&self, id: &str) -> teams_proto::Team {
        let teams = self.listed().await;
        teams
            .into_iter()
            .find(|team| team.id == id)
            .unwrap_or_else(|| panic!("{id} isn't listed"))
    }

    /// Makes a private team named `name` through the engine; its thread and group.
    pub(crate) async fn make_team(&self, name: &str, public: bool) -> (String, String) {
        let written = self
            .engine
            .team_write(
                TeamWrite::Create(NewTeam {
                    name: name.into(),
                    description: format!("{name}'s people"),
                    first_channel: "General".into(),
                    public,
                }),
                None,
            )
            .await
            .unwrap();
        let (thread, group) = written.created.unwrap();
        (thread, group.unwrap())
    }

    pub(crate) fn stop(self) {
        self.engine.shutdown();
    }
}

/// Waits until the engine's stored teams satisfy `ready`.
pub(crate) async fn stored_teams(
    engine: &EngineHandle,
    ready: impl Fn(&[teams_store::Team]) -> bool,
) -> Vec<teams_store::Team> {
    for _ in 0..200 {
        if let Some(teams) = engine.teams().await.unwrap()
            && ready(&teams)
        {
            return teams;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the engine's teams never settled");
}
