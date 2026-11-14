//! Synthetic operators and native client sessions against real server processes, with no in-process server API.

mod support;

use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::SPACES_RESOURCE;

use super::support::{Deployment, Node};
use crate::support::{Backend, PASSWORD, PG_ENV, backends, redis_url};
use support::{
    AUDIT, BOB, CHANGE, ME, ROOT, SETUP, USERS, account, add, assert_roles, bearer, call, code, first_admin,
    invalid_password, last_admin_guards, patch, path, revoked, send,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operator_manages_accounts_and_native_sessions() {
    for backend in backends() {
        eprintln!("binary admin account lifecycle: {backend:?}");
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        node.start().await;
        let (root, _) = first_admin(&node).await;
        let id = add(&node, &root, BOB).await;
        assert_roles(&node, ROOT, true).await;
        assert_roles(&node, BOB, false).await;
        eprintln!("lifecycle {backend:?}: search and rename");
        find_and_update(&node, &root, &id).await;
        eprintln!("lifecycle {backend:?}: disable and enable");
        disable_and_enable(&node, &root, &id).await;
        eprintln!("lifecycle {backend:?}: delete and recreate");
        let session = node.sign_in("robert@own.test", PASSWORD).await;
        let old = session.access_token(SPACES_RESOURCE).await.unwrap();
        assert_eq!(call(&node, Method::DELETE, &path(&id), Some(&root), None).await.0, 204);
        invalid_password(&node, "robert@own.test", PASSWORD, 50126).await;
        revoked(&session).await;
        assert_eq!(call(&node, Method::GET, ME, Some(&old.token), None).await.0, 403);
        assert_eq!(call(&node, Method::GET, &path(&id), Some(&root), None).await.0, 404);
        let (_, users) = call(&node, Method::GET, USERS, Some(&root), None).await;
        assert_eq!(users["users"].as_array().unwrap().len(), 1);
        assert!(!node.cli(&["user", "list"], "").contains("robert@own.test"));
        let replacement = add(&node, &root, "robert@own.test").await;
        assert_ne!(replacement, id);
        node.sign_in("robert@own.test", PASSWORD)
            .await
            .skypetoken()
            .await
            .unwrap();
        node.stop().await;
    }
}

async fn find_and_update(node: &Node, root: &Secret, id: &str) {
    assert_search(node, root, "BOB", id).await;
    let old_session = node.sign_in(BOB, PASSWORD).await;
    let body = json!({"upn": "Robert@Own.Test", "display_name": "Robert Operator"});
    let (status, changed) = patch(node, root, id, body).await;
    assert_eq!(status, 200);
    assert_eq!(changed["upn"], "robert@own.test");
    assert_eq!(changed["display_name"], "Robert Operator");
    old_session.skypetoken().await.unwrap();
    let (status, read) = call(node, Method::GET, &path(id), Some(root), None).await;
    assert_eq!(status, 200);
    assert_eq!(read, changed);
    assert_search(node, root, "OPERATOR", id).await;
    let listed = node.cli(&["user", "list"], "");
    assert!(listed.contains("robert@own.test\t"));
    assert!(listed.contains("Robert Operator"));
}

async fn assert_search(node: &Node, root: &Secret, query: &str, id: &str) {
    let (status, found) = call(node, Method::GET, &format!("{USERS}?q={query}"), Some(root), None).await;
    assert_eq!(status, 200);
    assert_eq!(found["users"].as_array().unwrap().len(), 1);
    assert_eq!(found["users"][0]["id"], id);
}

async fn disable_and_enable(node: &Node, root: &Secret, id: &str) {
    let session = node.sign_in("robert@own.test", PASSWORD).await;
    let token = session.access_token(SPACES_RESOURCE).await.unwrap();
    assert_eq!(patch(node, root, id, json!({"enabled": false})).await.0, 200);
    invalid_password(node, "robert@own.test", PASSWORD, 50057).await;
    revoked(&session).await;
    assert_eq!(call(node, Method::GET, ME, Some(&token.token), None).await.0, 403);
    assert_eq!(patch(node, root, id, json!({"enabled": true})).await.0, 200);
    node.sign_in("robert@own.test", PASSWORD)
        .await
        .skypetoken()
        .await
        .unwrap();
    revoked(&session).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn password_reset_requires_a_change_before_native_sign_in() {
    for backend in backends() {
        eprintln!("binary admin must-change password: {backend:?}");
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        node.start().await;
        let (root, _) = first_admin(&node).await;
        let id = add(&node, &root, BOB).await;
        let session = node.sign_in(BOB, PASSWORD).await;
        let reset = json!({"password": "temporary password", "must_change_password": true});
        assert_eq!(
            call(
                &node,
                Method::POST,
                &format!("{}/password", path(&id)),
                Some(&root),
                Some(reset)
            )
            .await
            .0,
            204
        );
        revoked(&session).await;
        invalid_password(&node, BOB, PASSWORD, 50126).await;
        let response = node.password(BOB, "temporary password").await;
        assert_eq!(response.status(), 400);
        let answer: Value = response.json().await.unwrap();
        assert_eq!(answer["error"], "invalid_grant");
        assert_eq!(answer["error_codes"][0], 50055);
        assert!(answer["error_description"].as_str().unwrap().contains("AADSTS50055"));
        assert_eq!(answer["suberror"], "user_password_expired");
        assert!(answer.get("access_token").is_none());
        let change =
            json!({"upn": BOB, "current_password": "temporary password", "new_password": "permanent password"});
        assert_eq!(call(&node, Method::POST, CHANGE, None, Some(change)).await.0, 204);
        let fresh = node.sign_in(BOB, "permanent password").await;
        fresh.skypetoken().await.unwrap();
        let token = fresh.access_token(SPACES_RESOURCE).await.unwrap();
        let (status, me) = call(&node, Method::GET, ME, Some(&token.token), None).await;
        assert_eq!(status, 200);
        assert_eq!(me["must_change_password"], false);
        invalid_password(&node, BOB, "temporary password", 50126).await;
        revoked(&session).await;
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_protected_admin_route_checks_identity_and_server_keys() {
    for backend in backends() {
        eprintln!("binary admin authorization: {backend:?}");
        let deployment = Deployment::new(backend, false, None).await;
        let foreign = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        let mut other = foreign.node("a");
        node.start().await;
        other.start().await;
        let (root, root_id) = first_admin(&node).await;
        let (foreign_admin, _) = first_admin(&other).await;
        let bob_id = add(&node, &root, BOB).await;
        let bob = bearer(&node, BOB).await;
        let wrong_audience = node
            .sign_in(ROOT, PASSWORD)
            .await
            .access_token(teams_proto::IC3_AUDIENCE)
            .await
            .unwrap();
        let routes = [
            (Method::GET, USERS.to_owned(), None),
            (Method::POST, USERS.to_owned(), Some(account("eve@own.test"))),
            (Method::GET, path(&root_id), None),
            (Method::PATCH, path(&bob_id), Some(json!({"admin": true}))),
            (Method::DELETE, path(&bob_id), None),
            (
                Method::POST,
                format!("{}/password", path(&bob_id)),
                Some(json!({"password": PASSWORD})),
            ),
            (Method::GET, AUDIT.to_owned(), None),
        ];
        for (method, route, body) in routes {
            for (token, expected) in [
                (None, 401),
                (Some(&foreign_admin), 401),
                (Some(&wrong_audience.token), 401),
                (Some(&bob), 403),
            ] {
                let (status, _) = call(&node, method.clone(), &route, token, body.clone()).await;
                assert_eq!(status, expected, "{backend:?}: {method} {route}");
            }
        }
        let (_, users) = call(&node, Method::GET, USERS, Some(&root), None).await;
        assert_eq!(users["users"].as_array().unwrap().len(), 2);
        assert_eq!(users["users"][0]["admin"], false);
        node.sign_in(BOB, PASSWORD).await.skypetoken().await.unwrap();
        let (_, audit) = call(&node, Method::GET, AUDIT, Some(&root), None).await;
        assert_eq!(audit["entries"].as_array().unwrap().len(), 2);
        other.stop().await;
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_and_api_roles_agree_and_preserve_the_last_enabled_admin() {
    for backend in backends() {
        eprintln!("binary admin CLI and last-admin guards: {backend:?}");
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        node.cli(
            &["user", "add", ROOT, "--name", "QA Root", "--admin"],
            &format!("{PASSWORD}\n"),
        );
        node.start().await;
        let root = bearer(&node, ROOT).await;
        let (_, me) = call(&node, Method::GET, ME, Some(&root), None).await;
        let root_id = me["id"].as_str().unwrap();
        last_admin_guards(&node, &root, root_id).await;
        for action in ["demote", "disable", "delete"] {
            assert!(!node.cli_output(&["user", action, ROOT], "").status.success());
        }
        assert!(!node.cli_output(&["setup-code"], "").status.success());
        let bob_id = add(&node, &root, BOB).await;
        let bob = bearer(&node, BOB).await;
        node.cli(&["user", "promote", BOB], "");
        assert_eq!(call(&node, Method::GET, USERS, Some(&bob), None).await.0, 200);
        assert_roles(&node, BOB, true).await;
        node.cli(&["user", "demote", ROOT], "");
        assert_eq!(call(&node, Method::GET, USERS, Some(&root), None).await.0, 403);
        assert_roles(&node, ROOT, false).await;
        assert_eq!(patch(&node, &bob, root_id, json!({"admin": true})).await.0, 200);
        assert_eq!(patch(&node, &root, &bob_id, json!({"enabled": false})).await.0, 200);
        last_admin_guards(&node, &root, root_id).await;
        node.cli(&["user", "enable", BOB], "");
        assert_eq!(patch(&node, &root, &bob_id, json!({"admin": false})).await.0, 200);
        let listed = node.cli(&["user", "list"], "");
        assert!(
            listed
                .lines()
                .any(|line| line.starts_with(BOB) && line.ends_with("\tuser"))
        );
        node.cli(&["user", "delete", BOB], "");
        assert_eq!(call(&node, Method::GET, &path(&bob_id), Some(&root), None).await.0, 404);
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_records_every_operator_change_with_actor_and_time() {
    for backend in backends() {
        eprintln!("binary admin audit: {backend:?}");
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        node.start().await;
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (root, root_id) = first_admin(&node).await;
        let bob_id = add(&node, &root, BOB).await;
        audit_changes(&node, &root, &bob_id).await;
        last_admin_guards(&node, &root, &root_id).await;
        let (status, audit) = call(&node, Method::GET, AUDIT, Some(&root), None).await;
        assert_eq!(status, 200);
        let rows = audit["entries"].as_array().unwrap();
        let actions: Vec<_> = rows
            .iter()
            .map(|row| (row["action"].as_str().unwrap(), row["actor"].as_str().unwrap()))
            .collect();
        assert_eq!(
            actions,
            [
                ("user.delete", "cli"),
                ("user.password_change", BOB),
                ("user.password_reset", ROOT),
                ("user.update", "cli"),
                ("user.update", ROOT),
                ("user.update", ROOT),
                ("user.update", ROOT),
                ("user.update", ROOT),
                ("user.add", ROOT),
                ("setup.first_admin", "setup"),
            ]
        );
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        for row in rows {
            assert!((before..=after).contains(&row["at"].as_u64().unwrap()));
            let is_setup = row["action"] == "setup.first_admin";
            assert_eq!(
                row["target_id"],
                if is_setup { root_id.as_str() } else { bob_id.as_str() }
            );
            if row["actor"] == ROOT {
                assert_eq!(row["actor_id"], root_id);
            } else if row["actor"] == BOB {
                assert_eq!(row["actor_id"], bob_id);
            } else {
                assert_eq!(row["actor_id"], Value::Null);
            }
        }
        let text = audit.to_string();
        for secret in [
            PASSWORD,
            "temporary password",
            "permanent password",
            "argon2",
            "password_hash",
        ] {
            assert!(!text.contains(secret), "audit must not contain credentials");
        }
        audit_pages(&node, &root, rows).await;
        node.stop().await;
    }
}

async fn audit_changes(node: &Node, root: &Secret, id: &str) {
    for body in [
        json!({"display_name": "Robert"}),
        json!({"enabled": false}),
        json!({"enabled": true}),
        json!({"admin": true}),
    ] {
        assert_eq!(patch(node, root, id, body).await.0, 200);
    }
    node.cli(&["user", "demote", BOB], "");
    let reset = json!({"password": "temporary password", "must_change_password": true});
    assert_eq!(
        call(
            node,
            Method::POST,
            &format!("{}/password", path(id)),
            Some(root),
            Some(reset)
        )
        .await
        .0,
        204
    );
    let change = json!({"upn": BOB, "current_password": "temporary password", "new_password": "permanent password"});
    assert_eq!(call(node, Method::POST, CHANGE, None, Some(change)).await.0, 204);
    node.cli(&["user", "delete", BOB], "");
}

async fn audit_pages(node: &Node, root: &Secret, rows: &[Value]) {
    let (status, page) = call(node, Method::GET, &format!("{AUDIT}?limit=3"), Some(root), None).await;
    assert_eq!(status, 200);
    assert_eq!(page["entries"].as_array().unwrap(), &rows[..3]);
    let next = page["next"].as_i64().unwrap();
    let (status, older) = call(node, Method::GET, &format!("{AUDIT}?before={next}"), Some(root), None).await;
    assert_eq!(status, 200);
    assert_eq!(older["entries"].as_array().unwrap(), &rows[3..]);
    assert_eq!(older["next"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_setup_codes_are_throttled_without_consuming_the_code() {
    for backend in backends() {
        let deployment = Deployment::new(backend, false, None).await;
        let mut node = deployment.node("a");
        node.start().await;
        let issued = node.cli(&["setup-code"], "");
        for _ in 0..5 {
            let mut wrong = account(ROOT);
            wrong["code"] = "wrong code".into();
            let (status, answer) = call(&node, Method::POST, SETUP, None, Some(wrong)).await;
            assert_eq!((status, code(&answer)), (403, "SetupCode"));
        }
        let mut right = account(ROOT);
        right["code"] = issued.lines().next().unwrap().into();
        let response = send(&node, Method::POST, SETUP, None, Some(right)).await;
        assert_eq!(response.status(), 429);
        let retry: u64 = response.headers()["retry-after"].to_str().unwrap().parse().unwrap();
        assert!((1..=300).contains(&retry));
        let (_, state) = call(&node, Method::GET, SETUP, None, None).await;
        assert_eq!(state["needs_admin"], true);
        node.stop().await;
        node.start().await;
        let mut right = account(ROOT);
        right["code"] = issued.lines().next().unwrap().into();
        assert_eq!(call(&node, Method::POST, SETUP, None, Some(right)).await.0, 201);
        node.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_nodes_share_setup_limits_and_serialize_mutual_admin_removal() {
    if std::env::var_os(PG_ENV).is_none() || redis_url().is_none() {
        eprintln!("SKIP two-node admin race: set {PG_ENV} and NGTEAMS_SERVER_REDIS_URL");
        return;
    }
    let deployment = Deployment::new(Backend::Postgres, false, redis_url()).await;
    let mut a = deployment.node("a");
    let mut b = deployment.node("b");
    tokio::join!(a.start(), b.start());
    let (root, root_id) = first_admin(&a).await;
    let bob_id = add(&b, &root, BOB).await;
    for action in ["demote", "disable", "delete"] {
        mutual_removal(&a, &b, &root, &root_id, &bob_id, action).await;
    }
    shared_setup_limit(&a, &b).await;
    b.stop().await;
    a.stop().await;
}

async fn mutual_removal(a: &Node, b: &Node, root: &Secret, root_id: &str, bob_id: &str, action: &str) {
    assert_eq!(patch(a, root, bob_id, json!({"admin": true})).await.0, 200);
    let bob = bearer(b, BOB).await;
    let (method, body) = match action {
        "demote" => (Method::PATCH, Some(json!({"admin": false}))),
        "disable" => (Method::PATCH, Some(json!({"enabled": false}))),
        _ => (Method::DELETE, None),
    };
    let bob_path = path(bob_id);
    let root_path = path(root_id);
    let (left, right) = tokio::join!(
        call(a, method.clone(), &bob_path, Some(root), body.clone()),
        call(b, method, &root_path, Some(&bob), body),
    );
    let success = if action == "delete" { 204 } else { 200 };
    let mut statuses = [left.0, right.0];
    statuses.sort_unstable();
    eprintln!("two-node mutual {action}: {statuses:?}");
    assert!(
        statuses == [success, 409] || statuses == [success, 403],
        "{action}: {statuses:?}"
    );
    let survivor = if left.0 == success { root } else { &bob };
    let survivor_id = if left.0 == success { root_id } else { bob_id };
    one_admin_on_both_nodes(a, b, survivor, survivor_id).await;
    if action != "delete" {
        let removed_id = if left.0 == success { bob_id } else { root_id };
        assert_eq!(
            patch(a, survivor, removed_id, json!({"admin": true, "enabled": true}))
                .await
                .0,
            200
        );
    }
}

async fn one_admin_on_both_nodes(a: &Node, b: &Node, survivor: &Secret, survivor_id: &str) {
    for node in [a, b] {
        let (status, users) = call(node, Method::GET, USERS, Some(survivor), None).await;
        assert_eq!(status, 200);
        let enabled_admins = users["users"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|user| user["admin"] == true && user["enabled"] == true)
            .count();
        assert_eq!(enabled_admins, 1);
        last_admin_guards(node, survivor, survivor_id).await;
    }
}

async fn shared_setup_limit(a: &Node, b: &Node) {
    for node in [a, b, a, b] {
        let (status, answer) = call(node, Method::POST, SETUP, None, Some(account(ROOT))).await;
        assert_eq!((status, code(&answer)), (409, "AdminExists"));
    }
    for node in [a, b] {
        let held = send(node, Method::POST, SETUP, None, Some(account(ROOT))).await;
        assert_eq!(held.status(), 429);
        assert!(held.headers().contains_key("retry-after"));
    }
}
