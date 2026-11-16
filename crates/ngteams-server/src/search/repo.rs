//! The search's reads, each limited to what the reader may see: the messages of the threads they sit in, within the
//! history they were given; everyone of the directory who can sign in; their own group chats; the channels they sit
//! in of the teams they are on; the files they own or were sent. Message text is matched by each backend's full-text
//! index; names are few and short: a name's folded words (`search_names`) pick its candidates, and the name itself is
//! matched here as the starts of its words. A name whose folded words are missing or older than it is a candidate of
//! every search, and is folded again by the search that reads it ([`super::index::fold_names`]).

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Bool, Nullable, Text};
use diesel_async::RunQueryDsl;

use super::index::fold_names;
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
    /// Whether the reader left the thread: its people now are not theirs to see.
    #[diesel(sql_type = Bool)]
    pub gone: bool,
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
    #[diesel(embed)]
    pub folded: Folded,
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
    #[diesel(embed)]
    pub folded: Folded,
}

/// A team the reader is on: its General channel's thread and its name.
#[derive(Debug, QueryableByName)]
pub struct TeamRow {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(embed)]
    pub folded: Folded,
}

#[derive(Debug, QueryableByName)]
pub struct FileRow {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub owner_id: String,
    #[diesel(sql_type = Text)]
    pub owner_name: String,
    #[diesel(sql_type = Text)]
    pub name: String,
    #[diesel(sql_type = BigInt)]
    pub created_at: i64,
    #[diesel(embed)]
    pub folded: Folded,
}

/// A found name's text its folded words come from now, and the text they were folded from (none: never folded).
#[derive(Debug, QueryableByName)]
pub struct Folded {
    #[diesel(sql_type = Text)]
    pub source: String,
    #[diesel(sql_type = Nullable<Text>)]
    pub folded_from: Option<String>,
}

impl Folded {
    pub fn stale(&self) -> bool {
        self.folded_from.as_deref() != Some(self.source.as_str())
    }
}

/// A name of a kind the search finds: its row's id and the name's whole text, as each kind's reads write them.
#[derive(Debug, QueryableByName)]
pub struct Name {
    #[diesel(sql_type = Text)]
    pub id: String,
    #[diesel(sql_type = Text)]
    pub source: String,
}

/// A kind of name the search finds: its `kind` in `search_names`, the table its rows are in (`from`), and the SQL of
/// a row's id and of the text its words are folded from.
#[derive(Clone, Copy, Debug)]
pub struct Named {
    pub kind: &'static str,
    from: &'static str,
    id: &'static str,
    source: &'static str,
}

pub const PERSON: Named = Named {
    kind: "person",
    from: "users u LEFT JOIN user_profiles p ON p.user_id = u.id",
    id: "u.id",
    source: "u.display_name || ' ' || u.upn || ' ' || COALESCE(p.given_name, '') || ' ' || COALESCE(p.surname, '')",
};
pub const TEAM: Named = Named {
    kind: "team",
    from: "teams t",
    id: "t.id",
    source: "t.name",
};
pub const CHANNEL: Named = Named {
    kind: "channel",
    from: "channels ch",
    id: "ch.id",
    source: "ch.name",
};
pub const FILE: Named = Named {
    kind: "file",
    from: "drive_items d",
    id: "d.id",
    source: "d.name",
};
pub const NAMES: [Named; 4] = [PERSON, TEAM, CHANNEL, FILE];

impl Named {
    /// `sql` with `NAMED` as the join of the row's folded words (`n`), `STALE` as the test that they are missing or
    /// older than the name, and `FOLDED` as the columns of a [`Folded`].
    fn sql(&self, db: &Db, sql: &str) -> String {
        let named = format!(
            "LEFT JOIN search_names n ON n.kind = '{}' AND n.id = {}",
            self.kind, self.id
        );
        on(
            db,
            &sql.replace("NAMED", &named)
                .replace("STALE", "(n.source IS NULL OR n.source <> (SOURCE))")
                .replace("FOLDED", "SOURCE AS source, n.source AS folded_from")
                .replace("SOURCE", self.source),
        )
    }
}

/// Up to `limit` names of `named` after the row `after` (by id) whose folded words are missing or older than the
/// name, by id.
///
/// # Errors
/// The database fails.
pub async fn stale(db: &Db, named: Named, after: &str, limit: i64) -> Result<Vec<Name>, DbError> {
    let sql = format!(
        "SELECT {id} AS id, SOURCE AS source FROM {} NAMED WHERE {id} > $1 AND STALE ORDER BY {id} LIMIT $2",
        named.from,
        id = named.id
    );
    let sql = named.sql(db, &sql);
    Ok(with_conn!(db, |conn| diesel::sql_query(&sql)
        .bind::<Text, _>(after)
        .bind::<BigInt, _>(limit)
        .load(conn)
        .await?))
}

/// Folds again the names of `rows` of `named` whose folded words are missing or older than them.
async fn refold<'a>(db: &Db, named: Named, rows: impl Iterator<Item = (&'a str, &'a Folded)>) -> Result<(), DbError> {
    let stale: Vec<(&str, &str)> = rows
        .filter(|(_, folded)| folded.stale())
        .map(|(id, folded)| (id, folded.source.as_str()))
        .collect();
    fold_names(db, named.kind, &stale).await
}

/// `sql`, its `$n` parameters as the pool's backend numbers them (`?n` on SQLite).
pub fn on(db: &Db, sql: &str) -> String {
    match db.pool() {
        Pool::Postgres(_) => sql.to_owned(),
        Pool::Sqlite(_) => sql.replace('$', "?"),
    }
}

/// Loads the rows of name search `$sql` of `$named` for `$terms`: its binds `$bind` (each a type and a value), the most
/// candidates, then a `LIKE` pattern per word, all of which a candidate's folded words hold (`WORDS`).
macro_rules! candidates {
    ($db:expr, $named:expr, $sql:expr, $terms:expr, [$(($ty:ty, $bind:expr)),*]) => {{
        let patterns = $terms.patterns();
        let first = <[&str]>::len(&[$(stringify!($bind)),*]) + 2;
        let sql = $named.sql($db, &$sql.replace("WORDS", &words(first, patterns.len())));
        with_conn!($db, |conn| {
            let mut query = diesel::sql_query(&sql)
                .into_boxed()
                $(.bind::<$ty, _>($bind))*
                .bind::<BigInt, _>(CANDIDATES);
            for pattern in &patterns {
                query = query.bind::<Text, _>(pattern.as_str());
            }
            query.load(conn).await?
        })
    }};
}

/// The test that a row's folded words (`n.words`) hold each of `count` patterns, bound from `$first`.
fn words(first: usize, count: usize) -> String {
    if count == 0 {
        return "1 = 1".to_owned();
    }
    (first..first + count)
        .map(|at| format!("n.words LIKE ${at}"))
        .collect::<Vec<_>>()
        .join(" AND ")
}

const MESSAGES: &str = "SELECT m.chat_id, m.id, m.root_id, s.text, u.id AS owner_id, u.display_name, u.upn, \
     c.left_at IS NOT NULL AS gone \
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

const PEOPLE: &str = "SELECT u.id, u.upn, u.display_name, p.given_name, p.surname, p.job_title, p.department, \
     FOLDED FROM users u LEFT JOIN user_profiles p ON p.user_id = u.id NAMED \
     WHERE u.kind = 'user' AND u.disabled = FALSE AND u.deleted_at IS NULL AND (WORDS OR STALE) \
     ORDER BY u.display_name, u.id LIMIT $1";

/// The people of the directory who can sign in, each word the start of a word of their name or address.
///
/// # Errors
/// The database fails.
pub async fn people(db: &Db, terms: &Terms, page: Page) -> Result<Vec<PersonRow>, DbError> {
    let rows: Vec<PersonRow> = candidates!(db, PERSON, PEOPLE, terms, []);
    refold(db, PERSON, rows.iter().map(|row| (row.id.as_str(), &row.folded))).await?;
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

const CHANNELS: &str = "SELECT ch.id, ch.name, t.name AS team_name, FOLDED FROM channels ch \
     JOIN teams t ON t.id = ch.team_id \
     JOIN team_members tm ON tm.team_id = t.id AND tm.user_id = $1 \
     JOIN chat_members c ON c.chat_id = ch.id AND c.user_id = $1 AND c.left_at IS NULL NAMED \
     WHERE (WORDS OR STALE) ORDER BY t.name, ch.name, ch.id LIMIT $2";

/// The channels `reader` sits in of the teams they are on, each word the start of a word of the channel's name.
///
/// # Errors
/// The database fails.
pub async fn channels(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<ChannelRow>, DbError> {
    let rows: Vec<ChannelRow> = candidates!(db, CHANNEL, CHANNELS, terms, [(Text, reader)]);
    refold(db, CHANNEL, rows.iter().map(|row| (row.id.as_str(), &row.folded))).await?;
    Ok(paged(
        rows.into_iter().filter(|row| terms.all_in(&[row.name.as_str()])),
        page,
    ))
}

const TEAMS: &str = "SELECT t.id, t.name, FOLDED FROM teams t \
     JOIN team_members tm ON tm.team_id = t.id AND tm.user_id = $1 NAMED \
     WHERE (WORDS OR STALE) ORDER BY t.name, t.id LIMIT $2";

/// The teams `reader` is on, each word the start of a word of the team's name.
///
/// # Errors
/// The database fails.
pub async fn teams(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<TeamRow>, DbError> {
    let rows: Vec<TeamRow> = candidates!(db, TEAM, TEAMS, terms, [(Text, reader)]);
    refold(db, TEAM, rows.iter().map(|row| (row.id.as_str(), &row.folded))).await?;
    Ok(paged(
        rows.into_iter().filter(|row| terms.all_in(&[row.name.as_str()])),
        page,
    ))
}

const FILES: &str = "SELECT d.id, d.owner_id, u.display_name AS owner_name, d.name, d.created_at, FOLDED \
     FROM drive_items d JOIN users u ON u.id = d.owner_id NAMED \
     WHERE (d.owner_id = $1 OR EXISTS (SELECT 1 FROM drive_grants g WHERE g.item_id = d.id AND g.user_id = $1)) \
     AND (WORDS OR STALE) ORDER BY d.created_at DESC, d.id LIMIT $2";

/// The files `reader` owns or was sent, each word the start of a word of the file's name, the newest first.
///
/// # Errors
/// The database fails.
pub async fn files(db: &Db, reader: &str, terms: &Terms, page: Page) -> Result<Vec<FileRow>, DbError> {
    let rows: Vec<FileRow> = candidates!(db, FILE, FILES, terms, [(Text, reader)]);
    refold(db, FILE, rows.iter().map(|row| (row.id.as_str(), &row.folded))).await?;
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
