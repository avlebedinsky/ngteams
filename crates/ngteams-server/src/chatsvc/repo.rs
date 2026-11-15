use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{chat_clock, chat_members, chats};
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};
use crate::teams::schema::channels;

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = chats)]
pub(crate) struct Chat {
    pub id: String,
    pub creator: String,
    pub topic: Option<String>,
    pub unique_roster: bool,
    pub created_at: i64,
    pub version: i64,
    /// When the thread's people last changed; 0 for a thread whose people never did.
    pub roster_version: i64,
}

impl Chat {
    pub(crate) fn roster(&self) -> i64 {
        self.roster_version.max(self.created_at)
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = chat_members)]
pub(crate) struct Member {
    pub user_id: String,
    pub joined_at: i64,
    pub history_from: i64,
    pub left_at: Option<i64>,
}

/// What a person may read of a thread they are in or were in: its messages from `from` on, and for someone who
/// left, nothing written or changed after `until`.
#[derive(Clone, Copy)]
pub(crate) struct Sight {
    pub from: i64,
    pub until: Option<i64>,
}

impl Member {
    pub(crate) fn sight(&self) -> Sight {
        Sight {
            from: self.history_from,
            until: self.left_at,
        }
    }
}

macro_rules! advance {
    ($conn:expr, $at:expr) => {
        diesel::update($crate::chatsvc::schema::chat_clock::table)
            .set(
                $crate::chatsvc::schema::chat_clock::version.eq(diesel::dsl::case_when::<
                    _,
                    _,
                    diesel::sql_types::BigInt,
                >(
                    $crate::chatsvc::schema::chat_clock::version.lt($at),
                    $at,
                )
                .otherwise($crate::chatsvc::schema::chat_clock::version + 1)),
            )
            .returning($crate::chatsvc::schema::chat_clock::version)
            .get_result($conn)
            .await?
    };
}
pub(crate) use advance;

pub(super) async fn create(db: &Db, chat: &Chat, members: &[String]) -> Result<bool, DbError> {
    let at = super::wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            let active: i64 = users::table
                .filter(users::id.eq_any(members))
                .filter(users::disabled.eq(false))
                .count()
                .get_result(conn)
                .await?;
            if usize::try_from(active).ok() != Some(members.len()) {
                return Err(DbError::Conflict);
            }
            let mut chat = chat.clone();
            chat.version = version;
            chat.created_at = version;
            chat.roster_version = version;
            let inserted = diesel::insert_into(chats::table)
                .values(&chat)
                .on_conflict_do_nothing()
                .execute(conn)
                .await?;
            if inserted == 0 {
                return Ok(false);
            }
            for (position, user) in members.iter().enumerate() {
                diesel::insert_into(chat_members::table)
                    .values((
                        chat_members::chat_id.eq(&chat.id),
                        chat_members::user_id.eq(user),
                        chat_members::position.eq(i32::try_from(position).unwrap_or(i32::MAX)),
                        chat_members::joined_at.eq(chat.created_at),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(true)
        })
        .await)
}

pub(super) async fn chat(db: &Db, id: &str) -> Result<Option<Chat>, DbError> {
    with_conn!(db, |conn| Ok(chats::table
        .find(id)
        .select(Chat::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The people in the thread now: someone who left is no longer one of them.
pub(super) async fn members(db: &Db, id: &str) -> Result<Vec<Member>, DbError> {
    with_conn!(db, |conn| Ok(chat_members::table
        .filter(chat_members::chat_id.eq(id))
        .filter(chat_members::left_at.is_null())
        .order(chat_members::position)
        .select(Member::as_select())
        .load(conn)
        .await?))
}

pub(super) async fn watermark(db: &Db) -> Result<i64, DbError> {
    with_conn!(db, |conn| Ok(chat_clock::table
        .select(chat_clock::version)
        .first(conn)
        .await?))
}

/// `user`'s place in the thread, also after they left it.
pub(super) async fn seat(db: &Db, id: &str, user: &str) -> Result<Option<Member>, DbError> {
    with_conn!(db, |conn| Ok(chat_members::table
        .find((id, user))
        .select(Member::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The version a thread has for one of its people: frozen at the moment they left it.
fn seen() -> diesel::expression::SqlLiteral<diesel::sql_types::BigInt> {
    diesel::dsl::sql("COALESCE(chat_members.left_at, chats.version)")
}

/// The account's threads that changed for it in a version window, each as the account may see it.
pub(super) async fn list(
    db: &Db,
    user: &str,
    after: i64,
    until: i64,
    limit: i64,
) -> Result<Vec<(Chat, Sight)>, DbError> {
    let rows: Vec<(Chat, i64, Option<i64>)> = with_conn!(db, |conn| chats::table
        .inner_join(chat_members::table)
        .left_join(channels::table.on(channels::id.eq(chats::id)))
        .filter(chat_members::user_id.eq(user))
        // of a team's channels only the general one is in the list; the others are read by their id
        .filter(channels::id.is_null().or(channels::id.eq(channels::team_id)))
        .filter(chats::id.not_like("48:notes:%"))
        .filter(seen().gt(after))
        .filter(seen().le(until))
        .order(seen().asc())
        .limit(limit)
        .select((Chat::as_select(), chat_members::history_from, chat_members::left_at))
        .load(conn)
        .await?);
    Ok(rows
        .into_iter()
        .map(|(mut chat, from, until)| {
            if let Some(left) = until {
                chat.version = left;
                chat.roster_version = chat.roster_version.min(left);
            }
            (chat, Sight { from, until })
        })
        .collect())
}

pub(super) async fn tick(db: &Db) -> Result<i64, DbError> {
    let at = super::wire::now();
    with_conn!(db, |conn| {
        let version: i64 = advance!(conn, at);
        Ok(version)
    })
}
