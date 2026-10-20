//! The session against `wiremock` with a fake clock: an access token per audience cached until the margin, one refresh
//! for concurrent callers, the stored rotated refresh token, a forced refresh for a refused token, the skypetoken
//! renewed only after refreshing `api.spaces.skype.com`, throttling handed back with no hidden retry, renewal
//! notifications, the region's service bases, and no token in any `Debug` or error.
//!
//! The answers are shaped from the fixtures under `tests/fixtures/auth/` (`entra-token-refresh.json`,
//! `authz-ok.json`, `entra-error-429.json`); each test puts made-up tokens and lifetimes of its own into them. A
//! skypetoken that is a JWT (for its `exp`) is built unsigned at run time: the fixture guard keeps JWTs out of the
//! repo.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, AuthzError, Region, skypetoken_key};
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::session::{Clock, REFRESH_MARGIN, Renewal, Session};
use teams_auth::token::{EntraConfig, SPACES_RESOURCE, TokenClient, TokenError};
use tokio::sync::broadcast::error::TryRecvError;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/auth");
const ACCOUNT: &str = "a@lab.example.test";
const IC3: &str = "https://ic3.teams.office.com";
const TOKEN_PATH: &str = "/organizations/oauth2/v2.0/token";
const AUTHZ_PATH: &str = "/api/authsvc/v1.0/authz";
const FIRST_REFRESH_TOKEN: &str = "synthetic-refresh-token-0";
/// Entra's `expires_in` in the answers unless a test says otherwise: about two hours, as the native client gets.
const LIFETIME: u64 = 7_000;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A clock the test moves by hand.
struct FakeClock(Mutex<SystemTime>);

impl FakeClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(UNIX_EPOCH + Duration::from_secs(2_000_000_000))))
    }

    fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        *now += by;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn fixture_body(name: &str) -> TestResult<Value> {
    let json: Value = serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/{name}.json"))?)?;
    Ok(json["response"]["body"].clone())
}

/// Entra's token endpoint: every grant answers the refresh fixture's shape with a new access token and a rotated
/// refresh token, numbered from 1, for the scope asked.
struct Entra {
    template: Value,
    lifetime: u64,
    delay: Duration,
    grants: Arc<AtomicUsize>,
}

impl Respond for Entra {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let grant = self.grants.fetch_add(1, Ordering::SeqCst) + 1;
        let form: BTreeMap<String, String> = url::form_urlencoded::parse(&request.body).into_owned().collect();
        let mut body = self.template.clone();
        body["access_token"] = json!(access_token(grant));
        body["refresh_token"] = json!(format!("synthetic-refresh-token-{grant}"));
        body["expires_in"] = json!(self.lifetime);
        body["ext_expires_in"] = json!(self.lifetime);
        body["scope"] = json!(form.get("scope").cloned().unwrap_or_default());
        ResponseTemplate::new(200).set_body_json(body).set_delay(self.delay)
    }
}

fn access_token(grant: usize) -> String {
    format!("synthetic-access-token-{grant}")
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

/// A skypetoken JWT expiring at `expires_at`, with a marker telling tokens apart.
fn skypetoken_jwt(expires_at: SystemTime, marker: &str) -> String {
    jwt(&json!({"skypeid": "orgid:synthetic", "exp": unix(expires_at), "marker": marker}))
}

/// The stored region of the session: chat on the AFD with stray whitespace, no `mtImageService`.
fn stored_region() -> Region {
    Region {
        name: Some("amer".into()),
        partition: Some("amer03".into()),
        endpoints: BTreeMap::from([
            (
                "chatServiceAfd".into(),
                " https://teams.example.test/api/chatsvc/amer ".into(),
            ),
            ("chatService".into(), "https://amer.msg.example.test".into()),
            (
                "middleTier".into(),
                "https://teams.example.test/api/mt/part/amer-03".into(),
            ),
        ]),
    }
}

struct Lab {
    server: MockServer,
    clock: Arc<FakeClock>,
    grants: Arc<AtomicUsize>,
    session: Session<MemorySecretStore>,
}

impl Lab {
    /// A session over a store holding the first refresh token, Entra answering with `lifetime` and `delay`.
    async fn start(lifetime: u64, delay: Duration) -> TestResult<Self> {
        let server = MockServer::start().await;
        let grants = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path(TOKEN_PATH))
            .respond_with(Entra {
                template: fixture_body("entra-token-refresh")?,
                lifetime,
                delay,
                grants: Arc::clone(&grants),
            })
            .mount(&server)
            .await;
        let store = MemorySecretStore::new();
        store
            .put(&refresh_token_key(ACCOUNT), &Secret::new(FIRST_REFRESH_TOKEN))
            .await?;
        let clock = FakeClock::new();
        let tokens =
            TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri())))?;
        let authz = AuthzClient::with_endpoint(&format!("{}{AUTHZ_PATH}", server.uri()))?;
        let session = Session::new(ACCOUNT, store, tokens, authz, stored_region()).with_clock(clock.clone());
        Ok(Self {
            server,
            clock,
            grants,
            session,
        })
    }

    async fn default() -> TestResult<Self> {
        Self::start(LIFETIME, Duration::ZERO).await
    }

    fn grants(&self) -> usize {
        self.grants.load(Ordering::SeqCst)
    }

    async fn requests_to(&self, wanted: &str) -> TestResult<Vec<Request>> {
        let requests = self.server.received_requests().await.ok_or("recording is off")?;
        Ok(requests
            .into_iter()
            .filter(|request| request.url.path() == wanted)
            .collect())
    }

    /// The scope of each grant sent, in order.
    async fn granted_scopes(&self) -> TestResult<Vec<String>> {
        Ok(self
            .requests_to(TOKEN_PATH)
            .await?
            .iter()
            .map(|request| {
                url::form_urlencoded::parse(&request.body)
                    .find(|(key, _)| key == "scope")
                    .map(|(_, scope)| scope.into_owned())
                    .unwrap_or_default()
            })
            .collect())
    }

    async fn store_skypetoken(&self, token: &str) -> TestResult {
        self.session
            .store()
            .put(&skypetoken_key(ACCOUNT), &Secret::new(token))
            .await?;
        Ok(())
    }

    /// Authz answering the fixture's success with this skypetoken and region name.
    async fn mount_authz(&self, skypetoken: &str, region: &str) -> TestResult {
        let mut body = fixture_body("authz-ok")?;
        body["tokens"]["skypeToken"] = json!(skypetoken);
        body["region"] = json!(region);
        Mock::given(method("POST"))
            .and(path(AUTHZ_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
        Ok(())
    }
}

fn margin() -> Duration {
    REFRESH_MARGIN
}

#[tokio::test]
async fn an_access_token_is_kept_per_audience_and_refreshed_a_margin_before_it_runs_out() -> TestResult {
    let lab = Lab::default().await?;
    let start = lab.clock.now();

    let ic3 = lab.session.access_token(IC3).await?;
    assert_eq!(ic3.token.expose(), access_token(1));
    assert_eq!(ic3.expires_at, start + Duration::from_secs(LIFETIME));
    assert!(
        ic3.scopes
            .iter()
            .any(|scope| scope == "https://ic3.teams.office.com/.default")
    );
    // cached: the same token, no grant
    assert_eq!(lab.session.access_token(IC3).await?.token.expose(), access_token(1));
    assert_eq!(lab.grants(), 1);

    // another audience gets its own token
    let spaces = lab.session.access_token(SPACES_RESOURCE).await?;
    assert_eq!(spaces.token.expose(), access_token(2));
    assert_eq!(
        lab.granted_scopes().await?,
        [
            "https://ic3.teams.office.com/.default openid profile offline_access",
            "https://api.spaces.skype.com/.default openid profile offline_access",
        ]
    );

    // just outside the margin it is still good
    lab.clock
        .advance(Duration::from_secs(LIFETIME - margin().as_secs() - 1));
    assert_eq!(lab.session.access_token(IC3).await?.token.expose(), access_token(1));
    assert_eq!(lab.grants(), 2);
    // at the margin it is refreshed, and the new one counts from the new request
    lab.clock.advance(Duration::from_secs(1));
    let renewed = lab.session.access_token(IC3).await?;
    assert_eq!(renewed.token.expose(), access_token(3));
    assert_eq!(renewed.expires_at, lab.clock.now() + Duration::from_secs(LIFETIME));
    assert_eq!(lab.grants(), 3);
    Ok(())
}

#[tokio::test]
async fn a_rotated_refresh_token_is_stored_and_redeemed_next() -> TestResult {
    let lab = Lab::default().await?;

    lab.session.access_token(IC3).await?;
    lab.session.access_token(SPACES_RESOURCE).await?;

    let stored = lab.session.store().get(&refresh_token_key(ACCOUNT)).await?;
    assert_eq!(
        stored.map(|token| token.expose().to_owned()).as_deref(),
        Some("synthetic-refresh-token-2")
    );
    let redeemed: Vec<String> = lab
        .requests_to(TOKEN_PATH)
        .await?
        .iter()
        .map(|request| {
            url::form_urlencoded::parse(&request.body)
                .find(|(key, _)| key == "refresh_token")
                .map(|(_, token)| token.into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(redeemed, [FIRST_REFRESH_TOKEN, "synthetic-refresh-token-1"]);
    Ok(())
}

#[tokio::test]
async fn concurrent_callers_share_one_refresh() -> TestResult {
    let lab = Lab::start(LIFETIME, Duration::from_millis(200)).await?;
    let session = &lab.session;

    let (first, second, third, fourth) = tokio::join!(
        session.access_token(IC3),
        session.access_token(IC3),
        session.access_token(IC3),
        session.access_token(IC3),
    );

    for token in [first?, second?, third?, fourth?] {
        assert_eq!(token.token.expose(), access_token(1));
    }
    assert_eq!(lab.grants(), 1);
    Ok(())
}

#[tokio::test]
async fn concurrent_grants_for_different_audiences_go_one_at_a_time() -> TestResult {
    let lab = Lab::start(LIFETIME, Duration::from_millis(100)).await?;
    let session = &lab.session;

    let (ic3, spaces) = tokio::join!(session.access_token(IC3), session.access_token(SPACES_RESOURCE));
    ic3?;
    spaces?;

    // each grant redeemed the refresh token the one before it stored: none was redeemed twice
    let redeemed: Vec<String> = lab
        .requests_to(TOKEN_PATH)
        .await?
        .iter()
        .map(|request| {
            url::form_urlencoded::parse(&request.body)
                .find(|(key, _)| key == "refresh_token")
                .map(|(_, token)| token.into_owned())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(redeemed, [FIRST_REFRESH_TOKEN, "synthetic-refresh-token-1"]);
    Ok(())
}

#[tokio::test]
async fn a_refused_token_is_refreshed_once_however_many_callers_it_was_refused_to() -> TestResult {
    let lab = Lab::default().await?;
    let refused = lab.session.access_token(IC3).await?;

    let first = lab.session.refresh_rejected(IC3, &refused.token).await?;
    let second = lab.session.refresh_rejected(IC3, &refused.token).await?;

    assert_eq!(first.token.expose(), access_token(2));
    assert_eq!(second.token.expose(), access_token(2));
    assert_eq!(lab.grants(), 2);
    // and the refreshed one is what everyone gets from now on
    assert_eq!(lab.session.access_token(IC3).await?.token.expose(), access_token(2));
    // a refusal of the new one refreshes again
    let third = lab.session.refresh_rejected(IC3, &first.token).await?;
    assert_eq!(third.token.expose(), access_token(3));
    Ok(())
}

#[tokio::test]
async fn a_refused_token_never_held_still_gets_a_fresh_one() -> TestResult {
    let lab = Lab::default().await?;

    let token = lab
        .session
        .refresh_rejected(IC3, &Secret::new("synthetic-token-from-elsewhere"))
        .await?;

    assert_eq!(token.token.expose(), access_token(1));
    assert_eq!(lab.grants(), 1);
    Ok(())
}

#[tokio::test]
async fn without_a_stored_refresh_token_nothing_is_sent() -> TestResult {
    let lab = Lab::default().await?;
    lab.session.store().delete(&refresh_token_key(ACCOUNT)).await?;

    let error = lab.session.access_token(IC3).await.expect_err("not signed in");

    assert!(
        matches!(&error, TokenError::NotSignedIn { account } if account == ACCOUNT),
        "{error:?}"
    );
    assert_eq!(lab.grants(), 0);
    Ok(())
}

#[tokio::test]
async fn the_stored_skypetoken_is_used_while_it_is_good_past_the_margin() -> TestResult {
    let lab = Lab::default().await?;
    let expires_at = lab.clock.now() + Duration::from_secs(3_600);
    let stored = skypetoken_jwt(expires_at, "stored");
    lab.store_skypetoken(&stored).await?;

    let skypetoken = lab.session.skypetoken().await?;

    assert_eq!(skypetoken.token.expose(), stored);
    assert_eq!(skypetoken.expires_at, expires_at);
    assert_eq!(skypetoken.skype_id.as_deref(), Some("orgid:synthetic"));
    assert_eq!(skypetoken.region, stored_region());
    assert!(
        lab.server
            .received_requests()
            .await
            .ok_or("recording is off")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn the_skypetoken_is_renewed_only_after_refreshing_spaces_and_never_outlives_that_token() -> TestResult {
    let lab = Lab::default().await?;
    let start = lab.clock.now();
    // the stored one is within the margin
    lab.store_skypetoken(&skypetoken_jwt(start + Duration::from_secs(60), "stored"))
        .await?;
    // a cached spaces token, good for hours: the renewal refreshes anyway
    let cached_spaces = lab.session.access_token(SPACES_RESOURCE).await?;
    // authz answers a token that would run past the new Entra token
    let renewed_jwt = skypetoken_jwt(start + Duration::from_secs(LIFETIME + 3_600), "renewed");
    lab.mount_authz(&renewed_jwt, "emea").await?;
    let mut renewals = lab.session.subscribe();

    let skypetoken = lab.session.skypetoken().await?;

    assert_eq!(skypetoken.token.expose(), renewed_jwt);
    assert_eq!(
        skypetoken.expires_at,
        start + Duration::from_secs(LIFETIME),
        "capped at the Entra token"
    );
    // a fresh spaces grant, then authz with that token as a TokenRefresh
    assert_eq!(lab.grants(), 2);
    let authz = lab.requests_to(AUTHZ_PATH).await?;
    assert_eq!(authz.len(), 1);
    let header = |name: &str| {
        authz[0]
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("authorization"), Some(format!("Bearer {}", access_token(2))));
    assert_ne!(cached_spaces.token.expose(), access_token(2));
    assert_eq!(header("ms-teams-authz-type").as_deref(), Some("TokenRefresh"));
    // stored, the region taken, and both renewals announced, by expiry only
    let stored = lab.session.store().get(&skypetoken_key(ACCOUNT)).await?;
    assert_eq!(stored.map(|token| token.expose().to_owned()), Some(renewed_jwt.clone()));
    assert_eq!(lab.session.region().name.as_deref(), Some("emea"));
    assert_eq!(
        renewals.try_recv()?,
        Renewal::AccessToken {
            resource: SPACES_RESOURCE.to_owned(),
            expires_at: start + Duration::from_secs(LIFETIME),
        }
    );
    assert_eq!(
        renewals.try_recv()?,
        Renewal::Skypetoken {
            expires_at: start + Duration::from_secs(LIFETIME),
        }
    );
    // cached from now on
    assert_eq!(lab.session.skypetoken().await?.token.expose(), renewed_jwt);
    assert_eq!(lab.requests_to(AUTHZ_PATH).await?.len(), 1);
    assert!(matches!(renewals.try_recv(), Err(TryRecvError::Empty)));
    Ok(())
}

#[tokio::test]
async fn a_refused_skypetoken_is_renewed_once_however_many_callers_it_was_refused_to() -> TestResult {
    let lab = Lab::default().await?;
    let start = lab.clock.now();
    // the stored one is good for an hour, yet a service refused it
    let stored = skypetoken_jwt(start + Duration::from_secs(3_600), "stored");
    lab.store_skypetoken(&stored).await?;
    let renewed_jwt = skypetoken_jwt(start + Duration::from_secs(3_600), "renewed");
    lab.mount_authz(&renewed_jwt, "amer").await?;

    let first = lab.session.skypetoken_rejected(&Secret::new(stored.as_str())).await?;
    let second = lab.session.skypetoken_rejected(&Secret::new(stored.as_str())).await?;

    assert_eq!(first.token.expose(), renewed_jwt);
    assert_eq!(second.token.expose(), renewed_jwt);
    // one spaces refresh, one authz exchange
    assert_eq!(lab.grants(), 1);
    assert_eq!(lab.requests_to(AUTHZ_PATH).await?.len(), 1);
    assert_eq!(lab.session.skypetoken().await?.token.expose(), renewed_jwt);
    Ok(())
}

#[tokio::test]
async fn a_refused_skypetoken_not_the_held_one_gets_the_held_one() -> TestResult {
    let lab = Lab::default().await?;
    let stored = skypetoken_jwt(lab.clock.now() + Duration::from_secs(3_600), "stored");
    lab.store_skypetoken(&stored).await?;

    let skypetoken = lab
        .session
        .skypetoken_rejected(&Secret::new("synthetic-older-skypetoken"))
        .await?;

    assert_eq!(skypetoken.token.expose(), stored);
    assert!(
        lab.server
            .received_requests()
            .await
            .ok_or("recording is off")?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn without_a_stored_skypetoken_one_is_renewed() -> TestResult {
    let lab = Lab::default().await?;
    let renewed_jwt = skypetoken_jwt(lab.clock.now() + Duration::from_secs(3_600), "renewed");
    lab.mount_authz(&renewed_jwt, "amer").await?;

    let skypetoken = lab.session.skypetoken().await?;

    assert_eq!(skypetoken.token.expose(), renewed_jwt);
    assert_eq!(lab.granted_scopes().await?.len(), 1);
    assert_eq!(lab.requests_to(AUTHZ_PATH).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_renewal_with_an_old_entra_token_is_refused_before_it_is_sent() -> TestResult {
    // Entra hands out tokens that run out within the margin
    let lab = Lab::start(margin().as_secs() - 1, Duration::ZERO).await?;
    lab.mount_authz(
        &skypetoken_jwt(lab.clock.now() + Duration::from_secs(3_600), "never"),
        "amer",
    )
    .await?;

    let error = lab.session.skypetoken().await.expect_err("stale");

    assert!(matches!(error, TokenError::StaleEntraToken), "{error:?}");
    assert_eq!(lab.grants(), 1);
    assert!(lab.requests_to(AUTHZ_PATH).await?.is_empty());
    assert!(lab.session.store().get(&skypetoken_key(ACCOUNT)).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn entra_throttling_is_handed_back_with_its_wait_and_no_retry() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(TOKEN_PATH))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_json(fixture_body("entra-error-429")?),
        )
        .expect(1)
        .mount(&server)
        .await;
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(ACCOUNT), &Secret::new(FIRST_REFRESH_TOKEN))
        .await?;
    let tokens =
        TokenClient::new(EntraConfig::teams_native().with_authority(format!("{}/organizations/", server.uri())))?;
    let authz = AuthzClient::with_endpoint(&format!("{}{AUTHZ_PATH}", server.uri()))?;
    let session = Session::new(ACCOUNT, store, tokens, authz, stored_region()).with_clock(FakeClock::new());

    let error = session.access_token(IC3).await.expect_err("throttled");

    assert!(
        matches!(error, TokenError::Throttled { retry_after: Some(wait), .. } if wait == Duration::from_secs(30)),
        "{error:?}"
    );
    server.verify().await;
    Ok(())
}

/// Authz answering `status` once, with `Retry-After: 45`.
async fn authz_refusal(status: u16) -> TestResult<TokenError> {
    let lab = Lab::default().await?;
    Mock::given(method("POST"))
        .and(path(AUTHZ_PATH))
        .and(header("ms-teams-authz-type", "TokenRefresh"))
        .respond_with(ResponseTemplate::new(status).insert_header("retry-after", "45"))
        .expect(1)
        .mount(&lab.server)
        .await;

    let error = lab.session.skypetoken().await.err().ok_or("authz refused nothing")?;

    lab.server.verify().await;
    assert!(lab.session.store().get(&skypetoken_key(ACCOUNT)).await?.is_none());
    Ok(error)
}

#[tokio::test]
async fn authz_throttling_is_throttled_too_with_its_wait_and_no_retry() -> TestResult {
    for status in [429, 503] {
        let error = authz_refusal(status).await?;
        assert!(
            matches!(error, TokenError::Throttled { retry_after: Some(wait), error: None } if wait == Duration::from_secs(45)),
            "{status}: {error:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn any_other_authz_refusal_is_an_authz_error() -> TestResult {
    let error = authz_refusal(500).await?;
    assert!(
        matches!(&error, TokenError::Authz(authz) if matches!(**authz, AuthzError::Unavailable { status: 500, .. })),
        "{error:?}"
    );
    let error = authz_refusal(401).await?;
    assert!(
        matches!(&error, TokenError::Authz(authz) if matches!(**authz, AuthzError::Unauthorized { .. })),
        "{error:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_refreshed_access_token_is_announced_and_a_cached_one_is_not() -> TestResult {
    let lab = Lab::default().await?;
    let mut renewals = lab.session.subscribe();

    let token = lab.session.access_token(IC3).await?;
    lab.session.access_token(IC3).await?;

    assert_eq!(
        renewals.try_recv()?,
        Renewal::AccessToken {
            resource: IC3.to_owned(),
            expires_at: token.expires_at,
        }
    );
    assert!(matches!(renewals.try_recv(), Err(TryRecvError::Empty)));
    Ok(())
}

#[tokio::test]
async fn service_bases_come_from_the_stored_region_trimmed_with_a_fallback() -> TestResult {
    let lab = Lab::default().await?;

    assert_eq!(
        lab.session.service_base("chatServiceAfd").as_deref(),
        Some("https://teams.example.test/api/chatsvc/amer")
    );
    // absent: its fallback's
    assert_eq!(
        lab.session.service_base("mtImageService").as_deref(),
        Some("https://teams.example.test/api/mt/part/amer-03")
    );
    assert_eq!(lab.session.service_base("presenceUPS"), None);

    let mut region = stored_region();
    region.endpoints.remove("chatServiceAfd");
    assert_eq!(region.base("chatServiceAfd"), Some("https://amer.msg.example.test"));
    region.endpoints.insert("chatServiceAfd".into(), "   ".into());
    assert_eq!(
        region.base("chatServiceAfd"),
        Some("https://amer.msg.example.test"),
        "a blank GTM is absent"
    );
    region.endpoints.remove("chatService");
    assert_eq!(region.base("chatServiceAfd"), None);
    Ok(())
}

#[tokio::test]
async fn no_token_reaches_debug_or_an_error() -> TestResult {
    let lab = Lab::default().await?;
    let renewed_jwt = skypetoken_jwt(lab.clock.now() + Duration::from_secs(3_600), "renewed");
    lab.mount_authz(&renewed_jwt, "amer").await?;
    let ic3 = lab.session.access_token(IC3).await?;
    let skypetoken = lab.session.skypetoken().await?;
    let mut shown = vec![
        format!("{:?}", lab.session),
        format!("{ic3:?}"),
        format!("{skypetoken:?}"),
    ];

    // errors: a stale renewal, throttling, an authz refusal, a missing refresh token
    let stale = Lab::start(1, Duration::ZERO).await?;
    stale.mount_authz(&renewed_jwt, "amer").await?;
    let errors = [
        stale.session.skypetoken().await.expect_err("stale"),
        authz_refusal(429).await?,
        authz_refusal(500).await?,
        {
            lab.session.store().delete(&refresh_token_key(ACCOUNT)).await?;
            lab.session
                .refresh_rejected(IC3, &ic3.token)
                .await
                .expect_err("not signed in")
        },
    ];
    for error in &errors {
        shown.push(format!("{error:?}"));
        shown.push(error.to_string());
    }

    let mut secrets = vec![FIRST_REFRESH_TOKEN.to_owned(), renewed_jwt.clone()];
    secrets.extend((1..=3).map(access_token));
    secrets.extend((1..=3).map(|grant| format!("synthetic-refresh-token-{grant}")));
    for text in &shown {
        for secret in &secrets {
            assert!(!text.contains(secret.as_str()), "{text} shows {secret}");
        }
        // nor any part of a token
        assert!(!text.contains("synthetic-access"), "{text}");
        assert!(!text.contains("synthetic-refresh"), "{text}");
    }
    assert!(shown[0].contains(ACCOUNT), "{}", shown[0]);
    Ok(())
}
