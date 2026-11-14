use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{SPACES_RESOURCE, TokenError};
use teams_proto::IC3_AUDIENCE;

use super::super::support::Node;
use crate::support::PASSWORD;

pub const ROOT: &str = "root@own.test";
pub const BOB: &str = "bob@own.test";
pub const SETUP: &str = "/api/admin/v1/setup";
pub const USERS: &str = "/api/admin/v1/users";
pub const AUDIT: &str = "/api/admin/v1/audit";
pub const ME: &str = "/api/account/v1/me";
pub const CHANGE: &str = "/api/account/v1/password";

pub fn account(upn: &str) -> Value {
    json!({"upn": upn, "display_name": "QA Account", "password": PASSWORD})
}

pub async fn send(
    node: &Node,
    method: Method,
    path: &str,
    token: Option<&Secret>,
    body: Option<Value>,
) -> reqwest::Response {
    let mut request = node.http.request(method, format!("{}{path}", node.endpoint));
    if let Some(token) = token {
        request = request.bearer_auth(token.expose());
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    request.send().await.unwrap()
}

pub async fn call(
    node: &Node,
    method: Method,
    path: &str,
    token: Option<&Secret>,
    body: Option<Value>,
) -> (u16, Value) {
    let response = send(node, method, path, token, body).await;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    let answer = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).expect("API response must be JSON")
    };
    (status, answer)
}

pub fn code(answer: &Value) -> &str {
    answer["error"]["code"].as_str().unwrap()
}

pub fn path(id: &str) -> String {
    format!("{USERS}/{id}")
}

pub async fn bearer(node: &Node, upn: &str) -> Secret {
    node.sign_in(upn, PASSWORD)
        .await
        .access_token(SPACES_RESOURCE)
        .await
        .unwrap()
        .token
}

pub async fn first_admin(node: &Node) -> (Secret, String) {
    let issued = node.cli(&["setup-code"], "");
    let mut body = account(ROOT);
    body["code"] = issued.lines().next().unwrap().into();
    let (status, user) = call(node, Method::POST, SETUP, None, Some(body)).await;
    assert_eq!(status, 201);
    (bearer(node, ROOT).await, user["id"].as_str().unwrap().to_owned())
}

pub async fn add(node: &Node, token: &Secret, upn: &str) -> String {
    let (status, user) = call(node, Method::POST, USERS, Some(token), Some(account(upn))).await;
    assert_eq!(status, 201);
    user["id"].as_str().unwrap().to_owned()
}

pub async fn patch(node: &Node, token: &Secret, id: &str, body: Value) -> (u16, Value) {
    call(node, Method::PATCH, &path(id), Some(token), Some(body)).await
}

pub async fn invalid_password(node: &Node, upn: &str, password: &str, expected: u64) {
    let response = node.password(upn, password).await;
    assert_eq!(response.status(), 400);
    let answer: Value = response.json().await.unwrap();
    assert_eq!(answer["error"], "invalid_grant");
    assert_eq!(answer["error_codes"][0], expected);
    assert!(answer.get("access_token").is_none());
}

pub async fn revoked(session: &Session<MemorySecretStore>) {
    assert!(matches!(
        session.access_token(IC3_AUDIENCE).await,
        Err(TokenError::InvalidGrant(_))
    ));
}

pub fn claims(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

pub async fn assert_roles(node: &Node, upn: &str, admin: bool) {
    let response = node.password(upn, PASSWORD).await;
    assert_eq!(response.status(), 200);
    let grant: Value = response.json().await.unwrap();
    for field in ["access_token", "id_token"] {
        let claims = claims(grant[field].as_str().unwrap());
        if admin {
            assert_eq!(claims["roles"], json!(["admin"]));
        } else {
            assert!(claims.get("roles").is_none());
        }
    }
}

pub async fn last_admin_guards(node: &Node, token: &Secret, id: &str) {
    for body in [json!({"admin": false}), json!({"enabled": false})] {
        let (status, answer) = patch(node, token, id, body).await;
        assert_eq!((status, code(&answer)), (409, "LastAdmin"));
    }
    let (status, answer) = call(node, Method::DELETE, &path(id), Some(token), None).await;
    assert_eq!((status, code(&answer)), (409, "LastAdmin"));
}
