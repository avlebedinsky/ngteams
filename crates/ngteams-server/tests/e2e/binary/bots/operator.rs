//! The registry on the command line, as the operator of one `all` node on SQLite runs it: `ngteams-server app
//! add|list|disable|enable|rotate-secret|remove`, what it refuses, and the audit with `cli` as the actor.

use std::path::Path;

use reqwest::Method;
use serde_json::json;
use teams_auth::secrets::Secret;

use super::super::support::{Deployment, Node};
use super::packages::new_id;
use super::support::{AUDIT, ENDPOINT, app_path, bot_token, call, configure, first_admin, tenant};
use crate::bots::{good_package, manifest, package, png};
use crate::support::Backend;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_operator_runs_the_registry_on_the_command_line() {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    let files = deployment.dir.path().join("files");
    configure(&node, &format!("storage_url = \"file://{}\"\n", files.display()), "");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let tenant = tenant(&root);
    let dir = deployment.dir.path();

    let (id, secret) = add_from_the_form(&node, &root, &tenant).await;
    let package_secret = add_a_package(&node, &tenant, dir, &files).await;
    let listed = node.cli(&["app", "list"], "");
    assert!(
        listed.contains(&format!("{id}\tenabled\tOps Echo (synthetic)\t28:{id}")),
        "{listed}"
    );
    assert!(!listed.contains(&secret) && !listed.contains(&package_secret));
    let new = change(&node, &tenant, &id, &secret).await;
    refusals(&node, dir);
    audit(&node, &root, &[&secret, &new, &package_secret]).await;
    node.stop().await;
}

/// `app add --name`: the id and the secret it printed.
async fn add_from_the_form(node: &Node, root: &Secret, tenant: &str) -> (String, String) {
    eprintln!("operator: app add from the form");
    let args = [
        "app",
        "add",
        "--name",
        "Ops Echo (synthetic)",
        "--endpoint",
        ENDPOINT,
        "--description",
        "From the shell",
    ];
    let added = node.cli(&args, "");
    let mut lines = added.lines();
    let first = lines.next().unwrap();
    assert!(
        first.starts_with("added ") && first.ends_with(" (Ops Echo (synthetic))"),
        "{first}"
    );
    let id = first["added ".len()..first.find(" (").unwrap()].to_owned();
    let secret = lines.next().unwrap().strip_prefix("secret ").unwrap().to_owned();
    assert_eq!(bot_token(node, tenant, &id, &secret).await.status, 200);
    // the admin API sees what the shell made
    let (status, read) = call(node, Method::GET, &app_path(&id), Some(root), None).await;
    assert_eq!(
        (status, &read["short_description"]),
        (200, &json!("From the shell")),
        "{read}"
    );
    (id, secret)
}

/// `app add --package`: the bot's secret it printed.
async fn add_a_package(node: &Node, tenant: &str, dir: &Path, files: &Path) -> String {
    eprintln!("operator: app add from a package");
    let zip = dir.join("polls.zip");
    let (app, bot) = (new_id(), new_id());
    std::fs::write(&zip, good_package(&app, &bot, "1.0.0")).unwrap();
    let packaged = node.cli(&["app", "add", "--package", zip.to_str().unwrap()], "");
    let first = packaged.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("added {app} (Polls (synthetic))")),
        "{first}"
    );
    let secret = packaged
        .lines()
        .nth(1)
        .unwrap()
        .strip_prefix("secret ")
        .unwrap()
        .to_owned();
    assert!(files.join("apps").join(&app).join("color.png").is_file());
    assert_eq!(bot_token(node, tenant, &bot, &secret).await.status, 200);
    let listed = node.cli(&["app", "list"], "");
    assert!(
        listed.contains(&format!("{app}\tenabled\tPolls (synthetic)\t28:{bot}")),
        "{listed}"
    );
    secret
}

/// disable, enable, rotate-secret, remove: the last secret.
async fn change(node: &Node, tenant: &str, id: &str, secret: &str) -> String {
    eprintln!("operator: disable, enable, rotate-secret, remove");
    node.cli(&["app", "disable", id], "");
    assert!(node.cli(&["app", "list"], "").contains(&format!("{id}\tdisabled\t")));
    let refused = bot_token(node, tenant, id, secret).await;
    assert_eq!(refused.error().0, "unauthorized_client", "{}", refused.error_text());
    node.cli(&["app", "enable", id], "");
    assert_eq!(bot_token(node, tenant, id, secret).await.status, 200);
    let rotated = node.cli(&["app", "rotate-secret", id], "");
    let new = rotated
        .lines()
        .find_map(|line| line.strip_prefix("secret "))
        .unwrap_or_else(|| rotated.trim())
        .to_owned();
    assert_ne!(new, secret);
    assert_eq!(bot_token(node, tenant, id, secret).await.status, 401);
    assert_eq!(bot_token(node, tenant, id, &new).await.status, 200);
    node.cli(&["app", "remove", id], "");
    assert!(!node.cli(&["app", "list"], "").contains(id));
    assert_eq!(bot_token(node, tenant, id, &new).await.status, 401);
    new
}

fn refusals(node: &Node, dir: &Path) {
    eprintln!("operator: what the shell refuses");
    let bad = dir.join("bad.zip");
    let bad_bot = manifest(&new_id(), "polls-bot", "1.0.0");
    std::fs::write(&bad, package(&bad_bot, &png(192, 192), &png(32, 32), &[])).unwrap();
    let missing = dir.join("missing.zip");
    let unknown = new_id();
    let long = "x".repeat(31);
    let refusals: [&[&str]; 9] = [
        &["app", "add", "--package", bad.to_str().unwrap()],
        &["app", "add", "--package", missing.to_str().unwrap()],
        &["app", "add", "--name", &long],
        &[
            "app",
            "add",
            "--name",
            "Plain",
            "--endpoint",
            "http://bots.example.test/",
        ],
        &["app", "add", "--name", "Plain", "--unsigned"],
        &["app", "disable", &unknown],
        &["app", "rotate-secret", &unknown],
        &["app", "remove", "not-an-id"],
        &["app", "add"],
    ];
    for args in refusals {
        let (ok, error) = node.try_cli(args, "");
        assert!(!ok, "{args:?} succeeded");
        assert!(!error.is_empty(), "{args:?} says why");
        eprintln!("  {args:?}: {}", error.lines().next().unwrap_or_default());
    }
}

async fn audit(node: &Node, root: &Secret, secrets: &[&str]) {
    let (status, audit) = call(node, Method::GET, &format!("{AUDIT}?limit=200"), Some(root), None).await;
    assert_eq!(status, 200);
    let text = audit.to_string();
    assert!(
        secrets.iter().all(|secret| !text.contains(secret)),
        "no secret in the audit"
    );
    let entries = audit["entries"].as_array().unwrap();
    // a package from the shell is an `app.add`, as one from the admin API is; `app.package` is a new version
    for action in ["app.add", "app.update", "app.secret", "app.delete"] {
        let entry = entries
            .iter()
            .find(|entry| entry["action"] == action)
            .unwrap_or_else(|| panic!("{action}: {audit}"));
        assert_eq!(entry["actor"], "cli", "{entry}");
    }
}
