//! Workflows with incoming webhooks as their users and an outside sender meet them: real server processes (one node
//! on SQLite; two on Postgres and Redis, where the sender posts to the first node and the readers' clients run on the
//! second), operator-made synthetic accounts with odd names, each client's engine, store and push channel, and
//! plain HTTP posts to the webhook link with no credential. What a client shows came over its push channel.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use teams_core::engine::{Action, Event, MessageChange, Source};
use teams_proto::{
    MemberUpdate, Mri, NewChannelKind, NewTeam, Principal, TeamRole, TeamWrite, WorkflowFailure, WorkflowRequest,
};

use super::push::support::{OWN_REDIS_ENV, OwnRedis, User, options};
use super::support::{Deployment, Node, http_client};
use crate::support::{Backend, CLIENT_ID, PASSWORD, backends, redis_url};
use crate::teams::new_channel;
use crate::workflows::{CHANNEL_TEMPLATE, CHAT_TEMPLATE, Target, save_body};

const BOT: &str = "28:358f0194-6b0e-4dd3-af35-c24fe8a9ec87";
const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
/// Where the nodes' logs go, when set (a directory per test and backend); else into the deployment's directory.
const LOG_DIR: &str = "NGTS_QA_LOG_DIR";
/// Full enough that a secret the server logged anywhere would be in it.
const LOG_FILTER: &str = "debug,ngteams_server=trace,tower_http=trace,axum=trace";
/// Synthetic people with odd names, and how many more the big team gets.
const PEOPLE: [(&str, &str); 4] = [
    ("ada@own.test", "Ada Ö'Brien-Łukasiewicz 🛰"),
    ("bert@own.test", "Bert <b>&amp; \"Quoted\"</b>"),
    ("cora@own.test", "Cora 山田 — ✓"),
    ("dan@own.test", "Dan\u{200b}Zero Width"),
];
const CROWD: usize = 24;

/// Real server processes on one database: one node on SQLite, two on Postgres and Redis, each logging fully.
struct Rig {
    origin: String,
    first: Node,
    second: Option<Node>,
    logs: PathBuf,
    starts: u32,
    /// The ids of the crowd of extra members, made by the operator.
    crowd: Vec<String>,
    _deployment: Deployment,
}

impl Rig {
    async fn start(backend: Backend, test: &str, config: &str) -> Self {
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        Self::start_on(backend, test, config, redis).await
    }

    /// As [`Rig::start`], the nodes on the Redis at `redis`.
    async fn start_on(backend: Backend, test: &str, config: &str, redis: Option<String>) -> Self {
        eprintln!("real-binary workflows, {test}: {backend:?}");
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let logs = std::env::var_os(LOG_DIR).map_or_else(
            || deployment.dir.path().join("logs"),
            |dir| Path::new(&dir).join(format!("{test}-{backend:?}")),
        );
        std::fs::create_dir_all(&logs).unwrap();
        let first = node(&deployment, "a", config);
        first.cli(&["migrate"], "");
        for (upn, name) in PEOPLE {
            add_user(&first, upn, name);
        }
        let crowd = (0..if test == "posts" { CROWD } else { 0 })
            .map(|n| {
                add_user(
                    &first,
                    &format!("crowd{n:02}@own.test"),
                    &format!("Crowd #{n:02} «Ünïcode» 🧪"),
                )
            })
            .collect();
        let second = (backend == Backend::Postgres).then(|| node(&deployment, "b", config));
        let mut rig = Self {
            origin: deployment.origin.clone(),
            first,
            second,
            logs,
            starts: 0,
            crowd,
            _deployment: deployment,
        };
        rig.start_nodes().await;
        rig
    }

    async fn start_nodes(&mut self) {
        self.starts += 1;
        let log = self.logs.join(format!("node-a-start-{}.log", self.starts));
        self.first.start_logged(&log).await;
        if let Some(node) = &mut self.second {
            let log = self.logs.join(format!("node-b-start-{}.log", self.starts));
            node.start_logged(&log).await;
        }
    }

    async fn stop_nodes(&mut self) {
        if let Some(node) = &mut self.second {
            node.stop().await;
        }
        self.first.stop().await;
    }

    /// The node the readers' clients talk to: the other node when there are two.
    fn other(&self) -> &Node {
        self.second.as_ref().unwrap_or(&self.first)
    }

    /// `upn`'s running client at `node`, reaching the team services, connected and registered.
    async fn user(&self, node: &Node, upn: &str) -> User {
        let mut user = User::signed_in(node, &self.origin, upn, options()).await;
        user.teams = true;
        user.start();
        user.online(0).await;
        user
    }

    /// Posts `body` to `link` at `node` as an outside system does, with no credential: the status and Retry-After.
    async fn post_at(&self, node: &Node, link: &str, body: &[u8]) -> (u16, Option<String>) {
        let local = link.replacen(&self.origin, &node.endpoint, 1);
        assert_ne!(local, link, "the link names the public origin");
        let response = http_client()
            .post(local)
            .header("content-type", "application/json")
            .body(body.to_vec())
            .send()
            .await
            .unwrap();
        let retry = response
            .headers()
            .get("retry-after")
            .map(|value| value.to_str().unwrap().to_owned());
        (response.status().as_u16(), retry)
    }

    /// Posts `body` to `link` at the first node: its status.
    async fn post(&self, link: &str, body: &Value) -> u16 {
        self.post_at(&self.first, link, body.to_string().as_bytes()).await.0
    }

    /// Stops the nodes; no node log holds a webhook secret of `links`.
    async fn stop(mut self, links: &[&str]) {
        self.stop_nodes().await;
        no_secret_logged(&self.logs, links);
    }
}

fn node(deployment: &Deployment, name: &str, config: &str) -> Node {
    let mut node = deployment.node(name);
    node.log_filter = LOG_FILTER.into();
    let text = std::fs::read_to_string(&node.config).unwrap();
    std::fs::write(&node.config, format!("{text}{config}\n")).unwrap();
    node
}

/// Adds `upn` named `name` on the command line; its id.
fn add_user(node: &Node, upn: &str, name: &str) -> String {
    let added = node.cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
    let id = added.trim().strip_prefix(&format!("added {upn} (")).unwrap();
    id.strip_suffix(')').unwrap().to_owned()
}

/// The webhook secret (`sig`) of `link`.
fn secret(link: &str) -> String {
    let link = url::Url::parse(link).unwrap();
    let (_, sig) = link.query_pairs().find(|(key, _)| key == "sig").unwrap();
    sig.into_owned()
}

fn no_secret_logged(logs: &Path, links: &[&str]) {
    let mut read = 0;
    for entry in std::fs::read_dir(logs).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        read += text.len();
        for link in links {
            let sig = secret(link);
            assert!(!text.contains(&sig), "{} holds a webhook secret", path.display());
            let encoded: String = url::form_urlencoded::byte_serialize(sig.as_bytes()).collect();
            assert!(!text.contains(&encoded), "{} holds a webhook secret", path.display());
        }
    }
    assert!(read > 0, "the nodes logged nothing at {LOG_FILTER}");
}

async fn ask(user: &User, request: WorkflowRequest) -> Result<Value, WorkflowFailure> {
    user.engine().workflow_request(request).await.unwrap()
}

fn rows(answer: &Value) -> &[Value] {
    answer["value"].as_array().map_or(&[], Vec::as_slice)
}

/// A user's Workflows, as the dialog and the Overview reach them: its environment.
struct Flows<'a> {
    user: &'a User,
    environment: String,
}

impl<'a> Flows<'a> {
    async fn of(user: &'a User) -> Self {
        let answer = ask(user, WorkflowRequest::Environments).await.unwrap();
        let environment = rows(&answer)[0]["name"].as_str().unwrap().to_owned();
        Self { user, environment }
    }

    fn env(&self) -> String {
        self.environment.clone()
    }

    /// The card editor's Teams connection: a connected one, else one made behind the scenes.
    async fn connection(&self) -> String {
        let listed = ask(
            self.user,
            WorkflowRequest::Connections {
                environment: self.env(),
            },
        )
        .await
        .unwrap();
        if let Some(found) = rows(&listed).iter().find(|connection| {
            connection["properties"]["apiId"] == "/providers/Microsoft.PowerApps/apis/shared_teams"
                && connection["properties"]["statuses"][0]["status"] == "Connected"
        }) {
            return found["name"].as_str().unwrap().to_owned();
        }
        let name = format!("shared-teams-{}", uuid::Uuid::new_v4().simple());
        let authorize = WorkflowRequest::AuthorizeConnection {
            environment: self.env(),
            connector: "shared_teams".into(),
            connection: name.clone(),
        };
        assert_eq!(ask(self.user, authorize).await.unwrap()["name"], name.as_str());
        name
    }

    /// Picks the webhook template in the gallery, fills the card editor and saves: the workflow's id.
    async fn save(&self, target: &Target<'_>) -> String {
        let id = match target {
            Target::Channel { .. } => CHANNEL_TEMPLATE,
            Target::Chat(_) => CHAT_TEMPLATE,
        };
        let template = ask(self.user, WorkflowRequest::Template { id: id.into() })
            .await
            .unwrap();
        let properties = save_body(&template, target, &self.connection().await);
        let create = WorkflowRequest::Create {
            environment: self.env(),
            properties,
        };
        let made = ask(self.user, create).await.unwrap();
        made["name"].as_str().unwrap().to_owned()
    }

    /// "Copy webhook link".
    async fn link(&self, flow: &str) -> String {
        let address = WorkflowRequest::WebhookAddress {
            environment: self.env(),
            flow: flow.into(),
            trigger: "manual".into(),
        };
        let answer = ask(self.user, address).await.unwrap();
        let link = answer
            .get("value")
            .or_else(|| answer.pointer("/response/value"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert!(link.starts_with("https://"), "{link}");
        link
    }

    /// The run history, newest first: each status and, for a failure, its code.
    async fn runs(&self, flow: &str) -> Vec<(String, Option<String>)> {
        let history = WorkflowRequest::History {
            environment: self.env(),
            flow: flow.into(),
        };
        let answer = ask(self.user, history).await.unwrap();
        rows(&answer)
            .iter()
            .map(|run| {
                let properties = &run["properties"];
                let code = properties["code"].as_str().map(str::to_owned);
                (properties["status"].as_str().unwrap().to_owned(), code)
            })
            .collect()
    }

    async fn read(&self, flow: &str) -> Result<Value, WorkflowFailure> {
        let get = WorkflowRequest::Get {
            environment: self.env(),
            flow: flow.into(),
        };
        ask(self.user, get).await
    }

    async fn turn(&self, flow: &str, enabled: bool) {
        let set = WorkflowRequest::SetEnabled {
            environment: self.env(),
            flow: flow.into(),
            enabled,
        };
        ask(self.user, set).await.unwrap();
    }

    async fn owners(&self, flow: &str) -> Vec<String> {
        let owners = WorkflowRequest::Owners {
            environment: self.env(),
            flow: flow.into(),
        };
        let answer = ask(self.user, owners).await.unwrap();
        let mut ids: Vec<String> = rows(&answer)
            .iter()
            .map(|row| row["properties"]["principal"]["id"].as_str().unwrap().to_owned())
            .collect();
        ids.sort();
        ids
    }
}

/// The Teams Workflows payload carrying one card of `kind` that says `marker`.
fn payload(kind: &str, marker: &str) -> Value {
    let content = match kind {
        "hero" | "thumbnail" => json!({
            "title": marker, "subtitle": "Synthetic build bot", "text": "Nightly <i>build</i> & tests",
            "images": [{"url": "https://img.example.test/build.png"}],
            "buttons": [{"type": "openUrl", "title": "Open", "value": "https://ci.example.test/1042"}],
        }),
        _ => adaptive(marker),
    };
    let content_type = match kind {
        "hero" => "application/vnd.microsoft.card.hero",
        "thumbnail" => "application/vnd.microsoft.card.thumbnail",
        _ => ADAPTIVE,
    };
    json!({"type": "message", "attachments": [{"contentType": content_type, "contentUrl": null, "content": content}]})
}

/// An Adaptive Card that says `marker`.
fn adaptive(marker: &str) -> Value {
    json!({
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "type": "AdaptiveCard", "version": "1.4",
        "body": [
            {"type": "TextBlock", "text": marker, "weight": "bolder", "wrap": true},
            {"type": "FactSet", "facts": [{"title": "Build", "value": "1042"}, {"title": "Branch", "value": "main 🚀"}]},
            {"type": "ColumnSet", "columns": [{"type": "Column", "items": [{"type": "TextBlock", "text": "Ünïcode ✓"}]}]},
        ],
        "actions": [{"type": "Action.OpenUrl", "title": "Open", "url": "https://ci.example.test/1042"}],
    })
}

/// Whether the stored message's text or cards say `marker`.
fn says(message: &teams_store::Message, marker: &str) -> bool {
    if let Some(cards) = teams_proto::chat::card::card_message(&message.content) {
        assert!(!cards.unshown, "a card the client can't draw: {}", message.content);
        if teams_proto::chat::card::plain_words(&cards)
            .iter()
            .any(|word| word.contains(marker))
        {
            return true;
        }
    }
    message.content.contains(marker)
}

/// Waits until `user`'s client was pushed, since `mark`, a new message from the Workflows bot in `conversation` that
/// says `marker`; the message as stored.
async fn hears(user: &mut User, mark: usize, conversation: &str, marker: &str) -> teams_store::Message {
    let mut from = mark;
    loop {
        let at = user
            .since(from, &format!("the Workflows post {marker:?}"), |event| {
                matches!(event, Event::Message { conversation: of, change: MessageChange::New,
                    source: Source::Delivery | Source::Replay, .. } if of == conversation)
            })
            .await;
        let Event::Message { id, .. } = &user.log[at] else {
            unreachable!("picked a message")
        };
        let id = id.clone();
        if let Some(message) = user.engine().message(conversation, &id).await.unwrap()
            && message.sender.as_deref() == Some(BOT)
            && says(&message, marker)
        {
            assert_eq!(message.sender_name.as_deref(), Some("Workflows"), "{}", user.upn);
            return message;
        }
        from = at + 1;
    }
}

/// After a quiet stretch, `user`'s client holds no Workflows post in `conversation` that says `marker`.
async fn never_shows(user: &mut User, conversation: &str, marker: &str) {
    user.settle().await;
    let page = user.engine().messages(conversation, None, 200).await.unwrap();
    let shown = page
        .messages
        .iter()
        .any(|message| message.sender.as_deref() == Some(BOT) && says(message, marker));
    assert!(!shown, "{}: {marker:?} was posted", user.upn);
}

/// A team of `owner`'s with odd names, `people` and the crowd on it, and a second channel; its thread, group and
/// that channel's thread.
async fn team(owner: &User, people: &[&User], crowd: &[String]) -> (String, String, String) {
    let create = TeamWrite::Create(NewTeam {
        name: "Ops «Night» & <Day> \"Release\" 🚀".into(),
        description: "Synthetic team for workflow QA".into(),
        first_channel: "General".into(),
        public: false,
    });
    let written = owner.engine().team_write(create, None).await.unwrap();
    let (thread, group) = written.created.unwrap();
    let group = group.unwrap();
    let members = people
        .iter()
        .map(|user| user.mri.clone())
        .chain(crowd.iter().map(|id| Mri::new(format!("8:orgid:{id}"))))
        .map(|mri| (mri, TeamRole::Member))
        .collect();
    let add = TeamWrite::AddMembers {
        team: thread.clone(),
        group: group.clone(),
        members,
    };
    let written = owner.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
    let draft = teams_core::ChannelDraft {
        channel: new_channel(
            &thread,
            &group,
            "Alerts — night shift 🚨 ünïcødé",
            NewChannelKind::Standard,
        ),
        show_by_default: true,
    };
    let made = owner.engine().create_channel(draft).await.unwrap();
    (thread, group, made.thread.unwrap())
}

async fn group_chat(owner: &User, people: &[&User]) -> String {
    let create = Action::CreateChat {
        others: people.iter().map(|user| user.mri.clone()).collect(),
        topic: Some("Deploys 🛠 «prod» & <staging>".into()),
    };
    owner.write(create).await.message_id.unwrap()
}

/// Ada's team (Bert, Cora and the crowd on it) and her group chat with Bert and Cora, each with a webhook workflow of
/// Ada's saved from her client; the readers' clients run on the other node.
struct Scene {
    ada: User,
    bert: User,
    cora: User,
    environment: String,
    channel: String,
    chat: String,
    channel_flow: String,
    chat_flow: String,
    channel_link: String,
    chat_link: String,
}

impl Scene {
    async fn set(rig: &Rig) -> Self {
        let ada = rig.user(&rig.first, "ada@own.test").await;
        let bert = rig.user(rig.other(), "bert@own.test").await;
        let cora = rig.user(rig.other(), "cora@own.test").await;
        let (_, group, channel) = team(&ada, &[&bert, &cora], &rig.crowd).await;
        let chat = group_chat(&ada, &[&bert, &cora]).await;
        let flows = Flows::of(&ada).await;
        let catalog = ask(&ada, WorkflowRequest::Catalog).await.unwrap();
        assert!(rows(&catalog).iter().any(|row| row["id"] == CHANNEL_TEMPLATE));
        let channel_flow = flows
            .save(&Target::Channel {
                group: &group,
                channel: &channel,
            })
            .await;
        let chat_flow = flows.save(&Target::Chat(&chat)).await;
        let channel_link = flows.link(&channel_flow).await;
        let chat_link = flows.link(&chat_flow).await;
        assert!(channel_link.starts_with(&rig.origin) && chat_link.starts_with(&rig.origin));
        let environment = flows.env();
        Self {
            ada,
            bert,
            cora,
            environment,
            channel,
            chat,
            channel_flow,
            chat_flow,
            channel_link,
            chat_link,
        }
    }

    fn flows(&self) -> Flows<'_> {
        Flows {
            user: &self.ada,
            environment: self.environment.clone(),
        }
    }

    /// Every member's running client is pushed the post that says `marker` in `conversation`.
    async fn everyone_hears(&mut self, mark: [usize; 3], conversation: &str, marker: &str) -> teams_store::Message {
        hears(&mut self.ada, mark[0], conversation, marker).await;
        hears(&mut self.bert, mark[1], conversation, marker).await;
        hears(&mut self.cora, mark[2], conversation, marker).await
    }

    fn marks(&self) -> [usize; 3] {
        [self.ada.mark(), self.bert.mark(), self.cora.mark()]
    }
}

/// An Adaptive Card, a hero card, a thumbnail card and two cards at once into the channel; a bare Adaptive Card and
/// text into the chat: each answers 202 and reaches every member's client live, from "Workflows".
async fn valid_posts_land(rig: &Rig, scene: &mut Scene) {
    let channel = scene.channel.clone();
    for (kind, marker) in [
        ("adaptive", "QA-adaptive-1042"),
        ("hero", "QA-hero-1043"),
        ("thumbnail", "QA-thumbnail-1044"),
    ] {
        let mark = scene.marks();
        assert_eq!(
            rig.post(&scene.channel_link, &payload(kind, marker)).await,
            202,
            "{kind}"
        );
        let message = scene.everyone_hears(mark, &channel, marker).await;
        assert_eq!(message.message_type, "RichText/Media_Card", "{kind}");
    }
    two_cards_land(rig, scene).await;
}

/// Two cards in one post: two messages.
async fn two_cards_land(rig: &Rig, scene: &mut Scene) {
    let channel = scene.channel.clone();
    let mut two = payload("adaptive", "QA-first-of-two");
    two["attachments"]
        .as_array_mut()
        .unwrap()
        .push(payload("adaptive", "QA-second-of-two")["attachments"][0].clone());
    let mark = scene.marks();
    assert_eq!(rig.post(&scene.channel_link, &two).await, 202);
    scene.everyone_hears(mark, &channel, "QA-first-of-two").await;
    scene.everyone_hears(mark, &channel, "QA-second-of-two").await;
}

/// A bare Adaptive Card and text into the chat.
async fn chat_posts_land(rig: &Rig, scene: &mut Scene) {
    let chat = scene.chat.clone();
    let mark = scene.marks();
    assert_eq!(rig.post(&scene.chat_link, &adaptive("QA-bare-1045")).await, 202);
    scene.everyone_hears(mark, &chat, "QA-bare-1045").await;
    let mark = scene.marks();
    let text = json!({"text": "QA-text-1046 deploy <b>started</b> & ✓"});
    assert_eq!(rig.post(&scene.chat_link, &text).await, 202);
    let message = scene.everyone_hears(mark, &chat, "QA-text-1046").await;
    assert!(
        message.content.contains("&lt;b&gt;started&lt;/b&gt;"),
        "{}",
        message.content
    );
}

/// An invalid card, a legacy connector card, eleven cards, not JSON, too big, a wrong secret, no secret: each is
/// refused as the docs say and nobody's client shows it.
async fn refusals(rig: &Rig, scene: &mut Scene) {
    let link = scene.channel_link.clone();
    let mut untyped = adaptive("QA-untyped");
    untyped["body"].as_array_mut().unwrap().push(json!({"text": "no type"}));
    let invalid = json!({"type": "message", "attachments": [{"contentType": ADAPTIVE, "content": untyped}]});
    assert_eq!(rig.post(&link, &invalid).await, 400);
    let legacy = json!({"attachments": [{"contentType": "application/vnd.microsoft.teams.card.o365connector",
                                         "content": {"summary": "QA-legacy"}}]});
    assert_eq!(rig.post(&link, &legacy).await, 400);
    let mut eleven = payload("adaptive", "QA-eleven");
    let card = eleven["attachments"][0].clone();
    for _ in 0..10 {
        eleven["attachments"].as_array_mut().unwrap().push(card.clone());
    }
    assert_eq!(rig.post(&link, &eleven).await, 400);
    assert_eq!(rig.post_at(&rig.first, &link, b"QA-not-json {").await.0, 400);
    // the Overview says "The max payload is 256 KB"
    let big = payload("adaptive", &format!("QA-big {}", "x".repeat(300 * 1024)));
    assert_eq!(rig.post(&link, &big).await, 413);
    bad_links(rig, scene).await;
}

/// A wrong secret, no secret; then nobody's client shows any refused post.
async fn bad_links(rig: &Rig, scene: &mut Scene) {
    let link = scene.channel_link.clone();
    let wrong = link.replace(&format!("sig={}", secret(&link)), "sig=QA-wrong-secret");
    assert_ne!(wrong, link);
    assert_eq!(rig.post(&wrong, &payload("adaptive", "QA-wrong-sig")).await, 404);
    let unsigned = link.split('?').next().unwrap();
    assert_eq!(rig.post(unsigned, &payload("adaptive", "QA-wrong-sig")).await, 404);
    for marker in ["QA-untyped", "QA-eleven", "QA-wrong-sig", "QA-big"] {
        never_shows(&mut scene.bert, &scene.channel, marker).await;
    }
}

/// The Overview's run history: each run with its outcome, newest first.
async fn history(scene: &Scene) {
    let flows = scene.flows();
    let channel = flows.runs(&scene.channel_flow).await;
    let failed = ("Failed".to_owned(), Some("InvalidRequestContent".to_owned()));
    let succeeded = ("Succeeded".to_owned(), None);
    // newest first: four refused bodies (the 413 and the 404s keep no run), then four posts
    let mut expected = vec![failed; 4];
    expected.extend(vec![succeeded.clone(); 4]);
    assert_eq!(channel, expected);
    assert_eq!(flows.runs(&scene.chat_flow).await, [succeeded.clone(), succeeded]);
}

/// Rename, Turn off (the link refuses and runs nothing), Turn on (it posts again), Delete (the link is gone).
async fn manage(rig: &Rig, scene: &mut Scene) {
    let flows = scene.flows();
    let rename = WorkflowRequest::Rename {
        environment: flows.env(),
        flow: scene.chat_flow.clone(),
        name: "Deploy alerts «prod» 🛠".into(),
    };
    let renamed = ask(&scene.ada, rename).await.unwrap();
    assert_eq!(renamed["properties"]["displayName"], "Deploy alerts «prod» 🛠");
    let listed = ask(
        &scene.ada,
        WorkflowRequest::List {
            environment: flows.env(),
        },
    )
    .await
    .unwrap();
    assert!(
        rows(&listed)
            .iter()
            .any(|row| row["properties"]["displayName"] == "Deploy alerts «prod» 🛠")
    );
    off_and_on(rig, scene).await;
    deleted(rig, scene).await;
}

/// Turned off, the link refuses and runs nothing; on again, it posts.
async fn off_and_on(rig: &Rig, scene: &mut Scene) {
    let flows = scene.flows();
    flows.turn(&scene.chat_flow, false).await;
    let before = flows.runs(&scene.chat_flow).await.len();
    assert_eq!(
        rig.post(&scene.chat_link, &payload("adaptive", "QA-while-off")).await,
        409
    );
    assert_eq!(flows.runs(&scene.chat_flow).await.len(), before);
    flows.turn(&scene.chat_flow, true).await;
    let mark = scene.marks();
    assert_eq!(
        rig.post(&scene.chat_link, &payload("adaptive", "QA-on-again")).await,
        202
    );
    let chat = scene.chat.clone();
    scene.everyone_hears(mark, &chat, "QA-on-again").await;
    never_shows(&mut scene.cora, &chat, "QA-while-off").await;
}

/// Deleted, its link is gone.
async fn deleted(rig: &Rig, scene: &mut Scene) {
    let chat = scene.chat.clone();
    let flows = scene.flows();
    let delete = WorkflowRequest::Delete {
        environment: flows.env(),
        flow: scene.chat_flow.clone(),
    };
    ask(&scene.ada, delete).await.unwrap();
    assert_eq!(flows.read(&scene.chat_flow).await, Err(WorkflowFailure::NotFound));
    assert_eq!(
        rig.post(&scene.chat_link, &payload("adaptive", "QA-deleted")).await,
        404
    );
    never_shows(&mut scene.cora, &chat, "QA-deleted").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webhook_posts_reach_every_members_client_live_and_the_run_history_shows_each() {
    for backend in backends() {
        let rig = Rig::start(backend, "posts", "").await;
        let mut scene = Scene::set(&rig).await;
        valid_posts_land(&rig, &mut scene).await;
        chat_posts_land(&rig, &mut scene).await;
        refusals(&rig, &mut scene).await;
        history(&scene).await;
        manage(&rig, &mut scene).await;
        let links = [scene.channel_link.clone(), scene.chat_link.clone()];
        drop(scene);
        rig.stop(&[&links[0], &links[1]]).await;
    }
}

/// Cora's team with Ada, Bert and Dan on it; Ada's workflow in its channel, with Bert and Dan as co-owners.
async fn shared(rig: &Rig) -> (User, User, User, String, String, String) {
    let cora = rig.user(rig.other(), "cora@own.test").await;
    let ada = rig.user(&rig.first, "ada@own.test").await;
    let bert = rig.user(&rig.first, "bert@own.test").await;
    let dan = rig.user(rig.other(), "dan@own.test").await;
    let (team, group, channel) = team(&cora, &[&ada, &bert, &dan], &[]).await;
    let flows = Flows::of(&ada).await;
    let flow = flows
        .save(&Target::Channel {
            group: &group,
            channel: &channel,
        })
        .await;
    let principal = |user: &User| Principal {
        id: user.oid().to_owned(),
        name: user.upn.clone(),
        email: None,
    };
    for co_owner in [&bert, &dan] {
        let add = WorkflowRequest::AddOwner {
            environment: flows.env(),
            flow: flow.clone(),
            owner: principal(co_owner),
        };
        ask(&ada, add).await.unwrap();
    }
    let mut all = vec![ada.oid().to_owned(), bert.oid().to_owned(), dan.oid().to_owned()];
    all.sort();
    assert_eq!(flows.owners(&flow).await, all);
    drop(dan);
    (cora, ada, bert, team, group, flow)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn co_owners_come_and_go_one_by_one_and_one_off_the_team_posts_no_more() {
    for backend in backends() {
        let rig = Rig::start(backend, "owners", "").await;
        let (mut cora, ada, bert, team, group, flow) = shared(&rig).await;
        let flows = Flows::of(&ada).await;
        let link = flows.link(&flow).await;
        let channel = flows.read(&flow).await.unwrap();
        let channel = channel["properties"]["definition"]["parameters"]["teams.channel"]["defaultValue"]
            .as_str()
            .unwrap()
            .to_owned();

        // Ada takes Dan off; Bert, a co-owner, sees the workflow and its link
        let dan = PEOPLE[3].0;
        let owners = flows.owners(&flow).await;
        let dan_id = owners
            .iter()
            .find(|id| *id != ada.oid() && *id != bert.oid())
            .unwrap()
            .clone();
        let remove = WorkflowRequest::RemoveOwner {
            environment: flows.env(),
            flow: flow.clone(),
            owner: dan_id,
        };
        ask(&ada, remove).await.unwrap();
        assert_eq!(flows.owners(&flow).await.len(), 2, "{dan} is off");
        let berts = Flows::of(&bert).await;
        assert_eq!(berts.link(&flow).await, link);

        // Cora takes Ada off the team: Bert still sees the channel, so the link still posts
        let off = |member: &User| TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: member.mri.clone(),
            update: MemberUpdate::Removed,
        };
        cora.engine().team_write(off(&ada), None).await.unwrap();
        let mark = cora.mark();
        assert_eq!(rig.post(&link, &payload("adaptive", "QA-via-bert")).await, 202);
        hears(&mut cora, mark, &channel, "QA-via-bert").await;

        // and Bert off too: nobody who owns it may post there, so nothing is posted and the run says so
        cora.engine().team_write(off(&bert), None).await.unwrap();
        assert_eq!(rig.post(&link, &payload("adaptive", "QA-nobody")).await, 202);
        never_shows(&mut cora, &channel, "QA-nobody").await;
        let forbidden = ("Failed".to_owned(), Some("Forbidden".to_owned()));
        assert_eq!(flows.runs(&flow).await[0], forbidden);
        drop((cora, ada, bert));
        rig.stop(&[&link]).await;
    }
}

/// Waits until the posts that follow fall in one of Redis' minute windows, which start on the minute.
async fn within_one_minute_window() {
    let second = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            % 60
    };
    while second() > 50 {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_burst_over_the_rate_limit_is_held_off_on_every_node() {
    for backend in backends() {
        let rig = Rig::start(backend, "burst", "[workflows]\nrate_per_minute = 3").await;
        let ada = rig.user(&rig.first, "ada@own.test").await;
        let mut bert = rig.user(rig.other(), "bert@own.test").await;
        let chat = group_chat(&ada, &[&bert]).await;
        let flows = Flows::of(&ada).await;
        let flow = flows.save(&Target::Chat(&chat)).await;
        let link = flows.link(&flow).await;
        within_one_minute_window().await;
        let mut answers = Vec::new();
        for n in 0..5 {
            // alternately at each node: the count is the workflow's, across nodes
            let node = if n % 2 == 0 { &rig.first } else { rig.other() };
            let body = payload("adaptive", &format!("QA-burst-{n}")).to_string();
            answers.push(rig.post_at(node, &link, body.as_bytes()).await);
        }
        let statuses: Vec<u16> = answers.iter().map(|(status, _)| *status).collect();
        assert_eq!(statuses, [202, 202, 202, 429, 429]);
        assert_eq!(answers[3].1.as_deref(), Some("60"));
        hears(&mut bert, 0, &chat, "QA-burst-2").await;
        never_shows(&mut bert, &chat, "QA-burst-3").await;
        assert_eq!(flows.runs(&flow).await.len(), 3);
        drop((ada, bert));
        rig.stop(&[&link]).await;
    }
}

/// The status of a password grant of `upn` for `resource`.
async fn grant(node: &Node, upn: &str, resource: &str) -> u16 {
    let scope = format!("{resource}/.default openid profile offline_access");
    node.http
        .post(format!("{}/oauth2/v2.0/token", node.endpoint))
        .form(&[
            ("client_id", CLIENT_ID),
            ("grant_type", "password"),
            ("username", upn),
            ("password", PASSWORD),
            ("scope", &scope),
        ])
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_workflows_off_there_is_no_flow_token_and_no_webhook() {
    let flow_audience = teams_proto::workflow::WORKFLOW_AUDIENCE;
    let apps_audience = teams_proto::workflow::CONNECTION_AUDIENCE;
    for backend in backends() {
        let mut rig = Rig::start(backend, "off", "").await;
        let ada = rig.user(&rig.first, "ada@own.test").await;
        let bert = rig.user(rig.other(), "bert@own.test").await;
        let chat = group_chat(&ada, &[&bert]).await;
        let flows = Flows::of(&ada).await;
        let flow = flows.save(&Target::Chat(&chat)).await;
        let link = flows.link(&flow).await;
        for resource in [flow_audience, apps_audience] {
            assert_eq!(grant(&rig.first, "ada@own.test", resource).await, 200, "{resource}");
        }
        drop((ada, bert));

        // the operator turns Workflows off on every node and restarts them
        rig.stop_nodes().await;
        for node in std::iter::once(&rig.first).chain(rig.second.as_ref()) {
            let text = std::fs::read_to_string(&node.config).unwrap();
            std::fs::write(&node.config, format!("{text}[workflows]\nenabled = false\n")).unwrap();
        }
        rig.start_nodes().await;
        for resource in [flow_audience, apps_audience] {
            assert_ne!(grant(&rig.first, "ada@own.test", resource).await, 200, "{resource}");
        }
        assert_eq!(
            grant(&rig.first, "ada@own.test", "https://api.spaces.skype.com").await,
            200
        );
        let ada = rig.user(&rig.first, "ada@own.test").await;
        assert_eq!(
            ask(&ada, WorkflowRequest::Environments).await,
            Err(WorkflowFailure::Authentication)
        );
        for node in std::iter::once(&rig.first).chain(rig.second.as_ref()) {
            let body = payload("adaptive", "QA-off").to_string();
            assert_eq!(rig.post_at(node, &link, body.as_bytes()).await.0, 404);
        }
        drop(ada);
        rig.stop(&[&link]).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_redis_down_a_post_is_let_through_and_the_warning_names_no_secret() {
    let Some(redis) = OwnRedis::take().await else {
        eprintln!("skipped: {OWN_REDIS_ENV} names no Redis of the test's own");
        return;
    };
    redis.start().await;
    let rig = Rig::start_on(Backend::Sqlite, "redis-down", "", Some(redis.url.clone())).await;
    let ada = rig.user(&rig.first, "ada@own.test").await;
    let bert = rig.user(&rig.first, "bert@own.test").await;
    let chat = group_chat(&ada, &[&bert]).await;
    let flows = Flows::of(&ada).await;
    let flow = flows.save(&Target::Chat(&chat)).await;
    let link = flows.link(&flow).await;
    redis.stop().await;
    // the rate limit can't count and the chat service can't fan out: the post is let through rather than lost, and
    // answered at once, so a sender that retries a slow answer doesn't post it twice
    let local = link.replacen(&rig.origin, &rig.first.endpoint, 1);
    let started = std::time::Instant::now();
    let answer = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap()
        .post(local)
        .header("content-type", "application/json")
        .body(payload("adaptive", "QA-redis-down").to_string())
        .send()
        .await
        .map(|response| response.status().as_u16());
    let took = started.elapsed();
    assert_eq!(answer.ok(), Some(202), "after {took:?}");
    assert!(took < Duration::from_secs(5), "the post took {took:?}");
    assert_eq!(flows.runs(&flow).await, [("Succeeded".to_owned(), None)]);
    redis.start().await;
    bert.engine().sync().await.unwrap();
    let page = bert.engine().messages(&chat, None, 50).await.unwrap();
    let posted = page
        .messages
        .iter()
        .filter(|message| message.sender.as_deref() == Some(BOT) && says(message, "QA-redis-down"))
        .count();
    assert_eq!(posted, 1);
    drop((ada, bert));
    let logs = rig.logs.clone();
    rig.stop(&[&link]).await;
    // the warning is logged inside the webhook's own span, which names its path and no query
    let log = std::fs::read_to_string(logs.join("node-a-start-1.log")).unwrap();
    let warned = log
        .lines()
        .find(|line| line.contains("can't count a workflow's webhook posts"))
        .expect("the rate limit's warning");
    let path = format!("path=/workflows/automations/direct/workflows/{flow}/triggers/manual/paths/invoke");
    assert!(warned.contains(&path), "{warned}");
    assert!(!warned.contains("sig") && !warned.contains('?'), "{warned}");
}
