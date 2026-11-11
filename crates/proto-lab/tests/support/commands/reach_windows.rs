//! Synthetic chat service reads exercise the operator audit, including paging and evidence publication.
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const BASE: &str = "https://synthetic.msg.teams.microsoft.com";
const OID: &str = "00000000-0000-4000-8000-00000000000a";
const TID: &str = "00000000-0000-4000-8000-000000000001";

struct SyntheticCredentials;

impl Credentials for SyntheticCredentials {
    fn bearer(&self, _: &str) -> impl std::future::Future<Output = Result<Secret, TokenError>> + Send {
        std::future::ready(Ok(Secret::new("synthetic-reach-token")))
    }

    async fn bearer_rejected(&self, audience: &str, _: &Secret) -> Result<Secret, TokenError> {
        self.bearer(audience).await
    }
}

fn config(root: &std::path::Path) -> LabConfig {
    let data = root.join("audit").to_string_lossy().replace('\\', "/");
    let text = format!(
        r#"data_dir = "{data}"
[[accounts]]
alias = "a"
upn = "synthetic-a@example.test"
mri = "8:orgid:{OID}"
[[accounts]]
alias = "b"
upn = "synthetic-b@example.test"
mri = "8:orgid:synthetic-b"
[[accounts]]
alias = "c"
upn = "synthetic-c@example.test"
[[contacts]]
name = "owner"
mri = "8:orgid:synthetic-owner"
[[service_participants]]
name = "echo"
mri = "8:echo123"
"#
    );
    LabConfig::parse(&text, root).unwrap()
}

fn client(server: &MockServer) -> Client<HttpTransport<SyntheticCredentials>> {
    Client::new(HttpTransport::with_mock_origin(Arc::new(SyntheticCredentials), BASE, &server.uri()).unwrap())
}

async fn conversations(server: &MockServer, body: Value) {
    Mock::given(method("GET"))
        .and(path("/v1/users/ME/conversations"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn members(server: &MockServer, id: &str, people: &[&str]) {
    let people: Vec<_> = people.iter().map(|id| json!({"id":id})).collect();
    Mock::given(method("GET"))
        .and(path(format!("/v1/threads/{id}/members")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "totalMemberCount":people.len(),"members":people
        })))
        .expect(1)
        .mount(server)
        .await;
}

fn evidence(config: &LabConfig) -> Value {
    let path = config.paths().data_dir().join("reach-a.json");
    teams_platform::private::check_file(&std::fs::File::open(&path).unwrap()).unwrap();
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn paged_gets_publish_private_evidence_without_selecting_visible_outside_chats() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let server = MockServer::start().await;
    let next = format!("{BASE}/v1/users/ME/conversations?page=2");
    conversations(
        &server,
        json!({"conversations":[{"id":"synthetic-ab"}],"_metadata":{"forwardLink":next}}),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/v1/users/ME/conversations"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "conversations":[{"id":"48:synthetic-private"},{"id":"synthetic-outside"},{"id":"synthetic-services"}]
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    members(
        &server,
        "synthetic-ab",
        &[&format!("8:orgid:{OID}"), "8:orgid:synthetic-b"],
    )
    .await;
    members(&server, "synthetic-outside", &["8:orgid:synthetic-outsider"]).await;
    members(
        &server,
        "synthetic-services",
        &["8:orgid:synthetic-owner", "8:echo123", "28:synthetic-bot"],
    )
    .await;
    audit_reads(
        &config,
        Alias::A,
        &["synthetic-ab".into()],
        &ChatReads::new(&client(&server)),
    )
    .await
    .unwrap();
    let result = evidence(&config);
    assert_eq!(result["all_rosters_complete"], true);
    assert_eq!(result["all_participants_allowed"], false);
    assert_eq!(result["selected_targets_checked"], true);
    assert_eq!(result["chats"][1]["source"], "account-private-stream");
    assert_eq!(result["chats"][3]["members"][0]["allowed_as"], "own-account");
    assert_eq!(result["chats"][3]["members"][1]["allowed_as"], "echo");
    assert!(!config.paths().data_dir().join("reach-a-pending.json").exists());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method == "GET")
    );
}

#[tokio::test]
async fn selected_outsider_and_missing_target_refuse_after_publishing_the_reason() {
    for selected in ["synthetic-outside", "missing"] {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let server = MockServer::start().await;
        conversations(&server, json!({"conversations":[{"id":"synthetic-outside"}]})).await;
        members(&server, "synthetic-outside", &["8:orgid:synthetic-outsider"]).await;
        let error = audit_reads(&config, Alias::A, &[selected.into()], &ChatReads::new(&client(&server)))
            .await
            .unwrap_err();
        assert!(error.is::<Refused>());
        assert_eq!(evidence(&config)["selected_targets_checked"], false);
    }
}

#[tokio::test]
async fn malformed_pages_foreign_links_and_paging_cycles_never_publish_success() {
    for body in [
        json!({"conversations":[{"missing":"id"}]}),
        json!({"conversations":[],"_metadata":{"forwardLink":"https://foreign.example.test/v1/users/ME/conversations"}}),
        json!({"conversations":[],"_metadata":{"forwardLink":format!("{BASE}/v1/users/ME/conversations?page=loop")}}),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        let server = MockServer::start().await;
        conversations(&server, body).await;
        assert!(
            audit_reads(&config, Alias::A, &[], &ChatReads::new(&client(&server)))
                .await
                .is_err()
        );
        assert!(!config.paths().data_dir().join("reach-a.json").exists());
        assert!(server.received_requests().await.unwrap().len() <= 2);
    }
}

#[tokio::test]
async fn failed_member_read_retains_pending_stage_without_evidence_of_success() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let server = MockServer::start().await;
    conversations(&server, json!({"conversations":[{"id":"synthetic-ab"}]})).await;
    // An unhandled members request gets wiremock's 404, without retry.
    let error = audit_reads(&config, Alias::A, &[], &ChatReads::new(&client(&server)))
        .await
        .unwrap_err();
    assert!(error.is::<teams_proto::chat::ChatReadError>());
    let pending: Value =
        serde_json::from_slice(&std::fs::read(config.paths().data_dir().join("reach-a-pending.json")).unwrap())
            .unwrap();
    assert_eq!(pending, json!({"chat":"synthetic-ab","stage":"members"}));
    assert!(!config.paths().data_dir().join("reach-a.json").exists());
}

fn native_target(root: &std::path::Path) -> NativeTarget {
    NativeTarget::resolve(Alias::A, None, None, Some(root.as_os_str().to_owned())).unwrap()
}

#[tokio::test]
async fn native_client_checks_identity_and_region_before_any_service_or_credential_read() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let target = native_target(dir.path());
    assert!(native_client(&config, Alias::A, &target).await.is_err());
    let identity = teams_auth::id_token::Account {
        username: "synthetic-a@example.test".into(),
        object_id: OID.into(),
        tenant: TID.into(),
        display_name: None,
    };
    target.save(&identity, None).unwrap();
    assert!(native_client(&config, Alias::A, &target).await.is_err());
    for region in [
        json!({"region":null,"partition":null,"regionGtms":{}}),
        json!({"region":null,"partition":null,"regionGtms":{"chatServiceAfd":BASE}}),
    ] {
        let region = Region::from_json(&region).unwrap();
        target.save(&identity, Some(&region)).unwrap();
        let result = native_client(&config, Alias::A, &target).await;
        if region.base(CHAT_BASE_GTM).is_some() {
            assert!(result.is_ok());
        } else {
            assert!(result.err().unwrap().is::<Refused>());
        }
    }
    let file = target.root().join("accounts.toml");
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace(OID, "00000000-0000-4000-8000-00000000000b");
    std::fs::write(&file, text).unwrap();
    let error = native_client(&config, Alias::A, &target).await.err().unwrap();
    assert!(error.is::<Refused>());
    assert!(!error.to_string().contains("synthetic-a"));
}
