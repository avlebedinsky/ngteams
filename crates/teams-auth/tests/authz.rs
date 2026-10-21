//! The skypetoken exchange against `wiremock`: the request authz gets, the skypetoken, its expiry and the region GTMs
//! from the answer, the refusals, and storing the skypetoken.
//!
//! The answers come from the fixtures under `tests/fixtures/auth/` (`authz-*.json`, see `docs/protocol/auth.md`); the
//! tests read their expectations from the fixture, so a fixture rewritten synthetic-from-capture (written by hand from
//! a local reference copy, same shape, made-up values; captures never enter the repo) drops in. A skypetoken that is a
//! JWT (for the `exp` rule) is built unsigned at run time: the fixture guard keeps JWTs out of the repo.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::authz::{
    AUTHZ_ENDPOINT, AuthzClient, AuthzError, AuthzKind, CHAT_BASE_GTM, Region, RegionFileError, skypetoken_key,
};
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/auth");
const ACCOUNT: &str = "a@lab.example.test";
const SPACES_TOKEN: &str = "made-up-spaces-access-token";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// One recorded authz exchange.
struct Fixture {
    path: String,
    status: u16,
    body: Value,
}

impl Fixture {
    fn load(name: &str) -> TestResult<Self> {
        let json: Value = serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/{name}.json"))?)?;
        let url = url::Url::parse(json["request"]["url"].as_str().ok_or("request.url")?)?;
        Ok(Self {
            path: url.path().to_owned(),
            status: u16::try_from(json["response"]["status"].as_u64().ok_or("response.status")?)?,
            body: json["response"]["body"].clone(),
        })
    }

    /// The same exchange with another answer body.
    fn with_body(mut self, body: Value) -> Self {
        self.body = body;
        self
    }

    fn response(&self) -> ResponseTemplate {
        ResponseTemplate::new(self.status).set_body_json(&self.body)
    }

    /// Mounts the answer for POSTs to the fixture's path, and a client for it. An authzcore answer is served on the
    /// authz path: the client posts to nothing else.
    async fn mount(&self, server: &MockServer) -> TestResult<AuthzClient> {
        let authz = self.path.replace("authzcore", "authz");
        Mock::given(method("POST"))
            .and(path(authz.as_str()))
            .respond_with(self.response())
            .mount(server)
            .await;
        Ok(AuthzClient::with_endpoint(&format!("{}{authz}", server.uri()))?)
    }
}

fn spaces_token() -> Secret {
    Secret::new(SPACES_TOKEN)
}

/// An unsigned JWT with these claims.
fn jwt(claims: &Value) -> String {
    format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// The fixture's `regionGtms` (every entry is a URL string).
fn gtms(body: &Value) -> BTreeMap<String, String> {
    body["regionGtms"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, url)| Some((name.clone(), url.as_str()?.to_owned())))
        .collect()
}

#[tokio::test]
async fn the_exchange_posts_the_spaces_token_with_no_body_and_reads_the_region() {
    let fixture = Fixture::load("authz-ok").unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(fixture.path.as_str()))
        .and(header("authorization", format!("Bearer {SPACES_TOKEN}").as_str()))
        .and(header("claimsChallengeCapable", "true"))
        .and(header("X-MS-Migration", "True"))
        .and(header("ms-teams-authz-type", "ExplicitLogin"))
        .and(body_string(""))
        .respond_with(fixture.response())
        .expect(1)
        .mount(&server)
        .await;
    let client = AuthzClient::with_endpoint(&format!("{}{}", server.uri(), fixture.path)).unwrap();

    let before = SystemTime::now();
    let skypetoken = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap();
    let after = SystemTime::now();

    let tokens = &fixture.body["tokens"];
    assert_eq!(skypetoken.token.expose(), tokens["skypeToken"].as_str().unwrap());
    let lifetime = Duration::from_secs(tokens["expiresIn"].as_u64().unwrap());
    assert!(skypetoken.expires_at >= before + lifetime && skypetoken.expires_at <= after + lifetime);
    assert_eq!(skypetoken.region.name.as_deref(), fixture.body["region"].as_str());
    assert_eq!(
        skypetoken.region.partition.as_deref(),
        fixture.body["partition"].as_str()
    );
    assert_eq!(skypetoken.region.endpoints, gtms(&fixture.body));
    assert_eq!(
        skypetoken.region.endpoint("chatServiceAfd"),
        fixture.body["regionGtms"]["chatServiceAfd"].as_str()
    );
    assert_eq!(skypetoken.skype_id, None, "the fixture's token isn't a JWT");
    assert!(!format!("{skypetoken:?}").contains(tokens["skypeToken"].as_str().unwrap()));
}

#[tokio::test]
async fn every_request_carries_one_fresh_correlation_guid() {
    let fixture = Fixture::load("authz-ok").unwrap();
    let server = MockServer::start().await;
    let client = fixture.mount(&server).await.unwrap();

    client.exchange(&spaces_token(), AuthzKind::TokenRefresh).await.unwrap();
    client.exchange(&spaces_token(), AuthzKind::TokenRefresh).await.unwrap();

    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let ids: Vec<String> = requests
        .iter()
        .map(|request| {
            let id = request.headers.get("clientRequestId").unwrap().to_str().unwrap();
            assert_eq!(request.headers.get("x-ms-request-id").unwrap(), id);
            assert_eq!(request.headers.get("ms-teams-authz-type").unwrap(), "TokenRefresh");
            id.to_owned()
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    for id in ids {
        let groups: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12], "{id}");
        assert_eq!(&id[14..15], "4", "version 4: {id}");
    }
}

#[tokio::test]
async fn the_expiry_is_the_tokens_exp_and_the_region_falls_back_to_rgn() {
    let fixture = Fixture::load("authz-ok").unwrap();
    let exp = unix(SystemTime::now()) + 3_600;
    let token = jwt(&json!({"exp": exp, "rgn": "amer", "skypeid": "orgid:00000000-0000-4000-8000-00000000000a"}));
    let mut body = fixture.body.clone();
    body["tokens"] = json!({"skypeToken": token, "expiresIn": 86_400});
    body.as_object_mut().unwrap().remove("region");
    let server = MockServer::start().await;
    let client = fixture.with_body(body).mount(&server).await.unwrap();

    let skypetoken = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap();

    assert_eq!(unix(skypetoken.expires_at), exp, "exp wins over a longer expiresIn");
    assert_eq!(skypetoken.region.name.as_deref(), Some("amer"));
    assert_eq!(
        skypetoken.skype_id.as_deref(),
        Some("orgid:00000000-0000-4000-8000-00000000000a")
    );
}

#[tokio::test]
async fn a_top_level_skypetoken_object_is_read_first() {
    let fixture = Fixture::load("authz-ok").unwrap();
    let mut body = fixture.body.clone();
    body["skypeToken"] = json!({"skypetoken": "the-top-level-one", "expiresIn": "600"});
    let server = MockServer::start().await;
    let client = fixture.with_body(body).mount(&server).await.unwrap();

    let before = SystemTime::now();
    let skypetoken = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap();

    assert_eq!(skypetoken.token.expose(), "the-top-level-one");
    assert!(skypetoken.expires_at >= before + Duration::from_secs(600));
    assert!(skypetoken.expires_at <= SystemTime::now() + Duration::from_secs(600));
}

#[tokio::test]
async fn an_unlicensed_account_is_refused_with_teams_error_code() {
    let fixture = Fixture::load("authz-403-no-license").unwrap();
    let server = MockServer::start().await;
    let client = fixture.mount(&server).await.unwrap();

    let error = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .expect_err("refused");

    let code = fixture.body["errorCode"].as_str().unwrap();
    let AuthzError::Refused {
        status: 403,
        code: Some(ref got),
    } = error
    else {
        panic!("{error:?}");
    };
    assert_eq!(got, code);
    assert_eq!(
        error.to_string(),
        format!("Teams refused the account (authz answered 403: {code})")
    );
    let message = fixture.body["message"].as_str().unwrap();
    assert!(
        !format!("{error} {error:?}").contains(message),
        "the message isn't kept"
    );
}

#[tokio::test]
async fn a_refusal_keeps_only_an_identifier_shaped_error_code() {
    let server = MockServer::start().await;
    for (body, expected) in [
        (json!({"errorCode": "has spaces and <markup>"}), None),
        (json!({"errorCode": "x".repeat(101)}), None),
        (json!({"errorCode": 7}), None),
        (json!({"message": "no code"}), None),
        (
            json!({"errorCode": "TeamsDisabledForTenantForbidden"}),
            Some("TeamsDisabledForTenantForbidden"),
        ),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_json(&body))
            .mount(&server)
            .await;
        let client = AuthzClient::with_endpoint(&format!("{}/v1.0/authz", server.uri())).unwrap();
        let error = client
            .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AuthzError::Refused { status: 403, ref code } if code.as_deref() == expected),
            "{body}: {error:?}"
        );
    }
}

#[tokio::test]
async fn other_failures_are_told_apart_by_status() {
    let server = MockServer::start().await;
    let cases = [
        (
            ResponseTemplate::new(401).insert_header(
                "WWW-Authenticate",
                r#"Bearer error="insufficient_claims", claims="e30""#,
            ),
            "unauthorized challenge",
        ),
        (ResponseTemplate::new(401), "unauthorized"),
        (
            ResponseTemplate::new(429).insert_header("Retry-After", "7"),
            "throttled 7",
        ),
        (ResponseTemplate::new(503), "unavailable"),
        (ResponseTemplate::new(404), "unexpected"),
    ];
    for (response, expected) in cases {
        server.reset().await;
        Mock::given(method("POST")).respond_with(response).mount(&server).await;
        let client = AuthzClient::with_endpoint(&format!("{}/v1.0/authz", server.uri())).unwrap();
        let error = client
            .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
            .await
            .unwrap_err();
        let kind = match error {
            AuthzError::Unauthorized { claims_challenge: true } => "unauthorized challenge".to_owned(),
            AuthzError::Unauthorized {
                claims_challenge: false,
            } => "unauthorized".to_owned(),
            AuthzError::Unavailable {
                status: 429,
                retry_after: Some(wait),
            } => format!("throttled {}", wait.as_secs()),
            AuthzError::Unavailable {
                status: 503,
                retry_after: None,
            } => "unavailable".to_owned(),
            AuthzError::UnexpectedResponse { status: 404 } => "unexpected".to_owned(),
            ref other => format!("{other:?}"),
        };
        assert_eq!(kind, expected);
        assert!(!format!("{error} {error:?}").contains(SPACES_TOKEN));
    }
}

#[tokio::test]
async fn an_http_date_retry_after_is_kept_as_a_wait() {
    let server = MockServer::start().await;
    let when = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(120));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).insert_header("Retry-After", when.as_str()))
        .mount(&server)
        .await;
    let client = AuthzClient::with_endpoint(&format!("{}/v1.0/authz", server.uri())).unwrap();

    let error = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();

    let AuthzError::Unavailable {
        status: 503,
        retry_after: Some(wait),
    } = error
    else {
        panic!("{error:?}");
    };
    // the date has a one-second resolution, and the request took a moment
    assert!((100..=120).contains(&wait.as_secs()), "{wait:?}");
}

#[tokio::test]
async fn an_answer_without_a_skypetoken_or_region_is_refused() {
    let core = Fixture::load("authzcore-ok").unwrap();
    let server = MockServer::start().await;
    let client = core.mount(&server).await.unwrap();
    let error = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();
    assert!(
        matches!(error, AuthzError::MissingField("tokens.skypeToken")),
        "{error:?}"
    );

    let ok = Fixture::load("authz-ok").unwrap();
    let cases = [
        ("regionGtms", json!(null), "regionGtms"),
        ("regionGtms", json!({}), "regionGtms"),
        ("tokens", json!({"skypeToken": "t"}), "tokens.expiresIn"),
        (
            "tokens",
            json!({"skypeToken": "", "expiresIn": 60}),
            "tokens.skypeToken",
        ),
    ];
    for (field, value, missing) in cases {
        let mut body = ok.body.clone();
        body[field] = value;
        server.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;
        let error = client
            .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AuthzError::MissingField(name) if name == missing),
            "{field}: {error:?}"
        );
    }
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
        .mount(&server)
        .await;
    let error = client
        .exchange(&spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap_err();
    assert!(matches!(error, AuthzError::MissingField("JSON body")), "{error:?}");
}

#[test]
fn only_an_authz_url_is_an_endpoint() {
    assert_eq!(AuthzClient::new().unwrap().endpoint(), AUTHZ_ENDPOINT);
    for good in [
        "https://authsvc.teams.microsoft.com/v1.0/authz",
        "http://127.0.0.1:4000/api/authsvc/v1.0/authz",
        "http://localhost:4000/v1.0/authz",
        "http://[::1]:4000/v1.0/authz",
    ] {
        assert!(AuthzClient::with_endpoint(good).is_ok(), "{good}");
    }
    for bad in [
        "https://teams.microsoft.com/api/authsvc/v1.0/authz/signuptrial",
        "https://teams.microsoft.com/api/authsvc/v1.0/authzcore/signuptrial",
        "https://teams.microsoft.com/api/authsvc/v1.0/authzcore",
        "https://teams.microsoft.com/api/authsvc/v1.0/authz/",
        "https://teams.microsoft.com/api/authsvc/v1.0/authz?trial=1",
        "https://teams.microsoft.com/api/authsvc/v1.0/authz#x",
        "https://user@teams.microsoft.com/api/authsvc/v1.0/authz",
        "http://teams.microsoft.com/api/authsvc/v1.0/authz",
        "ftp://127.0.0.1/authz",
        "not a url",
    ] {
        assert!(
            matches!(AuthzClient::with_endpoint(bad), Err(AuthzError::InvalidEndpoint)),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn the_skypetoken_is_stored_under_the_account_and_a_refusal_stores_nothing() {
    let store = MemorySecretStore::new();
    let key = skypetoken_key(ACCOUNT);
    assert_eq!(key.audience(), "skypetoken");

    let refused = Fixture::load("authz-403-no-license").unwrap();
    let server = MockServer::start().await;
    let client = refused.mount(&server).await.unwrap();
    store.put(&key, &Secret::new("an-older-skypetoken")).await.unwrap();
    client
        .exchange_and_store(&store, ACCOUNT, &spaces_token(), AuthzKind::TokenRefresh)
        .await
        .unwrap_err();
    assert_eq!(
        store.get(&key).await.unwrap().unwrap().expose(),
        "an-older-skypetoken",
        "a refusal leaves the stored one"
    );

    let ok = Fixture::load("authz-ok").unwrap();
    server.reset().await;
    let client = ok.mount(&server).await.unwrap();
    let skypetoken = client
        .exchange_and_store(&store, ACCOUNT, &spaces_token(), AuthzKind::ExplicitLogin)
        .await
        .unwrap();
    assert_eq!(
        store.get(&key).await.unwrap().unwrap().expose(),
        skypetoken.token.expose()
    );
}

#[test]
fn the_region_round_trips_through_its_json() {
    let fixture = Fixture::load("authz-ok").unwrap();
    let region = Region {
        name: Some("emea".to_owned()),
        partition: None,
        endpoints: gtms(&fixture.body),
    };
    let json = region.to_json();
    assert_eq!(json["regionGtms"], fixture.body["regionGtms"]);
    assert_eq!(json["partition"], Value::Null);
    assert_eq!(Region::from_json(&json), Some(region));
    assert_eq!(
        Region::from_json(&json!({"region": 1, "partition": null, "regionGtms": {}})),
        None
    );
    assert_eq!(Region::from_json(&json!({"region": null, "partition": null})), None);
}

#[test]
fn a_saved_region_record_is_read_back_from_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.json");
    let region = Region {
        name: Some("emea".to_owned()),
        partition: None,
        endpoints: BTreeMap::from([(
            "chatService".to_owned(),
            " https://teams.microsoft.com/api/chatsvc/emea ".to_owned(),
        )]),
    };
    // the lab saves the skypetoken's expiry beside the region's keys
    let mut saved = region.to_json();
    saved["skypetokenExpiresAt"] = json!(1_700_000_000);
    std::fs::write(&file, saved.to_string()).unwrap();
    let read = Region::load(&file).unwrap();
    assert_eq!(read, region);
    assert_eq!(
        read.base(CHAT_BASE_GTM),
        Some("https://teams.microsoft.com/api/chatsvc/emea")
    );

    std::fs::write(&file, "{\"region\": 1}").unwrap();
    assert!(matches!(Region::load(&file), Err(RegionFileError::NotARecord(path)) if path == file));
    let missing = dir.path().join("b.json");
    assert!(matches!(Region::load(&missing), Err(RegionFileError::Read { path, .. }) if path == missing));
}
