//! Binary QA for outgoing webhooks with synthetic accounts and loopback receivers.

mod delivery;
use crate::outgoing::receiver;
mod registry;

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};
use teams_proto::Request;
use teams_store::Message;

use super::teams::support::{Names, Rig, Seat};
use crate::chatsvc::activity::RawPost;
use crate::support::Backend;
use receiver::Receiver;

const OPEN: &str = "\n[bots]\nallow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\n";
const APOLOGY: &str = "Sorry, there was a problem encountered with your request";

struct Fixture {
    rig: Rig,
    team: String,
    group: String,
    channel: String,
}

impl Fixture {
    async fn new(backend: Backend, flow: &str, extra: &str) -> Self {
        let rig = Rig::start_configured(backend, flow, extra).await;
        let [owner, member, _] = &rig.seats;
        let (team, group) = owner.make_team("QA webhook team (synthetic)", false).await;
        owner.add(&team, &group, &[member]).await;
        let channel = owner.make_channel(&team, &group, "QA channel", true).await;
        Self {
            rig,
            team,
            group,
            channel,
        }
    }

    fn path(&self) -> String {
        format!("{{MT}}/teams/{}/outgoingWebhooks", self.team)
    }

    async fn call(&self, who: usize, method: &str, suffix: &str, body: Option<Value>) -> (u16, Value) {
        call(&self.rig.seats[who], method, &format!("{}{suffix}", self.path()), body).await
    }

    async fn create(&self, receiver: &Receiver) -> (String, String) {
        let (status, made) = self.call(0, "POST", "", Some(form(&receiver.url))).await;
        assert_eq!(status, 201, "the owner can create the webhook");
        let token = made["value"]["securityToken"].as_str().unwrap().to_owned();
        receiver.trust(base64::engine::general_purpose::STANDARD.decode(&token).unwrap());
        (made["value"]["id"].as_str().unwrap().to_owned(), token)
    }

    async fn members_manage(&self, permitted: bool) {
        let owner = &self.rig.seats[0];
        let raw = owner.engine().team_settings(&self.team).await.unwrap().unwrap();
        let mut settings: Value = serde_json::from_str(&raw).unwrap();
        settings["userSettings"]["uploadCustomApp"] = json!(permitted);
        owner
            .engine()
            .team_write(
                teams_proto::TeamWrite::Settings {
                    team: self.team.clone(),
                    settings: settings.to_string(),
                },
                None,
            )
            .await
            .unwrap();
    }

    async fn stop(self, secrets: &[String]) {
        self.rig.stop().await;
        Self::check_logs(secrets);
    }

    fn check_logs(secrets: &[String]) {
        let Some(root) = std::env::var_os(super::teams::support::LOG_DIR) else {
            return;
        };
        check_log_tree(std::path::Path::new(&root), secrets);
    }
}

fn check_log_tree(path: &std::path::Path, secrets: &[String]) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            check_log_tree(&path, secrets);
        } else if path.extension().is_some_and(|extension| extension == "log") {
            let log = std::fs::read_to_string(&path).unwrap();
            for secret in secrets {
                assert!(!log.contains(secret), "a secret occurs in {}", path.display());
            }
        }
    }
}

fn form(url: &str) -> Value {
    json!({"name": "QA Echo", "description": "Synthetic QA receiver", "callbackUrl": url})
}

async fn call(seat: &Seat, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    seat.call(
        method,
        path,
        &body.map_or_else(String::new, |body| body.to_string()),
        &Names::default(),
    )
    .await
}

fn mention_body(hook: &str, words: &str) -> Value {
    let mentions = json!([{"@type":"http://schema.skype.com/Mention", "itemid":"0", "mri":format!("28:{hook}"),
        "mentionType":"person", "displayName":"QA Echo"}]);
    json!({"messagetype":"RichText/Html", "contenttype":"Text",
        "clientmessageid":uuid::Uuid::new_v4().as_u128().to_string(),
        "content":format!("<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">QA Echo</span> {words}</p>"),
        "properties":{"mentions":mentions.to_string()}})
}

async fn mention(seat: &Seat, conversation: &str, hook: &str, words: &str) -> String {
    let answer = seat
        .client
        .write(RawPost {
            chat: conversation.into(),
            body: mention_body(hook, words),
        })
        .await
        .unwrap();
    assert_eq!(answer.status(), 201);
    let body: Value = serde_json::from_slice(answer.body()).unwrap();
    body["OriginalArrivalTime"]
        .as_str()
        .map_or_else(|| body["OriginalArrivalTime"].to_string(), str::to_owned)
}

async fn replies(seat: &Seat, channel: &str, post: &str, hook: &str, count: usize) -> Vec<Message> {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let found = thread(seat, channel, post, hook).await;
        if found.len() >= count || Instant::now() > deadline {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn thread(seat: &Seat, channel: &str, post: &str, hook: &str) -> Vec<Message> {
    seat.engine().sync_conversation(channel).await.unwrap();
    let mut found = Vec::new();
    for (id, root) in seat.engine().reply_roots(channel).await.unwrap() {
        if root == post && id != post {
            let message = seat.engine().message(channel, &id).await.unwrap().unwrap();
            if message.sender.as_deref() == Some(format!("28:{hook}").as_str()) {
                found.push(message);
            }
        }
    }
    found.sort_by_key(|message| (message.arrival_time, message.id.clone()));
    found
}

struct Edit {
    post: RawPost,
    id: String,
}

impl Request for Edit {
    fn service(&self) -> teams_proto::Service {
        teams_proto::Service::ChatService
    }
    fn method(&self) -> teams_proto::Method {
        teams_proto::Method::Put
    }
    fn endpoint(&self) -> teams_proto::Endpoint {
        teams_proto::Endpoint::new([
            "v1",
            "users",
            "ME",
            "conversations",
            &self.post.chat,
            "messages",
            &self.id,
        ])
    }
    fn body(&self) -> Option<Value> {
        Some(self.post.body.clone())
    }
    fn is_write(&self) -> bool {
        true
    }
}
