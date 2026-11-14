//! An account's own streams (`48:notes`, `48:notifications`, `48:mentions`) are each kept as a thread only that
//! account is in, under an id no request can name: a request's `48:…` id always resolves to the caller's own stream,
//! so the membership check that guards every chat keeps one account's stream from another.

use std::borrow::Cow;

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::repo::advance;
use super::schema::{chat_members, chats};
use super::{Caller, Error};
use crate::db::{Db, DbError, with_conn};

pub(super) const NOTES: &str = "48:notes";
pub(super) const FEED: &str = "48:notifications";
pub(super) const MENTIONS: &str = "48:mentions";

const NAMES: [&str; 3] = [NOTES, FEED, MENTIONS];

/// The stream a stored thread id is, when it is one.
pub(super) fn name_of(stored: &str) -> Option<&'static str> {
    NAMES
        .into_iter()
        .find(|name| stored.strip_prefix(name).is_some_and(|rest| rest.starts_with(':')))
}

pub(super) fn stored_id(name: &str, user: &str) -> String {
    format!("{name}:{user}")
}

/// A stream is no thread: it has no thread or member read.
pub(super) fn no_thread(target: &str) -> Result<(), Error> {
    match target {
        "48:generic" => Err(Error::InvalidThread),
        target if target.starts_with("48:") => Err(Error::Missing),
        _ => Ok(()),
    }
}

/// The stored thread a request's conversation id names: a chat's own id, or the caller's stream.
pub(super) fn stored<'a>(target: &'a str, caller: &Caller) -> Result<Cow<'a, str>, Error> {
    if !target.starts_with("48:") {
        return Ok(Cow::Borrowed(target));
    }
    if target == "48:generic" {
        return Err(Error::InvalidThread);
    }
    if NAMES.contains(&target) {
        Ok(Cow::Owned(stored_id(target, &caller.id)))
    } else {
        Err(Error::Missing)
    }
}

/// [`ensure`] inside a transaction that holds the clock.
macro_rules! ensure_in {
    ($conn:expr, $id:expr, $user:expr, $version:expr) => {{
        let made = diesel::insert_into(chats::table)
            .values((
                chats::id.eq($id),
                chats::creator.eq($user),
                chats::unique_roster.eq(false),
                chats::created_at.eq($version),
                chats::version.eq($version),
            ))
            .on_conflict_do_nothing()
            .execute($conn)
            .await?;
        if made == 1 {
            diesel::insert_into(chat_members::table)
                .values((
                    chat_members::chat_id.eq($id),
                    chat_members::user_id.eq($user),
                    chat_members::position.eq(0),
                    chat_members::joined_at.eq($version),
                ))
                .execute($conn)
                .await?;
        }
    }};
}
pub(super) use ensure_in;

/// Makes `user`'s stream `name` when it has none yet.
pub(super) async fn ensure(db: &Db, name: &str, user: &str) -> Result<(), DbError> {
    let id = stored_id(name, user);
    let at = super::wire::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            let version: i64 = advance!(conn, at);
            ensure_in!(conn, &id, user, version);
            Ok(())
        })
        .await)
}
