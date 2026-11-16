//! Two `api` nodes and a `media` node on one Postgres and one Redis (when both are named): an app made on one node is
//! the other's, a wrong secret on either counts toward one hold, a change on one is in force on the other, a key
//! rotation on one is published by both, and a `media` node rotates no bot key.

use std::time::Duration;

use reqwest::Method;
use serde_json::json;
use teams_auth::secrets::Secret;

use super::super::support::{Deployment, Node, Port};
use super::support::{
    APPS, ODD_BOTS, USERS, app_path, bot_token, call, configure, first_admin, kids, out_of_order, register,
    seed_people, tenant,
};
use crate::support::{Backend, PG_ENV, redis_url};

/// Seconds a bot key signs before the next one: short, so that a rotation falls due during the test.
const ROTATION: u64 = 8;
const KEYS: &str = "/bot/v1/.well-known/keys";

/// The two API nodes, the administrator's token and the tenant.
struct Pair<'a> {
    a: &'a Node,
    b: &'a Node,
    root: Secret,
    tenant: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_api_nodes_and_a_media_node_share_bots_holds_and_keys() {
    let Some(redis) = redis_url().filter(|_| std::env::var_os(PG_ENV).is_some()) else {
        eprintln!("skipped: needs Postgres and Redis");
        return;
    };
    let deployment = Deployment::new(Backend::Postgres, false, Some(redis)).await;
    let (mut a, mut b, mut media) = (deployment.node("a"), deployment.node("b"), deployment.node("media"));
    let internal = Port::take();
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], internal.number));
    for node in [&a, &b, &media] {
        configure(node, "", &format!("[bots]\nkey_rotation = {ROTATION}\n"));
    }
    let mixer = format!("[mixer]\nudp_listen = \"127.0.0.1:0\"\ninternal_listen = \"{address}\"\n");
    configure(&media, "", &mixer);
    a.role = Some(("api", None));
    b.role = Some(("api", None));
    media.role = Some(("media", Some(address)));
    a.cli(&["migrate"], "");
    a.start().await;
    b.start().await;
    let root = first_admin(&a).await;
    let pair = Pair {
        a: &a,
        b: &b,
        tenant: tenant(&root),
        root,
    };
    let mut problems = Vec::new();
    let (bot, secret) = shared_app(&pair).await;
    problems.extend(many_apps(&pair).await);
    shared_hold(&pair, &bot, &secret).await;
    changes_in_force(&pair, &bot, &secret).await;
    let before = kids(&a, KEYS).await;
    assert_eq!(kids(&b, KEYS).await, before);
    assert_eq!(kids(&b, &format!("/{}/discovery/v2.0/keys", pair.tenant)).await, before);
    drop(pair);
    let after_a = rotation_on_a(&mut a, &b, &before).await;
    problems.extend(media_rotates_nothing(&mut media, &b, &after_a).await);
    for node in [&mut a, &mut b, &mut media] {
        node.stop().await;
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// An app made on a: b lists it and gives its bot a token; no bot among b's accounts. The bot's id and secret.
async fn shared_app(pair: &Pair<'_>) -> (String, String) {
    eprintln!("nodes: an app made on a is b's");
    let people = seed_people(pair.b, &pair.root, "nodes", 25).await;
    let (bot, secret) = register(pair.a, &pair.root, "Echo (synthetic)").await;
    let (status, listed) = call(pair.b, Method::GET, APPS, Some(&pair.root), None).await;
    assert_eq!(status, 200);
    assert!(
        listed["apps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|app| app["id"] == bot.as_str())
    );
    assert_eq!(bot_token(pair.b, &pair.tenant, &bot, &secret).await.status, 200);
    let (_, users) = call(
        pair.b,
        Method::GET,
        &format!("{USERS}?limit=200"),
        Some(&pair.root),
        None,
    )
    .await;
    assert_eq!(
        users["users"].as_array().unwrap().len(),
        people.len() + 1,
        "no bot among the accounts"
    );
    (bot, secret)
}

/// Forty more apps, made on both nodes: each lists all; what is wrong with the order, if anything.
async fn many_apps(pair: &Pair<'_>) -> Vec<String> {
    eprintln!("nodes: many apps, made on both nodes, listed by either");
    for index in 0..40 {
        let node = if index % 2 == 0 { pair.a } else { pair.b };
        let name = format!("{} {index:02}", ODD_BOTS[index % ODD_BOTS.len()].trim());
        let name: String = name.chars().take(30).collect();
        register(node, &pair.root, &name).await;
    }
    let mut problems = Vec::new();
    for node in [pair.a, pair.b] {
        let (_, listed) = call(node, Method::GET, APPS, Some(&pair.root), None).await;
        let names: Vec<&str> = listed["apps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|app| app["name"].as_str().unwrap())
            .collect();
        assert_eq!(names.len(), 41);
        problems.extend(out_of_order(&names));
    }
    problems
}

async fn shared_hold(pair: &Pair<'_>, bot: &str, secret: &str) {
    eprintln!("nodes: wrong secrets on both nodes count toward one hold");
    for (node, attempt) in [(pair.a, 0), (pair.b, 1), (pair.a, 2)] {
        let wrong = bot_token(node, &pair.tenant, bot, &format!("synthetic-wrong-{attempt}")).await;
        assert_eq!(wrong.status, 401, "{}", wrong.error_text());
    }
    for node in [pair.b, pair.a] {
        let held = bot_token(node, &pair.tenant, bot, "synthetic-wrong-x").await;
        let got = (held.status, held.error().0);
        assert_eq!(got, (429, "temporarily_unavailable"), "{}", held.error_text());
        assert!(
            held.retry_after.is_some_and(|seconds| (1..=30).contains(&seconds)),
            "{held:?}"
        );
    }
    let right = bot_token(pair.b, &pair.tenant, bot, secret).await;
    assert_eq!(right.status, 200, "the right secret is never held off");
}

async fn changes_in_force(pair: &Pair<'_>, bot: &str, secret: &str) {
    eprintln!("nodes: a change on one node is in force on the other");
    let (root, tenant) = (Some(&pair.root), pair.tenant.as_str());
    let path = app_path(bot);
    call(pair.b, Method::PATCH, &path, root, Some(json!({"enabled": false}))).await;
    assert_eq!(
        bot_token(pair.a, tenant, bot, secret).await.error().0,
        "unauthorized_client"
    );
    call(pair.a, Method::PATCH, &path, root, Some(json!({"enabled": true}))).await;
    assert_eq!(bot_token(pair.b, tenant, bot, secret).await.status, 200);
    secret_and_delete_in_force(pair, bot, secret).await;
}

async fn secret_and_delete_in_force(pair: &Pair<'_>, bot: &str, secret: &str) {
    let (root, tenant) = (Some(&pair.root), pair.tenant.as_str());
    let path = app_path(bot);
    let (_, rotated) = call(pair.a, Method::POST, &format!("{path}/secret"), root, None).await;
    let new = rotated["secret"].as_str().unwrap().to_owned();
    assert_eq!(bot_token(pair.b, tenant, bot, secret).await.status, 401);
    assert_eq!(bot_token(pair.b, tenant, bot, &new).await.status, 200);
    assert_eq!(call(pair.b, Method::DELETE, &path, root, None).await.0, 204);
    assert_eq!(bot_token(pair.a, tenant, bot, &new).await.status, 401);
}

/// Restarts a once a rotation is due: b publishes a's new key at once. The keys then.
async fn rotation_on_a(a: &mut Node, b: &Node, before: &[String]) -> Vec<String> {
    eprintln!("nodes: a rotation on a is published by b at once");
    tokio::time::sleep(Duration::from_secs(ROTATION + 2)).await;
    a.stop().await;
    a.start().await;
    let after = kids(b, KEYS).await;
    assert_eq!(after.len(), before.len() + 1, "a published the next key");
    assert_eq!(
        &after[1..],
        before,
        "the new key comes first, the old ones stay published"
    );
    assert_eq!(kids(a, KEYS).await, after);
    after
}

/// Starts the media node once the next rotation is due; what it published, if anything.
async fn media_rotates_nothing(media: &mut Node, b: &Node, before: &[String]) -> Option<String> {
    eprintln!("nodes: a media node, started once the next rotation is due, rotates no bot key");
    tokio::time::sleep(Duration::from_secs(2 * ROTATION + 2)).await;
    assert_eq!(
        kids(b, KEYS).await,
        before,
        "nothing rotates before the media node starts"
    );
    media.start().await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = kids(b, KEYS).await;
    eprintln!("  keys before the media node: {}, after: {}", before.len(), after.len());
    (after != before).then(|| format!("the media node published a bot key: {before:?} became {after:?}"))
}
