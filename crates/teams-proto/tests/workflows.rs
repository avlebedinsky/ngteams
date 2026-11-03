//! Synthetic protocol and recipient-safety tests. No tenant requests, real credentials or real identities.
#![cfg(test)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_proto::{Credentials, HttpTransport, WorkflowFailure, WorkflowRequest};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct Tokens(Mutex<Vec<String>>);
impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.0.lock().unwrap().push(audience.to_owned());
        std::future::ready(Ok(Secret::new("synthetic-flow-token")))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}

fn transport(server: &MockServer, tokens: Arc<Tokens>) -> HttpTransport<Tokens> {
    let fence = Arc::new(Fence::new(
        Roster::new([Mri::new("8:orgid:synthetic-a")], []),
        FenceLog::from_writer(std::io::sink()),
    ));
    HttpTransport::with_mock_origin(
        tokens,
        fence,
        "https://teams.microsoft.com/api/chatsvc/amer",
        &server.uri(),
    )
    .unwrap()
}

#[tokio::test]
async fn catalog_uses_exact_first_party_route_and_only_the_flow_audience() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/providers/Microsoft.ProcessSimple/galleries/public/teamsworkflowtemplates",
        ))
        .and(query_param("useNewConfigUrl", "true"))
        .and(query_param("supportBigTiles", "true"))
        .and(header("authorization", "Bearer synthetic-flow-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::default());
    let response = transport(&server, Arc::clone(&tokens))
        .workflow(&WorkflowRequest::Catalog)
        .await
        .unwrap();
    assert_eq!(response, json!({"value": []}));
    assert_eq!(*tokens.0.lock().unwrap(), ["https://service.flow.microsoft.com/"]);
}

#[tokio::test]
async fn every_cloud_mutation_is_refused_for_labs_before_credentials_or_network() {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::default());
    let transport = transport(&server, Arc::clone(&tokens));
    let environment = "Default-synthetic".to_owned();
    let flow = "synthetic".to_owned();
    let requests = [
        WorkflowRequest::Create {
            environment: environment.clone(),
            properties: json!({"definition": {"actions": {"post": {"recipient": "external"}}}}),
        },
        WorkflowRequest::Update {
            environment: environment.clone(),
            flow: flow.clone(),
            properties: json!({}),
            expected_modified: None,
        },
        WorkflowRequest::SetEnabled {
            environment: environment.clone(),
            flow: flow.clone(),
            enabled: true,
        },
        WorkflowRequest::SetEnabled {
            environment: environment.clone(),
            flow: flow.clone(),
            enabled: false,
        },
        WorkflowRequest::Delete {
            environment: environment.clone(),
            flow: flow.clone(),
        },
        WorkflowRequest::Run {
            environment,
            flow,
            trigger: "manual".to_owned(),
        },
    ];
    for request in requests {
        assert_eq!(
            transport.workflow(&request).await,
            Err(WorkflowFailure::LabCloudMutation)
        );
    }
    assert!(tokens.0.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn denied_access_and_malformed_responses_do_not_become_empty_lists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        transport(&server, Arc::default())
            .workflow(&WorkflowRequest::Environments)
            .await,
        Err(WorkflowFailure::Forbidden)
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not JSON"))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        transport(&server, Arc::default())
            .workflow(&WorkflowRequest::Environments)
            .await,
        Err(WorkflowFailure::InvalidResponse)
    );
}

#[tokio::test]
async fn read_only_mode_refuses_mutations_before_credentials() {
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::default());
    let transport = transport(&server, Arc::clone(&tokens)).read_only();
    let request = WorkflowRequest::Delete {
        environment: "synthetic".into(),
        flow: "synthetic".into(),
    };
    assert_eq!(transport.workflow(&request).await, Err(WorkflowFailure::ReadOnly));
    assert!(tokens.0.lock().unwrap().is_empty());
}
