//! `proto-lab probe profile`: our own Graph token from the stored refresh token, then only `GET`s to Graph's user and
//! picture reads of the lab accounts, and a report of statuses, key names, types and sizes only. Against wiremock:
//! Entra on one mock server, Graph on another, reached through the `test-support` mock origin. Every value here is
//! made up.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proto_lab::commands::probe::{Origin, ProbeError};
use proto_lab::commands::profile_probe::{GRAPH_RESOURCE, ProfileReadKind, object_id, probe_profile};
use proto_lab::config::{Alias, LabConfig};
use serde_json::{Value, json};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::token::{EntraConfig, TokenClient};
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const GUID_B: &str = "0000000b-0000-4000-8000-00000000000b";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";
const UPN_A: &str = "bot-a@lab.example.com";
const REFRESH_TOKEN: &str = "made-up-refresh-token-0123456789";
/// A word only Graph's answers hold: it must never be printed.
const BODY_MARKER: &str = "PlantedBodyMarker";

fn lab(dir: &Path) -> TestResult<LabConfig> {
    let toml = format!(
        "data_dir = \"{}\"\n\n\
         [[accounts]]\nalias = \"a\"\nupn = \"{UPN_A}\"\nmri = \"8:orgid:{GUID_A}\"\n\n\
         [[accounts]]\nalias = \"b\"\nupn = \"bot-b@lab.example.com\"\nmri = \"8:orgid:{GUID_B}\"\n\n\
         [[accounts]]\nalias = \"c\"\nupn = \"bot-c@lab.example.com\"\n",
        dir.display()
    );
    Ok(LabConfig::parse(&toml, dir)?)
}

/// An unsigned access token for Graph, built at run time.
fn graph_token() -> String {
    let part = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let exp = (SystemTime::now() + Duration::from_secs(7_200))
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let claims = json!({"aud": GRAPH_RESOURCE, "exp": exp, "scp": "User.ReadBasic.All", "tid": TENANT});
    format!(
        "{}.{}.c2ln",
        part(&json!({"alg": "RS256", "typ": "JWT"})),
        part(&claims)
    )
}

/// Entra's answer to a refresh grant for Graph, and nothing else.
async fn entra(token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("graph.microsoft.com%2F.default"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "scope": "https://graph.microsoft.com/User.ReadBasic.All https://graph.microsoft.com/.default",
            "expires_in": 7_000,
            "ext_expires_in": 7_000,
            "access_token": token,
        })))
        .mount(&server)
        .await;
    server
}

fn user(guid: &str) -> Value {
    json!({
        "@odata.context": "https://graph.microsoft.com/v1.0/$metadata#users/$entity",
        "id": guid,
        "displayName": format!("{BODY_MARKER} Name"),
        "mail": null,
        "businessPhones": [],
    })
}

/// Graph: a's and b's user records, a's picture, and b without one.
async fn graph() -> MockServer {
    let server = MockServer::start().await;
    for guid in [GUID_A, GUID_B] {
        Mock::given(method("GET"))
            .and(path(format!("/v1.0/users/{guid}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(user(guid)))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}")))
        .and(query_param("$select", "id,displayName"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": GUID_A, "displayName": BODY_MARKER})))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}/photo/$value")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 4], "image/jpeg"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_A}/photos/96x96/$value")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0xFF, 0xD8, 0xFF, 0xE0], "image/jpeg"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1.0/users/{GUID_B}/photo/$value")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": "ImageNotFound", "message": format!("{BODY_MARKER} not found")}
        })))
        .mount(&server)
        .await;
    server
}

async fn store() -> TestResult<MemorySecretStore> {
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(UPN_A), &Secret::new(REFRESH_TOKEN))
        .await?;
    Ok(store)
}

#[tokio::test]
async fn the_profile_probe_reads_each_lab_account_with_our_graph_token_by_get_only() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path())?;
    let token = graph_token();
    let entra = entra(&token).await;
    let graph = graph().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    let probe = probe_profile(&store().await?, &tokens, &config, Alias::A, &Origin::Mock(graph.uri())).await?;

    // a and b, each read every way; c has no MRI and is skipped
    let requests = graph.received_requests().await.ok_or("no recording")?;
    assert_eq!(requests.len(), 2 * ProfileReadKind::ALL.len());
    let bearer = format!("Bearer {token}");
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        assert!(request.body.is_empty());
        let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
        assert_eq!(header("authorization"), Some(bearer.as_str()));
        assert_eq!(header("accept"), Some("application/json"));
        assert!(!request.headers.contains_key("x-ms-migration"));
    }
    let selected = requests
        .iter()
        .filter(|request| request.url.query() == Some("%24select=id%2CdisplayName"))
        .count();
    assert_eq!(selected, 2);
    assert_eq!(probe.skipped, vec![Alias::C]);

    let text = format!("{}\n{probe:?}", probe.lines(SystemTime::now()).join("\n"));
    for hidden in [token.as_str(), BODY_MARKER, REFRESH_TOKEN, GUID_A, GUID_B] {
        assert!(!text.contains(hidden), "{hidden} shown: {text}");
    }
    assert!(text.contains(&format!("aud {GRAPH_RESOURCE}")), "{text}");
    assert!(text.contains("User.ReadBasic.All"), "{text}");
    assert!(text.contains("displayName:string"), "{text}");
    assert!(text.contains("businessPhones:array"), "{text}");
    assert!(text.contains("mail:null"), "{text}");
    assert!(text.contains("content-type image/jpeg; 8 bytes"), "{text}");
    assert!(text.contains("content-type image/jpeg; 4 bytes"), "{text}");
    assert!(text.contains("error code ImageNotFound"), "{text}");
    assert!(text.contains("404"), "{text}");
    Ok(())
}

#[tokio::test]
async fn without_a_stored_refresh_token_nothing_reaches_graph() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path())?;
    let entra = entra(&graph_token()).await;
    let graph = graph().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    let outcome = probe_profile(
        &MemorySecretStore::new(),
        &tokens,
        &config,
        Alias::A,
        &Origin::Mock(graph.uri()),
    )
    .await;
    assert!(matches!(outcome, Err(ProbeError::Token(_))), "{outcome:?}");
    assert!(graph.received_requests().await.ok_or("no recording")?.is_empty());
    Ok(())
}

#[test]
fn only_an_orgid_mri_with_a_guid_names_an_object_id() {
    assert_eq!(object_id(&format!("8:orgid:{GUID_A}")), Some(GUID_A));
    for mri in [
        "8:orgid:not-a-guid",
        "28:0000000a-0000-4000-8000-00000000000a",
        "8:orgid:0000000a-0000-4000-8000-00000000000a/x",
        "8:orgid:",
    ] {
        assert_eq!(object_id(mri), None, "{mri}");
    }
}
