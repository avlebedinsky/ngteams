//! What the server's other services do in the chat service: open a thread of their own (a meeting's chat), change
//! who is in it, and post or change a message as an account that is in it. Each goes through the same rows and
//! the same fan-out as a client's request, so every reader and every sync sees it as one.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value, json};

use super::activity::Told;
use super::repo::Chat;
pub(crate) use super::repo::set_topic;
use super::schema::{chat_members, chat_messages, chat_properties, chats};
use super::{Caller, events, message_repo, property_repo, repo, streams, wire};
use crate::app::App;
use crate::db::schema::users;
use crate::db::users::User;
use crate::db::{Db, DbError, with_conn};

fn caller(app: &App, user: &User) -> Caller {
    Caller {
        id: user.id.clone(),
        tenant: app.issuer.tenant().to_owned(),
        name: user.display_name.clone(),
    }
}

/// Opens the thread `id` with `creator` alone in it.
pub(crate) async fn open_thread(db: &Db, id: &str, creator: &str, topic: Option<&str>) -> Result<(), DbError> {
    let chat = Chat {
        id: id.to_owned(),
        creator: creator.to_owned(),
        topic: topic.map(str::to_owned),
        unique_roster: false,
        created_at: 0,
        version: 0,
        roster_version: 0,
    };
    if repo::create(db, &chat, &[creator.to_owned()]).await? {
        Ok(())
    } else {
        Err(DbError::Conflict)
    }
}

/// Tells everyone in the thread that it changed, as its creation told them.
pub(crate) async fn announce(app: &App, thread: &str, as_user: &User) -> Result<(), DbError> {
    let Some(chat) = repo::chat(&app.db, thread).await? else {
        return Ok(());
    };
    let members = repo::members(&app.db, thread).await?;
    let caller = caller(app, as_user);
    let mut resource = wire::thread(app, &caller, &chat, &members);
    crate::calendar::describe_thread(app, thread, &mut resource["properties"]).await?;
    events::publish(app, &members, "ThreadUpdate", &resource).await;
    Ok(())
}

/// A message the server posts for `author`, who is in the thread.
pub(crate) struct Post<'a> {
    pub thread: &'a str,
    pub author: &'a User,
    pub kind: &'a str,
    pub content: &'a str,
    pub properties: Map<String, Value>,
    /// In a channel: the post it replies under, or `None` for a post of its own.
    pub under: Option<i64>,
}

/// Who is in the thread.
pub(crate) async fn members(db: &Db, thread: &str) -> Result<Vec<String>, DbError> {
    let members = repo::members(db, thread).await?;
    Ok(members.into_iter().map(|member| member.user_id).collect())
}

/// Posts it and tells the thread's people; its id, or `None` when the author isn't in the thread.
pub(crate) async fn post(app: &App, post: Post<'_>) -> Result<Option<i64>, DbError> {
    let Some(posted) = keep(app, post).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(Some(posted.row.id))
}

/// Keeps it without telling anyone yet: for a message that names its own id, which [`set_property`] then tells
/// as the new message it is. Its id, or `None` when the author isn't in the thread.
pub(crate) async fn post_untold(app: &App, post: Post<'_>) -> Result<Option<i64>, DbError> {
    let Some(posted) = keep(app, post).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &Value::Null, &[], &posted.told).await;
    Ok(Some(posted.row.id))
}

/// A message the server posts as an app (`28:<id>`), which is none of the thread's people; its row names the app's
/// own account (`users.kind` `app`).
pub(crate) struct AppPost<'a> {
    pub thread: &'a str,
    /// The app's account id.
    pub app: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub content: &'a str,
    pub properties: Map<String, Value>,
}

/// Posts it (a new post of its own in a channel) and tells the thread's people; its id, or `None` when the thread
/// isn't there.
pub(crate) async fn post_as_app(app: &App, post: AppPost<'_>) -> Result<Option<i64>, DbError> {
    let base = wire::base(app);
    let body = json!({
        "type": "Message", "conversationid": post.thread,
        "conversationLink": format!("{base}/v1/users/ME/conversations/{}", post.thread),
        "from": format!("{base}/v1/users/ME/contacts/28:{}", post.app),
        "fromDisplayNameInToken": post.name, "imdisplayname": post.name,
        "messagetype": post.kind, "contenttype": "Text", "content": post.content,
        "properties": post.properties,
    });
    let in_channel = super::channels::place(&app.db, post.thread).await?.is_some();
    let draft = message_repo::Message {
        chat_id: post.thread.to_owned(),
        id: 0,
        owner_id: post.app.to_owned(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id: in_channel.then_some(0),
    };
    let Some(posted) = message_repo::insert_by(&app.db, &draft, &base, message_repo::Poster::App).await? else {
        return Ok(None);
    };
    events::tell(app, "NewMessage", &posted.row.value()?, &posted.readers, &posted.told).await;
    Ok(Some(posted.row.id))
}

async fn keep(app: &App, post: Post<'_>) -> Result<Option<message_repo::Posted>, DbError> {
    let caller = caller(app, post.author);
    let base = wire::base(app);
    let body = json!({
        "type": "Message", "conversationid": post.thread,
        "conversationLink": format!("{base}/v1/users/ME/conversations/{}", post.thread),
        "from": format!("{base}/v1/users/ME/contacts/{}", caller.mri()),
        "fromDisplayNameInToken": caller.name, "imdisplayname": caller.name,
        "messagetype": post.kind, "contenttype": "Text", "content": post.content,
        "properties": post.properties,
    });
    let in_channel = super::channels::place(&app.db, post.thread).await?.is_some();
    let draft = message_repo::Message {
        chat_id: post.thread.to_owned(),
        id: 0,
        owner_id: caller.id.clone(),
        client_id: None,
        version: 0,
        body: body.to_string(),
        root_id: in_channel.then_some(post.under.unwrap_or(0)),
    };
    message_repo::insert(&app.db, &draft, &base).await
}

/// One property of a message its author posted, set.
pub(crate) struct Property<'a> {
    pub thread: &'a str,
    pub message: i64,
    pub author: &'a User,
    pub name: &'a str,
    pub value: &'a Value,
    /// What the thread's people are told it is: `MessageUpdate`, or `NewMessage` after [`post_untold`].
    pub told_as: &'a str,
}

/// Sets it and tells the thread's people. Whether the message was there.
pub(crate) async fn set_property(app: &App, set: Property<'_>) -> Result<bool, DbError> {
    let Property {
        thread,
        message,
        author,
        name,
        value,
        told_as,
    } = set;
    let base = wire::base(app);
    let change = message_repo::Change {
        chat: thread,
        id: message,
        actor: &author.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::None,
        base: &base,
    };
    let changed = message_repo::update(&app.db, change, |body, _| {
        body["properties"][name] = value.clone();
        Ok(true)
    })
    .await?;
    let message_repo::Updated::Message(changed) = changed else {
        return Ok(false);
    };
    events::tell(app, told_as, &changed.row.value()?, &changed.readers, &changed.told).await;
    Ok(true)
}

/// Something another service tells accounts of in their activity feeds.
pub(crate) struct Activity<'a> {
    pub people: &'a [String],
    /// The thread it happened in. An account that muted the thread is told only of what is `personal`.
    pub thread: Option<&'a str>,
    /// It names the account itself, as a mention of it does.
    pub personal: bool,
    /// The feed message for an account, under the id the feed gives it.
    pub item: &'a (dyn Fn(&str, i64) -> Value + Sync),
}

/// Writes one item into the feed of each of the people whose account is in use and whose settings of the thread
/// want it, and tells each on its push channel. An item is a message of the account's own stream, as a mention's
/// is: it is synced, marked read and removed the same way.
pub(crate) async fn tell_activity(app: &App, activity: Activity<'_>) -> Result<(), DbError> {
    keep_items(app, streams::FEED, activity).await
}

/// The account's activity feed and its call log, as [`stream_message`] names them.
pub(crate) const FEED: &str = streams::FEED;
pub(crate) const CALL_LOG: &str = streams::CALL_LOG;

/// Writes one entry into the call log of each of `people` whose account is in use, and tells each on its push
/// channel, as [`tell_activity`] does an activity item.
pub(crate) async fn tell_call_log(
    app: &App,
    people: &[String],
    entry: &(dyn Fn(&str, i64) -> Value + Sync),
) -> Result<(), DbError> {
    let logged = Activity {
        people,
        thread: None,
        personal: true,
        item: entry,
    };
    keep_items(app, streams::CALL_LOG, logged).await
}

/// A message of the account's own stream `stream` (its feed or its call log) under the id `at` the stream gives it,
/// from `from` (an MRI), as the chat service delivers one.
pub(crate) fn stream_message(
    app: &App,
    stream: &str,
    (at, from): (i64, &str),
    content: &str,
    properties: &Value,
) -> Value {
    let base = wire::base(app);
    let time = wire::iso(at);
    json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "sequenceId": at,
        "clientmessageid": at.to_string(), "messagetype": "Text", "contenttype": "Text", "content": content,
        "conversationid": stream, "to": stream,
        "threadtype": format!("streamof{}", stream.trim_start_matches("48:")),
        "conversationLink": format!("{base}/v1/users/ME/conversations/{stream}"),
        "from": format!("{base}/v1/users/ME/contacts/{from}"), "imdisplayname": "",
        "composetime": time, "originalarrivaltime": time,
        "properties": properties,
    })
}

async fn keep_items(app: &App, stream_name: &'static str, activity: Activity<'_>) -> Result<(), DbError> {
    let at = wire::now();
    let told: Vec<Told> = with_conn!(&app.db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let mut told = Vec::new();
            for user in activity.people {
                let muted = match activity.thread {
                    Some(thread) if !activity.personal => {
                        let own: Option<String> = chat_properties::table
                            .find((thread, user))
                            .select(chat_properties::body)
                            .first(conn)
                            .await
                            .optional()?;
                        property_repo::decode(own)?.get("alerts").and_then(Value::as_str) == Some("false")
                    }
                    _ => false,
                };
                let active: i64 = users::table
                    .filter(users::id.eq(user))
                    .filter(users::disabled.eq(false))
                    .count()
                    .get_result(conn)
                    .await?;
                if muted || active == 0 {
                    continue;
                }
                let version: i64 = repo::advance!(conn, at);
                let stream = streams::stored_id(stream_name, user);
                streams::ensure_in!(conn, &stream, user, version);
                let mut resource = (activity.item)(user, version);
                resource["version"] = version.to_string().into();
                let row = message_repo::Message {
                    chat_id: stream.clone(),
                    id: version,
                    owner_id: user.clone(),
                    client_id: None,
                    version,
                    body: resource.to_string(),
                    root_id: None,
                };
                diesel::insert_into(chat_messages::table)
                    .values(&row)
                    .execute(conn)
                    .await?;
                diesel::update(chats::table.find(&stream))
                    .set(chats::version.eq(version))
                    .execute(conn)
                    .await?;
                told.push(Told {
                    user: user.clone(),
                    kind: "NewMessage",
                    resource,
                });
            }
            Ok(told)
        })
        .await)?;
    events::tell(app, "NewMessage", &Value::Null, &[], &told).await;
    Ok(())
}
