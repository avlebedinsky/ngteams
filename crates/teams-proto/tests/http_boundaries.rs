//! Synthetic credentials and local HTTP peers exercise the read-only and file-download boundaries.

#![cfg(test)]

use std::future::{self, Future};
use std::io::{self, Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{
    CredentialSource, Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, Roster, Scope,
};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Authorized, Credentials, Endpoint, EndpointRegistration, FileContentLink, FileDownload, HttpError,
    HttpTransport, MAX_STREAM, Method, Mutating as _, Read, Refusal, Request, Response, SendMessage, Service,
    StreamStart, SubstrateSearch, Transport,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const LAB_A: &str = "8:orgid:synthetic-a";
const LAB_B: &str = "8:orgid:synthetic-b";
const CHAT: &str = "19:synthetic-chat@thread.v2";
const DOWNLOAD: &str = "https://contoso-my.sharepoint.com/_layouts/15/download.aspx?tempauth=synthetic-link-secret";

struct Tokens {
    value: Option<String>,
    asked: Mutex<Vec<String>>,
}

impl Tokens {
    fn new(value: Option<String>) -> Self {
        Self {
            value,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(
            self.value
                .clone()
                .map(Secret::new)
                .ok_or_else(|| TokenError::NotSignedIn {
                    account: "synthetic-account".to_owned(),
                }),
        )
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn records(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn fence(log: impl io::Write + Send + 'static) -> Arc<Fence> {
    Arc::new(Fence::new(
        Roster::new([Mri::new(LAB_A), Mri::new(LAB_B)], []),
        FenceLog::from_writer(log),
    ))
}

fn transport(origin: &str, tokens: Arc<Tokens>, log: impl io::Write + Send + 'static) -> HttpTransport<Tokens> {
    HttpTransport::with_mock_origin(tokens, fence(log), CHAT_BASE, origin).unwrap()
}

fn link() -> FileContentLink {
    FileDownload::of("https://contoso-my.sharepoint.com/:v:/g/synthetic-sharing-link")
        .unwrap()
        .read_answer(&Response::new(
            200,
            json!({"_synthetic":true, "size":10, "@microsoft.graph.downloadUrl":DOWNLOAD}).to_string(),
        ))
        .unwrap()
        .link
}

struct LabOnly;
impl MembershipSource for LabOnly {
    fn members<'a>(&'a self, _: &'a Scope) -> MembershipFuture<'a> {
        Box::pin(async { Membership::Complete(vec![Mri::new(LAB_A), Mri::new(LAB_B)]) })
    }
}

struct Plain(Method);
impl Request for Plain {
    fn service(&self) -> Service {
        Service::Graph
    }
    fn method(&self) -> Method {
        self.0
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["synthetic-read"])
    }
    fn body(&self) -> Option<Value> {
        None
    }
}
impl Read for Plain {
    fn service(&self) -> Service {
        Service::Graph
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["synthetic-read"])
    }
}

fn message() -> SendMessage {
    SendMessage::new(
        CHAT,
        Author::new(Mri::new(LAB_A), "Synthetic A"),
        MessageText::plain("synthetic text").unwrap(),
    )
}

#[tokio::test]
async fn read_only_refuses_all_mutating_routes_before_tokens_or_network() {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::new(None));
    let lines = Lines::default();
    let transport = transport(&server.uri(), Arc::clone(&tokens), lines.clone()).read_only();
    assert!(transport.is_read_only());
    assert!(Transport::is_read_only(&transport));
    for method in [Method::Post, Method::Put, Method::Delete, Method::Patch] {
        assert!(matches!(
            transport.send(&Plain(method)).await,
            Err(HttpError::Refused(Refusal::ReadOnly))
        ));
    }
    let own = EndpointRegistration::messaging(
        "0000000a-0000-4000-8000-00000000000a",
        "https://amer.trouter.teams.microsoft.com/v4/f/synthetic-endpoint/",
    )
    .unwrap();
    assert!(matches!(
        transport.own_state(&own).await,
        Err(HttpError::Refused(Refusal::ReadOnly))
    ));
    let write = message();
    assert!(matches!(
        transport.read_only_refusal(&write),
        Some(HttpError::Refused(Refusal::ReadOnly))
    ));
    let permit = transport.fence().check(write.target(), &LabOnly).await.unwrap();
    let authorized = Authorized::new(write, permit).unwrap();
    assert!(matches!(
        transport.deliver(authorized).await,
        Err(HttpError::Refused(Refusal::ReadOnly))
    ));
    assert!(tokens.asked().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
    let records = lines.records();
    assert_eq!(
        records
            .iter()
            .filter(|record| record["event"] == "refused_read_only")
            .count(),
        7
    );
    assert!(
        records
            .iter()
            .filter(|record| record["event"] == "refused_read_only")
            .all(|record| record["verdict"] == "deny")
    );
    assert_eq!(records[0]["method"], "POST");
    assert_eq!(records[0]["host"], "graph.microsoft.com");
    assert_eq!(records.last().unwrap()["host"], "teams.microsoft.com");
    assert!(!serde_json::to_string(&records).unwrap().contains("synthetic text"));
}

#[tokio::test]
async fn read_only_keeps_get_and_search_reads_and_hides_credentials_in_debug() {
    let server = MockServer::start().await;
    Mock::given(path("/v1.0/synthetic-read"))
        .respond_with(ResponseTemplate::new(200).set_body_string("read"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/search/api/v1/suggestions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("search"))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::new(Some("synthetic-bearer-secret".to_owned())));
    let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink()).read_only();
    assert!(!transport.is_personal());
    assert!(transport.frame_log().is_none());
    assert!(transport.own_account().is_none());
    assert_eq!(transport.chat_base(), CHAT_BASE);
    assert!(!format!("{transport:?}").contains("synthetic-bearer-secret"));
    assert_eq!(transport.read(&Plain(Method::Post)).await.unwrap().body(), b"read");
    assert_eq!(
        transport
            .search(&SubstrateSearch::suggestions("synthetic query"))
            .await
            .unwrap()
            .body(),
        b"search"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method.as_str(), "GET");
    assert_eq!(requests[1].method.as_str(), "POST");
    assert!(
        requests
            .iter()
            .all(|request| request.headers.get("authorization").unwrap() == "Bearer synthetic-bearer-secret")
    );
    assert_eq!(
        tokens.asked(),
        [teams_proto::GRAPH_AUDIENCE, teams_proto::SUBSTRATE_AUDIENCE]
    );
}

struct BrokenLog;
impl io::Write for BrokenLog {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("synthetic unavailable log"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_read_only_refusal_stays_closed_when_its_log_is_unavailable() {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::new(None));
    let transport = transport(&server.uri(), Arc::clone(&tokens), BrokenLog).read_only();
    assert!(matches!(
        transport.send(&Plain(Method::Post)).await,
        Err(HttpError::Refused(Refusal::ReadOnly))
    ));
    assert!(tokens.asked().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn tenant_metadata_is_read_without_network_or_disclosing_the_token() {
    let server = MockServer::start().await;
    let claims = URL_SAFE_NO_PAD.encode(br#"{"tid":"synthetic-tenant","aud":"synthetic"}"#);
    for (value, expected) in [
        (
            Some(format!("e30.{claims}.synthetic-signature")),
            Some("synthetic-tenant"),
        ),
        (Some("opaque-synthetic-token".to_owned()), None),
        (Some(format!("e30.{}.signature", URL_SAFE_NO_PAD.encode(b"{}"))), None),
        (None, None),
    ] {
        let tokens = Arc::new(Tokens::new(value));
        let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink());
        assert_eq!(transport.account_tenant().await.as_deref(), expected);
        assert_eq!(tokens.asked(), [teams_proto::IC3_AUDIENCE]);
        assert!(tokens.skypetoken().await.unwrap().is_none());
        assert!(
            tokens
                .skypetoken_rejected(&Secret::new("synthetic"))
                .await
                .unwrap()
                .is_none()
        );
        assert!(!transport.has_middle_tier());
        assert!(!Transport::is_read_only(&transport));
        assert!(transport.read_only_refusal(&message()).is_none());
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn middle_tier_credentials_need_a_configured_base_and_an_available_token() {
    const BASE: &str = "https://teams.microsoft.com/api/mt/part/amer-03";
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::new(Some("synthetic-middle-tier-secret".to_owned())));
    let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink());
    assert!(transport.middle_tier().await.is_none());
    assert!(tokens.asked().is_empty());
    let configured = transport.with_middle_tier(BASE).unwrap();
    assert!(configured.has_middle_tier());
    let credential = configured.middle_tier().await.unwrap();
    assert_eq!(credential.expose_token(), "synthetic-middle-tier-secret");
    assert!(format!("{credential:?}").contains(BASE));
    assert!(!format!("{credential:?}").contains("synthetic-middle-tier-secret"));
    assert_eq!(tokens.asked(), [teams_proto::MIDDLE_TIER_AUDIENCE]);
    let unavailable =
        HttpTransport::with_mock_origin(Arc::new(Tokens::new(None)), fence(io::sink()), CHAT_BASE, &server.uri())
            .unwrap()
            .with_middle_tier(BASE)
            .unwrap();
    assert!(unavailable.middle_tier().await.is_none());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_streamed_range_sends_no_account_credentials_and_finishes_exactly_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/_layouts/15/download.aspx"))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", "bytes 4-7/10")
                .set_body_bytes(b"4567"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::new(None));
    let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink()).read_only();
    let mut body = transport.file_stream(&link(), 4, 4).await.unwrap();
    assert_eq!(
        body.start(),
        Some(StreamStart {
            offset: 4,
            len: 4,
            total: 10
        })
    );
    assert!(!format!("{body:?}").contains("synthetic-link-secret"));
    let mut received = Vec::new();
    while let Some(chunk) = body.chunk().await.unwrap() {
        received.extend(chunk);
    }
    assert_eq!(received, b"4567");
    assert_eq!(body.chunk().await.unwrap(), None);
    assert!(tokens.asked().is_empty());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].headers.get("range").unwrap(), "bytes=4-7");
    assert!(requests[0].headers.get("authorization").is_none());
    assert!(requests[0].headers.get("x-skypetoken").is_none());
}

#[tokio::test]
async fn whole_streams_are_cut_to_the_requested_length_and_wrong_ranges_are_unreadable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"0123456789"))
        .expect(2)
        .mount(&server)
        .await;
    let transport = transport(&server.uri(), Arc::new(Tokens::new(None)), io::sink());
    let mut whole = transport.file_stream(&link(), 0, 4).await.unwrap();
    assert_eq!(
        whole.start(),
        Some(StreamStart {
            offset: 0,
            len: 4,
            total: 10
        })
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = whole.chunk().await.unwrap() {
        bytes.extend(chunk);
    }
    assert_eq!(bytes, b"0123");
    let mut wrong = transport.file_stream(&link(), 2, 4).await.unwrap();
    assert_eq!(wrong.start(), None);
    assert_eq!(wrong.chunk().await.unwrap(), None);
}

#[tokio::test]
async fn invalid_file_ranges_are_refused_before_any_request_and_the_last_byte_is_not_overflow() {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::new(None));
    let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink());
    let link = link();
    for (offset, len) in [(0, 0), (0, MAX_STREAM + 1), (u64::MAX, 2)] {
        assert!(matches!(
            transport.file_stream(&link, offset, len).await,
            Err(HttpError::Refused(Refusal::FileContent))
        ));
    }
    assert!(matches!(
        transport.file_range(&link, u64::MAX, 2).await,
        Err(HttpError::Refused(Refusal::FileContent))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(206).set_body_bytes(b"x"))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(transport.file_range(&link, u64::MAX, 1).await.unwrap().body(), b"x");
    assert_eq!(
        server.received_requests().await.unwrap()[0]
            .headers
            .get("range")
            .unwrap(),
        "bytes=18446744073709551615-18446744073709551615"
    );
    assert!(tokens.asked().is_empty());
}

#[tokio::test]
async fn streamed_http_failures_are_reported_without_retry_or_token_renewal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_string("synthetic unavailable"))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::new(None));
    let transport = transport(&server.uri(), Arc::clone(&tokens), io::sink());
    match transport.file_stream(&link(), 0, 4).await {
        Err(HttpError::Status(answer)) => {
            assert_eq!(answer.status(), 503);
            assert_eq!(answer.body(), b"synthetic unavailable");
        }
        other => panic!("{other:?}"),
    }
    assert!(tokens.asked().is_empty());
}

#[tokio::test]
async fn a_stream_without_headers_reports_failure_without_requesting_credentials() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        drop(socket);
    });
    let tokens = Arc::new(Tokens::new(None));
    let transport = transport(&origin, Arc::clone(&tokens), io::sink());
    let result = transport.file_stream(&link(), 0, 4).await;
    peer.join().unwrap();
    let error = result.unwrap_err();
    assert!(matches!(error, HttpError::Failed(_)));
    assert!(!format!("{error:?}").contains("synthetic-link-secret"));
    assert!(tokens.asked().is_empty());
}

#[tokio::test]
async fn a_stream_cut_short_is_an_error_instead_of_a_silent_end() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).unwrap(), 1);
            head.push(byte[0]);
            assert!(head.len() < 8192);
        }
        socket.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/10\r\nConnection: close\r\n\r\nab").unwrap();
    });
    let transport = transport(&origin, Arc::new(Tokens::new(None)), io::sink());
    let mut body = transport.file_stream(&link(), 0, 4).await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match body.chunk().await {
                Err(error) => return error,
                Ok(Some(_)) => {}
                Ok(None) => panic!("a short stream silently ended"),
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(ended, HttpError::Failed(_)));
    peer.join().unwrap();
}
