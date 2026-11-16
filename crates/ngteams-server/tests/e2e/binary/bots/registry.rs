//! The app registry on one `all` node on SQLite, as an admin runs it through the admin API: bots from the form and
//! from Teams app packages, each secret shown once; list, read, enable, disable, limit to people, endpoint and mode,
//! a new secret, delete; the audit of each; and the packages Teams would refuse, refused.

use std::path::Path;

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;

use super::super::support::{Deployment, Node};
use super::packages::{PACKAGE, new_id, refused_packages};
use super::support::{
    APPS, AUDIT, ENDPOINT, ODD_BOTS, WORKFLOWS_BOT, app_path, bearer, bot_token, call, code, configure, first_admin,
    out_of_order, register, seed_people, tenant, upload,
};
use crate::bots::{good_package, manifest, package, png};
use crate::support::Backend;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_admin_runs_the_registry_from_the_form_and_from_packages() {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    let files = deployment.dir.path().join("files");
    configure(&node, &format!("storage_url = \"file://{}\"\n", files.display()), "");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let tenant = tenant(&root);
    let people = seed_people(&node, &root, "reg", 30).await;

    let (ids, secrets) = form_registrations(&node, &root, &tenant).await;
    let order = listed_once(&node, &root, &secrets).await;
    form_refusals(&node, &root).await;
    only_admins(&node).await;
    packages(&node, &root, &files).await;
    refused_packages(&node, &root, &people[0]).await;
    enable_and_disable(&node, &root, &tenant, &ids[1], &secrets[1]).await;
    limit_to_people(&node, &root, &ids[1], &people).await;
    endpoint_and_mode(&node, &root, &ids[1]).await;
    let new = rotate(&node, &root, &tenant, &ids[1], &secrets[1]).await;
    let echo = delete_and_revive(&node, &root, &tenant, &ids[1], &new).await;
    audit(&node, &root, &secrets, &echo).await;
    node.stop().await;
    assert!(order.is_none(), "{}", order.unwrap_or_default());
}

/// Registers a bot of each odd name; their ids and secrets.
async fn form_registrations(node: &Node, root: &Secret, tenant: &str) -> (Vec<String>, Vec<String>) {
    eprintln!("registry: the form, odd names, the secret once");
    let (mut ids, mut secrets) = (Vec::new(), Vec::new());
    for name in ODD_BOTS {
        let (id, secret) = register(node, root, name).await;
        let (status, read) = call(node, Method::GET, &app_path(&id), Some(root), None).await;
        assert_eq!(status, 200);
        assert_eq!(read["name"], name.trim(), "{read}");
        assert_eq!(read["source"], "form");
        assert_eq!(read["bot"]["mri"], format!("28:{id}"));
        assert_eq!(read["bot"]["endpoint"], ENDPOINT);
        assert_eq!(read["bot"]["auth_mode"], "signed");
        assert!(read.get("secret").is_none());
        assert_eq!(bot_token(node, tenant, &id, &secret).await.status, 200);
        ids.push(id);
        secrets.push(secret);
    }
    (ids, secrets)
}

/// Lists the apps: no secret in the list; what is wrong with its order, if anything.
async fn listed_once(node: &Node, root: &Secret, secrets: &[String]) -> Option<String> {
    let (status, listed) = call(node, Method::GET, APPS, Some(root), None).await;
    assert_eq!(status, 200);
    let names: Vec<&str> = listed["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|app| app["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), ODD_BOTS.len());
    let shown = listed.to_string();
    assert!(secrets.iter().all(|secret| !shown.contains(secret.as_str())));
    out_of_order(&names)
}

async fn form_refusals(node: &Node, root: &Secret) {
    eprintln!("registry: what the form refuses");
    let refused = [
        (json!({"name": ""}), "InvalidName"),
        (json!({"name": "     "}), "InvalidName"),
        (json!({"name": "1234567890123456789012345678901"}), "InvalidName"),
        (json!({"name": "ØØØØØØØØØØØØØØØØØØØØØØØØØØØØØØØ"}), "InvalidName"),
        (
            json!({"name": "Plain", "endpoint": "http://bots.example.test/api"}),
            "InvalidEndpoint",
        ),
        (
            json!({"name": "Plain", "endpoint": "https://user:pw@bots.example.test/api"}),
            "InvalidEndpoint",
        ),
        (
            json!({"name": "Plain", "endpoint": "https://bots.example.test/api#frag"}),
            "InvalidEndpoint",
        ),
        (
            json!({"name": "Plain", "endpoint": "ftp://bots.example.test/"}),
            "InvalidEndpoint",
        ),
        (json!({"name": "Plain", "endpoint": "not a url"}), "InvalidEndpoint"),
        (json!({"name": "Plain", "auth_mode": "unsigned"}), "UnsignedNotAllowed"),
    ];
    for (body, expected) in refused {
        let (status, answer) = call(node, Method::POST, APPS, Some(root), Some(body.clone())).await;
        assert_eq!((status, code(&answer)), (400, expected), "{body}: {answer}");
    }
    for body in [
        json!({"auth_mode": "signed"}),
        json!({"name": 5}),
        json!({"name": "x", "auth_mode": "both"}),
    ] {
        let (status, _) = call(node, Method::POST, APPS, Some(root), Some(body.clone())).await;
        assert!((400..500).contains(&status), "{body}: {status}");
    }
    let body = json!({"name": "No endpoint yet"});
    let (status, no_endpoint) = call(node, Method::POST, APPS, Some(root), Some(body)).await;
    assert_eq!(status, 201, "{no_endpoint}");
    assert!(no_endpoint["bot"]["endpoint"].is_null(), "{no_endpoint}");
}

async fn only_admins(node: &Node) {
    eprintln!("registry: only an admin, with a token");
    let (status, _) = call(node, Method::GET, APPS, None, None).await;
    assert_eq!(status, 401);
    let person = bearer(node, "reg.0@bots.own.test").await;
    for method in [Method::GET, Method::POST] {
        let (status, _) = call(node, method, APPS, Some(&person), Some(json!({"name": "Sneaky"}))).await;
        assert_eq!(status, 403);
    }
    let sneaky = good_package(&new_id(), &new_id(), "1.0.0");
    let (status, _) = upload(node, &person, Method::POST, PACKAGE, sneaky).await;
    assert_eq!(status, 403);
}

/// A package's app, a second package naming it or its bot, a new version, and an app without a bot.
async fn packages(node: &Node, root: &Secret, files: &Path) {
    eprintln!("registry: packages");
    let (app, bot) = (new_id(), new_id());
    let (status, added) = upload(node, root, Method::POST, PACKAGE, good_package(&app, &bot, "1.0.0")).await;
    assert_eq!(status, 201, "{added}");
    assert_eq!((&added["id"], &added["source"]), (&json!(app), &json!("package")));
    assert_eq!(
        (&added["bot"]["id"], &added["bot"]["mri"]),
        (&json!(bot), &json!(format!("28:{bot}")))
    );
    assert!(added["bot"]["endpoint"].is_null());
    assert_eq!(added["bot"]["scopes"], json!(["personal", "team", "groupChat"]));
    assert_eq!(added["accent_color"], "#4F6BED");
    assert!(added["secret"].as_str().unwrap().len() >= 40);
    let kept = ["color.png", "outline.png"].map(|icon| files.join("apps").join(&app).join(icon));
    assert!(kept.iter().all(|icon| icon.is_file()), "{kept:?} kept");
    named_again(node, root, &app, &bot).await;
    new_version(node, root, &app, &bot).await;
    app_without_bot(node, root).await;
}

/// A second package naming the app or its bot.
async fn named_again(node: &Node, root: &Secret, app: &str, bot: &str) {
    let again = [
        (good_package(app, &new_id(), "1.0.1"), "AppExists"),
        (good_package(&new_id(), bot, "1.0.0"), "BotExists"),
        // an uppercase GUID is the same id
        (good_package(&app.to_uppercase(), &new_id(), "1.0.2"), "AppExists"),
    ];
    for (bytes, expected) in again {
        let (status, answer) = upload(node, root, Method::POST, PACKAGE, bytes).await;
        assert_eq!((status, code(&answer)), (409, expected), "{answer}");
    }
}

async fn new_version(node: &Node, root: &Secret, app: &str, bot: &str) {
    let path = format!("{}/package", app_path(app));
    let (status, replaced) = upload(node, root, Method::PUT, &path, good_package(app, bot, "2.0.0")).await;
    assert_eq!(status, 200, "{replaced}");
    assert_eq!(replaced["version"], "2.0.0");
    assert!(replaced.get("secret").is_none());
    let (status, answer) = upload(node, root, Method::PUT, &path, good_package(app, &new_id(), "3.0.0")).await;
    assert_eq!((status, code(&answer)), (400, "BotChanged"), "{answer}");
    let (status, answer) = upload(node, root, Method::PUT, &path, good_package(&new_id(), bot, "3.0.0")).await;
    assert!((400..500).contains(&status), "another app's package: {status} {answer}");
    let elsewhere = format!("{}/package", app_path(&new_id()));
    let (status, answer) = upload(node, root, Method::PUT, &elsewhere, good_package(app, bot, "3.0.0")).await;
    assert!(matches!(status, 400 | 404), "{status} {answer}");
}

async fn app_without_bot(node: &Node, root: &Secret) {
    let mut tab_only = manifest(&new_id(), &new_id(), "1.0.0");
    tab_only["bots"] = json!([]);
    tab_only.as_object_mut().unwrap().remove("composeExtensions");
    let bytes = package(&tab_only, &png(192, 192), &png(32, 32), &[]);
    let (status, no_bot) = upload(node, root, Method::POST, PACKAGE, bytes).await;
    assert_eq!(status, 201, "{no_bot}");
    assert!(
        no_bot["bot"].is_null() && no_bot.get("secret").is_none_or(Value::is_null),
        "{no_bot}"
    );
    let path = app_path(no_bot["id"].as_str().unwrap());
    for (method, path, body) in [
        (Method::PATCH, path.clone(), Some(json!({"endpoint": ENDPOINT}))),
        (Method::PATCH, path.clone(), Some(json!({"auth_mode": "signed"}))),
        (Method::POST, format!("{path}/secret"), None),
    ] {
        let (status, answer) = call(node, method, &path, Some(root), body).await;
        assert_eq!((status, code(&answer)), (400, "NoBot"), "{path}: {answer}");
    }
}

async fn enable_and_disable(node: &Node, root: &Secret, tenant: &str, id: &str, secret: &str) {
    eprintln!("registry: disable, enable");
    let path = app_path(id);
    let (status, off) = call(node, Method::PATCH, &path, Some(root), Some(json!({"enabled": false}))).await;
    assert_eq!((status, &off["enabled"]), (200, &json!(false)), "{off}");
    let refused = bot_token(node, tenant, id, secret).await;
    assert_eq!(
        (refused.status, refused.error()),
        (400, ("unauthorized_client", 7_000_112)),
        "{}",
        refused.error_text()
    );
    let (status, on) = call(node, Method::PATCH, &path, Some(root), Some(json!({"enabled": true}))).await;
    assert_eq!((status, &on["enabled"]), (200, &json!(true)));
    assert_eq!(bot_token(node, tenant, id, secret).await.status, 200);
}

async fn limit_to_people(node: &Node, root: &Secret, id: &str, people: &[String]) {
    eprintln!("registry: who may use it");
    let path = app_path(id);
    let chosen = json!({"everyone": false, "users": [people[2], people[5], people[2]]});
    let (status, limited) = call(node, Method::PATCH, &path, Some(root), Some(chosen)).await;
    assert_eq!((status, &limited["everyone"]), (200, &json!(false)), "{limited}");
    let mut users: Vec<&str> = limited["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    users.sort_unstable();
    let mut expected = vec![people[2].as_str(), people[5].as_str()];
    expected.sort_unstable();
    assert_eq!(users, expected, "{limited}");
    for users in [
        json!([id]),
        json!([WORKFLOWS_BOT]),
        json!([new_id()]),
        json!(["not-an-id"]),
    ] {
        let (status, answer) = call(node, Method::PATCH, &path, Some(root), Some(json!({"users": users}))).await;
        assert_eq!((status, code(&answer)), (400, "UnknownUser"), "{users}: {answer}");
    }
    let (status, everyone) = call(node, Method::PATCH, &path, Some(root), Some(json!({"everyone": true}))).await;
    assert_eq!((status, &everyone["everyone"]), (200, &json!(true)), "{everyone}");
}

async fn endpoint_and_mode(node: &Node, root: &Secret, id: &str) {
    eprintln!("registry: endpoint and mode");
    let path = app_path(id);
    let moved = "https://bots.example.test/v2/messages?tenant=synthetic";
    let (status, changed) = call(node, Method::PATCH, &path, Some(root), Some(json!({"endpoint": moved}))).await;
    assert_eq!((status, &changed["bot"]["endpoint"]), (200, &json!(moved)), "{changed}");
    for (body, expected) in [
        (json!({"endpoint": "http://bots.example.test/"}), "InvalidEndpoint"),
        (json!({"endpoint": "https://a:b@bots.example.test/"}), "InvalidEndpoint"),
        (json!({"auth_mode": "unsigned"}), "UnsignedNotAllowed"),
    ] {
        let (status, answer) = call(node, Method::PATCH, &path, Some(root), Some(body.clone())).await;
        assert_eq!((status, code(&answer)), (400, expected), "{body}: {answer}");
    }
    let (status, signed) = call(
        node,
        Method::PATCH,
        &path,
        Some(root),
        Some(json!({"auth_mode": "signed"})),
    )
    .await;
    assert_eq!(
        (status, &signed["bot"]["auth_mode"]),
        (200, &json!("signed")),
        "{signed}"
    );
    for missing in [app_path(&new_id()), app_path("not-an-id")] {
        let body = Some(json!({"enabled": false}));
        let (status, answer) = call(node, Method::PATCH, &missing, Some(root), body).await;
        assert_eq!(status, 404, "{missing}: {answer}");
        assert_eq!(
            call(node, Method::GET, &missing, Some(root), None).await.0,
            404,
            "{missing}"
        );
    }
}

/// A new secret: the old one stops at once. The new one.
async fn rotate(node: &Node, root: &Secret, tenant: &str, id: &str, old: &str) -> String {
    eprintln!("registry: a new secret");
    let (status, rotated) = call(
        node,
        Method::POST,
        &format!("{}/secret", app_path(id)),
        Some(root),
        None,
    )
    .await;
    assert_eq!(status, 200, "{rotated}");
    let new = rotated["secret"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    let stale = bot_token(node, tenant, id, old).await;
    assert_eq!(
        (stale.status, stale.error()),
        (401, ("invalid_client", 7_000_215)),
        "{}",
        stale.error_text()
    );
    assert_eq!(bot_token(node, tenant, id, &new).await.status, 200);
    new
}

/// Deletes the app, then registers its bot again from a package; the deleted app's name.
async fn delete_and_revive(node: &Node, root: &Secret, tenant: &str, id: &str, secret: &str) -> String {
    eprintln!("registry: delete, the bot again");
    let path = app_path(id);
    let name = call(node, Method::GET, &path, Some(root), None).await.1["name"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(call(node, Method::DELETE, &path, Some(root), None).await.0, 204);
    assert_eq!(call(node, Method::GET, &path, Some(root), None).await.0, 404);
    assert_eq!(call(node, Method::DELETE, &path, Some(root), None).await.0, 404);
    let gone = bot_token(node, tenant, id, secret).await;
    assert_eq!(
        (gone.status, gone.error().0),
        (401, "invalid_client"),
        "{}",
        gone.error_text()
    );
    assert!(
        !call(node, Method::GET, APPS, Some(root), None)
            .await
            .1
            .to_string()
            .contains(id)
    );
    revive(node, root, tenant, id, secret).await;
    name
}

/// The deleted bot's id comes back with a package of a new app, with a new secret.
async fn revive(node: &Node, root: &Secret, tenant: &str, id: &str, secret: &str) {
    let (status, back) = upload(node, root, Method::POST, PACKAGE, good_package(&new_id(), id, "1.0.0")).await;
    assert_eq!(status, 201, "{back}");
    let again = back["secret"].as_str().unwrap();
    assert_eq!(bot_token(node, tenant, id, again).await.status, 200);
    assert_eq!(bot_token(node, tenant, id, secret).await.status, 401);
}

async fn audit(node: &Node, root: &Secret, secrets: &[String], echo: &str) {
    eprintln!("registry: the audit");
    let (status, audit) = call(node, Method::GET, &format!("{AUDIT}?limit=500"), Some(root), None).await;
    assert_eq!(status, 200, "{audit}");
    let text = audit.to_string();
    assert!(
        secrets.iter().all(|secret| !text.contains(secret.as_str())),
        "no secret in the audit"
    );
    let entries: Vec<(String, String)> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let field = |key: &str| entry[key].as_str().unwrap_or_default().to_owned();
            (field("action"), field("target"))
        })
        .collect();
    let has = |action: &str, target: &str| entries.iter().any(|(a, t)| a == action && t == target);
    for name in ODD_BOTS {
        assert!(has("app.add", name.trim()), "app.add {name}: {entries:?}");
    }
    for action in ["app.update", "app.secret", "app.delete"] {
        assert!(has(action, echo), "{action} {echo}: {entries:?}");
    }
    assert!(has("app.package", "Polls (synthetic)"), "the new version: {entries:?}");
    // a refused request leaves no entry: the odd names, the form's bot without an endpoint and three packages
    let adds = entries.iter().filter(|(action, _)| action == "app.add").count();
    assert_eq!(adds, ODD_BOTS.len() + 1 + 3, "{entries:?}");
}
