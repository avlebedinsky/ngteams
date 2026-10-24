//! CSA's read of the account's teams and channels (M2 Task 16) on wiremock, through the `test-support` mock origin: a
//! `GET` of `api/v3/teams/users/me` with the web client's query on the CSA base the transport was given, with the
//! account's `chatsvcagg` token and nothing else, in the shape `proto-lab probe teams` read live (the synthetic
//! fixture `tests/fixtures/csa/teams.json`); any other CSA read goes to the same base; a CSA read on a transport without
//! a CSA base, and a plain request to CSA that changes anything (CSA is a contacting service), are refused before
//! anything is sent, but for the one read sent as a `POST` the fence's classifier carves out. And what a channel's
//! posts are read with: a channel's thread type and a message's reply chain.
//!
//! Tokens and ids here are placeholders, never real ones.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::chat::{ConversationKind, Message};
use teams_proto::{
    BuildError, CSA_AUDIENCE, Client, Credentials, CsaAnswerError, CsaTeams, Endpoint, HttpError, HttpTransport,
    Method, Read, Refusal, Request, Service,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const CSA_BASE: &str = "https://teams.microsoft.com/api/csa/amer";
const TEAMS_PATH: &str = "/api/csa/amer/api/v3/teams/users/me";
const FIXTURE: &str = include_str!("../../../tests/fixtures/csa/teams.json");
const TEAM: &str = "19:channel0001@thread.tacv2";
const CHANNEL: &str = "19:channel0002@thread.tacv2";

/// The fixture's response body, its placeholders filled in.
fn fixture() -> Value {
    let text = FIXTURE
        .replace("<region>", "amer")
        .replace("<guid:a>", "0000000a-0000-4000-8000-00000000000a")
        .replace("<guid:tenant>", "00000000-0000-4000-8000-0000000000ff")
        .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
        .replace("<token:1>", "made-up-sync-token");
    serde_json::from_str(&text).unwrap()
}

/// Hands out placeholder tokens and records the audiences asked for.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn audiences(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn current(&self, audience: &str) -> Secret {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        Secret::new("placeholder-csa-token")
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(self.current(audience)))
    }
}

/// A client on the mock, with the CSA base when `csa`.
async fn setup(csa: bool) -> (MockServer, Arc<FakeTokens>, Client<HttpTransport<FakeTokens>>) {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new("8:orgid:0000000a-0000-4000-8000-00000000000a")], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let mut transport = HttpTransport::with_mock_origin(Arc::clone(&tokens), fence, CHAT_BASE, &server.uri()).unwrap();
    if csa {
        transport = transport.with_csa(CSA_BASE).unwrap();
    }
    (server, tokens, Client::new(transport))
}

#[tokio::test]
async fn the_teams_read_is_a_get_of_its_probed_shape_with_the_csa_token_and_its_teams_are_read() {
    let (server, tokens, client) = setup(true).await;
    let fixture = fixture();
    Mock::given(method("GET"))
        .and(path(TEAMS_PATH))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture["response"]["body"].to_string(), "application/json"),
        )
        .mount(&server)
        .await;
    let answer = client.read(&CsaTeams).await.unwrap();
    let read = CsaTeams::read_answer(answer.body()).unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method.as_str(), "GET");
    assert!(request.body.is_empty());
    assert_eq!(request.url.path(), TEAMS_PATH);
    assert_eq!(
        request.url.query(),
        fixture["request"]["query"].as_str(),
        "the web client's query, exactly"
    );
    let mut names: Vec<&str> = request.headers.keys().map(wiremock::http::HeaderName::as_str).collect();
    names.sort_unstable();
    assert!(
        names.contains(&"accept") && names.contains(&"authorization"),
        "{names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("x-ms-") || *name == "behavioroverride"),
        "{names:?}"
    );
    assert_eq!(
        request.headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer placeholder-csa-token"
    );
    assert_eq!(tokens.audiences(), [CSA_AUDIENCE]);

    // the deleted team and the deleted channel left out; the general channel first
    assert_eq!(read.skipped, 0);
    let teams: Vec<(&str, &str, bool)> = read
        .teams
        .iter()
        .map(|team| (team.id.as_str(), team.name.as_str(), team.archived))
        .collect();
    assert_eq!(
        teams,
        [
            (TEAM, "A made-up team", false),
            ("19:channel0004@thread.tacv2", "A made-up archived team", true)
        ]
    );
    let channels: Vec<(&str, &str, bool)> = read.teams[0]
        .channels
        .iter()
        .map(|channel| (channel.id.as_str(), channel.name.as_str(), channel.general))
        .collect();
    assert_eq!(
        channels,
        [(TEAM, "General", true), (CHANNEL, "A made-up channel", false)]
    );
    assert_eq!(read.teams[1].channels.len(), 1);
    assert!(read.teams[1].channels[0].general);
    let shown = format!("{read:?}");
    assert!(!shown.contains("made-up team") && !shown.contains("General"), "{shown}");
}

#[test]
fn a_teams_answer_is_read_only_in_its_probed_shape() {
    for body in [&b"[]"[..], b"{}", b"{\"teams\": {}}", b"not json"] {
        assert_eq!(CsaTeams::read_answer(body), Err(CsaAnswerError::Malformed));
    }
    let answer = json!({"metadata": {"isPartialData": false}, "teams": [
        {"id": TEAM, "displayName": "A", "channels": [
            {"id": TEAM, "displayName": "General"},
            {"id": CHANNEL},
            {"displayName": "no id"},
            {"id": "19:x@thread.tacv2", "displayName": "B", "isGeneral": "true"},
        ]},
        {"id": "19:y@thread.tacv2"},
        {"id": 7, "displayName": "C"},
    ]});
    let read = CsaTeams::read_answer(answer.to_string().as_bytes()).unwrap();
    assert_eq!(read.skipped, 4);
    assert_eq!(read.teams.len(), 1);
    let general: Vec<bool> = read.teams[0].channels.iter().map(|channel| channel.general).collect();
    // a channel is general by its id or by a boolean `isGeneral`, never by a string
    assert_eq!(general, [true, false]);
}

#[test]
fn a_teams_answer_that_doesn_t_say_it_is_whole_is_refused() {
    let teams = json!([{"id": TEAM, "displayName": "A", "channels": [{"id": TEAM, "displayName": "General"}]}]);
    let whole = json!({"teams": teams, "metadata": {"isPartialData": false, "hasMoreChats": true}});
    assert_eq!(
        CsaTeams::read_answer(whole.to_string().as_bytes()).unwrap().teams.len(),
        1
    );
    // partial (as a cold boot might answer) or not saying: never taken for the whole list the store replaces its own
    // with
    for metadata in [
        json!({"isPartialData": true}),
        json!({"isPartialData": "false"}),
        json!({"isPartialData": 0}),
        json!({"isPartialData": null}),
        json!({}),
        json!(null),
        json!([]),
    ] {
        let answer = json!({"teams": teams, "metadata": metadata});
        assert_eq!(
            CsaTeams::read_answer(answer.to_string().as_bytes()),
            Err(CsaAnswerError::Malformed),
            "{metadata}"
        );
    }
    let bare = json!({"teams": teams});
    assert_eq!(
        CsaTeams::read_answer(bare.to_string().as_bytes()),
        Err(CsaAnswerError::Malformed)
    );
}

#[tokio::test]
async fn a_csa_read_without_a_csa_base_is_refused_before_anything_is_sent() {
    let (server, tokens, client) = setup(false).await;
    assert!(!client.transport().has_csa());
    match client.read(&CsaTeams).await {
        Err(HttpError::Refused(Refusal::NoCsa)) => {}
        other => panic!("{other:?}"),
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.audiences().is_empty());
}

/// A read of CSA in a shape the probe didn't show: any type may implement `Read`.
struct OtherCsaRead(Endpoint);

impl Read for OtherCsaRead {
    fn service(&self) -> Service {
        Service::Csa
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }
}

/// A plain `POST` to CSA: any type may implement `Request`, so the fence's classifier is what decides it.
struct CsaPost(Endpoint);

impl Request for CsaPost {
    fn service(&self) -> Service {
        Service::Csa
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        self.0.clone()
    }

    fn body(&self) -> Option<Value> {
        Some(json!({}))
    }
}

#[tokio::test]
async fn any_other_csa_read_goes_to_the_csa_base_with_its_query() {
    let (server, _tokens, client) = setup(true).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .mount(&server)
        .await;
    let others = [
        Endpoint::new(["api", "v3", "teams", "users", "me", "updates"]),
        Endpoint::new(["api", "v1", "teams", "users", "me", "pinnedChannels"]),
        Endpoint::new(["api", "v1", "containers", CHANNEL, "posts"]).with_query("pageSize", "20"),
    ];
    for endpoint in others {
        client.read(&OtherCsaRead(endpoint)).await.unwrap();
    }
    let requests = server.received_requests().await.unwrap();
    let urls: Vec<String> = requests
        .iter()
        .map(|request| format!("{}?{}", request.url.path(), request.url.query().unwrap_or_default()))
        .collect();
    assert_eq!(
        urls,
        [
            "/api/csa/amer/api/v3/teams/users/me/updates?".to_owned(),
            "/api/csa/amer/api/v1/teams/users/me/pinnedChannels?".to_owned(),
            format!("/api/csa/amer/api/v1/containers/{CHANNEL}/posts?pageSize=20"),
        ]
    );
    assert!(requests.iter().all(|request| request.method.as_str() == "GET"));
}

#[tokio::test]
async fn a_csa_post_is_refused_unless_the_classifier_carves_it_out() {
    let (server, tokens, client) = setup(true).await;
    // a path segment `..` is refused before anything else
    match client
        .read(&OtherCsaRead(Endpoint::new([
            "api", "v3", "teams", "users", "..", "me",
        ])))
        .await
    {
        Err(HttpError::Refused(Refusal::Segment)) => {}
        other => panic!("{other:?}"),
    }
    for endpoint in [
        CsaTeams.endpoint(),
        Endpoint::new(["api", "v1", "containers", CHANNEL, "posts"]),
        Endpoint::new(["api", "v1", "teams", TEAM, "channels"]),
    ] {
        let shown = format!("{endpoint:?}");
        match client.send(&CsaPost(endpoint)).await {
            Err(HttpError::Refused(Refusal::Unreadable)) => {}
            other => panic!("{shown}: {other:?}"),
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.audiences().is_empty(), "no token is asked for a refused request");

    // the read sent as a `POST` the classifier carves out goes
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("{}", "application/json"))
        .expect(1)
        .mount(&server)
        .await;
    let by_ids = Endpoint::new(["api", "v2", "teams", "users", "me", "chatsAndChannelsByIds"]);
    client.send(&CsaPost(by_ids)).await.unwrap();
}

#[test]
fn only_a_csa_base_of_the_gtm_form_is_taken() {
    assert!(HttpTransport::<FakeTokens>::accepts_csa_base(CSA_BASE));
    assert!(HttpTransport::<FakeTokens>::accepts_csa_base(
        "https://teams.cloud.microsoft/api/csa/emea/"
    ));
    for base in [
        "https://chatsvcagg.teams.microsoft.com/api/csa/amer",
        "http://teams.microsoft.com/api/csa/amer",
        "https://teams.microsoft.com:8443/api/csa/amer",
        "https://teams.microsoft.com/api/csa/AMER",
        "https://teams.microsoft.com/api/csa/amer/api",
        "https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/csa/amer?x=1",
        " https://teams.microsoft.com/api/csa/amer",
        "https://user@teams.microsoft.com/api/csa/amer",
    ] {
        assert!(!HttpTransport::<FakeTokens>::accepts_csa_base(base), "{base}");
    }
    let fence = Arc::new(Fence::new(Roster::new([], []), FenceLog::from_writer(std::io::sink())));
    let transport = HttpTransport::new(Arc::new(FakeTokens::default()), fence, CHAT_BASE).unwrap();
    assert!(matches!(
        transport.with_csa("https://elsewhere.example/api/csa/amer"),
        Err(BuildError::CsaBase)
    ));
}

/// A channel message as the probe saw one: a `conversationLink` ending in `;messageid=<post>`.
fn channel_message(id: &str, link: &str) -> Message {
    serde_json::from_value(json!({
        "id": id,
        "type": "Message",
        "messagetype": "RichText/Html",
        "conversationid": CHANNEL,
        "conversationLink": link,
        "version": "1",
        "content": "<p>made up</p>",
    }))
    .unwrap()
}

#[test]
fn a_channel_message_names_its_post_and_a_channel_thread_is_a_channel() {
    let link = |rest: &str| format!("{CHAT_BASE}/v1/users/ME/conversations/{rest}");
    let post = channel_message("1700000000001", &link(&format!("{CHANNEL};messageid=1700000000001")));
    assert_eq!(post.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    let reply = channel_message("1700000000002", &link(&format!("{CHANNEL};messageid=1700000000001")));
    assert_eq!(reply.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    // percent-encoded as one segment
    let encoded = channel_message(
        "1700000000003",
        &link("19%3Achannel0002%40thread.tacv2%3Bmessageid%3D1700000000001"),
    );
    assert_eq!(encoded.reply_root(CHANNEL).as_deref(), Some("1700000000001"));
    for other in [
        link(CHANNEL),
        link(&format!("{TEAM};messageid=1700000000001")),
        link(&format!("{CHANNEL};messageid=")),
        link(&format!("{CHANNEL};messageid=17x")),
        "not a link".to_owned(),
    ] {
        assert_eq!(channel_message("1", &other).reply_root(CHANNEL), None, "{other}");
    }
    assert_eq!(ConversationKind::of(CHANNEL, Some("topic")), ConversationKind::Channel);
    assert_eq!(ConversationKind::of(TEAM, Some("space")), ConversationKind::Channel);
    assert!(ConversationKind::of(CHANNEL, Some("topic")).is_read_only());
}
