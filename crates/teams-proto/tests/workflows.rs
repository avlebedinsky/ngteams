//! Synthetic protocol and recipient-safety tests. No tenant requests, real credentials or real identities.
#![cfg(test)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{Credentials, HttpTransport, WorkflowFailure, WorkflowRequest};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct Tokens(Mutex<Vec<String>>);
impl Credentials for Tokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.0.lock().unwrap().push(audience.to_owned());
        std::future::ready(Ok(Secret::new(if audience == "https://apihub.azure.com" {
            "synthetic-runtime-token"
        } else if audience == "https://service.powerapps.com/" {
            "synthetic-connections-token"
        } else {
            "synthetic-flow-token"
        })))
    }
    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}

fn transport(server: &MockServer, tokens: Arc<Tokens>) -> HttpTransport<Tokens> {
    HttpTransport::with_mock_origin(tokens, "https://teams.microsoft.com/api/chatsvc/amer", &server.uri()).unwrap()
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
    let transport = transport(&server, Arc::default());
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
        let transport = transport(&server, Arc::default());
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
    let transport = transport(&server, Arc::default());
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

fn input_flow() -> serde_json::Value {
    json!({"name":"synthetic","properties":{"definition":{"triggers":{"manual":{"type":"Request","kind":"Button","inputs":{"schema":{"type":"object","required":["text"],"additionalProperties":false,"properties":{"text":{"type":"string","minLength":1}}}}}}}}})
}
fn logic_metadata(runtime: &str) -> serde_json::Value {
    json!({"name":"shared_logicflows","id":"/providers/Microsoft.PowerApps/apis/shared_logicflows","properties":{"primaryRuntimeUrl":runtime}})
}
async fn mount_input_reads(server: &MockServer, runtime: &str) {
    Mock::given(method("GET"))
        .and(path(
            "/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows/synthetic",
        ))
        .and(header("authorization", "Bearer synthetic-flow-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(input_flow()))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/providers/Microsoft.PowerApps/apis/shared_logicflows"))
        .and(query_param("$filter", "environment eq 'Default-synthetic'"))
        .and(header("authorization", "Bearer synthetic-connections-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(logic_metadata(runtime)))
        .expect(1)
        .mount(server)
        .await;
}
fn input_request() -> WorkflowRequest {
    WorkflowRequest::RunWithInputs {
        environment: "Default-synthetic".into(),
        flow: "synthetic".into(),
        trigger: "manual".into(),
        inputs: json!({"text":"Synthetic input"}),
    }
}
#[tokio::test]
async fn trigger_inputs_reach_only_the_discovered_own_logicflows_runtime_once() {
    let server = MockServer::start().await;
    mount_input_reads(
        &server,
        "https://default-synthetic.00.common.synthetic.azure-apihub.net/apim/logicflows",
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/apim/logicflows/synthetic/triggers/manual/run"))
        .and(header("authorization", "Bearer synthetic-runtime-token"))
        .and(wiremock::matchers::body_json(json!({"text":"Synthetic input"})))
        .respond_with(ResponseTemplate::new(202).set_body_string("Accepted"))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::default());
    assert_eq!(
        transport(&server, tokens.clone()).workflow(&input_request()).await,
        Ok(serde_json::Value::Null)
    );
    assert_eq!(
        *tokens.0.lock().unwrap(),
        [
            "https://service.flow.microsoft.com/",
            "https://service.powerapps.com/",
            "https://apihub.azure.com"
        ]
    );
}
#[tokio::test]
async fn a_runtime_failure_is_never_automatically_replayed() {
    let server = MockServer::start().await;
    mount_input_reads(
        &server,
        "https://default-synthetic.00.common.synthetic.azure-apihub.net/apim/logicflows",
    )
    .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        transport(&server, Arc::default()).workflow(&input_request()).await,
        Err(WorkflowFailure::Service(503))
    );
}
#[tokio::test]
async fn metadata_cannot_send_a_runtime_credential_to_an_external_or_different_environment() {
    for runtime in [
        "https://outside.example.test/apim/logicflows",
        "https://another-environment.00.common.synthetic.azure-apihub.net/apim/logicflows",
        "https://default-synthetic.00.common.synthetic.azure-apihub.net/apim/custom",
        "https://default-synthetic.00.common.synthetic.azure-apihub.net.evil.test/apim/logicflows",
        "https://user@default-synthetic.00.common.synthetic.azure-apihub.net/apim/logicflows",
    ] {
        let server = MockServer::start().await;
        mount_input_reads(&server, runtime).await;
        let tokens = Arc::new(Tokens::default());
        assert_eq!(
            transport(&server, tokens.clone()).workflow(&input_request()).await,
            Err(WorkflowFailure::InvalidResponse)
        );
        assert!(
            !tokens
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|aud| aud == "https://apihub.azure.com")
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}
#[tokio::test]
async fn fresh_trigger_validation_prevents_running_invalid_or_stale_input() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(input_flow()))
        .expect(1)
        .mount(&server)
        .await;
    let mut request = input_request();
    if let WorkflowRequest::RunWithInputs { inputs, .. } = &mut request {
        *inputs = json!({"text":false});
    }
    assert_eq!(
        transport(&server, Arc::default()).workflow(&request).await,
        Err(WorkflowFailure::InvalidInput)
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
