//! `proto-lab probe object`: our own `ic3` token from the stored refresh token, then only `GET`s of one media service
//! object, and a report of statuses, header names and the answer's outline, where lab ids are named for what they are
//! and no other value is shown. Against wiremock: Entra on one mock server, the async gateway on another, reached
//! through the `test-support` mock origin. Every value here is made up, the answer's shape too (no capture has shown
//! it yet).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use proto_lab::commands::object_probe::{
    Known, OBJECT_FORMS, ObjectProbeError, is_object_id, is_plain_word, object_url, outline, probe_object,
};
use proto_lab::commands::probe::{IC3_RESOURCE, Origin, ProbeError};
use proto_lab::config::{Alias, LabConfig};
use serde_json::{Value, json};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore as _};
use teams_auth::token::{EntraConfig, TokenClient};
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "0000000a-0000-4000-8000-00000000000a";
const GUID_B: &str = "0000000b-0000-4000-8000-00000000000b";
const GUID_X: &str = "000000ff-0000-4000-8000-0000000000ff";
const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";
const UPN_A: &str = "bot-a@lab.example.com";
const REFRESH_TOKEN: &str = "made-up-refresh-token-0123456789";
const OBJECT: &str = "0-wus-d1-0123456789abcdef0123456789abcdef";
/// A word only the gateway's answers hold: it must never be printed.
const BODY_MARKER: &str = "PlantedBodyMarker7";

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

fn ab_chat() -> String {
    format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")
}

/// An unsigned `ic3` access token, built at run time.
fn ic3_token() -> String {
    let part = |value: &Value| URL_SAFE_NO_PAD.encode(value.to_string());
    let exp = (SystemTime::now() + Duration::from_secs(7_200))
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let claims = json!({"aud": IC3_RESOURCE, "exp": exp, "scp": "Teams.AccessAsUser.All", "tid": TENANT});
    format!(
        "{}.{}.c2ln",
        part(&json!({"alg": "RS256", "typ": "JWT"})),
        part(&claims)
    )
}

async fn entra(token: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/v2.0/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("ic3.teams.office.com%2F.default"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_type": "Bearer",
            "scope": "https://ic3.teams.office.com/Teams.AccessAsUser.All",
            "expires_in": 7_000,
            "ext_expires_in": 7_000,
            "access_token": token,
        })))
        .mount(&server)
        .await;
    server
}

/// A made-up object answer: who may read it, its views, and values that must not be shown.
fn answer() -> Value {
    json!({
        "id": OBJECT,
        "type": "pish/image",
        "permissions": {ab_chat(): ["read"], format!("19:{GUID_A}_{GUID_X}@unq.gbl.spaces"): ["read"]},
        "owner": format!("8:orgid:{GUID_A}"),
        "filename": BODY_MARKER,
        "size": 1234,
        "expiry": 1_800_000_000,
        "views": [{"name": "imgo", "ready": true}],
    })
}

async fn gateway() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1/objects/{OBJECT}/")))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer()))
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
async fn the_object_probe_reads_the_object_by_get_only_and_names_only_lab_ids() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path())?;
    let token = ic3_token();
    let entra = entra(&token).await;
    let gateway = gateway().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    let probe = probe_object(
        &store().await?,
        &tokens,
        &config,
        Alias::A,
        OBJECT,
        &Origin::Mock(gateway.uri()),
    )
    .await?;

    let requests = gateway.received_requests().await.ok_or("no recording")?;
    assert_eq!(requests.len(), OBJECT_FORMS.len());
    let bearer = format!("Bearer {token}");
    for request in &requests {
        assert_eq!(request.method.as_str(), "GET");
        assert!(request.body.is_empty());
        let header = |name: &str| request.headers.get(name).and_then(|value| value.to_str().ok());
        assert_eq!(header("authorization"), Some(bearer.as_str()));
    }
    let migrating = requests
        .iter()
        .filter(|request| request.headers.get("x-ms-migration").is_some())
        .count();
    assert_eq!(
        migrating, 1,
        "once with the web client's headers, once with the token alone"
    );

    let text = format!("{}\n{probe:?}", probe.lines(SystemTime::now()).join("\n"));
    // the object id is the operator's own input (Debug holds it); in the answer it is named for what it is
    for hidden in [token.as_str(), BODY_MARKER, REFRESH_TOKEN, GUID_A, GUID_B, GUID_X] {
        assert!(!text.contains(hidden), "{hidden} shown: {text}");
    }
    for shown in [
        "$.permissions.<the a-b chat>: array, 1 item(s)",
        "$.permissions.<the a-b chat>[0]: string \"read\"",
        "$.permissions.?: array, 1 item(s)",
        "$.owner: string <a's MRI>",
        "$.id: string <the object>",
        "$.type: string \"pish/image\"",
        "$.size: number",
        "$.views[0].ready: bool true",
        "$.filename: string of 18 char(s)",
        "aud https://ic3.teams.office.com",
        "content-type application/json",
    ] {
        assert!(text.contains(shown), "{shown} missing: {text}");
    }
    let events = probe.reference_events();
    assert_eq!(events.len(), OBJECT_FORMS.len());
    assert!(events.iter().all(|event| event["url"] == object_url(OBJECT)));
    assert!(!serde_json::to_string(&events)?.contains(&token));
    Ok(())
}

#[tokio::test]
async fn a_bad_object_id_is_refused_before_anything_goes_out() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = lab(dir.path())?;
    let entra = entra(&ic3_token()).await;
    let gateway = gateway().await;
    let tokens = TokenClient::new(EntraConfig::teams_native().with_authority(entra.uri()))?;
    for id in ["", "..", "a/b", "a?b=1", "0-wus-d1-0000/content/imgpsh", "a b"] {
        let outcome = probe_object(
            &store().await?,
            &tokens,
            &config,
            Alias::A,
            id,
            &Origin::Mock(gateway.uri()),
        )
        .await;
        assert!(matches!(outcome, Err(ObjectProbeError::BadId)), "{id}: {outcome:?}");
    }
    assert!(entra.received_requests().await.ok_or("no recording")?.is_empty());
    assert!(gateway.received_requests().await.ok_or("no recording")?.is_empty());
    // and without a refresh token nothing reaches the gateway
    let outcome = probe_object(
        &MemorySecretStore::new(),
        &tokens,
        &config,
        Alias::A,
        OBJECT,
        &Origin::Mock(gateway.uri()),
    )
    .await;
    assert!(
        matches!(outcome, Err(ObjectProbeError::Probe(ProbeError::Token(_)))),
        "{outcome:?}"
    );
    assert!(gateway.received_requests().await.ok_or("no recording")?.is_empty());
    Ok(())
}

#[test]
fn an_object_id_is_one_plain_segment() {
    assert!(is_object_id(OBJECT));
    assert!(is_object_id("0-eus-d3-ABC_def.1"));
    for id in ["", ".", "..", "a/b", "a%2Fb", "a b", &"x".repeat(129)] {
        assert!(!is_object_id(id), "{id}");
    }
}

#[test]
fn only_a_plain_word_is_shown() {
    for word in ["read", "pish/image", "Inline", "sharing/file", "imgpsh_fullsize_anim"] {
        assert!(is_plain_word(word), "{word}");
    }
    for value in [
        "",
        "8:orgid:x1",
        "0-wus-d1-0000",
        "eyJhbGciOiJub25lIn0",
        &"a".repeat(33),
        "a b",
    ] {
        assert!(!is_plain_word(value), "{value}");
    }
}

#[test]
fn an_outline_stops_at_its_limits() {
    let deep = (0..20).fold(json!("leaf"), |inner, _| json!({ "k": inner }));
    let lines = outline(&deep, &Known::default());
    assert!(lines.len() <= 10, "{lines:?}");
    let long = json!((0..500).map(|at| json!({ "n": at })).collect::<Vec<_>>());
    let lines = outline(&long, &Known::default());
    assert!(lines.len() <= 201, "{}", lines.len());
    assert!(lines[0].contains("500 item(s)"));
}
