//! The native request policy's refusals: which chat bases and mock origins a transport accepts, which endpoints it
//! refuses before sending, and how an endpoint's segments stay segments. The compile-time half (no request of another
//! class, no write without a permit, no mock origin in a normal build) is in `crates/fence-seal`.
//!
//! Tokens and ids here are placeholders, never real ones.

use std::future::{self, Future};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Roster};
use teams_proto::{
    BuildError, Credentials, Endpoint, HttpError, HttpTransport, ProbeOwnState, Read, Refusal, Service, Transport,
};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Hands out one placeholder token.
struct OneToken;

impl Credentials for OneToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("placeholder-token-for-policy-tests")))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("placeholder-token-for-policy-tests")))
    }
}

#[derive(Clone, Default)]
struct SharedLog(Arc<Mutex<Vec<u8>>>);

impl SharedLog {
    #[cfg(test)]
    fn urls(&self) -> Vec<String> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["url"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

impl Write for SharedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fence(log: SharedLog) -> Arc<Fence> {
    Arc::new(Fence::new(Roster::default(), FenceLog::from_writer(log)))
}

fn live(chat_base: &str) -> Result<HttpTransport<OneToken>, BuildError> {
    HttpTransport::new(Arc::new(OneToken), fence(SharedLog::default()), chat_base)
}

struct Get(Service, Endpoint);

impl Read for Get {
    fn service(&self) -> Service {
        self.0
    }

    fn endpoint(&self) -> Endpoint {
        self.1.clone()
    }
}

/// A mock that answers everything, and a transport on it over `chat_base`, logging to `log`.
#[cfg(test)]
async fn mocked(chat_base: &str, log: SharedLog) -> (MockServer, HttpTransport<OneToken>) {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let transport = HttpTransport::with_mock_origin(Arc::new(OneToken), fence(log), chat_base, &server.uri()).unwrap();
    (server, transport)
}

#[test]
fn a_chat_base_is_exactly_one_the_chat_service_check_accepts() {
    for (base, normalized) in [
        (
            "https://teams.microsoft.com/api/chatsvc/amer",
            "https://teams.microsoft.com/api/chatsvc/amer",
        ),
        (
            "https://teams.cloud.microsoft/api/chatsvc/emea/",
            "https://teams.cloud.microsoft/api/chatsvc/emea",
        ),
        (
            "https://amer.ng.msg.teams.microsoft.com",
            "https://amer.ng.msg.teams.microsoft.com/",
        ),
    ] {
        let transport = live(base).unwrap_or_else(|error| panic!("{base}: {error}"));
        assert_eq!(transport.chat_base(), normalized);
    }
    for base in [
        "http://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com:8443/api/chatsvc/amer",
        "https://user@teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/chatsvc",
        "https://teams.microsoft.com/api/chatsvc/amer/v1",
        "https://teams.microsoft.com/api/chatsvc/amer?x=1",
        "https://teams.microsoft.com/api/chatsvc/amer#x",
        " https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/chatsvc/AMER",
        "https://teams.microsoft.com.example.com/api/chatsvc/amer",
        "https://example.com/api/chatsvc/amer",
        "https://msg.teams.microsoft.com",
        "https://teams.microsoft.com/api/mt/part/amer-03",
        "",
    ] {
        assert!(
            matches!(live(base), Err(BuildError::ChatBase)),
            "{base:?} must be refused"
        );
    }
}

#[test]
fn a_mock_origin_is_plain_http_on_loopback_with_a_port() {
    let base = "https://teams.microsoft.com/api/chatsvc/amer";
    let mock =
        |origin: &str| HttpTransport::with_mock_origin(Arc::new(OneToken), fence(SharedLog::default()), base, origin);
    for origin in ["http://127.0.0.1:9", "http://[::1]:9/", "http://localhost:9"] {
        assert!(mock(origin).is_ok(), "{origin}");
    }
    for origin in [
        "https://127.0.0.1:9",
        "http://127.0.0.1",
        "http://10.0.0.1:9",
        "http://example.com:9",
        "http://127.0.0.1:9/api",
        "http://127.0.0.1:9/?x=1",
        "http://user@127.0.0.1:9",
    ] {
        assert!(
            matches!(mock(origin), Err(BuildError::MockOrigin)),
            "{origin} must be refused"
        );
    }
}

#[tokio::test]
async fn an_endpoint_with_an_empty_or_dot_segment_is_refused_before_sending() {
    let (server, transport) = mocked("https://teams.microsoft.com/api/chatsvc/amer", SharedLog::default()).await;
    for segments in [
        vec!["v1", "users", "", "conversations"],
        vec!["v1", ".", "conversations"],
        vec!["v1", "users", "..", "..", "mt"],
    ] {
        for service in [Service::ChatService, Service::Registrar, Service::Trouter] {
            let error = transport
                .read(&Get(service, Endpoint::new(segments.clone())))
                .await
                .unwrap_err();
            assert!(
                matches!(error, HttpError::Refused(Refusal::Segment)),
                "{segments:?} on {service:?}: {error:?}"
            );
        }
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_chat_service_endpoint_starts_with_v1() {
    let (server, transport) = mocked("https://teams.microsoft.com/api/chatsvc/amer", SharedLog::default()).await;
    for segments in [vec![], vec!["v2", "users"], vec!["users", "ME"]] {
        let error = transport
            .read(&Get(Service::ChatService, Endpoint::new(segments.clone())))
            .await
            .unwrap_err();
        assert!(
            matches!(error, HttpError::Refused(Refusal::ChatPath)),
            "{segments:?}: {error:?}"
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_segment_stays_one_segment_whatever_it_holds() {
    let (server, transport) = mocked("https://teams.microsoft.com/api/chatsvc/amer", SharedLog::default()).await;
    let endpoint = Endpoint::new(["v1", "threads", "19:a/../b?view=x#y@thread.v2"]);

    transport.read(&Get(Service::ChatService, endpoint)).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url.path(),
        "/api/chatsvc/amer/v1/threads/19:a%2F..%2Fb%3Fview=x%23y@thread.v2"
    );
    assert_eq!(requests[0].url.query(), None);
}

#[tokio::test]
async fn the_registrar_is_on_the_chat_bases_afd_host_or_the_first_one() {
    for (base, registrations) in [
        (
            "https://teams.cloud.microsoft/api/chatsvc/amer",
            "https://teams.cloud.microsoft/registrar/prod/V2/registrations",
        ),
        (
            "https://teams.microsoft.com/api/chatsvc/amer",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
        ),
        (
            "https://amer.ng.msg.teams.microsoft.com",
            "https://teams.microsoft.com/registrar/prod/V2/registrations",
        ),
    ] {
        let log = SharedLog::default();
        let (server, transport) = mocked(base, log.clone()).await;
        let registration = ProbeOwnState::new(Service::Registrar, Endpoint::new(["prod", "V2", "registrations"]), None);

        transport.own_state(&registration).await.unwrap();

        assert_eq!(log.urls(), [registrations], "from {base}");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].url.path(), "/registrar/prod/V2/registrations");
    }
}

#[tokio::test]
async fn a_regional_chat_base_keeps_its_path_from_v1() {
    let (server, transport) = mocked("https://amer.ng.msg.teams.microsoft.com/", SharedLog::default()).await;

    transport
        .read(&Get(
            Service::ChatService,
            Endpoint::new(["v1", "users", "ME", "properties"]),
        ))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].url.path(), "/v1/users/ME/properties");
}
