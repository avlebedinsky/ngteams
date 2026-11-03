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

#[tokio::test]
async fn explicit_native_create_preserves_definition_and_sends_once() {
    use wiremock::matchers::body_json;
    let server = MockServer::start().await;
    let properties = json!({"displayName":"Synthetic reminder","state":"Stopped","definition":{"triggers":{},"actions":{}},"connectionReferences":{}});
    Mock::given(method("POST"))
        .and(path(
            "/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows",
        ))
        .and(body_json(json!({"properties":properties})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"name":"synthetic-created"})))
        .expect(1)
        .mount(&server)
        .await;
    let transport = transport(&server, Arc::default()).with_workflow_mutations(true);
    let response = transport
        .workflow(&WorkflowRequest::Create {
            environment: "Default-synthetic".into(),
            properties,
        })
        .await
        .unwrap();
    assert_eq!(response["name"], "synthetic-created");
}

#[tokio::test]
async fn throttled_or_failed_mutations_are_never_automatically_replayed() {
    for code in [429, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(code))
            .expect(1)
            .mount(&server)
            .await;
        let transport = transport(&server, Arc::default()).with_workflow_mutations(true);
        let request = WorkflowRequest::SetEnabled {
            environment: "synthetic".into(),
            flow: "synthetic".into(),
            enabled: true,
        };
        assert!(transport.workflow(&request).await.is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn an_edit_checks_the_last_read_version_before_any_patch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"name":"synthetic-flow","properties":{"lastModifiedTime":"newer-version"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let transport = transport(&server, Arc::default()).with_workflow_mutations(true);
    let request = WorkflowRequest::Update {
        environment: "synthetic".into(),
        flow: "synthetic-flow".into(),
        properties: json!({}),
        expected_modified: Some("older-version".into()),
    };
    assert_eq!(transport.workflow(&request).await, Err(WorkflowFailure::Changed));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn lab_context_overrides_explicit_workflow_capability_in_both_builder_orders() {
    use teams_lab_targets::{LabAccount, LabRoster};
    let accounts = ["a", "b", "c"].map(|id| LabAccount {
        mri: format!("8:orgid:00000000-0000-4000-8000-00000000000{id}"),
        upn: format!("synthetic-{id}@lab.example.test"),
    });
    let own = accounts[0].clone();
    let roster = Arc::new(LabRoster::new(accounts).unwrap());
    let server = MockServer::start().await;
    let tokens = Arc::new(Tokens::default());
    let first = transport(&server, tokens.clone())
        .with_workflow_mutations(true)
        .with_lab_targets(own.mri.clone(), own.upn.clone(), roster.clone())
        .unwrap();
    let second = transport(&server, tokens.clone())
        .with_lab_targets(own.mri, own.upn, roster)
        .unwrap()
        .with_workflow_mutations(true);
    let request = WorkflowRequest::CreateConnection {
        environment: "synthetic".into(),
        connector: "shared_teams".into(),
        connection: "synthetic".into(),
    };
    assert_eq!(first.workflow(&request).await, Err(WorkflowFailure::LabCloudMutation));
    assert_eq!(second.workflow(&request).await, Err(WorkflowFailure::LabCloudMutation));
    assert!(tokens.0.lock().unwrap().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn connection_requests_use_only_their_separate_microsoft_audience() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/providers/Microsoft.PowerApps/connections"))
        .and(query_param("$filter", "environment eq 'synthetic'"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[]})))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::default());
    assert!(
        transport(&server, tokens.clone())
            .workflow(&WorkflowRequest::Connections {
                environment: "synthetic".into()
            })
            .await
            .is_ok()
    );
    assert_eq!(*tokens.0.lock().unwrap(), ["https://service.powerapps.com/"]);
}

#[test]
fn continuations_and_consent_links_reject_external_or_different_contexts() {
    use teams_proto::workflow::is_workflow_consent_link;
    let base = "https://api.flow.microsoft.com/providers/Microsoft.ProcessSimple/environments/synthetic/flows";
    let page = |next: String| WorkflowRequest::Page {
        environment: "synthetic".into(),
        next,
    };
    assert!(
        page(format!("{base}?api-version=2016-11-01&$skiptoken=synthetic"))
            .checked_endpoint()
            .is_ok()
    );
    for url in [
        base.replace("api.flow.microsoft.com", "outside.example.test"),
        base.replace("/synthetic/", "/another/"),
        format!("{base}#fragment"),
        base.replace("https://", "http://"),
        base.replace("https://", "https://user@"),
        format!("{base}/another"),
    ] {
        assert!(page(url).checked_endpoint().is_err());
    }
    assert!(is_workflow_consent_link(
        "https://global.consent.azure-apim.net/redirect/teams?state=synthetic"
    ));
    for url in [
        "https://global.consent.azure-apim.net.evil.test/",
        "https://outside.example.test/",
        "http://global.consent.azure-apim.net/",
        "https://user@global.consent.azure-apim.net/",
        "https://global.consent.azure-apim.net:444/",
        "https://global.consent.azure-apim.net/#fragment",
    ] {
        assert!(!is_workflow_consent_link(url));
    }
}

#[test]
fn copilot_entitlement_uses_the_teams_own_settings_read() {
    let request = WorkflowRequest::CopilotEligibility;
    assert!(!request.writes());
    assert_eq!(request.method(), "POST");
    assert_eq!(request.service(), teams_proto::Service::MiddleTier);
    assert_eq!(
        request.endpoint().segments(),
        &["beta", "users", "useraggregatesettings"]
    );
    assert_eq!(request.body().unwrap()["userCopilotSettings"], true);
}

#[test]
fn history_pages_cannot_cross_flow_or_environment() {
    let request = |next: &str| WorkflowRequest::HistoryPage {
        environment: "synthetic".into(),
        flow: "one".into(),
        next: next.into(),
    };
    let base = "https://api.flow.microsoft.com/providers/Microsoft.ProcessSimple/environments/synthetic/flows/one/runs";
    assert!(request(base).checked_endpoint().is_ok());
    assert!(request(&base.replace("/one/", "/two/")).checked_endpoint().is_err());
    assert!(
        request(&base.replace("/synthetic/", "/different/"))
            .checked_endpoint()
            .is_err()
    );
}
