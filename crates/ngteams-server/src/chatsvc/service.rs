//! What the server's other services do in the chat service: open a thread of their own (a meeting's chat), change
//! who is in it, and post or change a message as an account that is in it. Each goes through the same rows and
//! the same fan-out as a client's request, so every reader and every sync sees it as one.

use serde_json::{Map, Value, json};

use super::repo::Chat;
pub(crate) use super::repo::set_topic;
use super::{Caller, events, message_repo, repo, wire};
use crate::app::App;
use crate::db::users::User;
use crate::db::{Db, DbError};

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
    events::publish(app, &caller, &members, "ThreadUpdate", &resource).await;
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
    let (thread, author) = (post.thread, post.author);
    let Some(row) = keep(app, post).await? else {
        return Ok(None);
    };
    let members = repo::members(&app.db, thread).await?;
    events::publish(app, &caller(app, author), &members, "NewMessage", &row.value()?).await;
    Ok(Some(row.id))
}

/// Keeps it without telling anyone yet: for a message that names its own id, which [`set_property`] then tells
/// as the new message it is. Its id, or `None` when the author isn't in the thread.
pub(crate) async fn post_untold(app: &App, post: Post<'_>) -> Result<Option<i64>, DbError> {
    Ok(keep(app, post).await?.map(|row| row.id))
}

async fn keep(app: &App, post: Post<'_>) -> Result<Option<message_repo::Message>, DbError> {
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
    Ok(message_repo::insert(&app.db, &draft).await?.map(|(row, _)| row))
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
    let changed = message_repo::update(
        &app.db,
        thread,
        message,
        &author.id,
        message_repo::Who::Owner,
        |body, _| {
            body["properties"][name] = value.clone();
            Ok(true)
        },
    )
    .await?;
    let message_repo::Updated::Message(row) = changed else {
        return Ok(false);
    };
    let members = repo::members(&app.db, thread).await?;
    events::publish(app, &caller(app, author), &members, told_as, &row.value()?).await;
    Ok(true)
}
