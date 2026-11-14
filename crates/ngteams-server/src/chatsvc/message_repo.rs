use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::Value;

use super::activity::{self, Told};
use super::repo::advance;
use super::schema::{chat_members, chat_messages, chats};
use super::wire;
use crate::db::{Db, DbError, with_conn};

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = chat_messages)]
pub(super) struct Message {
    pub chat_id: String,
    pub id: i64,
    pub owner_id: String,
    pub client_id: Option<String>,
    pub version: i64,
    pub body: String,
    /// In a channel, the post the message belongs to: 0 in a draft of a new post, which is its own.
    pub root_id: Option<i64>,
}

impl Message {
    pub fn value(&self) -> Result<Value, DbError> {
        serde_json::from_str::<serde_json::Map<String, Value>>(&self.body)
            .map(Value::Object)
            .map_err(|_| super::property_repo::corrupt())
    }
}

/// The people who wrote in the reply chain of post `$root`.
macro_rules! thread_people {
    ($conn:expr, $chat:expr, $root:expr) => {
        chat_messages::table
            .filter(chat_messages::chat_id.eq($chat))
            .filter(chat_messages::root_id.eq($root))
            .select(chat_messages::owner_id)
            .distinct()
            .load($conn)
            .await?
    };
}

/// A message as posted: the people who may read it and the activity items it made, as of its transaction.
pub(super) struct Posted {
    pub row: Message,
    /// A second post of a client message is answered with the first and tells nobody.
    pub inserted: bool,
    pub readers: Vec<String>,
    pub told: Vec<Told>,
}

/// Posts `draft` for its owner, a member of its thread; `base` is the chat service's base, for an item's links.
pub(super) async fn insert(db: &Db, draft: &Message, base: &str) -> Result<Option<Posted>, DbError> {
    let at = wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: bool = diesel::select(diesel::dsl::exists(
                chat_members::table
                    .filter(chat_members::chat_id.eq(&draft.chat_id))
                    .filter(chat_members::user_id.eq(&draft.owner_id)),
            ))
            .get_result(conn)
            .await?;
            if !member {
                return Ok(None);
            }
            if let Some(id) = &draft.client_id {
                let found = chat_messages::table
                    .filter(chat_messages::chat_id.eq(&draft.chat_id))
                    .filter(chat_messages::owner_id.eq(&draft.owner_id))
                    .filter(chat_messages::client_id.eq(id))
                    .select(Message::as_select())
                    .first(conn)
                    .await
                    .optional()?;
                if let Some(row) = found {
                    return Ok(Some(Posted {
                        row,
                        inserted: false,
                        readers: Vec::new(),
                        told: Vec::new(),
                    }));
                }
            }
            let mut row = draft.clone();
            row.id = version;
            row.version = version;
            let mut body = row.value()?;
            if let Some(root) = row.root_id {
                let root = if root == 0 { version } else { root };
                row.root_id = Some(root);
                if let Some(link) = body["conversationLink"].as_str() {
                    body["conversationLink"] = format!("{link};messageid={root}").into();
                }
            }
            body["id"] = version.to_string().into();
            body["version"] = version.to_string().into();
            body["sequenceId"] = version.into();
            body["composetime"] = wire::iso(version).into();
            body["originalarrivaltime"] = wire::iso(version).into();
            row.body = body.to_string();
            diesel::insert_into(chat_messages::table)
                .values(&row)
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(&row.chat_id))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            let scene = activity::scene!(conn, &row.chat_id);
            let thread: Vec<String> = match row.root_id {
                Some(root) if root != row.id => thread_people!(conn, &row.chat_id, root),
                _ => Vec::new(),
            };
            let drafts = scene.of_post(&row, &body, &thread);
            let told = activity::put!(conn, at, base, &scene, &body, drafts);
            Ok(Some(Posted {
                readers: scene.readers(),
                row,
                inserted: true,
                told,
            }))
        })
        .await)
}

#[derive(Clone, Copy)]
pub(super) struct Window {
    pub since: i64,
    pub until: i64,
    pub before: i64,
    pub size: i64,
    pub incremental: bool,
    /// Only the messages of this post's chain.
    pub root: Option<i64>,
}

pub(super) async fn page(db: &Db, chat: &str, window: Window) -> Result<Vec<Message>, DbError> {
    with_conn!(db, |conn| {
        let mut query = chat_messages::table
            .filter(chat_messages::chat_id.eq(chat))
            .filter(chat_messages::version.le(window.until))
            .into_boxed();
        if let Some(root) = window.root {
            query = query.filter(chat_messages::root_id.eq(root));
        }
        if window.incremental {
            Ok(query
                .filter(chat_messages::version.gt(window.since))
                .filter(chat_messages::id.lt(window.before))
                .order(chat_messages::id.desc())
                .limit(window.size)
                .select(Message::as_select())
                .load(conn)
                .await?)
        } else {
            Ok(query
                .filter(chat_messages::id.lt(window.before))
                .order(chat_messages::id.desc())
                .limit(window.size)
                .select(Message::as_select())
                .load(conn)
                .await?)
        }
    })
}

pub(super) async fn latest(db: &Db, chat: &str) -> Result<Option<Message>, DbError> {
    with_conn!(db, |conn| Ok(chat_messages::table
        .filter(chat_messages::chat_id.eq(chat))
        .order(chat_messages::id.desc())
        .select(Message::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn last_updated(db: &Db, chat: &str) -> Result<Option<(i64, i64)>, DbError> {
    with_conn!(db, |conn| Ok(chat_messages::table
        .filter(chat_messages::chat_id.eq(chat))
        .order(chat_messages::version.desc())
        .select((chat_messages::id, chat_messages::version))
        .first(conn)
        .await
        .optional()?))
}

pub(super) enum Updated {
    Missing,
    Denied,
    Message(Changed),
    Unchanged(Message),
}

/// A changed message, with the people who may read it and the activity items the change made or removed.
pub(super) struct Changed {
    pub row: Message,
    pub readers: Vec<String>,
    pub told: Vec<Told>,
}

/// What a change means for the activity of the message's people.
#[derive(Clone, Copy)]
pub(super) enum Effect<'a> {
    None,
    /// The actor's reaction, by its key.
    Reacted(&'a str),
    Unreacted,
    Edited,
    Deleted,
}

/// One change of a message.
#[derive(Clone, Copy)]
pub(super) struct Change<'a> {
    pub chat: &'a str,
    pub id: i64,
    pub actor: &'a str,
    pub who: Who,
    pub effect: Effect<'a>,
    /// The chat service's base, for an item's links.
    pub base: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Who {
    Owner,
    AnyMember,
}

pub(super) async fn update<F>(db: &Db, change: Change<'_>, transform: F) -> Result<Updated, DbError>
where
    F: Fn(&mut Value, i64) -> Result<bool, DbError> + Sync,
{
    let at = wire::now();
    let Change { chat, id, actor, .. } = change;
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let member: bool = diesel::select(diesel::dsl::exists(
                chat_members::table
                    .filter(chat_members::chat_id.eq(chat))
                    .filter(chat_members::user_id.eq(actor)),
            ))
            .get_result(conn)
            .await?;
            if !member {
                return Ok(Updated::Denied);
            }
            let Some(mut row) = chat_messages::table
                .find((chat, id))
                .select(Message::as_select())
                .first(conn)
                .await
                .optional()?
            else {
                return Ok(Updated::Missing);
            };
            if change.who == Who::Owner && row.owner_id != actor {
                return Ok(Updated::Denied);
            }
            let before = row.value()?;
            let mut body = before.clone();
            if !transform(&mut body, version)? {
                return Ok(Updated::Unchanged(row));
            }
            row.version = version;
            body["version"] = version.to_string().into();
            row.body = body.to_string();
            diesel::update(chat_messages::table.find((chat, id)))
                .set((chat_messages::version.eq(version), chat_messages::body.eq(&row.body)))
                .execute(conn)
                .await?;
            diesel::update(chats::table.find(chat))
                .set(chats::version.eq(version))
                .execute(conn)
                .await?;
            let scene = activity::scene!(conn, chat);
            let told = match change.effect {
                Effect::None => Vec::new(),
                Effect::Reacted(key) => {
                    let drafts: Vec<_> = scene.of_reaction(&row, actor, key).into_iter().collect();
                    activity::put!(conn, at, change.base, &scene, &body, drafts)
                }
                Effect::Edited => {
                    let drafts = scene.of_mentions(&row, &body);
                    activity::put!(conn, at, change.base, &scene, &body, drafts)
                }
                Effect::Unreacted => activity::withdraw!(conn, at, scene.reaction_gone(&row, &body, actor)),
                Effect::Deleted => activity::withdraw!(conn, at, scene.items_of(&row, &before)),
            };
            Ok(Updated::Message(Changed {
                readers: scene.readers(),
                row,
                told,
            }))
        })
        .await)
}

pub(super) async fn around(db: &Db, chat: &str, at: i64, size: i64) -> Result<Vec<Message>, DbError> {
    with_conn!(db, |conn| {
        let mut past = chat_messages::table
            .filter(chat_messages::chat_id.eq(chat))
            .filter(chat_messages::id.le(at))
            .order(chat_messages::id.desc())
            .limit(size)
            .select(Message::as_select())
            .load::<Message>(conn)
            .await?;
        let mut future = chat_messages::table
            .filter(chat_messages::chat_id.eq(chat))
            .filter(chat_messages::id.gt(at))
            .order(chat_messages::id.asc())
            .limit(size)
            .select(Message::as_select())
            .load::<Message>(conn)
            .await?;
        let size = usize::try_from(size).unwrap_or(200);
        let initial_past = size.div_ceil(2).min(past.len());
        let future_count = (size - initial_past).min(future.len());
        let past_count = (size - future_count).min(past.len());
        past.truncate(past_count);
        future.truncate(future_count);
        past.extend(future);
        past.sort_unstable_by_key(|row| std::cmp::Reverse(row.id));
        Ok(past)
    })
}

/// Whether `post` is a post of the channel that replies can go under: there, a post itself, not deleted.
pub(super) async fn is_post(db: &Db, chat: &str, post: i64) -> Result<bool, DbError> {
    let found: Option<Message> = with_conn!(db, |conn| chat_messages::table
        .find((chat, post))
        .select(Message::as_select())
        .first(conn)
        .await
        .optional()?);
    match found {
        Some(row) if row.root_id == Some(post) => Ok(row.value()?["properties"].get("deletetime").is_none()),
        _ => Ok(false),
    }
}
