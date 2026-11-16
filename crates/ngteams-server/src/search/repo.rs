//! The search's reads, each limited to what the reader may see: the messages of the threads they sit in, within the
//! history they were given; everyone of the directory who can sign in; their own group chats; the channels they sit
//! in of the teams they are on; the files they own or were sent. Message text is matched by each backend's full-text
//! index; names are few and short, and matched here as the starts of their words.

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel_async::RunQueryDsl;

use super::terms::Terms;
use crate::chatsvc::schema::chat_members;
use crate::db::schema::users;
use crate::db::{Db, DbError, Pool, with_conn};

/// The most rows a name match reads before it checks each word.
const CANDIDATES: i64 = 1000;

/// A page of a search: how many to skip and how many to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub from: i64,
    pub size: i64,
}

/// A message found.
#[derive(Debug, QueryableByName)]
pub struct MessageHit {
    #[diesel(sql_type = Text)]
    pub chat_id: String,
    #[diesel(sql_type = BigInt)]
    pub id: i64,
    #[diesel(sql_type = Nullable<BigInt>)]
    pub root_id: Option<i64>,
    #[diesel(sql_type = Text)]
    pub text: String,
    #[diesel(sql_type = Text)]
    pub owner_id: String,
    #[diesel(sql_type = Text)]
    pub display_name: String,
    #[diesel(sql_type = Text)]
    pub upn: String,
}

#[derive(Debug, QueryableByName)]
pub struct PersonRow {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub upn: String,
    #[diesel(sql_type = Text)]
    pub display_name: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub given_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub surname: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub job_title: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub department: Option<String>,
}

#[derive(Debug, QueryableByName)]
struct ChatRow {
    #[diesel(sql_type = Text)]
    id: String,
    #[diesel(sql_type = Nullable<Text>)]
    topic: Option<String>,
}

/// A group chat found: its thread, its topic, and its other people's names.
#[derive(Debug)]
pub struct ChatHit {
    pub id: String,
    pub topic: Option<String>,
    pub members: Vec<String>,
}

/// A channel the reader sits in, with its team.
#[derive(Debug, QueryableByName)]
pub struct ChannelRow {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = Text)]
    pub team_name: String,
}

/// A team the reader is on: its General channel's thread and its name.
#[derive(Debug, QueryableByName)]
pub struct TeamRow {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub name: String,
}

#[derive(Debug, QueryableByName)]
pub struct FileRow {
    #[diesel(sql_type = Text)]
    pub owner_id: String,
    #[diesel(sql_type = Text)]
    pub owner_name: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub created_at: i64,
}

/// `sql`, its `$n` parameters as the pool's backend numbers them (`?n` on SQLite).
fn on(db: &Db, sql: &str) -> String {
    match db.pool() {
        Pool::Postgres(_) => sql.to_owned(),
        Pool::Sqlite(_) => sql.replace('$', "?"),
    }
}

/// The `LIKE` pattern of the first word: its candidates contain it.
fn containing(terms: &Terms) -> String {
    format!("%{}%", terms.first().unwrap_or_default())
}

const MESSAGES: &str = "SELECT m.chat_id, m.id, m.root_id, s.text, u.id AS owner_id, u.display_name, u.upn \
     FROM message_search s \
     JOIN chat_messages m ON m.chat_id = s.chat_id AND m.id = s.message_id \
     JOIN chat_members c ON c.chat_id = s.chat_id AND c.user_id = $1 \
     JOIN users u ON u.id = m.owner_id \
     WHERE MATCHED AND m.id >= c.history_from AND m.version <= COALESCE(c.left_at, 9223372036854775807) \
     ORDER BY m.id DESC LIMIT $3 OFFSET $4";

/// The messages `reader` may read whose text has every word, the newest first: `page.size` of them from `page.from`.
///
/// # Errors
/// The database fails.
pub async fn messages(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<MessageHit>, DbError> {
    let (matched, query) = match db.pool() {
        Pool::Postgres(_) => (
            "to_tsvector('simple', s.words) @@ to_tsquery('simple', $2)",
            terms.ts_query(),
        ),
        Pool::Sqlite(_) => (
            "s.id IN (SELECT rowid FROM message_search_fts WHERE message_search_fts MATCH $2)",
            terms.fts_match(),
        ),
    };
    let sql = on(db, &MESSAGES.replace("MATCHED", matched));
    Ok(with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(reader)
        .bind::<Text, _>(&query)
        .bind::<BigInt, _>(page.size)
        .bind::<BigInt, _>(page.from)
        .load(conn)
        .await?))
}

/// The people of `chats` who are in them now, by chat: their ids and names, in the chats' order of seats.
///
/// # Errors
/// The database fails.
pub async fn members(db: &Db, chats: &[String]) -> Result<Vec<(String, String, String)>, DbError> {
    if chats.is_empty() {
        return Ok(Vec::new());
    }
    Ok(with_conn!(db, |conn| chat_members::table
        .inner_join(users::table.on(users::id.eq(chat_members::user_id)))
        .filter(chat_members::chat_id.eq_any(chats))
        .filter(chat_members::left_at.is_null())
        .order((chat_members::chat_id, chat_members::position))
        .select((chat_members::chat_id, users::id, users::display_name))
        .load(conn)
        .await?))
}

const PEOPLE: &str = "SELECT u.id, u.upn, u.display_name, p.given_name, p.surname, p.job_title, p.department \
     FROM users u LEFT JOIN user_profiles p ON p.user_id = u.id \
     WHERE u.kind = 'user' AND u.disabled = FALSE AND u.deleted_at IS NULL \
     AND (lower(u.display_name) LIKE $1 OR lower(u.upn) LIKE $1 \
          OR lower(COALESCE(p.given_name, '')) LIKE $1 OR lower(COALESCE(p.surname, '')) LIKE $1) \
     ORDER BY u.display_name, u.id LIMIT $2";

/// The people of the directory who can sign in, each word the start of a word of their name or address.
///
/// # Errors
/// The database fails.
pub async fn people(db: &Db, terms: &Terms, page: Page) -> Result<Vec<PersonRow>, DbError> {
    let sql = on(db, PEOPLE);
    let pattern = containing(terms);
    let rows: Vec<PersonRow> = with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(&pattern)
        .bind::<BigInt, _>(CANDIDATES)
        .load(conn)
        .await?);
    Ok(paged(
        rows.into_iter().filter(|row| {
            let more = [row.given_name.as_deref(), row.surname.as_deref()];
            let names: Vec<&str> = [row.display_name.as_str(), row.upn.as_str()]
                .into_iter()
                .chain(more.into_iter().flatten())
                .collect();
            terms.all_in(&names)
        }),
        page,
    ))
}

const CHATS: &str = "SELECT ch.id, ch.topic FROM chats ch \
     JOIN chat_members c ON c.chat_id = ch.id AND c.user_id = $1 AND c.left_at IS NULL \
     WHERE ch.id LIKE '19:%' AND ch.unique_roster = FALSE \
     AND NOT EXISTS (SELECT 1 FROM channels WHERE channels.id = ch.id) \
     AND NOT EXISTS (SELECT 1 FROM teams WHERE teams.id = ch.id) \
     ORDER BY ch.version DESC LIMIT $2";

/// `reader`'s group chats, each word the start of a word of its topic or of another of its people's names, the
/// latest first.
///
/// # Errors
/// The database fails.
pub async fn chats(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<ChatHit>, DbError> {
    let sql = on(db, CHATS);
    let rows: Vec<ChatRow> = with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(reader)
        .bind::<BigInt, _>(CANDIDATES)
        .load(conn)
        .await?);
    let ids: Vec<String> = rows.iter().map(|row| row.id.clone()).collect();
    let seats = members(db, &ids).await?;
    let hits = rows.into_iter().map(|row| {
        let members = seats
            .iter()
            .filter(|(chat, user, _)| *chat == row.id && user != reader)
            .map(|(_, _, name)| name.clone())
            .collect();
        ChatHit {
            id: row.id,
            topic: row.topic.filter(|topic| !topic.trim().is_empty()),
            members,
        }
    });
    Ok(paged(
        hits.filter(|hit| {
            let names: Vec<&str> = hit.topic.iter().chain(&hit.members).map(String::as_str).collect();
            terms.all_in(&names)
        }),
        page,
    ))
}

const CHANNELS: &str = "SELECT ch.id, ch.name, t.name AS team_name FROM channels ch \
     JOIN teams t ON t.id = ch.team_id \
     JOIN team_members tm ON tm.team_id = t.id AND tm.user_id = $1 \
     JOIN chat_members c ON c.chat_id = ch.id AND c.user_id = $1 AND c.left_at IS NULL \
     WHERE lower(ch.name) LIKE $2 ORDER BY t.name, ch.name, ch.id LIMIT $3";

/// The channels `reader` sits in of the teams they are on, each word the start of a word of the channel's name.
///
/// # Errors
/// The database fails.
pub async fn channels(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<ChannelRow>, DbError> {
    let sql = on(db, CHANNELS);
    let pattern = containing(terms);
    let rows: Vec<ChannelRow> = with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(reader)
        .bind::<Text, _>(&pattern)
        .bind::<BigInt, _>(CANDIDATES)
        .load(conn)
        .await?);
    Ok(paged(
        rows.into_iter().filter(|row| terms.all_in(&[row.name.as_str()])),
        page,
    ))
}

const TEAMS: &str = "SELECT t.id, t.name FROM teams t \
     JOIN team_members tm ON tm.team_id = t.id AND tm.user_id = $1 \
     WHERE lower(t.name) LIKE $2 ORDER BY t.name, t.id LIMIT $3";

/// The teams `reader` is on, each word the start of a word of the team's name.
///
/// # Errors
/// The database fails.
pub async fn teams(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<TeamRow>, DbError> {
    let sql = on(db, TEAMS);
    let pattern = containing(terms);
    let rows: Vec<TeamRow> = with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(reader)
        .bind::<Text, _>(&pattern)
        .bind::<BigInt, _>(CANDIDATES)
        .load(conn)
        .await?);
    Ok(paged(
        rows.into_iter().filter(|row| terms.all_in(&[row.name.as_str()])),
        page,
    ))
}

const FILES: &str = "SELECT d.owner_id, u.display_name AS owner_name, d.name, d.created_at \
     FROM drive_items d JOIN users u ON u.id = d.owner_id \
     WHERE (d.owner_id = $1 OR EXISTS (SELECT 1 FROM drive_grants g WHERE g.item_id = d.id AND g.user_id = $1)) \
     AND lower(d.name) LIKE $2 ORDER BY d.created_at DESC, d.id LIMIT $3";

/// The files `reader` owns or was sent, each word the start of a word of the file's name, the newest first.
///
/// # Errors
/// The database fails.
pub async fn files(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<FileRow>, DbError> {
    let sql = on(db, FILES);
    let pattern = containing(terms);
    let rows: Vec<FileRow> = with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(reader)
        .bind::<Text, _>(&pattern)
        .bind::<BigInt, _>(CANDIDATES)
        .load(conn)
        .await?);
    Ok(paged(
        rows.into_iter().filter(|row| terms.all_in(&[row.name.as_str()])),
        page,
    ))
}

/// `page` of `rows`.
fn paged<T>(rows: impl Iterator<Item = T>, page: Page) -> Vec<T> {
    let skip = usize::try_from(page.from).unwrap_or(0);
    let take = usize::try_from(page.size).unwrap_or(0);
    rows.skip(skip).take(take).collect()
}
