use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::meeting::Meeting;
use super::schema::{meeting_options, meetings};
use crate::db::{Db, DbError, conflict, with_conn};

/// Keeps a new meeting: a conflict when another has its code.
pub(super) async fn insert(db: &Db, meeting: &Meeting) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(meetings::table)
            .values(meeting)
            .execute(conn)
            .await
            .map_err(conflict)?;
    });
    Ok(())
}

pub(super) async fn of_chat(db: &Db, chat: &str) -> Result<Option<Meeting>, DbError> {
    with_conn!(db, |conn| Ok(meetings::table
        .find(chat)
        .select(Meeting::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn of_code(db: &Db, code: &str) -> Result<Option<Meeting>, DbError> {
    with_conn!(db, |conn| Ok(meetings::table
        .filter(meetings::code.eq(code))
        .select(Meeting::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn of_chats(db: &Db, chats: &[String]) -> Result<Vec<Meeting>, DbError> {
    with_conn!(db, |conn| Ok(meetings::table
        .filter(meetings::chat_id.eq_any(chats))
        .select(Meeting::as_select())
        .load(conn)
        .await?))
}

pub(super) async fn set_subject(db: &Db, chat: &str, subject: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::update(meetings::table.find(chat))
            .set(meetings::subject.eq(subject))
            .execute(conn)
            .await?;
    });
    Ok(())
}

/// The options kept for the chat, as their JSON text.
pub(super) async fn options(db: &Db, chat: &str) -> Result<Option<String>, DbError> {
    with_conn!(db, |conn| Ok(meeting_options::table
        .find(chat)
        .select(meeting_options::body)
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn set_options(db: &Db, chat: &str, text: &str) -> Result<(), DbError> {
    with_conn!(db, |conn| {
        diesel::insert_into(meeting_options::table)
            .values((meeting_options::chat_id.eq(chat), meeting_options::body.eq(text)))
            .on_conflict(meeting_options::chat_id)
            .do_update()
            .set(meeting_options::body.eq(text))
            .execute(conn)
            .await?;
    });
    Ok(())
}
