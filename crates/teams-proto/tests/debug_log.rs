//! Every HTTP send path reaches the diagnostic wire log. All credentials and identifiers here are synthetic.

#![cfg(test)]

use std::fs;
use std::future::{self, Future};
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::Mri;
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, CallContext, CallParticipant, ClientInfo, Credentials, Endpoint, EndpointRegistration, FileDownload,
    HttpError, HttpTransport, Links, MediaContent, Method, RawBody, Read, Refusal, Request, Response, SendMessage,
    Service, ShortProfileLookup, StartCall, StartParts, TEAMS_ECHO_BOT_MRI, TeamWrite, Transport,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const MIDDLE_BASE: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
const MRI: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const SKY: &str = "SkYpEtOkEnVaLuE-not-a-jwt";
const COOKIE: &str = "CoOkIeVaLuE42";
const DOWNLOAD_SECRET: &str = "syntheticDownloadCredential";

fn jwt() -> String {
    let body = "DeBuGlOgToKeNaBcD0123456789-_".repeat(5);
    format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{body}")
}

struct OneToken(String);

impl Credentials for OneToken {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, _: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(Ok(Some(Secret::new(SKY))))
    }
}

struct Get;

impl Read for Get {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "users", "ME", "conversations"]).with_query("view", "wire-visible-query")
    }
}

struct Plain;

impl Request for Plain {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn method(&self) -> Method {
        Method::Put
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "wire-plain"])
    }
    fn body(&self) -> Option<Value> {
        Some(json!({"plain": "wire-visible-plain", "skypetoken": SKY}))
    }
    fn headers(&self) -> Vec<(String, String)> {
        vec![("x-wire-test".into(), "wire-visible-header".into())]
    }
}

fn call() -> StartCall {
    let parts = StartParts {
        from: CallParticipant {
            mri: Mri::new(MRI),
            display_name: Some("Wire Caller".into()),
            endpoint_id: "wire-endpoint".into(),
            participant_id: Some("wire-participant".into()),
            language_id: Some("en-US".into()),
        },
        roster_update: "https://mock.trouter.teams.microsoft.com/v4/f/wire/callAgent/roster".into(),
        conversation_links: Links::new(),
        call_links: Links::new(),
        media_controller_links: Links::new(),
        modalities: vec!["audio".into()],
        media: MediaContent::new("wire-visible-offer", "application/sdp"),
        client: ClientInfo::default(),
    };
    StartCall::echo_test(
        &CallContext::new("00000000-0000-4000-8000-000000000001"),
        &parts,
        &Mri::new(TEAMS_ECHO_BOT_MRI),
    )
    .unwrap()
}

async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/chatsvc/amer/v1/wire-plain"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("Retry-After", "0")
                .set_body_json(json!({"note": "wire-visible-answer-retry"})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(wiremock::matchers::any())
        .respond_with(|request: &wiremock::Request| {
            ResponseTemplate::new(200)
                .insert_header("set-cookie", format!("session={COOKIE}; Secure").as_str())
                .insert_header("x-wire-answer", "wire-visible-response-header")
                .set_body_json(json!({"reply": request.url.path(), "note": "wire-visible-answer", "skypetoken": SKY}))
        })
        .with_priority(10)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/_layouts/15/download.aspx"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 0-3/4")
                .set_body_raw(b"RANG".to_vec(), "text/plain"),
        )
        .mount(&server)
        .await;
    server
}

async fn send_each(token: &str) {
    let server = server().await;
    let transport = HttpTransport::with_mock_origin(Arc::new(OneToken(token.into())), CHAT_BASE, &server.uri())
        .unwrap()
        .with_middle_tier(MIDDLE_BASE)
        .unwrap();
    assert_eq!(transport.read(&Get).await.unwrap().status(), 200);
    assert_eq!(transport.send(&Plain).await.unwrap().status(), 200);
    send_control(&transport).await;
    send_files(&transport).await;
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        9,
        "eight kinds and the plain request's retry"
    );
}

async fn send_control(transport: &HttpTransport<OneToken>) {
    let message = SendMessage::new(
        "19:wire-chat@thread.v2",
        Author::new(Mri::new(MRI), "Wire Writer"),
        MessageText::plain("wire-visible-message").unwrap(),
    );
    assert_eq!(transport.deliver(message).await.unwrap().status(), 200);
    assert_eq!(
        transport
            .post_read(ShortProfileLookup::new([Mri::new(MRI)]).unwrap())
            .await
            .unwrap()
            .status(),
        200
    );
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-000000000002",
        "https://mock.trouter.teams.microsoft.com/v4/f/wire-endpoint/",
    )
    .unwrap();
    assert_eq!(transport.own_state(&registration).await.unwrap().status(), 200);
    assert_eq!(transport.deliver(call()).await.unwrap().status(), 200);
}

async fn send_files(transport: &HttpTransport<OneToken>) {
    let picture = TeamWrite::SetPicture {
        team: "19:wire-team@thread.tacv2".into(),
        group: "wire-group".into(),
        picture: RawBody::from(vec![0x89, b'P', b'N', b'G', 13, 10, 26, 10]),
    };
    assert_eq!(transport.deliver(picture).await.unwrap().status(), 200);
    let item = FileDownload::of("https://synthetic.sharepoint.com/shared-file").unwrap();
    let link = format!(
        "https://synthetic.sharepoint.com/_layouts/15/download.aspx?tempauth={DOWNLOAD_SECRET}&name=wire-range"
    );
    let content = item
        .read_answer(&Response::new(
            200,
            json!({"size": 4, "@microsoft.graph.downloadUrl": link}).to_string(),
        ))
        .unwrap();
    assert_eq!(transport.file_range(&content.link, 0, 4).await.unwrap().body(), b"RANG");
}

async fn refuse_read_only(token: &str) {
    let server = server().await;
    let transport = HttpTransport::with_mock_origin(Arc::new(OneToken(token.into())), CHAT_BASE, &server.uri())
        .unwrap()
        .read_only();
    transport.read(&Get).await.unwrap();
    let before = server.received_requests().await.unwrap().len();
    assert!(matches!(
        transport.send(&Plain).await,
        Err(HttpError::Refused(Refusal::ReadOnly))
    ));
    let message = SendMessage::new(
        "19:read-only-private-chat@thread.v2",
        Author::new(Mri::new(MRI), "ReadOnly Sender"),
        MessageText::plain("read-only-private-message").unwrap(),
    );
    assert!(matches!(
        transport.deliver(message).await,
        Err(HttpError::Refused(Refusal::ReadOnly))
    ));
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

fn assert_refusals(info: &str) {
    let refusals: Vec<_> = info.lines().filter(|line| line.contains("refused_read_only")).collect();
    assert_eq!(refusals.len(), 2, "both refusal paths log their diagnostic: {info}");
    for expected in [
        "refused_read_only PUT teams.microsoft.com/api/chatsvc/amer/v1/wire-plain",
        "refused_read_only POST teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/<chat>/messages",
    ] {
        assert!(
            refusals.iter().any(|line| line.contains(expected)),
            "missing {expected:?}: {refusals:?}"
        );
    }
    for line in refusals {
        // what the line says, after its timestamp (19:… is also an hour of the day)
        let said = line.split_once("refused_read_only").map_or(line, |(_, said)| said);
        for absent in [
            "19:",
            "read-only-private",
            "ReadOnly Sender",
            "wire-visible",
            "skypetoken",
            "?",
            "body",
            "authorization",
        ] {
            assert!(
                !said.contains(absent),
                "refusal discloses payload or credentials: {line}"
            );
        }
    }
}

fn assert_exchanges(text: &str) {
    for direction in ["request", "response"] {
        assert_eq!(
            text.lines()
                .filter(|line| line.contains(&format!("{direction} PUT https://"))
                    && line.contains("/api/chatsvc/amer/v1/wire-plain"))
                .count(),
            2,
            "the first plain request and its retry both log their {direction}: {text}"
        );
    }
    for (method, route, body) in [
        (
            "GET",
            "/api/chatsvc/amer/v1/users/ME/conversations",
            "wire-visible-query",
        ),
        ("PUT", "/api/chatsvc/amer/v1/wire-plain", "wire-visible-plain"),
        (
            "POST",
            "/api/chatsvc/amer/v1/users/ME/conversations/19:wire-chat@thread.v2/messages",
            "wire-visible-message",
        ),
        ("POST", "/api/mt/part/amer-03/beta/users/fetchShortProfile", MRI),
        ("POST", "/registrar/prod/V2/registrations", "wire-endpoint"),
        ("POST", "/api/v2/epconv", "wire-visible-offer"),
        (
            "POST",
            "/api/mt/part/amer-03/beta/teams/wire-group/profilepicture",
            "[application/json;charset=UTF-8, 8 bytes]",
        ),
        ("GET", "/_layouts/15/download.aspx", "range: bytes=0-3"),
    ] {
        let request = text
            .lines()
            .find(|line| line.contains(&format!("request {method} https://")) && line.contains(route))
            .unwrap_or_else(|| panic!("missing request {method} {route}:\n{text}"));
        assert!(request.contains(body), "missing {body:?}: {request}");
        let response = text
            .lines()
            .find(|line| line.contains(&format!("response {method} https://")) && line.contains(route))
            .unwrap_or_else(|| panic!("missing response {method} {route}:\n{text}"));
        let expected = if route.contains("download.aspx") {
            "RANG"
        } else {
            "wire-visible-answer"
        };
        assert!(response.contains(expected), "missing response body: {response}");
        assert!(response.contains("headers {"), "missing response headers: {response}");
    }
    for kept in [
        "authorization: [redacted]",
        "x-skypetoken: [redacted]",
        "set-cookie: [redacted]",
        "x-wire-test: wire-visible-header",
        "x-wire-answer: wire-visible-response-header",
        "\"skypetoken\":\"[redacted]\"",
        "x-microsoft-skype-chain-id:",
        "tempauth=[redacted]&name=wire-range",
    ] {
        assert!(text.contains(kept), "missing {kept:?}:\n{text}");
    }
}

#[tokio::test]
async fn every_send_logs_both_sides_at_debug_redacted_and_no_payload_at_info() {
    let dir = tempfile::tempdir().unwrap();
    let log = teams_log::install(&teams_log::Config {
        dir: dir.path().into(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let token = jwt();
    send_each(&token).await;
    assert!(log.flush_written());
    let debug = fs::read_to_string(log.path()).unwrap();
    assert_exchanges(&debug);
    for secret in [token.as_str(), SKY, COOKIE, DOWNLOAD_SECRET] {
        assert!(!debug.contains(secret), "credential leaked: {secret}");
    }
    log.apply_setting(teams_log::Level::Info);
    send_each(&token).await;
    refuse_read_only(&token).await;
    assert!(log.flush_written());
    let all = fs::read_to_string(log.path()).unwrap();
    let info = &all[debug.len()..];
    assert_refusals(info);
    assert!(
        info.contains("HTTP 200"),
        "info still reports exchange outcomes: {info}"
    );
    for absent in [
        "teams_wire::http",
        "wire-visible",
        "wire-range",
        "RANG",
        token.as_str(),
        SKY,
        COOKIE,
        DOWNLOAD_SECRET,
        "body",
        "?",
        "Wire Caller",
        "Wire Writer",
    ] {
        assert!(!info.contains(absent), "payload at info: {absent:?}:\n{info}");
    }
}
