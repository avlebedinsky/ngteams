//! A bot's credentials: the client-credentials grant as MSAL sends it (msal-node, MSAL.NET under the tenant's
//! authority, msal-python under `/adfs`), found through the `OpenID` metadata MSAL reads first; refused for a wrong
//! secret (and held off after too many), a disabled or deleted bot, another scope or tenant; never a user's token.

use reqwest::Method;
use serde_json::{Value, json};

use super::{Registry, app_path, call, claims};
use crate::support::{Backend, TestServer, backends, redis_url};

const BOT_SCOPE: &str = "https://api.botframework.com/.default";

/// The form msal-node's confidential client posts for `acquireTokenByClientCredential`.
fn msal_node_form(client_id: &str, secret: &str, scope: &str) -> Vec<(&'static str, String)> {
    vec![
        ("client_id", client_id.to_owned()),
        ("scope", scope.to_owned()),
        ("grant_type", "client_credentials".to_owned()),
        ("x-client-SKU", "msal.js.node".to_owned()),
        ("x-client-VER", "3.8.1".to_owned()),
        ("x-client-OS", "linux".to_owned()),
        ("x-client-CPU", "x64".to_owned()),
        ("x-ms-lib-capability", "retry-after, h429".to_owned()),
        ("x-client-current-telemetry", "5|771,2,,,|,".to_owned()),
        ("x-client-last-telemetry", "5|0|||0,0".to_owned()),
        ("client-request-id", uuid::Uuid::new_v4().to_string()),
        ("client_secret", secret.to_owned()),
    ]
}

/// The form MSAL.NET posts (its library info goes in headers).
fn msal_net_form(client_id: &str, secret: &str) -> Vec<(&'static str, String)> {
    vec![
        ("client_id", client_id.to_owned()),
        ("client_secret", secret.to_owned()),
        ("scope", BOT_SCOPE.to_owned()),
        ("grant_type", "client_credentials".to_owned()),
        ("client_info", "1".to_owned()),
    ]
}

/// Posts `form` to `endpoint` with MSAL's headers; the status, the `Retry-After` and the JSON.
async fn post(endpoint: &str, form: &[(&str, String)], basic: Option<(&str, &str)>) -> (u16, Option<String>, Value) {
    let mut request = reqwest::Client::new()
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded;charset=utf-8")
        .header("x-client-SKU", "MSAL.NetCore")
        .header("x-client-Ver", "4.66.1.0")
        .header("x-client-OS", "Linux")
        .header("return-client-request-id", "true")
        .header("client-request-id", uuid::Uuid::new_v4().to_string())
        .body(serde_urlencoded(form));
    if let Some((user, password)) = basic {
        request = request.basic_auth(user, Some(password));
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().unwrap().to_owned());
    (status, retry, response.json().await.unwrap_or(Value::Null))
}

fn serde_urlencoded(form: &[(&str, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().map(|(name, value)| (*name, value.as_str())))
        .finish()
}

async fn get(server: &TestServer, path: &str) -> Value {
    let (status, answer) = call(server, Method::GET, path, None, None).await;
    assert_eq!(status, 200, "{path}: {answer}");
    answer
}

fn aadsts(answer: &Value) -> (String, u64) {
    (
        answer["error"].as_str().unwrap_or_default().to_owned(),
        answer["error_codes"][0].as_u64().unwrap_or_default(),
    )
}

/// A registered bot and where MSAL finds the token endpoint for it.
struct Bot {
    id: String,
    secret: String,
    token_endpoint: String,
}

async fn bot(registry: &Registry, name: &str) -> Bot {
    let added = registry.register(name, "https://bot.example.test/api/messages").await;
    let tenant = registry.server.app.issuer.tenant().to_owned();
    let metadata = get(&registry.server, &format!("/{tenant}/v2.0/.well-known/openid-configuration")).await;
    Bot {
        id: added["id"].as_str().unwrap().to_owned(),
        secret: added["secret"].as_str().unwrap().to_owned(),
        token_endpoint: metadata["token_endpoint"].as_str().unwrap().to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_gets_a_token_in_msals_exact_form_through_the_metadata_msal_reads() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let server = &registry.server;
        let tenant = server.app.issuer.tenant().to_owned();
        let origin = server.origin.clone();
        let metadata = get(server, &format!("/{tenant}/v2.0/.well-known/openid-configuration")).await;
        assert_eq!(metadata["token_endpoint"], format!("{origin}/{tenant}/oauth2/v2.0/token"));
        assert_eq!(metadata["issuer"], format!("{origin}/{tenant}/v2.0"));
        assert_eq!(metadata["jwks_uri"], format!("{origin}/{tenant}/discovery/v2.0/keys"));
        assert!(metadata["authorization_endpoint"].as_str().unwrap().starts_with(&origin));
        assert_eq!(metadata["token_endpoint_auth_methods_supported"], json!(["client_secret_post", "client_secret_basic"]));
        let echo = bot(&registry, "Echo (synthetic)").await;

        let (status, _, answer) = post(&echo.token_endpoint, &msal_node_form(&echo.id, &echo.secret, BOT_SCOPE), None).await;
        assert_eq!(status, 200, "{answer}");
        assert_eq!(answer["token_type"], "Bearer");
        let lifetime = answer["expires_in"].as_u64().unwrap();
        assert!((3000..=3600).contains(&lifetime), "{answer}");
        assert_eq!(answer["ext_expires_in"].as_u64(), Some(lifetime));
        assert!(answer.get("refresh_token").is_none() && answer.get("id_token").is_none());
        let token = answer["access_token"].as_str().unwrap();
        let minted = claims(token);
        assert_eq!(minted["aud"], "https://api.botframework.com");
        assert_eq!(minted["appid"], echo.id);
        assert_eq!(minted["azp"], echo.id);
        assert_eq!(minted["sub"], echo.id);
        assert_eq!(minted["tid"], tenant);
        assert_eq!(minted["idtyp"], "app");
        assert!(minted.get("oid").is_none() && minted.get("upn").is_none() && minted.get("scp").is_none());

        // a bot's token opens nothing of a person's
        for path in ["/api/account/v1/me", "/api/admin/v1/apps"] {
            let (status, _) = call(server, Method::GET, path, Some(token), None).await;
            assert_eq!(status, 401, "{path}");
        }
        let authz = reqwest::Client::new()
            .post(server.authz_endpoint())
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        assert_eq!(authz.status().as_u16(), 401);

        // MSAL.NET, a secret sent as HTTP Basic, the Bot Framework's own tenant name
        let (status, _, answer) = post(&echo.token_endpoint, &msal_net_form(&echo.id, &echo.secret), None).await;
        assert_eq!(status, 200, "{answer}");
        let mut basic = msal_net_form(&echo.id, &echo.secret);
        basic.retain(|(name, _)| *name != "client_secret" && *name != "client_id");
        let (status, _, answer) = post(&echo.token_endpoint, &basic, Some((&echo.id, &echo.secret))).await;
        assert_eq!(status, 200, "{answer}");
        let botframework = format!("{origin}/botframework.com/oauth2/v2.0/token");
        let (status, _, answer) = post(&botframework, &msal_net_form(&echo.id, &echo.secret), None).await;
        assert_eq!(status, 200, "{answer}");

        // msal-python under an ADFS authority, which it takes without asking Microsoft
        let adfs = get(server, "/adfs/.well-known/openid-configuration").await;
        assert_eq!(adfs["token_endpoint"], format!("{origin}/adfs/oauth2/token"));
        assert_eq!(adfs["issuer"], format!("{origin}/adfs"));
        let python = vec![
            ("grant_type", "client_credentials".to_owned()),
            ("client_id", echo.id.clone()),
            ("client_secret", echo.secret.clone()),
            ("scope", BOT_SCOPE.to_owned()),
        ];
        let (status, _, answer) = post(adfs["token_endpoint"].as_str().unwrap(), &python, None).await;
        assert_eq!(status, 200, "{answer}");
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_for_another_scope_tenant_or_kind_is_refused_as_entra_refuses_it() {
    let registry = Registry::open(Backend::Sqlite).await;
    let server = &registry.server;
    let echo = bot(&registry, "Echo (synthetic)").await;
    let form = |scope: &str| msal_node_form(&echo.id, &echo.secret, scope);
    for scope in ["https://api.spaces.skype.com/.default", "https://ic3.teams.office.com/.default", "openid"] {
        let (status, _, answer) = post(&echo.token_endpoint, &form(scope), None).await;
        assert_eq!((status, aadsts(&answer)), (400, ("invalid_scope".to_owned(), 70_011)), "{scope}");
    }
    let elsewhere = format!("{}/contoso.example/oauth2/v2.0/token", server.origin);
    let (status, _, answer) = post(&elsewhere, &form(BOT_SCOPE), None).await;
    assert_eq!((status, aadsts(&answer)), (400, ("invalid_request".to_owned(), 90_002)));
    let (status, _) = call(server, Method::GET, "/contoso.example/v2.0/.well-known/openid-configuration", None, None).await;
    assert_eq!(status, 400);

    let mut password = form(BOT_SCOPE);
    password[2].1 = "password".to_owned();
    let (status, _, answer) = post(&echo.token_endpoint, &password, None).await;
    assert_eq!((status, aadsts(&answer)), (400, ("unsupported_grant_type".to_owned(), 70_003)));
    let mut no_grant = form(BOT_SCOPE);
    no_grant.retain(|(name, _)| *name != "grant_type");
    let (status, _, answer) = post(&echo.token_endpoint, &no_grant, None).await;
    assert_eq!((status, aadsts(&answer)), (400, ("invalid_request".to_owned(), 900_144)));
    let mut bare = form(BOT_SCOPE);
    bare.retain(|(name, _)| *name != "client_secret");
    let (status, _, answer) = post(&echo.token_endpoint, &bare, None).await;
    assert_eq!((status, aadsts(&answer).0.as_str()), (401, "invalid_client"));

    // a person's sign-in is where it was, and takes no client credentials
    let (status, _, answer) = post(&server.token_endpoint(), &form(BOT_SCOPE), None).await;
    assert_eq!((status, aadsts(&answer).0.as_str()), (400, "invalid_scope"));
    registry.server.add_user("bob@own.test", "Bob").await;
    registry.server.session("bob@own.test").await.skypetoken().await.unwrap();
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_secret_is_refused_and_too_many_hold_the_bot_off() {
    let registry = Registry::open_with(Backend::Sqlite, |config| config.login.max_failures = 3).await;
    let echo = bot(&registry, "Echo (synthetic)").await;
    let other = bot(&registry, "Other (synthetic)").await;
    let wrong = msal_node_form(&echo.id, "not-the-secret-0000000000000000000000000", BOT_SCOPE);
    for _ in 0..3 {
        let (status, _, answer) = post(&echo.token_endpoint, &wrong, None).await;
        assert_eq!((status, aadsts(&answer)), (401, ("invalid_client".to_owned(), 7_000_215)), "{answer}");
    }
    // an unknown bot is told the same
    let stranger = msal_node_form(&uuid::Uuid::new_v4().to_string(), &echo.secret, BOT_SCOPE);
    let (status, _, answer) = post(&echo.token_endpoint, &stranger, None).await;
    assert_eq!((status, aadsts(&answer)), (401, ("invalid_client".to_owned(), 7_000_215)));
    // held off, the right secret too, until the window ends; other bots aren't
    let right = msal_node_form(&echo.id, &echo.secret, BOT_SCOPE);
    let (status, retry, answer) = post(&echo.token_endpoint, &right, None).await;
    assert_eq!((status, answer["error"].as_str()), (429, Some("temporarily_unavailable")), "{answer}");
    assert!(retry.unwrap().parse::<u64>().unwrap() >= 1);
    let (status, _, answer) = post(&other.token_endpoint, &msal_node_form(&other.id, &other.secret, BOT_SCOPE), None).await;
    assert_eq!(status, 200, "{answer}");
    registry.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hold_is_shared_by_every_node_through_redis() {
    let Some(redis) = redis_url() else { return };
    for backend in backends() {
        let registry = Registry::open_with(backend, |config| {
            config.login.max_failures = 2;
            config.redis_url = Some(redis.clone());
        })
        .await;
        let second = TestServer::start_with(backend, |config| {
            config.database_url.clone_from(&registry.server.app.config.database_url);
            config.login.max_failures = 2;
            config.redis_url = Some(redis.clone());
        })
        .await;
        let echo = bot(&registry, "Echo (synthetic)").await;
        let on_second = echo.token_endpoint.replace(&registry.server.origin, &second.origin);
        let wrong = msal_node_form(&echo.id, "not-the-secret-0000000000000000000000000", BOT_SCOPE);
        post(&echo.token_endpoint, &wrong, None).await;
        post(&on_second, &wrong, None).await;
        let (status, _, answer) = post(&on_second, &msal_node_form(&echo.id, &echo.secret, BOT_SCOPE), None).await;
        assert_eq!(status, 429, "{answer}");
        second.stop().await;
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disabled_rotated_or_deleted_bot_gets_no_token_with_what_it_had() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let echo = bot(&registry, "Echo (synthetic)").await;
        let path = app_path(&echo.id);
        let form = msal_node_form(&echo.id, &echo.secret, BOT_SCOPE);

        let (status, _) = registry.call(Method::PATCH, &path, Some(json!({"enabled": false}))).await;
        assert_eq!(status, 200);
        let (status, _, answer) = post(&echo.token_endpoint, &form, None).await;
        assert_eq!((status, aadsts(&answer)), (400, ("unauthorized_client".to_owned(), 7_000_112)), "{answer}");
        // a wrong secret of a disabled bot is still only a wrong secret
        let wrong = msal_node_form(&echo.id, "not-the-secret-0000000000000000000000000", BOT_SCOPE);
        let (status, _, _) = post(&echo.token_endpoint, &wrong, None).await;
        assert_eq!(status, 401);
        let (status, _) = registry.call(Method::PATCH, &path, Some(json!({"enabled": true}))).await;
        assert_eq!(status, 200);
        let (status, _, answer) = post(&echo.token_endpoint, &form, None).await;
        assert_eq!(status, 200, "{answer}");

        let (_, rotated) = registry.call(Method::POST, &format!("{path}/secret"), None).await;
        let (status, _, _) = post(&echo.token_endpoint, &form, None).await;
        assert_eq!(status, 401);
        let fresh = msal_node_form(&echo.id, rotated["secret"].as_str().unwrap(), BOT_SCOPE);
        let (status, _, answer) = post(&echo.token_endpoint, &fresh, None).await;
        assert_eq!(status, 200, "{answer}");

        let (status, _) = registry.call(Method::DELETE, &path, None).await;
        assert_eq!(status, 204);
        let (status, _, answer) = post(&echo.token_endpoint, &fresh, None).await;
        assert_eq!((status, aadsts(&answer).0.as_str()), (401, "invalid_client"));
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn apps_turned_off_serve_no_credentials_or_metadata() {
    let registry = Registry::open_with(Backend::Sqlite, |config| config.apps.enabled = false).await;
    let server = &registry.server;
    let tenant = server.app.issuer.tenant().to_owned();
    for path in [
        format!("/{tenant}/v2.0/.well-known/openid-configuration"),
        "/adfs/.well-known/openid-configuration".to_owned(),
        "/bot/v1/.well-known/openidconfiguration".to_owned(),
        "/bot/v1/.well-known/keys".to_owned(),
        format!("/{tenant}/discovery/v2.0/keys"),
    ] {
        let (status, _) = call(server, Method::GET, &path, None, None).await;
        assert_eq!(status, 404, "{path}");
    }
    let endpoint = server.url(&format!("/{tenant}/oauth2/v2.0/token"));
    let (status, _, _) = post(&endpoint, &msal_node_form("x", "y", BOT_SCOPE), None).await;
    assert_eq!(status, 404);
    registry.server.stop().await;
}
