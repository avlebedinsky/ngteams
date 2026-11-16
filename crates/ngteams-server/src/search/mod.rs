//! Substrate's search under `/search/` (`docs/protocol/search.md`): the title bar's suggestions as the query is
//! typed (`search/api/v1/suggestions`) and its results once it is entered (`searchservice/api/v2/query`), of messages,
//! people, files, group chats, and teams and channels, in the shapes Teams' answers have. Each kind holds only what
//! the reader may see ([`repo`]); the user's text is words, never a query syntax ([`terms`]). Message text is
//! indexed with each message ([`index`]).

pub(crate) mod index;
mod repo;
mod terms;

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

pub use index::backfill;
use repo::{ChatHit, FileRow, MessageHit, Page, PersonRow};
pub(crate) use terms::indexed;
use terms::{Terms, escaped};

use crate::app::App;
use crate::auth::extract::Audience;
use crate::db::{Db, DbError};
use crate::people::Caller;

/// Substrate search's audience.
#[derive(Debug)]
pub(crate) struct Substrate;

impl Audience for Substrate {
    const AUDIENCE: &'static str = "https://outlook.office.com/search";
}

/// The largest ask.
const BODY_LIMIT: usize = 64 * 1024;
/// The most of one kind an ask gets.
const MAX_SIZE: i64 = 50;
/// The furthest a page starts.
const MAX_FROM: i64 = 1000;
/// The most kinds one ask holds.
const MAX_REQUESTS: usize = 16;
/// How much of a message's text a suggestion or a result shows as its preview.
const PREVIEW: usize = 200;

pub(crate) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/search/searchservice/api/v2/query", post(query))
        .route("/search/search/api/v1/suggestions", post(suggestions))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// A kind of thing found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Message,
    People,
    File,
    Chat,
    Team,
    Channel,
    /// A team and its channels: the results' `TeamsChannel`.
    TeamsChannel,
}

impl Kind {
    fn of(name: &str) -> Option<Self> {
        Some(match name {
            "Message" => Self::Message,
            "People" => Self::People,
            "File" => Self::File,
            "Chat" => Self::Chat,
            "Team" => Self::Team,
            "Channel" => Self::Channel,
            "TeamsChannel" => Self::TeamsChannel,
            _ => return None,
        })
    }
}

/// One entity request of an ask: its kind as named (`None` for a kind this server doesn't search), its words and
/// its page.
struct Request {
    named: String,
    kind: Option<Kind>,
    terms: Terms,
    page: Page,
}

/// A field of `value` by either spelling the two paths use (`entityType`, `EntityType`).
fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    let mut chars = name.chars();
    let upper: String = chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default();
    value.get(name).or_else(|| value.get(&upper)).unwrap_or(&Value::Null)
}

fn number(value: &Value, cap: i64) -> i64 {
    value.as_i64().unwrap_or(0).clamp(0, cap)
}

/// The entity requests of an ask; `None` for one with none (as the web client's empty contextual ask) or too many.
fn requests(body: &Value) -> Option<Vec<Request>> {
    let asked = field(body, "entityRequests").as_array().filter(|asked| !asked.is_empty())?;
    if asked.len() > MAX_REQUESTS {
        return None;
    }
    Some(
        asked
            .iter()
            .map(|request| {
                let named = field(request, "entityType").as_str().unwrap_or_default().to_owned();
                let query = field(request, "query");
                let text = field(query, "displayQueryString")
                    .as_str()
                    .or_else(|| field(query, "queryString").as_str())
                    .unwrap_or_default();
                Request {
                    kind: Kind::of(&named),
                    named,
                    terms: Terms::of(text),
                    page: Page {
                        from: number(field(request, "from"), MAX_FROM),
                        size: number(field(request, "size"), MAX_SIZE),
                    },
                }
            })
            .collect(),
    )
}

/// What one request found: its items as the answer shows them, and whether more are left.
struct Found {
    items: Vec<Value>,
    more: bool,
}

/// A failed read, answered `503`.
fn unavailable(error: &DbError) -> StatusCode {
    tracing::error!(%error, "search failed");
    StatusCode::SERVICE_UNAVAILABLE
}

async fn query(
    State(app): State<Arc<App>>,
    caller: Caller<Substrate>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, StatusCode> {
    let Json(body) = body.map_err(|_| StatusCode::BAD_REQUEST)?;
    let requests = requests(&body).ok_or(StatusCode::BAD_REQUEST)?;
    let mut sets = Vec::with_capacity(requests.len());
    for request in &requests {
        let found = find(&app, &caller, request, false).await.map_err(|error| unavailable(&error))?;
        let total = request.page.from + i64::try_from(found.items.len()).unwrap_or(0) + i64::from(found.more);
        let results: Vec<Value> = found.items.into_iter().map(result).collect();
        sets.push(json!({
            "EntityType": request.named,
            "ResultSets": [{"Total": total, "MoreResultsAvailable": found.more, "Results": results}],
        }));
    }
    Ok(Json(json!({"EntitySets": sets})))
}

/// An item as a result: its type, its text around the match apart from the item.
fn result(mut item: Value) -> Value {
    let summary = item
        .as_object_mut()
        .and_then(|item| item.remove("HitHighlightedSummary"))
        .unwrap_or_else(|| json!(""));
    let kind = item["EntityType"].clone();
    json!({"Type": kind, "HitHighlightedSummary": summary, "Source": item})
}

async fn suggestions(
    State(app): State<Arc<App>>,
    caller: Caller<Substrate>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, StatusCode> {
    let Json(body) = body.map_err(|_| StatusCode::BAD_REQUEST)?;
    let requests = requests(&body).ok_or(StatusCode::BAD_REQUEST)?;
    let mut groups = Vec::new();
    for request in requests.iter().filter(|request| request.kind.is_some()) {
        let found = find(&app, &caller, request, true).await.map_err(|error| unavailable(&error))?;
        let suggestions: Vec<Value> = found
            .items
            .into_iter()
            .map(|mut item| {
                if let Some(item) = item.as_object_mut() {
                    item.remove("HitHighlightedSummary");
                }
                item
            })
            .collect();
        groups.push(json!({"Type": request.named, "Suggestions": suggestions}));
    }
    Ok(Json(json!({"Groups": groups})))
}

/// What `request` finds for `caller`; `suggested` for a suggestion, whose message preview is marked up.
async fn find(app: &App, caller: &Caller<Substrate>, request: &Request, suggested: bool) -> Result<Found, DbError> {
    let Some(kind) = request.kind.filter(|_| !request.terms.is_empty() && request.page.size > 0) else {
        return Ok(Found {
            items: Vec::new(),
            more: false,
        });
    };
    // one more than asked tells whether more are left
    let page = Page {
        from: request.page.from,
        size: request.page.size + 1,
    };
    let (db, reader, terms) = (&app.db, caller.id.as_str(), &request.terms);
    let mut items = match kind {
        Kind::Message => messages(db, reader, terms, page, suggested).await?,
        Kind::People => repo::people(db, terms, page)
            .await?
            .iter()
            .map(|row| person(row, &caller.tenant))
            .collect(),
        Kind::File => repo::files(db, reader, terms, page)
            .await?
            .iter()
            .filter_map(|row| file(app, row))
            .collect(),
        Kind::Chat => repo::chats(db, reader, terms, page).await?.iter().map(chat).collect(),
        Kind::Team => teams(db, reader, terms, page).await?,
        Kind::Channel => channels(db, reader, terms, page).await?,
        Kind::TeamsChannel => {
            let mut found = teams(db, reader, terms, page).await?;
            found.extend(channels(db, reader, terms, page).await?);
            found
        }
    };
    let size = usize::try_from(request.page.size).unwrap_or(0);
    let more = items.len() > size;
    items.truncate(size);
    Ok(Found { items, more })
}

/// An MRI of an account.
fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// A time in ms as the answers give it.
fn time(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn messages(db: &Db, reader: &str, terms: &Terms, page: Page, suggested: bool) -> Result<Vec<Value>, DbError> {
    let hits = repo::messages(db, reader, terms, page).await?;
    let mut chats: Vec<String> = hits.iter().map(|hit| hit.chat_id.clone()).collect();
    chats.sort_unstable();
    chats.dedup();
    let mut others: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let seats = repo::members(db, &chats).await?;
    for (chat, user, name) in &seats {
        if user != reader {
            others.entry(chat.as_str()).or_default().push(name.as_str());
        }
    }
    Ok(hits
        .iter()
        .map(|hit| {
            let to = others.get(hit.chat_id.as_str()).map(|names| names.join("; "));
            message(hit, to.as_deref(), terms, suggested)
        })
        .collect())
}

/// A message found: its thread and id (where the client opens it), its sender, the thread's other people, and its
/// text around the match.
fn message(hit: &MessageHit, to: Option<&str>, terms: &Terms, suggested: bool) -> Value {
    let sender = json!({"EmailAddress": {"Name": hit.display_name, "Address": hit.upn}});
    let summary = terms.summary(&hit.text);
    let preview = if suggested {
        summary.clone()
    } else {
        escaped(&hit.text.chars().take(PREVIEW).collect::<String>())
    };
    let mut extensions = json!({
        "SkypeSpaces_ConversationPost_Extension_FromSkypeInternalId": mri(&hit.owner_id),
        "SkypeSpaces_ConversationPost_Extension_ThreadType": if hit.root_id.is_some() { "topic" } else { "chat" },
    });
    if let Some(root) = hit.root_id {
        extensions["SkypeSpaces_ConversationPost_Extension_ParentMessageId"] = json!(root.to_string());
    }
    json!({
        "EntityType": "Message",
        "ClientThreadId": hit.chat_id,
        "InternetMessageId": hit.id.to_string(),
        "DateTimeSent": time(hit.id),
        "DateTimeReceived": time(hit.id),
        "Sender": sender,
        "From": sender,
        "DisplayTo": to,
        "Preview": preview,
        "Extensions": extensions,
        "HitHighlightedSummary": summary,
    })
}

/// A person found, with what the person card opens by (their MRI and directory id).
fn person(row: &PersonRow, tenant: &str) -> Value {
    json!({
        "EntityType": "People",
        "Id": row.id,
        "DisplayName": row.display_name,
        "GivenName": row.given_name,
        "Surname": row.surname,
        "MRI": mri(&row.id),
        "UserPrincipalName": row.upn,
        "EmailAddresses": [row.upn],
        "JobTitle": row.job_title,
        "Department": row.department,
        "ExternalDirectoryObjectId": row.id,
        "TenantId": tenant,
        "PeopleType": "Person",
        "PeopleSubtype": "OrganizationUser",
    })
}

/// A file found: its name and type, where it opens, whose it is and when it was put there.
fn file(app: &App, row: &FileRow) -> Option<Value> {
    let url = crate::media::file_url(app, &row.owner_id, &row.name)?;
    let kind = row
        .name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase());
    let modified = time(row.created_at.saturating_mul(1000));
    Some(json!({
        "EntityType": "File",
        "FileName": row.name,
        "Title": row.name,
        "FileType": kind,
        "FileExtension": kind,
        "Path": url,
        "LinkingUrl": url,
        "WebUrl": url,
        "LastModifiedTime": modified,
        "ModifiedBy": row.owner_name,
        "SiteTitle": row.owner_name,
        "HitHighlightedSummary": escaped(&row.name),
    }))
}

/// A group chat found: its thread, its topic when it has one, and its other people's names.
fn chat(hit: &ChatHit) -> Value {
    let members: Vec<Value> = hit.members.iter().map(|name| json!({"DisplayName": name})).collect();
    json!({
        "EntityType": "Chat",
        "ThreadId": hit.id,
        "Topic": hit.topic,
        "Members": hit.members,
        "ChatMembers": members,
    })
}

async fn teams(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<Value>, DbError> {
    Ok(repo::teams(db, reader, terms, page)
        .await?
        .into_iter()
        .map(|row| json!({"EntityType": "Team", "Name": row.name, "ThreadId": row.id}))
        .collect())
}

async fn channels(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<Value>, DbError> {
    Ok(repo::channels(db, reader, terms, page)
        .await?
        .into_iter()
        .map(|row| {
            json!({
                "EntityType": "TeamsChannel",
                "Name": row.name,
                "ThreadId": row.id,
                "TeamName": row.team_name,
            })
        })
        .collect())
}
