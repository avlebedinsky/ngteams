use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;
use serde_json::Value;
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_core::Outbox;
use teams_core::account::{CSA_GTM, MIDDLE_TIER_GTM, PROVISIONING_GTM, USER_PROFILE_GTM};
use teams_core::engine::{self, Action, EngineHandle, EngineOptions, Parts};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, CSA_AUDIENCE, Client, Cloud, CsaTeams, HttpTransport, IC3_AUDIENCE, MIDDLE_TIER_AUDIENCE, Mri, NewTeam,
    PROVISIONING_AUDIENCE, TeamRole, TeamWrite,
};
use teams_store::{OutboxEntry, OutboxState, Store};
use tempfile::TempDir;

use super::super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD, redis_url};
use crate::teams::new_channel;

/// Where the nodes' logs go, when set: one file a start.
pub const LOG_DIR: &str = "NGTS_QA_LOG_DIR";

pub type TeamClient = Client<HttpTransport<Session<MemorySecretStore>>>;
type Transport = HttpTransport<Session<MemorySecretStore>>;

/// An account signed in at one node, with its engine on a store of its own and a typed client.
pub struct Seat {
    pub mri: String,
    pub client: Arc<TeamClient>,
    engine: Option<EngineHandle>,
    pub session: Arc<Session<MemorySecretStore>>,
    origin: String,
    endpoint: String,
    dir: TempDir,
}

impl Seat {
    async fn sign_in(node: &Node, origin: &str, upn: &str) -> Self {
        let session = Arc::new(node.sign_in(upn, PASSWORD).await);
        let skype = session.skypetoken().await.unwrap();
        let mri = format!("8:{}", skype.skype_id.as_deref().unwrap());
        let client = Arc::new(Client::new(transport(&session, origin, &node.endpoint, &mri)));
        let dir = tempfile::tempdir().unwrap();
        let mut seat = Self {
            mri,
            client,
            engine: None,
            session,
            origin: origin.into(),
            endpoint: node.endpoint.clone(),
            dir,
        };
        seat.engine = Some(seat.start_engine());
        seat
    }

    fn start_engine(&self) -> EngineHandle {
        engine::start(Parts {
            store: Store::open(&self.dir.path().join("account.sqlite")).unwrap(),
            client: Arc::clone(&self.client),
            outbox: Outbox::new(
                Arc::new(transport(&self.session, &self.origin, &self.endpoint, &self.mri)),
                Author::new(Mri::new(&self.mri), "QA Operator"),
            ),
            calls: None,
            push: None,
            options: EngineOptions {
                tick: Duration::from_millis(20),
                ..EngineOptions::default()
            },
            known_names: Vec::new(),
        })
        .unwrap()
    }

    pub fn engine(&self) -> &EngineHandle {
        self.engine.as_ref().unwrap()
    }

    /// The client closed and opened again on the same store.
    pub fn restart(&mut self) {
        self.engine.take().unwrap().shutdown();
        self.engine = Some(self.start_engine());
    }

    pub async fn sync(&self) {
        let report = self.engine().sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }

    /// The teams CSA lists for the account now.
    pub async fn listed(&self) -> Vec<teams_proto::Team> {
        let answer = self.client.read(&CsaTeams).await.unwrap();
        CsaTeams::read_answer(answer.body()).unwrap().teams
    }

    pub async fn team(&self, id: &str) -> teams_proto::Team {
        let teams = self.listed().await;
        teams
            .into_iter()
            .find(|team| team.id == id)
            .unwrap_or_else(|| panic!("{id} isn't listed"))
    }

    /// The engine's stored teams once they satisfy `ready`.
    pub async fn stored_teams(&self, ready: impl Fn(&[teams_store::Team]) -> bool) -> Vec<teams_store::Team> {
        for _ in 0..400 {
            if let Some(teams) = self.engine().teams().await.unwrap()
                && ready(&teams)
            {
                return teams;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the engine's teams never settled");
    }

    /// Makes a team through the engine; its thread and group.
    pub async fn make_team(&self, name: &str, public: bool) -> (String, String) {
        let create = TeamWrite::Create(NewTeam {
            name: name.into(),
            description: format!("{name}'s people"),
            first_channel: "General".into(),
            public,
        });
        let written = self.engine().team_write(create, None).await.unwrap();
        let (thread, group) = written.created.unwrap();
        (thread, group.unwrap())
    }

    /// Puts `people` on the team, as members.
    pub async fn add(&self, team: &str, group: &str, people: &[&Seat]) {
        let add = TeamWrite::AddMembers {
            team: team.into(),
            group: group.into(),
            members: people
                .iter()
                .map(|seat| (Mri::new(&seat.mri), TeamRole::Member))
                .collect(),
        };
        let written = self.engine().team_write(add, None).await.unwrap();
        assert!(written.not_added.is_empty(), "{written:?}");
    }

    /// Makes a standard channel through the engine; its thread.
    pub async fn make_channel(&self, team: &str, group: &str, name: &str, show_by_default: bool) -> String {
        let draft = teams_core::ChannelDraft {
            channel: new_channel(team, group, name, teams_proto::NewChannelKind::Standard),
            show_by_default,
        };
        let made = self.engine().create_channel(draft).await.unwrap();
        assert!(!made.not_shown_by_default);
        made.thread.unwrap()
    }

    /// Makes a private channel through the engine; its thread and its group.
    pub async fn make_private(&self, team: &str, group: &str, name: &str) -> (String, String) {
        let draft = teams_core::ChannelDraft {
            channel: new_channel(team, group, name, teams_proto::NewChannelKind::Private),
            show_by_default: false,
        };
        let made = self.engine().create_channel(draft).await.unwrap();
        (made.thread.unwrap(), made.group.unwrap())
    }

    /// The outbox row of a message written to a chat, a channel or a post's chain.
    pub async fn post(&self, conversation: &str, text: &str) -> OutboxEntry {
        let send = Action::Send {
            conversation: conversation.into(),
            text: MessageText::plain(text).unwrap(),
        };
        self.engine().write(send).await.unwrap()
    }

    /// A message that was taken; its id.
    pub async fn send(&self, conversation: &str, text: &str) -> String {
        let row = self.post(conversation, text).await;
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
        row.message_id.unwrap()
    }

    /// A write the engine sends that the service takes.
    pub async fn write(&self, action: Action) {
        let row = self.engine().write(action).await.unwrap();
        assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    }

    /// The ids of the stored messages of a conversation, the newest first.
    pub async fn stored_ids(&self, conversation: &str) -> Vec<String> {
        let page = self.engine().messages(conversation, None, 100).await.unwrap();
        page.messages.into_iter().map(|message| message.id).collect()
    }

    /// Sends `method path` as the account, with the token of the path's service: the path starts with `{MT}`,
    /// `{PROV}`, `{CSA}` or `{CHAT}`, and `names` fill its other `{names}` and the body's.
    pub async fn call(&self, method: &str, path: &str, body: &str, names: &Names) -> (u16, Value) {
        let (gtm, audience, under) = match &path[..=path.find('}').unwrap()] {
            "{MT}" => (MIDDLE_TIER_GTM, MIDDLE_TIER_AUDIENCE, "/beta"),
            "{PROV}" => (PROVISIONING_GTM, PROVISIONING_AUDIENCE, ""),
            "{CSA}" => (CSA_GTM, CSA_AUDIENCE, ""),
            "{CHAT}" => (teams_auth::authz::CHAT_BASE_GTM, IC3_AUDIENCE, "/v1"),
            other => panic!("no service for {other}"),
        };
        let base = self.session.service_base(gtm).unwrap();
        let base = format!("{}{}{under}", self.endpoint, base.strip_prefix(&self.origin).unwrap());
        let path = names.fill(&path[path.find('}').unwrap() + 1..]);
        let token = self.session.access_token(audience).await.unwrap();
        let mut request = super::super::support::http_client()
            .request(Method::from_bytes(method.as_bytes()).unwrap(), format!("{base}{path}"))
            .bearer_auth(token.token.expose());
        if !body.is_empty() {
            request = request.json(&serde_json::from_str::<Value>(&names.fill(body)).unwrap());
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
    }
}

/// The values a request's `{names}` stand for.
#[derive(Default)]
pub struct Names(Vec<(String, String)>);

impl Names {
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.0.push((format!("{{{name}}}"), value.to_owned()));
        self
    }

    fn fill(&self, text: &str) -> String {
        self.0
            .iter()
            .fold(text.to_owned(), |text, (name, value)| text.replace(name, value))
    }
}

/// The transport as the account bring-up builds it: every base from authz's GTMs.
fn transport(session: &Arc<Session<MemorySecretStore>>, origin: &str, endpoint: &str, mri: &str) -> Transport {
    let base = |gtm: &str| {
        session
            .service_base(gtm)
            .unwrap_or_else(|| panic!("authz names no {gtm}"))
    };
    HttpTransport::for_cloud_with_mock_origin(
        Arc::clone(session),
        &base(teams_auth::authz::CHAT_BASE_GTM),
        Cloud::own(origin).unwrap(),
        endpoint,
    )
    .unwrap()
    .for_account(Mri::new(mri))
    .with_middle_tier(&base(MIDDLE_TIER_GTM))
    .unwrap()
    .with_csa(&base(CSA_GTM))
    .unwrap()
    .with_provisioning(&base(PROVISIONING_GTM))
    .unwrap()
    .with_user_profile(&base(USER_PROFILE_GTM))
    .unwrap()
}

/// The nodes' logs, when [`LOG_DIR`] is set: one file a start.
struct Logs {
    dir: Option<PathBuf>,
    starts: u32,
}

impl Logs {
    async fn start(&mut self, node: &mut Node, name: &str) {
        self.starts += 1;
        let Some(dir) = &self.dir else {
            return node.start().await;
        };
        std::fs::create_dir_all(dir).unwrap();
        let log = dir.join(format!("node-{name}-start-{}.log", self.starts));
        node.start_logged(&log).await;
    }
}

/// Real server processes on one database with accounts a, b and c the operator added: one node on SQLite; two on
/// Postgres and Redis, a and c at the first node and b at the second.
pub struct Rig {
    pub seats: [Seat; 3],
    pub first: Node,
    pub second: Option<Node>,
    logs: Logs,
    _deployment: Deployment,
}

impl Rig {
    pub async fn start(backend: Backend, flow: &str) -> Self {
        Self::start_configured(backend, flow, "").await
    }

    pub async fn start_configured(backend: Backend, flow: &str, extra: &str) -> Self {
        eprintln!("real-binary teams, {flow}: {backend:?}");
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let mut first = deployment.node("a");
        append_config(&first, extra);
        if !extra.is_empty() {
            first.log_filter = "info".into();
            first.role = Some(("api", None));
        }
        first.cli(&["migrate"], "");
        for upn in ["a@own.test", "b@own.test", "c@own.test"] {
            first.add_user(upn, PASSWORD);
        }
        if !extra.is_empty() {
            first.cli(
                &["user", "add", "qa-admin@own.test", "--name", "QA Admin", "--admin"],
                &format!("{PASSWORD}\n"),
            );
        }
        let mut logs = Logs {
            dir: std::env::var_os(LOG_DIR).map(|dir| Path::new(&dir).join(format!("{flow}-{backend:?}"))),
            starts: 0,
        };
        logs.start(&mut first, "a").await;
        let mut second = (backend == Backend::Postgres).then(|| deployment.node("b"));
        if let Some(node) = &mut second {
            append_config(node, extra);
            if !extra.is_empty() {
                node.log_filter = "info".into();
                node.role = Some(("api", None));
            }
            logs.start(node, "b").await;
        }
        let origin = &deployment.origin;
        let a = Seat::sign_in(&first, origin, "a@own.test").await;
        let b = Seat::sign_in(second.as_ref().unwrap_or(&first), origin, "b@own.test").await;
        let c = Seat::sign_in(&first, origin, "c@own.test").await;
        Self {
            seats: [a, b, c],
            first,
            second,
            logs,
            _deployment: deployment,
        }
    }

    pub async fn stop_first(&mut self) {
        self.first.stop().await;
    }

    pub async fn start_first(&mut self) {
        self.logs.start(&mut self.first, "a").await;
    }

    /// Every node stopped, then started again.
    pub async fn restart_nodes(&mut self) {
        self.first.stop().await;
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.logs.start(&mut self.first, "a").await;
        if let Some(node) = &mut self.second {
            self.logs.start(node, "b").await;
        }
    }

    pub async fn stop(mut self) {
        for seat in &mut self.seats {
            seat.engine.take().unwrap().shutdown();
        }
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
    }
}

fn append_config(node: &Node, extra: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new().append(true).open(&node.config).unwrap();
    file.write_all(extra.as_bytes()).unwrap();
}
