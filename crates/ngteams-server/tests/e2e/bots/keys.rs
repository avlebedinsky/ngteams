//! The keys a bot checks the server's activities with: an RS256 token signed for one bot, verified the way the Bot
//! Framework and Teams SDKs verify a channel's token (the `OpenID` metadata, the JWKS it names, the key by `kid` with
//! the channel's endorsement, the issuer, the audience, the lifetime and `serviceurl`), the same keys at every path an
//! SDK computes, shared by the nodes of a deployment and rotated without breaking a token already out.

use std::collections::BTreeSet;
use std::time::Duration;

use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::Method;
use serde_json::Value;

use super::{Registry, call};
use crate::support::{TestServer, backends};

/// The clock skew the SDKs allow.
const SDK_SKEW: u64 = 300;

async fn get(server: &TestServer, url: &str) -> Value {
    // the nodes of a deployment share its public origin; each is asked on its own address
    let path = url.strip_prefix(&server.app.config.origin()).unwrap_or(url);
    let (status, answer) = call(server, Method::GET, path, None, None).await;
    assert_eq!(status, 200, "{url}: {answer}");
    answer
}

/// What a bot SDK does with an activity's token, given the metadata URL it was configured with: its claims, or why
/// it refuses it.
async fn sdk_verifies(
    server: &TestServer,
    metadata: &str,
    token: &str,
    bot: &str,
    service_url: &str,
) -> Result<Value, String> {
    let metadata = get(server, metadata).await;
    let keys = get(server, metadata["jwks_uri"].as_str().unwrap()).await;
    let header = jsonwebtoken::decode_header(token).map_err(|error| error.to_string())?;
    if header.alg != Algorithm::RS256 {
        return Err(format!("algorithm {:?}", header.alg));
    }
    let kid = header.kid.ok_or("no kid")?;
    let key = keys["keys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["kid"] == kid)
        .ok_or("unknown kid")?;
    let endorsed = key["endorsements"]
        .as_array()
        .is_some_and(|endorsements| endorsements.iter().any(|channel| channel == "msteams"));
    if !endorsed {
        return Err("not endorsed for msteams".to_owned());
    }
    let decoding = DecodingKey::from_rsa_components(key["n"].as_str().unwrap(), key["e"].as_str().unwrap())
        .map_err(|error| error.to_string())?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.leeway = SDK_SKEW;
    validation.set_audience(&[bot]);
    validation.set_issuer(&[metadata["issuer"].as_str().unwrap()]);
    validation.set_required_spec_claims(&["exp", "nbf", "aud", "iss"]);
    validation.validate_nbf = true;
    let claims = jsonwebtoken::decode::<Value>(token, &decoding, &validation)
        .map_err(|error| error.to_string())?
        .claims;
    if claims["serviceurl"] != service_url {
        return Err("serviceurl".to_owned());
    }
    Ok(claims)
}

/// The metadata and JWKS the SDKs read: botbuilder and the .NET SDKs' metadata, and the same RSA keys at every
/// path an SDK reads or computes.
async fn published_as_the_sdks_read_it(server: &TestServer, origin: &str, tenant: &str) {
    let metadata = get(server, "/bot/v1/.well-known/openidconfiguration").await;
    assert_eq!(metadata["issuer"], format!("{origin}/bot"));
    assert_eq!(metadata["jwks_uri"], format!("{origin}/bot/v1/.well-known/keys"));
    assert!(
        metadata["id_token_signing_alg_values_supported"]
            .as_array()
            .unwrap()
            .contains(&"RS256".into())
    );
    let published = get(server, "/bot/v1/.well-known/keys").await;
    for key in published["keys"].as_array().unwrap() {
        assert_eq!(key["kty"], "RSA");
        assert_eq!(key["use"], "sig");
        assert_eq!(key["alg"], "RS256");
        assert_eq!(key["e"], "AQAB");
        // a 2048-bit modulus in base64url
        assert_eq!(key["n"].as_str().unwrap().len(), 342);
        assert!(key.get("d").is_none() && key.get("p").is_none());
    }
    // the Teams SDK's rewritten path and the Agents SDK's computed one name the same keys
    assert_eq!(get(server, &format!("/{tenant}/discovery/v2.0/keys")).await, published);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_activity_token_verifies_against_the_published_keys_as_the_sdks_check_it() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let server = &registry.server;
        let origin = server.origin.clone();
        let tenant = server.app.issuer.tenant().to_owned();
        let added = registry
            .register("Echo (synthetic)", "https://bot.example.test/api/messages")
            .await;
        let bot = added["id"].as_str().unwrap();
        let service_url = ngteams_server::apps::service_url(&server.app.config);
        assert_eq!(service_url, format!("{origin}/bot/"));
        published_as_the_sdks_read_it(server, &origin, &tenant).await;

        let token = server.app.bot_keys.activity_token(bot, &service_url).unwrap();
        let header = jsonwebtoken::decode_header(&token).unwrap();
        assert_eq!(header.typ.as_deref(), Some("JWT"));
        let claims = sdk_verifies(
            server,
            "/bot/v1/.well-known/openidconfiguration",
            &token,
            bot,
            &service_url,
        )
        .await
        .unwrap();
        // exactly these: an SDK takes a token with `ver`, `appid` or `azp` for an emulator's or a skill's
        let names: BTreeSet<&str> = claims.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(names, BTreeSet::from(["aud", "exp", "iat", "iss", "nbf", "serviceurl"]));
        assert_eq!(claims["aud"], bot);
        let lifetime = claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap();
        assert!((300..=3600).contains(&lifetime), "{claims}");

        // another bot, another service URL or a token signed by anyone else is refused
        let other = registry
            .register("Other (synthetic)", "https://other.example.test/api/messages")
            .await;
        let other = other["id"].as_str().unwrap();
        let metadata = "/bot/v1/.well-known/openidconfiguration";
        assert!(
            sdk_verifies(server, metadata, &token, other, &service_url)
                .await
                .is_err()
        );
        assert!(
            sdk_verifies(server, metadata, &token, bot, "https://elsewhere.example.test/")
                .await
                .is_err()
        );
        let foreign = {
            let rsa = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).unwrap();
            let pkcs8 = aws_lc_rs::encoding::AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&rsa).unwrap();
            let pem = pem_of(pkcs8.as_ref());
            let mut header = jsonwebtoken::Header::new(Algorithm::RS256);
            header.kid = jsonwebtoken::decode_header(&token).unwrap().kid;
            let key = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
            jsonwebtoken::encode(&header, &claims, &key).unwrap()
        };
        assert!(
            sdk_verifies(server, metadata, &foreign, bot, &service_url)
                .await
                .is_err()
        );
        // an EdDSA token of the server's own (a person's access token) is no activity token
        let person = registry.token().await;
        assert!(
            sdk_verifies(server, metadata, &person, bot, &service_url)
                .await
                .is_err()
        );
        registry.server.stop().await;
    }
}

fn pem_of(der: &[u8]) -> String {
    use base64::Engine as _;
    let body = base64::engine::general_purpose::STANDARD.encode(der);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn two_nodes_share_the_bot_keys_and_a_rotation_on_one_is_published_by_both() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let first = &registry.server;
        let second = TestServer::start_with(backend, |config| {
            config.database_url.clone_from(&first.app.config.database_url);
            config.public_origin.clone_from(&first.app.config.public_origin);
        })
        .await;
        let added = registry
            .register("Echo (synthetic)", "https://bot.example.test/api/messages")
            .await;
        let bot = added["id"].as_str().unwrap();
        let service_url = ngteams_server::apps::service_url(&first.app.config);
        let metadata = "/bot/v1/.well-known/openidconfiguration";

        let before = first.app.bot_keys.activity_token(bot, &service_url).unwrap();
        let from_second = second.app.bot_keys.activity_token(bot, &service_url).unwrap();
        assert_eq!(
            jsonwebtoken::decode_header(&before).unwrap().kid,
            jsonwebtoken::decode_header(&from_second).unwrap().kid
        );
        sdk_verifies(&second, metadata, &before, bot, &service_url)
            .await
            .unwrap();

        // the first node publishes a new key: the second publishes it at once, and neither signs with it yet, as a
        // bot may still hold the keys it fetched before
        let kid = first.app.bot_keys.rotate().await.unwrap();
        let published = get(&second, "/bot/v1/.well-known/keys").await;
        assert!(
            published["keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|key| key["kid"] == kid.as_str()),
            "{published}"
        );
        for node in [first, &second] {
            node.app.bot_keys.reload().await.unwrap();
            let after = node.app.bot_keys.activity_token(bot, &service_url).unwrap();
            assert_eq!(
                jsonwebtoken::decode_header(&after).unwrap().kid,
                jsonwebtoken::decode_header(&before).unwrap().kid
            );
            sdk_verifies(node, metadata, &after, bot, &service_url).await.unwrap();
        }
        sdk_verifies(first, metadata, &before, bot, &service_url).await.unwrap();
        second.stop().await;
        registry.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_made_within_one_second_keep_the_order_they_were_made_in_and_the_first_still_signs() {
    for backend in backends() {
        let registry = Registry::open(backend).await;
        let server = &registry.server;
        let first = kids(server).await;
        assert_eq!(first.len(), 1, "{first:?}");
        let mut made = Vec::new();
        for _ in 0..4 {
            made.push(server.app.bot_keys.rotate().await.unwrap());
        }
        made.reverse();
        made.extend(first.iter().cloned());
        assert_eq!(kids(server).await, made);
        server.app.bot_keys.reload().await.unwrap();
        let token = server
            .app
            .bot_keys
            .activity_token("28:bot", "https://example.test/")
            .unwrap();
        assert_eq!(jsonwebtoken::decode_header(&token).unwrap().kid, first.first().cloned());
        registry.server.stop().await;
    }
}

diesel::table! {
    bot_signing_keys (kid) {
        kid -> Text,
        retired_at -> Nullable<BigInt>,
    }
}

/// Marks the bot signing key `kid` retired at `at`.
async fn retire(server: &TestServer, kid: &str, at: i64) -> Result<usize, ngteams_server::db::DbError> {
    use diesel::{ExpressionMethods as _, QueryDsl as _};
    use diesel_async::RunQueryDsl as _;
    let query =
        diesel::update(bot_signing_keys::table.find(kid.to_owned())).set(bot_signing_keys::retired_at.eq(Some(at)));
    let db = &server.app.db;
    ngteams_server::with_conn!(db, |conn| Ok(query.execute(conn).await?))
}

#[tokio::test(flavor = "multi_thread")]
async fn housekeeping_publishes_the_next_bot_key_before_it_signs_and_drops_one_nothing_it_signed_can_still_use() {
    let registry = Registry::open_with(crate::support::Backend::Sqlite, |config| {
        config.bots.key_rotation = 1;
        config.bots.activity_token_lifetime = 1;
    })
    .await;
    let server = &registry.server;
    let added = registry
        .register("Echo (synthetic)", "https://bot.example.test/api/messages")
        .await;
    let bot = added["id"].as_str().unwrap();
    let service_url = ngteams_server::apps::service_url(&server.app.config);
    let kid = |token: &str| jsonwebtoken::decode_header(token).unwrap().kid.unwrap();
    let signing = || kid(&server.app.bot_keys.activity_token(bot, &service_url).unwrap());
    let published = |server: &TestServer| {
        let server = server.origin.clone();
        async move {
            let keys: Value = reqwest::get(format!("{server}/bot/v1/.well-known/keys"))
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            keys["keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| key["kid"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };
    let first = signing();
    let shown = format!("{:?}", server.app.bot_keys);
    assert!(shown.contains(&first) && !shown.contains("PRIVATE"), "{shown}");

    // due, but a media node alone doesn't rotate the bots' keys: the API does
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ngteams_server::app::tidy_for(&server.app, ngteams_server::mixer::Role::Media).await;
    assert_eq!(published(server).await, std::slice::from_ref(&first));

    // due: the next key is published, and the first still signs
    ngteams_server::app::tidy_for(&server.app, ngteams_server::mixer::Role::Api).await;
    assert_eq!(signing(), first);
    let keys = published(server).await;
    assert_eq!(keys.len(), 2, "{keys:?}");
    let next = keys.iter().find(|key| **key != first).unwrap().clone();

    // published long enough, it signs; the first is retired but still published
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ngteams_server::app::tidy(&server.app).await;
    assert_eq!(signing(), next);
    let keys = published(server).await;
    assert!(keys.contains(&first) && keys.contains(&next), "{keys:?}");

    // a key retired longer ago than its tokens and the nodes' reload allowance goes; one retired since stays
    let keep = i64::try_from(ngteams_server::apps::keys::retention(&server.app.config).as_secs()).unwrap();
    assert!(keep < 4 * 3600);
    let recent = server.app.bot_keys.rotate().await.unwrap();
    let at = ngteams_server::db::now();
    assert_eq!(retire(server, &first, at - keep - 60).await.unwrap(), 1);
    assert_eq!(retire(server, &recent, at - keep + 60).await.unwrap(), 1);
    ngteams_server::app::tidy(&server.app).await;
    let keys = published(server).await;
    assert!(
        !keys.contains(&first) && keys.contains(&recent) && keys.contains(&next),
        "{keys:?}"
    );
    registry.server.stop().await;
}

/// Deletes every bot signing key.
async fn forget_keys(server: &TestServer) -> Result<usize, ngteams_server::db::DbError> {
    use diesel_async::RunQueryDsl as _;
    let query = diesel::delete(bot_signing_keys::table);
    let db = &server.app.db;
    ngteams_server::with_conn!(db, |conn| Ok(query.execute(conn).await?))
}

/// The `kid` of each published bot key.
async fn kids(server: &TestServer) -> Vec<String> {
    get(server, "/bot/v1/.well-known/keys").await["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["kid"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_node_that_serves_the_api_makes_or_rotates_the_bot_keys_at_its_start() {
    use ngteams_server::app::App;
    use ngteams_server::mixer::Role;
    let registry = Registry::open_with(crate::support::Backend::Sqlite, |config| {
        config.bots.key_rotation = 1;
    })
    .await;
    let server = &registry.server;
    let config = server.app.config.clone();
    let first = kids(server).await;
    assert_eq!(first.len(), 1, "{first:?}");

    // due: a media node starts on the keys as they are, and signs with the same key
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let media = App::start_for(config.clone(), Role::Media).await.unwrap();
    assert_eq!(kids(server).await, first);
    let token = media
        .bot_keys
        .activity_token("28:bot", "https://example.test/")
        .unwrap();
    assert_eq!(jsonwebtoken::decode_header(&token).unwrap().kid, first.first().cloned());

    // an api node publishes the next key at its start
    App::start_for(config.clone(), Role::Api).await.unwrap();
    assert_eq!(kids(server).await.len(), 2);

    // no key yet: a media node makes none; an api node makes the first
    assert_eq!(forget_keys(server).await.unwrap(), 2);
    let media = App::start_for(config.clone(), Role::Media).await.unwrap();
    assert!(kids(server).await.is_empty());
    assert!(
        media
            .bot_keys
            .activity_token("28:bot", "https://example.test/")
            .is_err()
    );
    App::start_for(config, Role::Api).await.unwrap();
    assert_eq!(kids(server).await.len(), 1);
    registry.server.stop().await;
}
