//! An own server's apps and bots as its administrator manages them (`docs/server.md`, "Apps and bots"):
//! `/api/admin/v1/apps…` with the account's access token, and the metadata bots check the server with
//! (`/bot/v1/.well-known/openidconfiguration`, asked with no token) that a bot's settings name.
//!
//! A bot's secret is in the answer that made it and nowhere else. It is plain text, as [`super::suggested_password`]
//! is: it is made to be shown to the administrator once and copied into the bot's settings; no `Debug` shows it.

use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use serde_json::{Map, Value, json};

use super::{AdminClient, AdminError, Refusal, bearer, json_body, unexpected};
use crate::secrets::Secret;

/// The largest app package the server takes, in bytes (`docs/server.md`: at most 5 MiB).
pub const MAX_PACKAGE: usize = 5 * 1024 * 1024;

/// How a bot and the server prove themselves to each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BotAuthMode {
    /// Each side signs: the server's RS256 token to the bot, the bot's client-credentials token to the server.
    #[default]
    Signed,
    /// Neither signs (development or a private network): the bot's own service URL holds its credential.
    Unsigned,
}

impl BotAuthMode {
    /// Its name on the wire.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Signed => "signed",
            Self::Unsigned => "unsigned",
        }
    }

    fn of(value: &str) -> Option<Self> {
        match value {
            "signed" => Some(Self::Signed),
            "unsigned" => Some(Self::Unsigned),
            _ => None,
        }
    }
}

/// Where an app came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppSource {
    /// Registered from the form: a bot with a name.
    Form,
    /// Uploaded as a Teams app package.
    Package,
    /// A source this client doesn't know.
    Other,
}

/// An app's bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminBot {
    /// The bot's id: its `MicrosoftAppId`.
    pub id: String,
    /// Its MRI in chats, `28:<id>`.
    pub mri: String,
    /// Where the server sends its activities; none until one is set.
    pub endpoint: Option<String>,
    pub auth_mode: BotAuthMode,
    /// Where it can be used: `personal`, `team`, `groupChat`.
    pub scopes: Vec<String>,
}

/// An app as its administrator sees it. Never its bot's secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminApp {
    pub id: String,
    pub version: String,
    pub name: String,
    pub short_description: String,
    pub developer: String,
    /// `#RRGGBB`, when the app has one.
    pub accent_color: Option<String>,
    pub source: AppSource,
    pub enabled: bool,
    /// Every account may use it; else only [`AdminApp::users`].
    pub everyone: bool,
    /// The account ids that may use it while not [`AdminApp::everyone`].
    pub users: Vec<String>,
    pub bot: Option<AdminBot>,
    /// Seconds since the Unix epoch.
    pub created_at: i64,
    pub updated_at: i64,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn texts(value: &Value, key: &str) -> Option<Vec<String>> {
    value
        .get(key)?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

impl AdminBot {
    fn parse(value: &Value) -> Option<Self> {
        let id = text(value, "id")?;
        Some(Self {
            mri: text(value, "mri").unwrap_or_else(|| format!("28:{id}")),
            id,
            endpoint: text(value, "endpoint").filter(|endpoint| !endpoint.is_empty()),
            auth_mode: BotAuthMode::of(value.get("auth_mode")?.as_str()?)?,
            scopes: texts(value, "scopes").unwrap_or_default(),
        })
    }
}

impl AdminApp {
    fn parse(value: &Value) -> Option<Self> {
        let flag = |key: &str| value.get(key).and_then(Value::as_bool);
        let time = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or_default();
        let bot = match value.get("bot") {
            None | Some(Value::Null) => None,
            Some(bot) => Some(AdminBot::parse(bot)?),
        };
        let source = match value.get("source").and_then(Value::as_str) {
            Some("form") => AppSource::Form,
            Some("package") => AppSource::Package,
            _ => AppSource::Other,
        };
        Some(Self {
            id: text(value, "id")?,
            version: text(value, "version").unwrap_or_default(),
            name: text(value, "name")?,
            short_description: text(value, "short_description").unwrap_or_default(),
            developer: text(value, "developer").unwrap_or_default(),
            accent_color: text(value, "accent_color").filter(|color| !color.is_empty()),
            source,
            enabled: flag("enabled")?,
            everyone: flag("everyone")?,
            users: texts(value, "users").unwrap_or_default(),
            bot,
            created_at: time("created_at"),
            updated_at: time("updated_at"),
        })
    }
}

/// An app just made, with its bot's secret when it has a bot: shown once, never read again.
#[derive(Clone)]
pub struct MadeApp {
    pub app: AdminApp,
    pub secret: Option<String>,
}

impl std::fmt::Debug for MadeApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MadeApp")
            .field("app", &self.app)
            .field("secret", &self.secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl MadeApp {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            app: AdminApp::parse(value)?,
            secret: text(value, "secret"),
        })
    }
}

/// A bot registered from the form.
#[derive(Clone, Copy, Debug)]
pub struct NewBot<'a> {
    /// 1–30 characters.
    pub name: &'a str,
    pub description: &'a str,
    /// An absolute `https` URL; none leaves the bot without one until it is set.
    pub endpoint: Option<&'a str>,
    pub auth_mode: BotAuthMode,
}

/// What to change of an app: each when given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppChanges {
    pub enabled: Option<bool>,
    pub everyone: Option<bool>,
    /// The account ids that may use it while not everyone may.
    pub users: Option<Vec<String>>,
    pub endpoint: Option<String>,
    pub auth_mode: Option<BotAuthMode>,
}

impl AppChanges {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn body(&self) -> Value {
        let mut body = Map::new();
        if let Some(enabled) = self.enabled {
            body.insert("enabled".to_owned(), json!(enabled));
        }
        if let Some(everyone) = self.everyone {
            body.insert("everyone".to_owned(), json!(everyone));
        }
        if let Some(users) = &self.users {
            body.insert("users".to_owned(), json!(users));
        }
        if let Some(endpoint) = &self.endpoint {
            body.insert("endpoint".to_owned(), json!(endpoint));
        }
        if let Some(mode) = self.auth_mode {
            body.insert("auth_mode".to_owned(), json!(mode.wire()));
        }
        Value::Object(body)
    }
}

/// What a bot's settings name of the server besides its origin, as the server publishes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotMetadata {
    /// The issuer of the server's tokens to bots.
    pub issuer: String,
}

impl AdminClient {
    /// The server's origin, `https://host[:port]` with no trailing slash.
    #[must_use]
    pub fn origin(&self) -> String {
        self.origin.as_str().trim_end_matches('/').to_owned()
    }

    fn app_url(&self, id: &str, tail: &[&str]) -> Result<url::Url, AdminError> {
        let mut segments = vec!["api", "admin", "v1", "apps", id];
        segments.extend_from_slice(tail);
        self.url(&segments)
    }

    /// `GET /api/admin/v1/apps`: every app, by name.
    ///
    /// # Errors
    /// [`AdminError`]: [`super::Refusal::NotAdmin`] for an account that isn't an administrator;
    /// [`AdminError::UnexpectedResponse`] with 404 where the server serves no apps.
    pub async fn apps(&self, token: &Secret) -> Result<Vec<AdminApp>, AdminError> {
        let url = self.url(&["api", "admin", "v1", "apps"])?;
        let (status, body) = self.send(bearer(self.http.get(url), token), &[token.expose()]).await?;
        body.get("apps")
            .and_then(Value::as_array)
            .ok_or_else(|| unexpected(status))?
            .iter()
            .map(AdminApp::parse)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unexpected(status))
    }

    /// `POST /api/admin/v1/apps`: a bot from the form, with its secret.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::InvalidName`], [`super::Refusal::InvalidEndpoint`] or
    /// [`super::Refusal::UnsignedNotAllowed`].
    pub async fn register_bot(&self, token: &Secret, new: NewBot<'_>) -> Result<MadeApp, AdminError> {
        let url = self.url(&["api", "admin", "v1", "apps"])?;
        let mut body = Map::new();
        body.insert("name".to_owned(), json!(new.name));
        body.insert("description".to_owned(), json!(new.description));
        if let Some(endpoint) = new.endpoint {
            body.insert("endpoint".to_owned(), json!(endpoint));
        }
        body.insert("auth_mode".to_owned(), json!(new.auth_mode.wire()));
        let request = json_body(bearer(self.http.post(url), token), &Value::Object(body));
        let (status, body) = self.send(request, &[token.expose()]).await?;
        MadeApp::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `POST /api/admin/v1/apps/package`: the app a Teams app package (`zip`) holds, with its bot's secret when it has
    /// a bot.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::InvalidPackage`], [`super::Refusal::AppExists`],
    /// [`super::Refusal::BotExists`] or [`super::Refusal::PackageTooLarge`].
    pub async fn upload_package(&self, token: &Secret, package: Vec<u8>) -> Result<MadeApp, AdminError> {
        let url = self.url(&["api", "admin", "v1", "apps", "package"])?;
        let request = package_body(bearer(self.http.post(url), token), package);
        let (status, body) = self.send(request, &[token.expose()]).await.map_err(too_large)?;
        MadeApp::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `PUT /api/admin/v1/apps/{id}/package`: a new package of the same app and bot.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::InvalidPackage`], [`super::Refusal::BotChanged`], …
    pub async fn replace_package(&self, token: &Secret, id: &str, package: Vec<u8>) -> Result<AdminApp, AdminError> {
        let url = self.app_url(id, &["package"])?;
        let request = package_body(bearer(self.http.put(url), token), package);
        let (status, body) = self.send(request, &[token.expose()]).await.map_err(too_large)?;
        AdminApp::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `PATCH /api/admin/v1/apps/{id}`.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::InvalidEndpoint`], [`super::Refusal::UnknownUser`],
    /// [`super::Refusal::NoBot`], [`super::Refusal::NotFound`], …
    pub async fn update_app(&self, token: &Secret, id: &str, changes: &AppChanges) -> Result<AdminApp, AdminError> {
        let url = self.app_url(id, &[])?;
        let request = json_body(bearer(self.http.patch(url), token), &changes.body());
        let (status, body) = self.send(request, &[token.expose()]).await?;
        AdminApp::parse(&body).ok_or_else(|| unexpected(status))
    }

    /// `POST /api/admin/v1/apps/{id}/secret`: a new secret for the app's bot; the old one stops working at once.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::NoBot`] or [`super::Refusal::NotFound`].
    pub async fn rotate_secret(&self, token: &Secret, id: &str) -> Result<String, AdminError> {
        let url = self.app_url(id, &["secret"])?;
        let (status, body) = self.send(bearer(self.http.post(url), token), &[token.expose()]).await?;
        text(&body, "secret").ok_or_else(|| unexpected(status))
    }

    /// `DELETE /api/admin/v1/apps/{id}`.
    ///
    /// # Errors
    /// [`AdminError::Refused`] with [`super::Refusal::NotFound`].
    pub async fn delete_app(&self, token: &Secret, id: &str) -> Result<(), AdminError> {
        let url = self.app_url(id, &[])?;
        self.send(bearer(self.http.delete(url), token), &[token.expose()])
            .await?;
        Ok(())
    }

    /// `GET /bot/v1/.well-known/openidconfiguration`: what the server publishes for bots to check its tokens with,
    /// asked with no token.
    ///
    /// # Errors
    /// [`AdminError`]: an answer without an `issuer`.
    pub async fn bot_metadata(&self) -> Result<BotMetadata, AdminError> {
        let url = self.url(&["bot", "v1", ".well-known", "openidconfiguration"])?;
        let (status, body) = self.send(self.http.get(url), &[]).await?;
        let issuer = text(&body, "issuer").filter(|issuer| !issuer.is_empty());
        issuer
            .map(|issuer| BotMetadata { issuer })
            .ok_or_else(|| unexpected(status))
    }
}

/// A 413 the server's body limit gave, with no body of its own, said as the server's own refusal.
fn too_large(error: AdminError) -> AdminError {
    match error {
        AdminError::UnexpectedResponse { status: 413 } => AdminError::Refused {
            refusal: Refusal::PackageTooLarge,
            status: 413,
            message: String::new(),
        },
        other => other,
    }
}

fn package_body(request: reqwest::RequestBuilder, package: Vec<u8>) -> reqwest::RequestBuilder {
    request.header(CONTENT_TYPE, "application/zip").body(package)
}

/// Whether `error` says the server serves no apps (`apps.enabled = false`, or a server from before them).
#[must_use]
pub fn apps_not_served(error: &AdminError) -> bool {
    matches!(error, AdminError::UnexpectedResponse { status } if *status == StatusCode::NOT_FOUND.as_u16())
}
