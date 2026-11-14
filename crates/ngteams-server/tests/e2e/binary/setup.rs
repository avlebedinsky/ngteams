//! The first-admin setup against the real binary: the code it logs when it starts without an admin, the code the
//! command line issues, and an install that asks for none.

use std::path::Path;

use serde_json::{Value, json};

use super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD, backends};

const SETUP: &str = "/api/admin/v1/setup";
const ROOT: &str = "root@own.test";

async fn setup_state(node: &Node) -> Value {
    let response = node.http.get(format!("{}{SETUP}", node.endpoint)).send().await.unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn create(node: &Node, code: Option<&str>) -> u16 {
    let mut body = json!({"upn": ROOT, "display_name": "Root", "password": PASSWORD});
    if let Some(code) = code {
        body["code"] = code.into();
    }
    let response = node
        .http
        .post(format!("{}{SETUP}", node.endpoint))
        .json(&body)
        .send()
        .await
        .unwrap();
    response.status().as_u16()
}

/// The setup code a node logged, when it logged one.
fn logged_code(log: &Path) -> Option<String> {
    const MARK: &str = "setup code: ";
    let text = std::fs::read_to_string(log).unwrap();
    let after = &text[text.find(MARK)? + MARK.len()..];
    Some(
        after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_server_logs_a_setup_code_and_the_first_admin_is_made_with_one() {
    for backend in backends() {
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        let log = deployment.dir.path().join("first.log");
        node.start_logged(&log).await;
        assert_eq!(
            setup_state(&node).await,
            json!({"needs_admin": true, "code_required": true})
        );
        let logged = logged_code(&log).expect("a node without an admin logs a setup code");
        assert_eq!(logged.len(), 29, "five groups of five");
        assert_eq!(create(&node, None).await, 403);

        // the command line issues a new code, which replaces the logged one
        let printed = node.cli(&["setup-code"], "");
        let issued = printed.lines().next().unwrap();
        assert_ne!(issued, logged);
        assert_eq!(create(&node, Some(&logged)).await, 403, "{backend:?}");
        assert_eq!(create(&node, Some(issued)).await, 201, "{backend:?}");
        assert_eq!(create(&node, Some(issued)).await, 409);

        // the admin signs in with the client's session and reaches the admin API
        let session = node.sign_in(ROOT, PASSWORD).await;
        session.skypetoken().await.unwrap();
        let token = session.access_token(teams_auth::token::SPACES_RESOURCE).await.unwrap();
        let users = node
            .http
            .get(format!("{}/api/admin/v1/users", node.endpoint))
            .bearer_auth(token.token.expose())
            .send()
            .await
            .unwrap();
        assert_eq!(users.status(), 200);
        let users: Value = users.json().await.unwrap();
        assert_eq!(users["users"][0]["upn"], ROOT);

        // a restart with an admin logs no code
        node.stop().await;
        let log = deployment.dir.path().join("second.log");
        node.start_logged(&log).await;
        assert_eq!(logged_code(&log), None);
        assert_eq!(
            setup_state(&node).await,
            json!({"needs_admin": false, "code_required": false})
        );
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_without_an_admin_keeps_the_issued_code() {
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    let log = deployment.dir.path().join("first.log");
    node.start_logged(&log).await;
    let logged = logged_code(&log).unwrap();
    node.stop().await;
    let log = deployment.dir.path().join("second.log");
    node.start_logged(&log).await;
    assert_eq!(logged_code(&log), None, "the code is logged once");
    assert!(
        std::fs::read_to_string(&log).unwrap().contains("setup-code"),
        "the log says how to get a new one"
    );
    assert_eq!(create(&node, Some(&logged)).await, 201);
    node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trusted_install_needs_no_setup_code() {
    use std::io::Write as _;
    let deployment = Deployment::new(Backend::Sqlite, false, None).await;
    let mut node = deployment.node("a");
    let mut config = std::fs::OpenOptions::new().append(true).open(&node.config).unwrap();
    writeln!(config, "[setup]\nrequire_code = false").unwrap();
    drop(config);
    let log = deployment.dir.path().join("node.log");
    node.start_logged(&log).await;
    assert_eq!(logged_code(&log), None);
    assert_eq!(
        setup_state(&node).await,
        json!({"needs_admin": true, "code_required": false})
    );
    assert_eq!(create(&node, None).await, 201);
    assert_eq!(create(&node, None).await, 409);
    node.sign_in(ROOT, PASSWORD).await.skypetoken().await.unwrap();
    node.stop().await;
}
