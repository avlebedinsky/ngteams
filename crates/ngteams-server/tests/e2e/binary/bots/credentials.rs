//! A bot's credentials and keys on one `all` node on SQLite, as raw MSAL requests and an SDK's checks see them: the
//! token under the tenant, `botframework.com` and `/adfs`, Entra's refusals and the hold after wrong secrets, a token
//! that opens no person's route and no sign-in by a bot, the keys at every path an SDK computes with an activity's
//! token checked against them; and `apps.enabled = false` serving none of it.

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;

use super::super::support::{Deployment, Node};
use super::support::{
    APPS, SCOPE, USERS, activity_token, app_path, bearer, bot_token, call, code, configure, first_admin, get, grant,
    kids, msal_form, register, seed_people, tenant,
};
use crate::support::{Backend, PASSWORD};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bot_gets_its_token_and_checks_activities_on_one_node() {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let tenant = tenant(&root);
    seed_people(&node, &root, "cred", 12).await;
    let (echo, secret) = register(&node, &root, "Echo (synthetic)").await;

    metadata(&node, &tenant).await;
    tokens(&node, &tenant, &echo, &secret).await;
    refusals(&node, &tenant, &echo, &secret).await;
    hold(&node, &tenant, &root).await;
    another_bot_is_not_held(&node, &tenant, &root).await;
    no_person_route(&node, &tenant, &echo, &secret).await;
    keys(&node, &tenant).await;
    activity_tokens(&node, &echo, &tenant, &secret).await;
    node.stop().await;
}

async fn metadata(node: &Node, tenant: &str) {
    eprintln!("credentials: the metadata MSAL reads");
    let origin = &node.endpoint;
    for authority in [tenant, "botframework.com"] {
        let (status, document) = get(node, &format!("/{authority}/v2.0/.well-known/openid-configuration")).await;
        assert_eq!(status, 200, "{authority}");
        assert_eq!(
            document["token_endpoint"],
            format!("{origin}/{authority}/oauth2/v2.0/token"),
            "{document}"
        );
        assert_eq!(
            document["jwks_uri"],
            format!("{origin}/{authority}/discovery/v2.0/keys"),
            "{document}"
        );
    }
    let (status, adfs) = get(node, "/adfs/.well-known/openid-configuration").await;
    assert_eq!(status, 200);
    assert_eq!(adfs["token_endpoint"], format!("{origin}/adfs/oauth2/token"), "{adfs}");
    let other = uuid::Uuid::new_v4();
    let (status, _) = get(node, &format!("/{other}/v2.0/.well-known/openid-configuration")).await;
    assert_ne!(status, 200, "another tenant's metadata");
}

fn assert_bot_claims(claims: &Value, bot: &str, tenant: &str) {
    assert_eq!(claims["aud"], SCOPE, "{claims}");
    for claim in ["appid", "azp", "sub"] {
        assert_eq!(claims[claim], bot, "{claim}: {claims}");
    }
    assert_eq!(claims["tid"], tenant);
    assert_eq!(claims["idtyp"], "app");
    assert!(
        claims.get("oid").is_none() && claims.get("upn").is_none() && claims.get("scp").is_none(),
        "{claims}"
    );
}

async fn tokens(node: &Node, tenant: &str, bot: &str, secret: &str) {
    eprintln!("credentials: a token under the tenant, botframework.com, /adfs and with HTTP Basic");
    let at_tenant = bot_token(node, tenant, bot, secret).await;
    assert_eq!(at_tenant.answer["token_type"], "Bearer");
    let lifetime = at_tenant.answer["expires_in"].as_u64().unwrap();
    assert!((3000..=3600).contains(&lifetime), "{lifetime}");
    assert!(at_tenant.answer["ext_expires_in"].as_u64().is_some());
    assert_bot_claims(&at_tenant.claims(), bot, tenant);
    let framework = grant(
        node,
        "/botframework.com/oauth2/v2.0/token",
        &msal_form(bot, secret),
        None,
    )
    .await;
    assert_bot_claims(&framework.claims(), bot, tenant);
    // msal-python's form under /adfs
    let python = [
        ("client_id", bot.to_owned()),
        ("client_secret", secret.to_owned()),
        ("scope", format!("{SCOPE}/.default")),
        ("grant_type", "client_credentials".to_owned()),
        ("client_info", "1".to_owned()),
    ];
    assert_bot_claims(
        &grant(node, "/adfs/oauth2/token", &python, None).await.claims(),
        bot,
        tenant,
    );
    let basic = [
        ("grant_type", "client_credentials".to_owned()),
        ("scope", format!("{SCOPE}/.default")),
    ];
    let with_basic = grant(
        node,
        &format!("/{tenant}/oauth2/v2.0/token"),
        &basic,
        Some((bot, secret)),
    )
    .await;
    assert_bot_claims(&with_basic.claims(), bot, tenant);
}

type Form = Vec<(&'static str, String)>;

/// A request Entra refuses: why, where, the form, and the status, error and `AADSTS` number it gets.
struct Refusal {
    why: &'static str,
    path: String,
    form: Form,
    expected: (u16, &'static str, u64),
}

fn refusal_cases(tenant: &str, bot: &str, secret: &str) -> Vec<Refusal> {
    let token = format!("/{tenant}/oauth2/v2.0/token");
    let changed = |key: &str, value: Option<&str>| -> Form {
        let mut form = msal_form(bot, secret);
        match value {
            Some(value) => form
                .iter_mut()
                .filter(|(name, _)| *name == key)
                .for_each(|field| field.1 = value.to_owned()),
            None => form.retain(|(name, _)| *name != key),
        }
        form
    };
    let unknown = uuid::Uuid::new_v4().to_string();
    let case = |why, form, expected| Refusal {
        why,
        path: token.clone(),
        form,
        expected,
    };
    vec![
        case(
            "no secret",
            changed("client_secret", None),
            (401, "invalid_client", 7_000_216),
        ),
        case(
            "an unknown bot",
            changed("client_id", Some(&unknown)),
            (401, "invalid_client", 7_000_215),
        ),
        case(
            "a scope of Graph",
            changed("scope", Some("https://graph.microsoft.com/.default")),
            (400, "invalid_scope", 70_011),
        ),
        case(
            "the bot scope without .default",
            changed("scope", Some(SCOPE)),
            (400, "invalid_scope", 70_011),
        ),
        case(
            "a person's scope",
            changed("scope", Some("https://api.spaces.skype.com/.default")),
            (400, "invalid_scope", 70_011),
        ),
        case(
            "another grant",
            changed("grant_type", Some("password")),
            (400, "unsupported_grant_type", 70_003),
        ),
        Refusal {
            why: "another tenant",
            path: format!("/{}/oauth2/v2.0/token", uuid::Uuid::new_v4()),
            form: msal_form(bot, secret),
            expected: (400, "invalid_request", 90_002),
        },
    ]
}

async fn refusals(node: &Node, tenant: &str, bot: &str, secret: &str) {
    eprintln!("credentials: Entra's refusals");
    for Refusal {
        why,
        path,
        form,
        expected: (status, error, number),
    } in refusal_cases(tenant, bot, secret)
    {
        let got = grant(node, &path, &form, None).await;
        assert_eq!(
            (got.status, got.error()),
            (status, (error, number)),
            "{why}: {}",
            got.error_text()
        );
        assert!(got.answer.get("access_token").is_none());
    }
    // Basic and a different form secret: the bot is refused, not let in by either
    let token = format!("/{tenant}/oauth2/v2.0/token");
    let wrong_basic = grant(node, &token, &msal_form(bot, secret), Some((bot, "synthetic-wrong"))).await;
    assert_ne!(wrong_basic.status, 200, "Basic and form disagree");
}

async fn hold(node: &Node, tenant: &str, root: &Secret) {
    eprintln!("credentials: wrong secrets, then the hold");
    let (bot, secret) = register(node, root, "Held (synthetic)").await;
    for attempt in 0..3 {
        let wrong = bot_token(node, tenant, &bot, &format!("synthetic-wrong-{attempt}")).await;
        assert_eq!(
            (wrong.status, wrong.error()),
            (401, ("invalid_client", 7_000_215)),
            "{}",
            wrong.error_text()
        );
    }
    let held = bot_token(node, tenant, &bot, "synthetic-wrong-3").await;
    assert_eq!(held.status, 429, "{}", held.error_text());
    assert_eq!(held.error().0, "temporarily_unavailable");
    let retry = held.retry_after.expect("Retry-After");
    assert!((1..=30).contains(&retry), "{retry}");
    // the right secret is never held off, and clears the count
    assert_eq!(bot_token(node, tenant, &bot, &secret).await.status, 200);
    let again = bot_token(node, tenant, &bot, "synthetic-wrong-4").await;
    assert_eq!(again.status, 401, "{}", again.error_text());
}

async fn another_bot_is_not_held(node: &Node, tenant: &str, root: &Secret) {
    let (other, other_secret) = register(node, root, "Unheld (synthetic)").await;
    assert_eq!(bot_token(node, tenant, &other, &other_secret).await.status, 200);
}

async fn no_person_route(node: &Node, tenant: &str, bot: &str, secret: &str) {
    eprintln!("credentials: a bot's token opens no person's route, and a bot can't sign in");
    let token = Secret::new(
        bot_token(node, tenant, bot, secret).await.answer["access_token"]
            .as_str()
            .unwrap(),
    );
    for (method, path) in [
        (Method::GET, "/api/account/v1/me".to_owned()),
        (Method::GET, APPS.to_owned()),
        (Method::GET, USERS.to_owned()),
        (Method::POST, "/api/authsvc/v1.0/authz".to_owned()),
    ] {
        let (status, _) = call(node, method.clone(), &path, Some(&token), None).await;
        assert!(matches!(status, 401 | 403), "{method} {path}: {status}");
    }
    for (upn, password) in [
        (format!("app:{bot}"), ""),
        (format!("app:{bot}"), PASSWORD),
        (bot.to_owned(), secret),
    ] {
        let response = node.password(&upn, password).await;
        let status = response.status().as_u16();
        let answer: Value = response.json().await.unwrap_or(Value::Null);
        assert_eq!(status, 400, "{answer}");
        assert!(
            matches!(answer["error"].as_str(), Some("invalid_grant" | "invalid_request")),
            "{answer}"
        );
        assert!(answer.get("access_token").is_none());
    }
}

/// The JWKS key `kid`, checked as botbuilder checks one, as a decoding key.
fn sdk_key(jwks: &Value, kid: &str) -> Result<DecodingKey, String> {
    let key = jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["kid"] == kid)
        .ok_or("unknown kid")?;
    let endorsed = key["endorsements"]
        .as_array()
        .is_some_and(|all| all.iter().any(|channel| channel == "msteams"));
    if !endorsed || key["kty"] != "RSA" {
        return Err(format!("not an endorsed RSA key: {key}"));
    }
    DecodingKey::from_rsa_components(key["n"].as_str().unwrap(), key["e"].as_str().unwrap())
        .map_err(|error| error.to_string())
}

/// An activity's token as an SDK takes it: its claims, or why not.
async fn sdk_takes(node: &Node, token: &str, bot: &str, service_url: &str) -> Result<Value, String> {
    let (_, metadata) = get(node, "/bot/v1/.well-known/openidconfiguration").await;
    let jwks_path = metadata["jwks_uri"].as_str().unwrap().replacen(&node.endpoint, "", 1);
    let (_, jwks) = get(node, &jwks_path).await;
    let header = jsonwebtoken::decode_header(token).map_err(|error| error.to_string())?;
    if header.alg != Algorithm::RS256 {
        return Err(format!("{:?}", header.alg));
    }
    let key = sdk_key(&jwks, &header.kid.ok_or("no kid")?)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = 300;
    validation.set_audience(&[bot]);
    validation.set_issuer(&[metadata["issuer"].as_str().unwrap()]);
    let claims = jsonwebtoken::decode::<Value>(token, &key, &validation)
        .map_err(|error| error.to_string())?
        .claims;
    if claims["serviceurl"] != service_url {
        return Err("serviceurl".to_owned());
    }
    Ok(claims)
}

async fn bot_metadata(node: &Node) {
    eprintln!("credentials: the metadata a bot SDK reads");
    let (status, metadata) = get(node, "/bot/v1/.well-known/openidconfiguration").await;
    assert_eq!(status, 200);
    assert_eq!(metadata["issuer"], format!("{}/bot", node.endpoint), "{metadata}");
    let jwks = format!("{}/bot/v1/.well-known/keys", node.endpoint);
    assert_eq!(metadata["jwks_uri"], jwks);
    assert_eq!(metadata["id_token_signing_alg_values_supported"], json!(["RS256"]));
}

async fn keys(node: &Node, tenant: &str) {
    bot_metadata(node).await;
    eprintln!("credentials: the keys at every path");
    let paths = [
        "/bot/v1/.well-known/keys".to_owned(),
        format!("/{tenant}/discovery/v2.0/keys"),
        "/botframework.com/discovery/v2.0/keys".to_owned(),
        "/adfs/discovery/keys".to_owned(),
    ];
    let published = kids(node, &paths[0]).await;
    assert!(!published.is_empty());
    for path in &paths[1..] {
        assert_eq!(kids(node, path).await, published, "{path}");
    }
    let (_, jwks) = get(node, &paths[0]).await;
    for key in jwks["keys"].as_array().unwrap() {
        assert_eq!(key["kty"], "RSA", "{key}");
        assert_eq!(key["endorsements"], json!(["msteams"]), "{key}");
        let modulus = key["n"].as_str().unwrap();
        assert!(modulus.len() >= 342, "RSA-2048: {}", modulus.len());
        assert!(key.get("d").is_none() && key.get("p").is_none(), "no private part");
    }
}

async fn activity_tokens(node: &Node, bot: &str, tenant: &str, secret: &str) {
    eprintln!("credentials: an activity's token, as an SDK checks it");
    let issuer = format!("{}/bot", node.endpoint);
    let service_url = format!("{}/bot/", node.endpoint);
    let token = activity_token(node, bot, &service_url).await;
    let claims = sdk_takes(node, &token, bot, &service_url).await.unwrap();
    let mut names: Vec<&str> = claims.as_object().unwrap().keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["aud", "exp", "iat", "iss", "nbf", "serviceurl"], "{claims}");
    assert_eq!(claims["iss"], issuer);
    let other = uuid::Uuid::new_v4().to_string();
    assert!(
        sdk_takes(node, &token, &other, &service_url).await.is_err(),
        "another bot"
    );
    assert!(
        sdk_takes(node, &token, bot, "https://elsewhere.example.test/bot/")
            .await
            .is_err(),
        "another service URL"
    );
    let own = bot_token(node, tenant, bot, secret).await.answer["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        sdk_takes(node, &own, bot, &service_url).await.is_err(),
        "the bot's own EdDSA token"
    );
    let mut forged = token.rsplit_once('.').unwrap().0.to_owned();
    forged.push_str(".c3ludGhldGljIHNpZ25hdHVyZQ");
    assert!(
        sdk_takes(node, &forged, bot, &service_url).await.is_err(),
        "a signature not the key's"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apps_turned_off_serve_none_of_it() {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let tenant = tenant(&root);
    let (bot, secret) = register(&node, &root, "Echo (synthetic)").await;
    assert_eq!(bot_token(&node, &tenant, &bot, &secret).await.status, 200);
    node.stop().await;

    eprintln!("apps off: nothing served");
    configure(&node, "", "[apps]\nenabled = false\n");
    node.start().await;
    let root = first_admin_token(&node).await;
    let paths = [
        (Method::GET, APPS.to_owned()),
        (Method::POST, APPS.to_owned()),
        (Method::GET, app_path(&bot)),
        (Method::PATCH, app_path(&bot)),
        (Method::DELETE, app_path(&bot)),
        (Method::POST, format!("{}/secret", app_path(&bot))),
        (Method::POST, "/api/admin/v1/apps/package".to_owned()),
        (Method::GET, "/bot/v1/.well-known/openidconfiguration".to_owned()),
        (Method::GET, "/bot/v1/.well-known/keys".to_owned()),
        (Method::GET, format!("/{tenant}/discovery/v2.0/keys")),
        (Method::GET, "/adfs/discovery/keys".to_owned()),
        (Method::GET, format!("/{tenant}/v2.0/.well-known/openid-configuration")),
        (Method::GET, "/adfs/.well-known/openid-configuration".to_owned()),
    ];
    for (method, path) in paths {
        let (status, answer) = call(&node, method.clone(), &path, Some(&root), Some(json!({"name": "Off"}))).await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
    }
    for path in [
        format!("/{tenant}/oauth2/v2.0/token"),
        "/adfs/oauth2/token".to_owned(),
        "/botframework.com/oauth2/v2.0/token".to_owned(),
    ] {
        let got = grant(&node, &path, &msal_form(&bot, &secret), None).await;
        assert_eq!(got.status, 404, "{path}: {}", got.error_text());
    }
    // people still sign in, and the app comes back when apps are on again
    assert_eq!(node.password(super::support::ROOT, PASSWORD).await.status(), 200);
    let (ok, why) = node.try_cli(&["app", "list"], "");
    eprintln!("  app list with apps off: ok {ok}, {why}");
    node.stop().await;
    let text = std::fs::read_to_string(&node.config)
        .unwrap()
        .replace("[apps]\nenabled = false\n", "");
    std::fs::write(&node.config, text).unwrap();
    node.start().await;
    assert_eq!(bot_token(&node, &tenant, &bot, &secret).await.status, 200);
    let root = first_admin_token(&node).await;
    let (status, answer) = call(&node, Method::GET, &app_path(&bot), Some(&root), None).await;
    assert_eq!((status, code(&answer)), (200, ""), "{answer}");
    node.stop().await;
}

async fn first_admin_token(node: &Node) -> Secret {
    bearer(node, super::support::ROOT).await
}
