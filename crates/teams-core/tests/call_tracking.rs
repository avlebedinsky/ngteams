//! Synthetic call callbacks around outbox transport outcomes; no tenant or media service is used.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_call::{Delivery, Observed, Tracker};
use teams_core::{CallFloor, CallRetries, CallTried, Outbox};
use teams_proto::Mri;
use teams_proto::{
    Author, Credentials, Endpoint, Failure, HttpError, HttpTransport, Method, Read, Refusal, Request, Response,
    Service, Transport,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ACCOUNT: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const CALLBACK_ROOT: &str = "/v4/f/SyntheticEndpoint/callAgent/agent-1";
const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";

struct Start {
    url: reqwest::Url,
}

impl Start {
    fn new() -> Self {
        Self {
            url: reqwest::Url::parse(teams_proto::CALL_START_URL).unwrap(),
        }
    }
}

impl Request for Start {
    fn service(&self) -> Service {
        Service::Calling
    }
    fn method(&self) -> Method {
        Method::Post
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new([] as [&str; 0])
    }
    fn body(&self) -> Option<Value> {
        Some(json!({"callInvitation": {"links": {
            "acceptance": format!("https://synthetic.trouter.invalid{CALLBACK_ROOT}/accept/call/acceptance/"),
            "end": format!("https://synthetic.trouter.invalid{CALLBACK_ROOT}/end/call/end/")
        }}}))
    }
    fn call_url(&self) -> Option<&reqwest::Url> {
        Some(&self.url)
    }
}

fn author() -> Author {
    Author::new(Mri::new(ACCOUNT), "Synthetic A")
}

fn acceptance() -> Delivery {
    delivery(
        "accept/call/acceptance/",
        &json!({"callAcceptance": {"links": {
            "acknowledgement": format!("{PROXY}/cc.invalid/cc/v1/callAcceptance/call-one/1/a/1/acknowledge")
        }}}),
    )
}

fn media() -> Delivery {
    delivery(
        "media/call/mediaRenegotiation/",
        &json!({"mediaNegotiation": {"links": {
            "mediaAnswer": format!("{PROXY}/cc.invalid/cc/v1/negotiations/call-one/1/a/1/answer")
        }}}),
    )
}

fn delivery(suffix: &str, body: &Value) -> Delivery {
    Delivery {
        connection: "synthetic-socket".into(),
        id: 1,
        path: format!("{CALLBACK_ROOT}/{suffix}"),
        trouter_request: None,
        chain_id: None,
        message_id: None,
        body: body.to_string().into_bytes(),
    }
}

#[derive(Default)]
struct ThrottledTokens(AtomicUsize);

impl Credentials for ThrottledTokens {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("synthetic-bearer")))
    }
    fn bearer_rejected(&self, _: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer("")
    }
    fn skypetoken(&self) -> impl Future<Output = Result<Option<Secret>, TokenError>> + Send {
        future::ready(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TokenError::Throttled {
                retry_after: Some(Duration::ZERO),
                error: None,
            })
        } else {
            Ok(Some(Secret::new("synthetic-skypetoken")))
        })
    }
}

#[tokio::test]
async fn credential_throttling_then_retry_binds_acceptance_and_media_to_the_live_start() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/epconv"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "conversationController": format!("{PROXY}/conv.invalid/conv/conversation-one"),
            "links": {}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(ThrottledTokens::default());
    let transport = HttpTransport::with_mock_origin(
        tokens.clone(),
        "https://teams.microsoft.com/api/chatsvc/amer",
        &server.uri(),
    )
    .unwrap();
    let outbox = Outbox::new(Arc::new(transport), author());
    let first = outbox.try_call(Start::new()).await;
    assert!(matches!(first, CallTried::Throttled(_)), "{first:?}");
    assert_eq!(CallRetries::default().again(&first), Some(Duration::ZERO));
    assert!(server.received_requests().await.unwrap().is_empty(), "no first send");

    assert!(matches!(outbox.try_call(Start::new()).await, CallTried::Answered(_)));
    assert_eq!(tokens.0.load(Ordering::SeqCst), 2);
    assert_eq!(outbox.tracker().observe_delivery(&acceptance()), Observed::Acceptance);
    assert!(outbox.tracker().holds_floor());
    assert_eq!(outbox.tracker().observe_delivery(&media()), Observed::Renegotiation);
    assert_eq!(
        outbox
            .tracker()
            .observe_delivery(&delivery("end/call/end/", &json!({}))),
        Observed::Ended
    );
    assert!(!outbox.tracker().holds_floor());
    assert_eq!(outbox.tracker().observe_delivery(&acceptance()), Observed::Nothing);
}

struct FailsOnce(Mutex<Option<HttpError>>);

impl Transport for FailsOnce {
    type Error = HttpError;

    fn read<R: Read>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }
    fn send<R: Request>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }
    fn deliver<R: Request + Send>(&self, _: R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(self.0.lock().unwrap().take().unwrap()))
    }
}

#[tokio::test]
async fn definite_failures_retire_pending_callbacks_while_uncertain_outcomes_keep_them() {
    let cases = [
        (HttpError::NoRoute, false),
        (HttpError::Unknown, false),
        (HttpError::Refused(Refusal::NoSkypetoken), false),
        (
            HttpError::Credentials(TokenError::NotSignedIn {
                account: ACCOUNT.into(),
            }),
            false,
        ),
        (
            HttpError::Credentials(TokenError::Throttled {
                retry_after: None,
                error: None,
            }),
            false,
        ),
        (HttpError::Failed(Failure::Network), true),
        (HttpError::Failed(Failure::Timeout), true),
        (HttpError::Failed(Failure::Body), true),
        (HttpError::BodyTooLarge, true),
    ];
    for (error, uncertain) in cases {
        let shown = error.to_string();
        let outbox = Outbox::new(Arc::new(FailsOnce(Mutex::new(Some(error)))), author());
        let tried = outbox.try_call(Start::new()).await;
        assert_eq!(matches!(tried, CallTried::Uncertain(_)), uncertain, "{shown}");
        assert_eq!(
            outbox.tracker().observe_delivery(&acceptance()),
            if uncertain {
                Observed::Acceptance
            } else {
                Observed::Nothing
            },
            "{shown}"
        );
    }
}

#[test]
fn rejoining_an_account_after_another_engine_leaves_keeps_the_floor_available() {
    let floor = CallFloor::new();
    let a = Arc::new(Tracker::new(Mri::new(teams_mock::LAB_A)));
    let b = Arc::new(Tracker::new(Mri::new(teams_mock::LAB_B)));
    floor.join(&a);
    floor.join(&b);
    let invitation = |from, to| Delivery {
        connection: format!("{to}/synthetic"),
        id: 1,
        path: "/v4/f/SyntheticEndpoint/".into(),
        trouter_request: None,
        chain_id: None,
        message_id: None,
        body: teams_mock::TrouterMock::call_invitation(from, to, |_| {})
            .to_string()
            .into_bytes(),
    };
    assert_eq!(
        floor.observe(&a, &invitation(teams_mock::LAB_B, teams_mock::LAB_A)),
        Observed::Offered
    );
    assert!(floor.others_busy(&b));
    drop(a);
    // Reconnecting the remaining account can join again without retaining the departed engine's ring.
    floor.join(&b);
    floor.join(&b);
    assert!(!floor.others_busy(&b));
    assert_eq!(
        floor.observe(&b, &invitation(teams_mock::LAB_A, teams_mock::LAB_B)),
        Observed::Offered
    );
    assert!(b.holds_floor());
}
