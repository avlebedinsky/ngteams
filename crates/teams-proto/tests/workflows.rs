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
struct Tokens(Mutex<Vec<String>>, Mutex<Vec<String>>);
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
        self.1.lock().unwrap().push(audience.to_owned());
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

#[tokio::test]
async fn refused_workflow_bearers_are_invalidated_without_replaying_any_write() {
    let requests = [
        (
            WorkflowRequest::Create {
                environment: "Default-synthetic".into(),
                properties: json!({}),
            },
            "https://service.flow.microsoft.com/",
        ),
        (
            WorkflowRequest::CreateConnection {
                environment: "Default-synthetic".into(),
                connector: "shared_teams".into(),
                connection: "synthetic".into(),
            },
            "https://service.powerapps.com/",
        ),
        (input_request(), "https://apihub.azure.com"),
    ];
    for (request, audience) in requests {
        for status in [401, 403] {
            let server = MockServer::start().await;
            if matches!(request, WorkflowRequest::RunWithInputs { .. }) {
                mount_input_reads(
                    &server,
                    "https://default-synthetic.00.common.synthetic.azure-apihub.net/apim/logicflows",
                )
                .await;
            }
            Mock::given(method(request.method()))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let tokens = Arc::new(Tokens::default());
            assert_eq!(
                transport(&server, tokens.clone()).workflow(&request).await,
                Err(if status == 401 {
                    WorkflowFailure::Authentication
                } else {
                    WorkflowFailure::Forbidden
                })
            );
            assert_eq!(
                *tokens.1.lock().unwrap(),
                if status == 401 {
                    vec![audience.to_owned()]
                } else {
                    vec![]
                }
            );
            assert_eq!(
                server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .filter(|sent| sent.method.as_str() == request.method())
                    .count(),
                1
            );
        }
    }
}

fn authorize() -> WorkflowRequest {
    WorkflowRequest::AuthorizeConnection {
        environment: "Default-synthetic".into(),
        connector: "shared_teams".into(),
        connection: "shared-teams-synthetic".into(),
    }
}
async fn mount_created(server: &MockServer, login: &str) {
    Mock::given(method("PUT"))
        .and(path(
            "/providers/Microsoft.PowerApps/apis/shared_teams/connections/shared-teams-synthetic",
        ))
        .and(query_param("$expand", "ConsentLink"))
        .and(header("authorization", "Bearer synthetic-connections-token"))
        .and(wiremock::matchers::body_partial_json(json!({"properties":{"consentInfo":{"redirectUrl":"https://make.powerautomate.com/oauth/redirect"},"environment":{"name":"Default-synthetic"}}})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"name":"shared-teams-synthetic","properties":{"statuses":[{"status":"Error"}],"consentInfo":{"firstPartyLoginUri":login}}})))
        .expect(1)
        .mount(server)
        .await;
}
#[tokio::test]
async fn a_teams_connection_is_signed_in_only_through_its_microsoft_first_party_login() {
    let server = MockServer::start().await;
    mount_created(
        &server,
        "https://synthetic.consent.azure-apihub.net/firstPartyLogin?data=synthetic",
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/powerautomate/users/me/onBehalfOfTokenBundle"))
        .and(query_param("api-version", "1"))
        .and(header("authorization", "Bearer synthetic-flow-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"audienceToToken":{"https://apihub.azure.com":"synthetic-bundle-token","https://graph.microsoft.com":"synthetic-other"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/firstPartyLogin"))
        .and(query_param("data", "synthetic"))
        .and(wiremock::matchers::body_json(
            json!({"accessToken":"synthetic-bundle-token"}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::default());
    let created = transport(&server, tokens.clone()).workflow(&authorize()).await.unwrap();
    assert_eq!(created["name"], "shared-teams-synthetic");
    assert!(!created.to_string().contains("synthetic-bundle-token"));
    let login = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.url.path() == "/firstPartyLogin")
        .unwrap();
    assert!(
        !login.headers.contains_key("authorization"),
        "the account's own token never goes to the consent host"
    );
    assert_eq!(
        *tokens.0.lock().unwrap(),
        ["https://service.powerapps.com/", "https://service.flow.microsoft.com/"]
    );
}
#[tokio::test]
async fn an_external_or_unexpected_first_party_login_receives_nothing() {
    for login in [
        "https://synthetic.consent.azure-apihub.net.evil.test/firstPartyLogin?data=synthetic",
        "https://synthetic.consent.azure-apihub.net/login?data=synthetic",
        "https://synthetic.consent.azure-apihub.net/firstPartyLogin?data=synthetic&next=elsewhere",
        "http://synthetic.consent.azure-apihub.net/firstPartyLogin?data=synthetic",
    ] {
        let server = MockServer::start().await;
        mount_created(&server, login).await;
        let tokens = Arc::new(Tokens::default());
        assert_eq!(
            transport(&server, tokens.clone()).workflow(&authorize()).await,
            Err(WorkflowFailure::InvalidResponse)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1, "{login}");
        assert_eq!(*tokens.0.lock().unwrap(), ["https://service.powerapps.com/"]);
    }
}
#[tokio::test]
async fn an_already_connected_connection_is_not_signed_in_again() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"name":"shared-teams-synthetic","properties":{"statuses":[{"status":"Connected"}]}}),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let tokens = Arc::new(Tokens::default());
    assert!(transport(&server, tokens).workflow(&authorize()).await.is_ok());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(authorize().writes());
}
#[test]
fn environment_hosts_follow_the_power_platform_form() {
    use teams_proto::workflow::platform_host;
    assert_eq!(
        platform_host("Default-0a1b2c3d-0000-4000-8000-000000000042").as_deref(),
        Some("default0a1b2c3d0000400080000000000000.42.environment.api.powerplatform.com")
    );
    assert_eq!(platform_host("evil.test/x"), None);
    assert_eq!(platform_host("a"), None);
}

const FLOW_PATH: &str = "/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows/synthetic-flow";

fn overview_requests() -> Vec<WorkflowRequest> {
    let environment = || "Default-synthetic".to_owned();
    let flow_id = || "synthetic-flow".to_owned();
    vec![
        WorkflowRequest::Rename {
            environment: environment(),
            flow: flow_id(),
            name: "Synthetic name".into(),
        },
        WorkflowRequest::SetEnabled {
            environment: environment(),
            flow: flow_id(),
            enabled: false,
        },
        WorkflowRequest::Owners {
            environment: environment(),
            flow: flow_id(),
        },
        WorkflowRequest::AddOwner {
            environment: environment(),
            flow: flow_id(),
            owner: teams_proto::Principal {
                id: "synthetic-owner".into(),
                name: "Synthetic Person".into(),
                email: Some("person@example.test".into()),
            },
        },
        WorkflowRequest::RemoveOwner {
            environment: environment(),
            flow: flow_id(),
            owner: "synthetic-owner".into(),
        },
    ]
}

/// Rename, Turn off/on and the owners' operations send what Teams' Overview sends, each once.
#[tokio::test]
async fn overview_changes_send_what_teams_sends() {
    use wiremock::matchers::body_json;
    let server = MockServer::start().await;
    let principal =
        json!({"id":"synthetic-owner","displayName":"Synthetic Person","email":"person@example.test","type":"User"});
    let routes = [
        (
            "PATCH",
            FLOW_PATH.to_owned(),
            Some(json!({"properties":{"displayName":"Synthetic name"}})),
        ),
        ("POST", format!("{FLOW_PATH}/stop"), Some(json!({}))),
        ("GET", format!("{FLOW_PATH}/owners"), None),
        (
            "POST",
            format!("{FLOW_PATH}/modifyowners"),
            Some(json!({"put":[{"name":"synthetic-owner","properties":{"principal":principal}}]})),
        ),
        ("DELETE", format!("{FLOW_PATH}/owners/synthetic-owner"), None),
    ];
    for (verb, route, body) in routes {
        let mock = Mock::given(method(verb))
            .and(path(route))
            .and(query_param("api-version", "2016-11-01"))
            .and(header("authorization", "Bearer synthetic-flow-token"));
        let mock = match body {
            Some(body) => mock.and(body_json(body)),
            None => mock,
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(1)
            .mount(&server)
            .await;
    }
    let requests = overview_requests();
    let transport = transport(&server, Arc::default());
    for request in &requests {
        assert!(transport.workflow(request).await.is_ok(), "{request:?}");
    }
    assert!(!requests[2].writes());
    assert!(
        requests
            .iter()
            .enumerate()
            .all(|(at, request)| at == 2 || request.writes())
    );
    let sent = server.received_requests().await.unwrap();
    assert!(sent.iter().any(|request| {
        request.url.path().ends_with("/modifyowners")
            && request
                .url
                .query()
                .is_some_and(|query| query.contains("cascadeoperation=true"))
    }));
}

/// The card editor's reads: a connector operation's definition and a list's or a folder tree's choices go to the
/// environment's Power Platform host with the account's own Flow token, and the triggers read to the Flow API. None
/// of them is a write.
#[tokio::test]
async fn card_editor_reads_go_to_the_environment_host_with_the_flow_token() {
    use wiremock::matchers::body_json;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/powerautomate/apis/shared_planner/apiOperations/OnCompleteTask_V3",
        ))
        .and(query_param("api-version", "1"))
        .and(query_param(
            "$expand",
            "properties/inputsDefinition,properties/responsesDefinition,properties/connector",
        ))
        .and(header("authorization", "Bearer synthetic-flow-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"properties":{"inputsDefinition":{}}})))
        .expect(1)
        .mount(&server)
        .await;
    let choices = json!({"parameters":{"groupId":"synthetic-group"},"dynamicInvocationDefinition":{"operationId":"ListGroupPlans"}});
    for (route, body) in [("listEnum", choices.clone()), ("listTree", json!({"parameters":{}}))] {
        Mock::given(method("POST"))
            .and(path(format!(
                "/powerautomate/apis/shared_planner/connections/shared-planner-synthetic/{route}"
            )))
            .and(query_param("api-version", "1"))
            .and(header("authorization", "Bearer synthetic-flow-token"))
            .and(body_json(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("{FLOW_PATH}/triggers")))
        .and(query_param("api-version", "2016-11-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .expect(1)
        .mount(&server)
        .await;
    let environment = || "Default-synthetic".to_owned();
    let requests = [
        WorkflowRequest::Operation {
            environment: environment(),
            api: "shared_planner".into(),
            operation: "OnCompleteTask_V3".into(),
        },
        WorkflowRequest::Choices {
            environment: environment(),
            api: "shared_planner".into(),
            connection: "shared-planner-synthetic".into(),
            tree: false,
            body: choices,
        },
        WorkflowRequest::Choices {
            environment: environment(),
            api: "shared_planner".into(),
            connection: "shared-planner-synthetic".into(),
            tree: true,
            body: json!({"parameters":{}}),
        },
        WorkflowRequest::Triggers {
            environment: environment(),
            flow: "synthetic-flow".into(),
        },
    ];
    let transport = transport(&server, Arc::default());
    for request in &requests {
        assert!(!request.writes(), "{request:?} is a read");
        assert!(transport.workflow(request).await.is_ok(), "{request:?}");
    }
}

#[test]
fn a_connector_read_names_only_plain_path_segments() {
    let read = |api: &str, operation: &str| WorkflowRequest::Operation {
        environment: "Default-synthetic".into(),
        api: api.into(),
        operation: operation.into(),
    };
    assert!(read("shared_planner", "ListGroupPlans").platform_endpoint().is_some());
    for (api, operation) in [
        ("shared_planner", "../flows"),
        ("shared/x", "op"),
        ("", "op"),
        ("api", "a?b"),
    ] {
        assert!(read(api, operation).platform_endpoint().is_none(), "{api} {operation}");
    }
    assert!(
        WorkflowRequest::List {
            environment: "Default-synthetic".into()
        }
        .platform_endpoint()
        .is_none()
    );
}
