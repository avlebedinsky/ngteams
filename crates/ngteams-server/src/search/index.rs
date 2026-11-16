//! The messages' text the search finds them by: written with each message in its own transaction ([`index!`]), and
//! for the messages from before the index at the server's start ([`backfill`]). The folded words of the names the
//! search finds: written by the search that reads a name changed since ([`fold_names`]), and for every such name at
//! the server's start.

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use diesel_async::RunQueryDsl;
use ego_tree::iter::Edge;
use scraper::{Html, Node};
use serde_json::Value;

use crate::chatsvc::schema::chat_messages;
use crate::db::{Db, DbError, with_conn};

diesel::table! {
    message_search (chat_id, message_id) {
        chat_id -> Text,
        message_id -> BigInt,
        text -> Text,
        words -> Text,
    }
}

diesel::allow_tables_to_appear_in_same_query!(message_search, chat_messages);

/// How many messages one step of the backfill indexes.
const BATCH: i64 = 500;

/// The elements whose ends part words.
const BLOCKS: [&str; 13] = [
    "br",
    "p",
    "div",
    "li",
    "ul",
    "ol",
    "blockquote",
    "tr",
    "td",
    "th",
    "h1",
    "h2",
    "h3",
];

/// The text message `body` of thread `chat` is found by: none for a thread the search doesn't cover (the activity
/// feed's streams, the notes), empty for a message with no text (an event, a deleted message, a call's record).
#[must_use]
pub fn text_of(chat: &str, body: &Value) -> Option<String> {
    if !chat.starts_with("19:") {
        return None;
    }
    if body["properties"].get("deletetime").is_some() {
        return Some(String::new());
    }
    let content = body["content"].as_str().unwrap_or_default();
    let kind = body["messagetype"].as_str().unwrap_or_default();
    let text = if kind == "Text" {
        content.to_owned()
    } else if kind.starts_with("RichText") {
        plain(content)
    } else {
        return Some(String::new());
    };
    let subject = body["properties"]["subject"].as_str().unwrap_or_default();
    Some(folded(&format!("{subject} {text}")))
}

/// The text of HTML `content` as a reader sees it: its text, a picture's `alt` (an emoji), words parted at blocks.
fn plain(content: &str) -> String {
    let html = Html::parse_fragment(content);
    let mut out = String::with_capacity(content.len());
    let block = |node: &Node| matches!(node, Node::Element(element) if BLOCKS.contains(&element.name()));
    for edge in html.tree.root().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Text(text) => out.push_str(&text.text),
                Node::Element(element) if element.name() == "img" => {
                    out.push(' ');
                    out.push_str(element.attr("alt").unwrap_or_default());
                    out.push(' ');
                }
                other if block(other) => out.push(' '),
                _ => {}
            },
            Edge::Close(node) if block(node.value()) => out.push(' '),
            Edge::Close(_) => {}
        }
    }
    out
}

/// `text` with its runs of white space as one space, and no private-use characters.
fn folded(text: &str) -> String {
    text.split_whitespace()
        .map(|word| word.replace(|c: char| ('\u{E000}'..='\u{F8FF}').contains(&c), ""))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Writes the search's text of message `$id` of `$chat`, whose body is `$body`, in the transaction `$conn` runs.
macro_rules! index {
    ($conn:expr, $chat:expr, $id:expr, $body:expr) => {
        if let Some(text) = $crate::search::index::text_of($chat, $body) {
            use $crate::search::index::message_search;
            let words = $crate::search::indexed(&text);
            diesel::insert_into(message_search::table)
                .values((
                    message_search::chat_id.eq($chat),
                    message_search::message_id.eq($id),
                    message_search::text.eq(&text),
                    message_search::words.eq(&words),
                ))
                .on_conflict((message_search::chat_id, message_search::message_id))
                .do_update()
                .set((message_search::text.eq(&text), message_search::words.eq(&words)))
                .execute($conn)
                .await?;
        }
    };
}
pub(crate) use index;

/// Writes the folded words of the names `names` (each its row's id and its text) of `kind`.
///
/// # Errors
/// The database fails.
pub async fn fold_names(db: &Db, kind: &str, names: &[(&str, &str)]) -> Result<(), DbError> {
    if names.is_empty() {
        return Ok(());
    }
    let rows: Vec<(&str, &str, String)> = names
        .iter()
        .map(|&(id, source)| (id, source, super::indexed(source)))
        .collect();
    let sql = super::repo::on(
        db,
        &format!(
            "INSERT INTO search_names (kind, id, source, words) VALUES {} \
             ON CONFLICT (kind, id) DO UPDATE SET source = excluded.source, words = excluded.words",
            placeholders(rows.len(), 4)
        ),
    );
    with_conn!(db, |conn| {
        let mut query = diesel::sql_query(&sql).into_boxed();
        for (id, source, words) in &rows {
            query = query
                .bind::<Text, _>(kind)
                .bind::<Text, _>(*id)
                .bind::<Text, _>(*source)
                .bind::<Text, _>(words.as_str());
        }
        query.execute(conn).await?;
    });
    Ok(())
}

/// The `VALUES` rows of a write of `rows` rows of `width` columns each: `($1, $2), ($3, $4)`.
fn placeholders(rows: usize, width: usize) -> String {
    (0..rows)
        .map(|row| {
            let columns: Vec<String> = (1..=width).map(|column| format!("${}", row * width + column)).collect();
            format!("({})", columns.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Indexes what the search holds no words of, a batch at a time, until none is left: the messages of the `19:`
/// threads from before the index (a message written since is indexed with it), and the names whose folded words are
/// missing or older than them. How many it indexed.
///
/// # Errors
/// The database fails; what was indexed stays.
pub async fn backfill(db: &Db) -> Result<usize, DbError> {
    let mut indexed = backfill_names(db).await?;
    let mut after = (String::new(), 0);
    loop {
        let (batch, last) = backfill_messages(db, &after).await?;
        indexed += batch;
        match last {
            Some(last) => after = last,
            None => return Ok(indexed),
        }
    }
}

/// Folds the names of each kind whose folded words are missing or older than them. How many it folded.
async fn backfill_names(db: &Db) -> Result<usize, DbError> {
    let mut folded = 0;
    for named in super::repo::NAMES {
        let mut after = String::new();
        loop {
            let stale = super::repo::stale(db, named, &after, BATCH).await?;
            let names: Vec<(&str, &str)> = stale
                .iter()
                .map(|name| (name.id.as_str(), name.source.as_str()))
                .collect();
            fold_names(db, named.kind, &names).await?;
            folded += names.len();
            match stale.last() {
                Some(last) if i64::try_from(names.len()).unwrap_or(i64::MAX) == BATCH => after.clone_from(&last.id),
                _ => break,
            }
        }
    }
    Ok(folded)
}

/// Indexes one batch of the messages the index doesn't hold, of those after `after` (a thread and a message id in
/// it). How many it indexed, and the last message it read while more may follow.
async fn backfill_messages(db: &Db, after: &(String, i64)) -> Result<(usize, Option<(String, i64)>), DbError> {
    let (after_chat, after_id) = after;
    let batch: Vec<(String, i64, String)> = with_conn!(db, |conn| chat_messages::table
        .filter(chat_messages::chat_id.like("19:%"))
        .filter(
            chat_messages::chat_id.gt(after_chat).or(chat_messages::chat_id
                .eq(after_chat)
                .and(chat_messages::id.gt(after_id))),
        )
        .filter(diesel::dsl::not(diesel::dsl::exists(
            message_search::table
                .filter(message_search::chat_id.eq(chat_messages::chat_id))
                .filter(message_search::message_id.eq(chat_messages::id)),
        )))
        .select((chat_messages::chat_id, chat_messages::id, chat_messages::body))
        .order((chat_messages::chat_id, chat_messages::id))
        .limit(BATCH)
        .load(conn)
        .await?);
    let rows: Vec<(String, i64, String, String)> = batch
        .iter()
        .map(|(chat, id, body)| {
            let body = serde_json::from_str(body).unwrap_or(Value::Null);
            let text = text_of(chat, &body).unwrap_or_default();
            let words = super::indexed(&text);
            (chat.clone(), *id, text, words)
        })
        .collect();
    if !rows.is_empty() {
        let sql = super::repo::on(
            db,
            &format!(
                "INSERT INTO message_search (chat_id, message_id, text, words) VALUES {} ON CONFLICT DO NOTHING",
                placeholders(rows.len(), 4)
            ),
        );
        with_conn!(db, |conn| {
            let mut query = diesel::sql_query(&sql).into_boxed();
            for (chat, id, text, words) in &rows {
                query = query
                    .bind::<Text, _>(chat.as_str())
                    .bind::<BigInt, _>(*id)
                    .bind::<Text, _>(text.as_str())
                    .bind::<Text, _>(words.as_str());
            }
            query.execute(conn).await?;
        });
    }
    let last = batch
        .last()
        .filter(|_| i64::try_from(batch.len()).unwrap_or(i64::MAX) == BATCH)
        .map(|(chat, id, _)| (chat.clone(), *id));
    Ok((rows.len(), last))
}

#[cfg(test)]
mod tests {
    use diesel_async::SimpleAsyncConnection as _;
    use serde_json::json;

    use super::{backfill, text_of};
    use crate::db::{Db, DbError, with_conn};
    use crate::search::repo::{self, Page};
    use crate::search::terms::Terms;

    const BEFORE_THE_INDEX: &str = "\
        INSERT INTO users (id, upn, display_name, password_hash, password_version, disabled, created_at, updated_at, \
            is_admin, must_change_password, kind) VALUES ('una', 'una@own.test', 'Úna Ørsted', '', 0, FALSE, 0, 0, FALSE, \
            FALSE, 'user');
        INSERT INTO chats (id, creator, unique_roster, created_at, version, roster_version) VALUES \
            ('19:old@thread.v2', 'una', FALSE, 0, 3, 0), ('48:notes', 'una', FALSE, 0, 3, 0);
        INSERT INTO chat_members (chat_id, user_id, position, joined_at, history_from) VALUES \
            ('19:old@thread.v2', 'una', 0, 0, 0);
        INSERT INTO chat_messages (chat_id, id, owner_id, version, body) VALUES \
            ('19:old@thread.v2', 1, 'una', 1, '{\"messagetype\":\"Text\",\"content\":\"the old lantern, crème brûlée\"}'), \
            ('19:old@thread.v2', 2, 'una', 2, '{\"messagetype\":\"ThreadActivity/AddMember\"}'), \
            ('48:notes', 3, 'una', 3, '{\"messagetype\":\"Text\",\"content\":\"lantern note\"}');";

    #[tokio::test]
    async fn the_backfill_indexes_the_messages_from_before_the_index_once() -> Result<(), DbError> {
        let dir = tempfile::tempdir().map_err(|error| DbError::Pool(error.to_string()))?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        with_conn!(db, |conn| conn.batch_execute(BEFORE_THE_INDEX).await?);
        let page = Page { from: 0, size: 5 };
        assert!(
            repo::messages(&db, "una", &Terms::of("lantern"), page)
                .await?
                .is_empty()
        );
        assert_eq!(backfill(&db).await?, 4, "two messages, Úna and the Workflows bot");
        assert_eq!(backfill(&db).await?, 0);
        let found = repo::messages(&db, "una", &Terms::of("lant creme"), page).await?;
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].id, found[0].text.as_str()),
            (1, "the old lantern, crème brûlée")
        );
        let found = repo::people(&db, &Terms::of("orsted un"), page).await?;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].display_name, "Úna Ørsted");
        Ok(())
    }

    #[tokio::test]
    async fn a_name_changed_behind_the_search_is_found_and_folded_again() -> Result<(), DbError> {
        let dir = tempfile::tempdir().map_err(|error| DbError::Pool(error.to_string()))?;
        let db = Db::connect(&format!("sqlite://{}", dir.path().join("server.sqlite").display()), 1).await?;
        with_conn!(db, |conn| conn.batch_execute(BEFORE_THE_INDEX).await?);
        assert_eq!(backfill(&db).await?, 4, "two messages, Úna and the Workflows bot");
        with_conn!(db, |conn| conn
            .batch_execute("UPDATE users SET display_name = 'Åsa Lindqvist' WHERE id = 'una'")
            .await?);
        let page = Page { from: 0, size: 5 };
        assert!(repo::people(&db, &Terms::of("orsted"), page).await?.is_empty());
        let found = repo::people(&db, &Terms::of("asa"), page).await?;
        assert_eq!(found.len(), 1);
        assert!(!found[0].folded.stale());
        assert!(repo::stale(&db, repo::PERSON, "", 10).await?.is_empty());
        assert_eq!(backfill(&db).await?, 0);
        Ok(())
    }

    #[test]
    fn a_message_is_found_by_the_text_its_reader_sees() {
        let rich = json!({
            "messagetype": "RichText/Html",
            "content": "<p>Hello <span itemtype=\"http://schema.skype.com/Mention\">Ada</span>,</p><p>ro<b>cket</b> \
                        <img alt=\"🚀\" itemtype=\"http://schema.skype.com/Emoji\"></p><div>next</div>",
            "properties": {"subject": "Launch"},
        });
        assert_eq!(
            text_of("19:a@thread.v2", &rich).as_deref(),
            Some("Launch Hello Ada, rocket 🚀 next")
        );
        let plain = json!({"messagetype": "Text", "content": "a <b> stays  \u{E000}text"});
        assert_eq!(text_of("19:x", &plain).as_deref(), Some("a <b> stays text"));
        let deleted = json!({"messagetype": "Text", "content": "", "properties": {"deletetime": 1}});
        assert_eq!(text_of("19:x", &deleted).as_deref(), Some(""));
        let event = json!({"messagetype": "ThreadActivity/AddMember", "content": "<addmember/>"});
        assert_eq!(text_of("19:x", &event).as_deref(), Some(""));
        assert_eq!(text_of("48:notifications", &plain), None);
    }
}
