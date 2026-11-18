//! A team's webhooks on the middle tier (`beta/teams/<team>/outgoingWebhooks`, `docs/protocol/bots.md`
//! "Outgoing webhooks"): listed for the team's people, made, changed and removed by those who may add apps to it
//! (its owners, its members while the team's `uploadCustomApp` setting lets them, and the server's administrators).
//! The token shows once, in the answer that made it.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use base64::Engine as _;
use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Value, json};

use super::schema::outgoing_webhooks;
use crate::app::App;
use crate::auth::extract::Spaces;
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};
use crate::teams::Actor;
use crate::teams::repo::{self, Team};

/// The longest name: Teams' own limit isn't documented.
const NAME_LIMIT: usize = 100;
const DESCRIPTION_LIMIT: usize = 1024;
/// The largest JSON body: a picture's base64 and the rest.
const BODY_LIMIT: usize = 4 * 1024 * 1024;

type Me = Actor<Spaces>;

pub(super) fn routes(config: &crate::config::Config) -> Router<Arc<App>> {
    let base = format!(
        "{}/beta/teams/{{team}}/outgoingWebhooks",
        crate::services::middle_tier_prefix(config)
    );
    Router::new()
        .route(&base, get(list).post(create))
        .route(&format!("{base}/{{id}}"), put(change).delete(remove))
        .route(&format!("{base}/{{id}}/picture"), get(picture))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// A refusal, in the shape of the middle tier's.
#[derive(Debug)]
pub(super) enum Refusal {
    Code(StatusCode, &'static str),
    Database(DbError),
    Storage(String),
}

impl Refusal {
    const BAD: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
    const MISSING: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    const ARCHIVED: Self = Self::Code(StatusCode::FORBIDDEN, "OperationNotAllowedOnArchivedTeam");
    const TAKEN: Self = Self::Code(StatusCode::CONFLICT, "OutgoingWebhookNameAlreadyExists");
}

impl From<DbError> for Refusal {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Conflict => Self::TAKEN,
            error => Self::Database(error),
        }
    }
}

impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => (status, Json(json!({"errorCode": code, "message": code}))).into_response(),
            Self::Database(error) => unavailable("database", &error),
            Self::Storage(error) => unavailable("picture storage", &error),
        }
    }
}

fn unavailable(what: &str, error: &dyn std::fmt::Display) -> Response {
    tracing::error!(%error, what, "outgoing webhook failure");
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

type Answer<T = Response> = Result<T, Refusal>;

/// A registered webhook.
#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = outgoing_webhooks)]
pub(crate) struct Hook {
    pub id: String,
    pub team_id: String,
    pub name: String,
    pub name_key: String,
    pub description: String,
    pub callback_url: String,
    pub security_key: String,
    pub picture_etag: Option<String>,
    pub created_by: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Hook {
    /// The HMAC key its token names.
    pub(crate) fn key(&self) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&self.security_key)
            .unwrap_or_default()
    }
}

/// Webhook `id`, when one is registered.
pub(crate) async fn hook(db: &Db, id: &str) -> Result<Option<Hook>, DbError> {
    with_conn!(db, |conn| Ok(outgoing_webhooks::table
        .find(id)
        .select(Hook::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The caller's access to a team: whether they may change its webhooks.
struct Access {
    team: Team,
    manage: bool,
}

/// What `user` may do with `team`'s webhooks; someone on neither the team nor the server's administrators learns
/// only that it isn't there for them.
async fn access(db: &Db, user: &str, team: &str) -> Answer<Access> {
    let team = repo::team(db, team).await?.ok_or(Refusal::MISSING)?;
    let admin = crate::db::users::by_id(db, user)
        .await?
        .is_some_and(|account| account.is_admin);
    let member = repo::membership(db, &team.id, user).await?;
    let manage = match &member {
        None if !admin => return Err(Refusal::MISSING),
        None => true,
        Some(member) => admin || member.owner || members_may(&team),
    };
    Ok(Access { team, manage })
}

/// Whether the team's settings let its members add custom apps, as Teams' "Allow members to upload custom apps".
fn members_may(team: &Team) -> bool {
    serde_json::from_str::<Value>(&team.settings)
        .ok()
        .and_then(|settings| settings["userSettings"]["uploadCustomApp"].as_bool())
        .unwrap_or(true)
}

impl Access {
    fn manager(self) -> Answer<Team> {
        if !self.manage {
            return Err(Refusal::FORBIDDEN);
        }
        if self.team.archived {
            return Err(Refusal::ARCHIVED);
        }
        Ok(self.team)
    }
}

/// A webhook as the middle tier shows it; its address only to those who may change it.
fn shown(app: &App, hook: &Hook, manage: bool) -> Value {
    let mut value = json!({
        "id": hook.id, "mri": format!("28:{}", hook.id), "teamId": hook.team_id,
        "name": hook.name, "description": hook.description, "canManage": manage,
        "createdBy": format!("8:orgid:{}", hook.created_by),
        "createdTime": crate::chatsvc::iso(hook.created_at),
    });
    if manage {
        value["callbackUrl"] = hook.callback_url.clone().into();
    }
    if let Some(etag) = &hook.picture_etag {
        value["pictureUrl"] = format!(
            "{}{}/beta/teams/{}/outgoingWebhooks/{}/picture?v={etag}",
            app.config.origin(),
            crate::services::middle_tier_prefix(&app.config),
            hook.team_id,
            hook.id
        )
        .into();
    }
    value
}

async fn team_hooks(db: &Db, team: &str) -> Result<Vec<Hook>, DbError> {
    with_conn!(db, |conn| Ok(outgoing_webhooks::table
        .filter(outgoing_webhooks::team_id.eq(team))
        .order((outgoing_webhooks::name_key.asc(), outgoing_webhooks::id.asc()))
        .select(Hook::as_select())
        .load(conn)
        .await?))
}

async fn list(State(app): State<Arc<App>>, actor: Me, Path(team): Path<String>) -> Answer<Json<Value>> {
    let access = access(&app.db, &actor.id, &team).await?;
    let hooks = team_hooks(&app.db, &access.team.id).await?;
    let shown: Vec<Value> = hooks.iter().map(|hook| shown(&app, hook, access.manage)).collect();
    Ok(Json(json!({ "value": shown })))
}

/// The fields a create or a change sends, checked; `None` where it sent none.
#[derive(Default)]
struct Form {
    name: Option<String>,
    callback_url: Option<String>,
    description: Option<String>,
    picture: Picture,
}

/// What a create or a change does with the picture.
#[derive(Default)]
enum Picture {
    #[default]
    Keep,
    Remove,
    Set(Bytes),
}

fn form(app: &App, input: Result<Json<Value>, JsonRejection>) -> Answer<Form> {
    let Json(input) = input.map_err(|_| Refusal::BAD)?;
    if !input.is_object() {
        return Err(Refusal::BAD);
    }
    let text = |key: &str, max: usize, empty: bool| -> Answer<Option<String>> {
        match input.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => {
                let text = text.trim();
                let length = text.chars().count();
                if length > max || (!empty && length == 0) {
                    return Err(Refusal::BAD);
                }
                Ok(Some(text.to_owned()))
            }
            Some(_) => Err(Refusal::BAD),
        }
    };
    let name = text("name", NAME_LIMIT, false)?;
    let description = text("description", DESCRIPTION_LIMIT, true)?;
    let callback_url = text("callbackUrl", 2048, false)?
        .map(|url| address(app, &url))
        .transpose()?;
    let picture = match input.get("picture") {
        None => Picture::Keep,
        Some(Value::Null) => Picture::Remove,
        Some(Value::String(encoded)) => Picture::Set(picture_of(app, encoded)?),
        Some(_) => return Err(Refusal::BAD),
    };
    Ok(Form {
        name,
        callback_url,
        description,
        picture,
    })
}

/// A callback address the server may send to, as the address guard reads it now.
fn address(app: &App, url: &str) -> Answer<String> {
    let parsed = url::Url::parse(url).map_err(|_| Refusal::BAD)?;
    app.outbound.check(&parsed).map_err(|_| Refusal::BAD)?;
    Ok(parsed.to_string())
}

/// A PNG or JPEG picture in base64, within the configured size.
fn picture_of(app: &App, encoded: &str) -> Answer<Bytes> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Refusal::BAD)?;
    if bytes.len() > app.config.outgoing_webhooks.max_picture_bytes || picture_type(&bytes).is_none() {
        return Err(Refusal::BAD);
    }
    Ok(Bytes::from(bytes))
}

fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xff, 0xd8, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        _ => None,
    }
}

fn picture_key(id: &str) -> String {
    format!("outgoing-webhooks/{id}/picture")
}

/// A new HMAC key, as the token shows it: 32 random bytes in base64.
fn new_key() -> Answer<String> {
    let mut bytes = [0_u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| Refusal::Storage("no random bytes".to_owned()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

async fn create(
    State(app): State<Arc<App>>,
    actor: Me,
    Path(team): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<(StatusCode, Json<Value>)> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let form = form(&app, input)?;
    let (Some(name), Some(callback_url)) = (form.name, form.callback_url) else {
        return Err(Refusal::BAD);
    };
    let id = uuid::Uuid::new_v4().to_string();
    let at = crate::chatsvc::now();
    let picture = match form.picture {
        Picture::Set(bytes) => Some(bytes),
        Picture::Keep | Picture::Remove => None,
    };
    let hook = Hook {
        name_key: name.to_lowercase(),
        name,
        id,
        team_id: team.id,
        description: form.description.unwrap_or_default(),
        callback_url,
        security_key: new_key()?,
        picture_etag: picture.as_ref().map(|_| at.to_string()),
        created_by: actor.id,
        created_at: at,
        updated_at: at,
    };
    if let Some(bytes) = picture {
        store_picture(&app, &hook.id, bytes).await?;
    }
    if let Err(error) = insert(&app.db, &hook).await {
        drop_picture(&app, &hook.id).await;
        return Err(error.into());
    }
    let mut made = shown(&app, &hook, true);
    made["securityToken"] = hook.security_key.clone().into();
    Ok((StatusCode::CREATED, Json(json!({ "value": made }))))
}

/// Keeps `hook` and its account (`28:<id>`, an app's, which signs in nowhere) in one transaction.
async fn insert(db: &Db, hook: &Hook) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(users::table)
                .values((
                    users::id.eq(&hook.id),
                    users::upn.eq(format!("webhook:{}", hook.id)),
                    users::display_name.eq(&hook.name),
                    users::password_hash.eq(""),
                    users::disabled.eq(true),
                    users::created_at.eq(hook.created_at),
                    users::updated_at.eq(hook.created_at),
                    users::kind.eq(crate::db::users::APP),
                ))
                .execute(conn)
                .await?;
            diesel::insert_into(outgoing_webhooks::table)
                .values(hook)
                .execute(conn)
                .await
                .map_err(crate::db::conflict)?;
            Ok(())
        })
        .await)
}

async fn store_picture(app: &App, id: &str, bytes: Bytes) -> Answer<()> {
    app.storage
        .put(&picture_key(id), bytes)
        .await
        .map_err(|error| Refusal::Storage(error.to_string()))
}

async fn drop_picture(app: &App, id: &str) {
    if let Err(error) = app.storage.delete(&picture_key(id)).await {
        tracing::warn!(%error, "an outgoing webhook's picture wasn't dropped");
    }
}

/// The team's webhook `id`.
async fn of_team(db: &Db, team: &str, id: &str) -> Answer<Hook> {
    hook(db, id)
        .await?
        .filter(|hook| hook.team_id == team)
        .ok_or(Refusal::MISSING)
}

async fn change(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((team, id)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let mut hook = of_team(&app.db, &team.id, &id).await?;
    let form = form(&app, input)?;
    let at = crate::chatsvc::now();
    if let Some(name) = form.name {
        hook.name_key = name.to_lowercase();
        hook.name = name;
    }
    if let Some(url) = form.callback_url {
        hook.callback_url = url;
    }
    if let Some(description) = form.description {
        hook.description = description;
    }
    match form.picture {
        Picture::Set(bytes) => {
            store_picture(&app, &hook.id, bytes).await?;
            hook.picture_etag = Some(at.to_string());
        }
        Picture::Remove => {
            drop_picture(&app, &hook.id).await;
            hook.picture_etag = None;
        }
        Picture::Keep => {}
    }
    hook.updated_at = at;
    update(&app.db, &hook).await?;
    Ok(Json(json!({ "value": shown(&app, &hook, true) })))
}

async fn update(db: &Db, hook: &Hook) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::update(outgoing_webhooks::table.find(&hook.id))
                .set((
                    outgoing_webhooks::name.eq(&hook.name),
                    outgoing_webhooks::name_key.eq(&hook.name_key),
                    outgoing_webhooks::description.eq(&hook.description),
                    outgoing_webhooks::callback_url.eq(&hook.callback_url),
                    outgoing_webhooks::picture_etag.eq(&hook.picture_etag),
                    outgoing_webhooks::updated_at.eq(hook.updated_at),
                ))
                .execute(conn)
                .await
                .map_err(crate::db::conflict)?;
            diesel::update(users::table.find(&hook.id))
                .set((
                    users::display_name.eq(&hook.name),
                    users::updated_at.eq(hook.updated_at),
                ))
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

async fn remove(
    State(app): State<Arc<App>>,
    actor: Me,
    Path((team, id)): Path<(String, String)>,
) -> Answer<StatusCode> {
    let team = access(&app.db, &actor.id, &team).await?.manager()?;
    let hook = of_team(&app.db, &team.id, &id).await?;
    let at = crate::chatsvc::now();
    delete(&app.db, &hook.id, at).await?;
    if hook.picture_etag.is_some() {
        drop_picture(&app, &hook.id).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Drops webhook `id`; its account stays, deleted, for the replies it posted.
async fn delete(db: &Db, id: &str, at: i64) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::delete(outgoing_webhooks::table.find(id)).execute(conn).await?;
            diesel::update(users::table.find(id))
                .set((users::deleted_at.eq(Some(at)), users::updated_at.eq(at)))
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

async fn picture(State(app): State<Arc<App>>, actor: Me, Path((team, id)): Path<(String, String)>) -> Answer {
    let access = access(&app.db, &actor.id, &team).await?;
    let hook = of_team(&app.db, &access.team.id, &id).await?;
    if hook.picture_etag.is_none() {
        return Err(Refusal::MISSING);
    }
    let stored = app.storage.get(&picture_key(&hook.id)).await;
    let bytes = stored
        .map_err(|error| Refusal::Storage(error.to_string()))?
        .ok_or(Refusal::MISSING)?;
    let kind = picture_type(&bytes).unwrap_or("application/octet-stream");
    Ok(([(CONTENT_TYPE, kind)], bytes).into_response())
}
