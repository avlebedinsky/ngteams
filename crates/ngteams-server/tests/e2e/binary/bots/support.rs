use bytes::Bytes;
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::admin::{AdminClient, FirstAdmin};
use teams_auth::secrets::Secret;
use teams_auth::token::SPACES_RESOURCE;

use super::super::support::Node;
use crate::support::PASSWORD;

pub const ROOT: &str = "root@bots.own.test";
pub const APPS: &str = "/api/admin/v1/apps";
pub const AUDIT: &str = "/api/admin/v1/audit";
pub const USERS: &str = "/api/admin/v1/users";
pub const SCOPE: &str = "https://api.botframework.com";
pub const ENDPOINT: &str = "https://bots.example.test/api/messages";
pub const WORKFLOWS_BOT: &str = "358f0194-6b0e-4dd3-af35-c24fe8a9ec87";

/// Synthetic people with odd names: scripts, marks, quotes, emoji, one named as a bot is.
pub const ODD_PEOPLE: &[&str] = &[
    "Zoë Ångström",
    "李小龙 (synthetic)",
    "عمر الفاروق",
    "O'Brien \"Quote\" <b>tag</b>",
    "😀 Emoji Person",
    "x",
    "A very long synthetic display name that goes on and on for a while, and then some more",
    "Ünïcödé Tëst",
    "Echo (synthetic)",
    "Polls (synthetic)",
];

/// Synthetic bot names at the edges of what the form takes: 30 characters, scripts, emoji, marks.
pub const ODD_BOTS: &[&str] = &[
    "a",
    "Echo (synthetic)",
    "123456789012345678901234567890",
    "ØØØØØØØØØØØØØØØØØØØØØØØØØØØØØØ",
    "李小龙机器人",
    "مساعد",
    "🤖🤖🤖 robot",
    "O'Brien \"bot\" <i>x</i>",
    "Zz last",
    "  padded  ",
];

/// Prepends top-level keys to `node`'s configuration and appends tables.
pub fn configure(node: &Node, top: &str, tables: &str) {
    let text = std::fs::read_to_string(&node.config).unwrap();
    std::fs::write(&node.config, format!("{top}{text}{tables}")).unwrap();
}

/// The first administrator, made as the client's admin page makes it, and the bearer token its session holds.
pub async fn first_admin(node: &Node) -> Secret {
    let code = Secret::new(node.cli(&["setup-code"], "").lines().next().unwrap());
    let admin = if node.endpoint.starts_with("https:") {
        AdminClient::for_cloud(&teams_proto::Cloud::own(&node.endpoint).unwrap()).unwrap()
    } else {
        AdminClient::with_loopback(&node.endpoint).unwrap()
    };
    let password = Secret::new(PASSWORD);
    admin
        .create_first_admin(FirstAdmin {
            code: Some(&code),
            upn: ROOT,
            display_name: "Root (synthetic)",
            password: &password,
        })
        .await
        .unwrap();
    bearer(node, ROOT).await
}

pub async fn bearer(node: &Node, upn: &str) -> Secret {
    node.sign_in(upn, PASSWORD)
        .await
        .access_token(SPACES_RESOURCE)
        .await
        .unwrap()
        .token
}

pub async fn call(
    node: &Node,
    method: Method,
    path: &str,
    token: Option<&Secret>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = node.http.request(method, format!("{}{path}", node.endpoint));
    if let Some(token) = token {
        request = request.bearer_auth(token.expose());
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub async fn upload(node: &Node, token: &Secret, method: Method, path: &str, package: Bytes) -> (u16, Value) {
    let response = node
        .http
        .request(method, format!("{}{path}", node.endpoint))
        .bearer_auth(token.expose())
        .header("content-type", "application/zip")
        .body(package)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

pub fn app_path(id: &str) -> String {
    format!("{APPS}/{id}")
}

pub fn code(answer: &Value) -> &str {
    answer["error"]["code"].as_str().unwrap_or_default()
}

/// Registers a bot from the form; its id and secret.
pub async fn register(node: &Node, token: &Secret, name: &str) -> (String, String) {
    let (status, added) = call(
        node,
        Method::POST,
        APPS,
        Some(token),
        Some(json!({"name": name, "endpoint": ENDPOINT})),
    )
    .await;
    assert_eq!(status, 201, "{name}: {added}");
    (
        added["id"].as_str().unwrap().to_owned(),
        added["secret"].as_str().unwrap().to_owned(),
    )
}

/// Adds `count` synthetic people through the admin API, odd names first; their ids.
pub async fn seed_people(node: &Node, token: &Secret, prefix: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for index in 0..count {
        let name = ODD_PEOPLE.get(index).copied().unwrap_or("Seeded Person");
        let body =
            json!({"upn": format!("{prefix}.{index}@bots.own.test"), "display_name": name, "password": PASSWORD});
        let (status, user) = call(node, Method::POST, USERS, Some(token), Some(body)).await;
        assert_eq!(status, 201, "{user}");
        ids.push(user["id"].as_str().unwrap().to_owned());
    }
    ids
}

/// The server's tenant: the `tid` of a person's token.
pub fn tenant(token: &Secret) -> String {
    crate::bots::claims(token.expose())["tid"].as_str().unwrap().to_owned()
}

/// The form msal-node's confidential client posts for `acquireTokenByClientCredential`.
pub fn msal_form(id: &str, secret: &str) -> Vec<(&'static str, String)> {
    vec![
        ("client_id", id.to_owned()),
        ("scope", format!("{SCOPE}/.default")),
        ("grant_type", "client_credentials".to_owned()),
        ("x-client-SKU", "msal.js.node".to_owned()),
        ("x-client-VER", "3.8.1".to_owned()),
        ("x-client-OS", "linux".to_owned()),
        ("x-client-CPU", "x64".to_owned()),
        ("x-ms-lib-capability", "retry-after, h429".to_owned()),
        ("client-request-id", uuid::Uuid::new_v4().to_string()),
        ("client_secret", secret.to_owned()),
    ]
}

/// What a token request got.
#[derive(Debug)]
pub struct Grant {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub answer: Value,
}

impl Grant {
    /// Entra's error and its first `AADSTS` number.
    pub fn error(&self) -> (&str, u64) {
        (
            self.answer["error"].as_str().unwrap_or_default(),
            self.answer["error_codes"][0].as_u64().unwrap_or_default(),
        )
    }

    pub fn claims(&self) -> Value {
        assert_eq!(self.status, 200, "{}", self.error_text());
        crate::bots::claims(self.answer["access_token"].as_str().unwrap())
    }

    /// The answer without its token, for a failure message.
    pub fn error_text(&self) -> String {
        format!("{} {:?} {:?}", self.status, self.error(), self.retry_after)
    }
}

/// Posts a client-credentials `form` to `path` at `node`, with HTTP Basic when `basic` is set.
pub async fn grant(node: &Node, path: &str, form: &[(&str, String)], basic: Option<(&str, &str)>) -> Grant {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(form.iter().map(|(name, value)| (*name, value.as_str())))
        .finish();
    let mut request = node
        .http
        .post(format!("{}{path}", node.endpoint))
        .header("content-type", "application/x-www-form-urlencoded;charset=utf-8")
        .body(body);
    if let Some((user, password)) = basic {
        request = request.basic_auth(user, Some(password));
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().unwrap().parse().unwrap());
    Grant {
        status,
        retry_after,
        answer: response.json().await.unwrap_or(Value::Null),
    }
}

/// The bot's token at the tenant's MSAL token endpoint.
pub async fn bot_token(node: &Node, tenant: &str, id: &str, secret: &str) -> Grant {
    grant(
        node,
        &format!("/{tenant}/oauth2/v2.0/token"),
        &msal_form(id, secret),
        None,
    )
    .await
}

/// A JSON document a route serves, with its status.
pub async fn get(node: &Node, path: &str) -> (u16, Value) {
    call(node, Method::GET, path, None, None).await
}

/// The `kid`s the JWKS at `path` publishes, newest first.
pub async fn kids(node: &Node, path: &str) -> Vec<String> {
    let (status, jwks) = get(node, path).await;
    assert_eq!(status, 200, "{path}");
    jwks["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["kid"].as_str().unwrap().to_owned())
        .collect()
}

/// The server's signer over the deployment's database, as the nodes load it: the token an activity to `bot` carries.
pub async fn activity_token(node: &Node, bot: &str, service_url: &str) -> String {
    let config = ngteams_server::config::Config::load(Some(&node.config)).unwrap();
    let db = ngteams_server::db::Db::connect(&config.database_url, 1).await.unwrap();
    let keys = ngteams_server::apps::keys::BotKeys::load(db, &config).await.unwrap();
    keys.activity_token(bot, service_url).unwrap()
}

/// Whether `names`, as the app list gave them, are by name as an admin reads a list (the names that start with a Latin
/// letter in alphabetical order whatever their case); if not, what is wrong.
pub fn out_of_order(names: &[&str]) -> Option<String> {
    let latin: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with(|first: char| first.is_ascii_alphabetic()))
        .map(|name| name.to_lowercase())
        .collect();
    let mut sorted = latin.clone();
    sorted.sort();
    (latin != sorted).then(|| format!("the apps are not by name: {names:?}"))
}
