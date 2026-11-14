use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use super::schema::{drive_grants, drive_items, media_object_readers, media_objects, media_usage};
use crate::db::{Db, DbError, conflict, with_conn};

/// Adds `$bytes` to what `$user` has stored when the sum stays within `$quota` (0: no limit; taking bytes away
/// always passes); whether it did. The row is updated in one statement, so two nodes can't both pass the limit.
macro_rules! charge {
    ($conn:expr, $user:expr, $bytes:expr, $quota:expr) => {{
        diesel::insert_into(media_usage::table)
            .values((media_usage::user_id.eq($user), media_usage::bytes.eq(0)))
            .on_conflict_do_nothing()
            .execute($conn)
            .await?;
        let limit = if $quota == 0 || $bytes <= 0 { i64::MAX } else { $quota };
        let changed = diesel::update(media_usage::table.find($user))
            .filter(media_usage::bytes.le(limit.saturating_sub($bytes.max(0))))
            .set(media_usage::bytes.eq(media_usage::bytes + $bytes))
            .execute($conn)
            .await?;
        changed == 1
    }};
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = media_objects)]
pub(super) struct Object {
    pub id: String,
    pub owner_id: String,
    pub kind: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size: i64,
    pub created_at: i64,
}

#[derive(Clone, Queryable, Selectable, Insertable)]
#[diesel(table_name = drive_items)]
pub(super) struct Item {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub size: i64,
    pub created_at: i64,
}

pub(super) async fn create_object(db: &Db, object: &Object, chats: &[String]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            diesel::insert_into(media_objects::table)
                .values(object)
                .execute(conn)
                .await?;
            for chat in chats {
                diesel::insert_into(media_object_readers::table)
                    .values((
                        media_object_readers::object_id.eq(&object.id),
                        media_object_readers::chat_id.eq(chat),
                    ))
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

pub(super) async fn object(db: &Db, id: &str) -> Result<Option<Object>, DbError> {
    with_conn!(db, |conn| Ok(media_objects::table
        .find(id)
        .select(Object::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn object_chats(db: &Db, id: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(media_object_readers::table
        .filter(media_object_readers::object_id.eq(id))
        .select(media_object_readers::chat_id)
        .load(conn)
        .await?))
}

/// Records what an object holds now (`content_type`, `size`) in place of `object`'s own, when the owner's stored
/// bytes stay within `quota` (0: no limit); `false` when they wouldn't.
pub(super) async fn fill_object(
    db: &Db,
    object: &Object,
    content_type: Option<&str>,
    size: i64,
    quota: i64,
) -> Result<bool, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            if !charge!(conn, &object.owner_id, size - object.size, quota) {
                return Ok(false);
            }
            diesel::update(media_objects::table.find(&object.id))
                .set((
                    media_objects::content_type.eq(content_type),
                    media_objects::size.eq(size),
                ))
                .execute(conn)
                .await?;
            Ok(true)
        })
        .await)
}

/// The bytes `user` has stored.
pub(super) async fn used(db: &Db, user: &str) -> Result<i64, DbError> {
    with_conn!(db, |conn| Ok(media_usage::table
        .find(user)
        .select(media_usage::bytes)
        .first(conn)
        .await
        .optional()?
        .unwrap_or(0)))
}

/// Adds a file to its owner's drive, readable by the owner alone, when the owner's stored bytes stay within `quota`.
/// `Ok(None)` when they wouldn't; [`DbError::Conflict`] when the owner has a file of that name.
pub(super) async fn add_item(db: &Db, item: &Item, quota: i64) -> Result<Option<()>, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            if !charge!(conn, &item.owner_id, item.size, quota) {
                return Ok(None);
            }
            diesel::insert_into(drive_items::table)
                .values(item)
                .execute(conn)
                .await
                .map_err(conflict)?;
            diesel::insert_into(drive_grants::table)
                .values((
                    drive_grants::item_id.eq(&item.id),
                    drive_grants::user_id.eq(&item.owner_id),
                ))
                .execute(conn)
                .await?;
            Ok(Some(()))
        })
        .await)
}

pub(super) async fn item(db: &Db, id: &str) -> Result<Option<Item>, DbError> {
    with_conn!(db, |conn| Ok(drive_items::table
        .find(id)
        .select(Item::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn item_named(db: &Db, owner: &str, name: &str) -> Result<Option<Item>, DbError> {
    with_conn!(db, |conn| Ok(drive_items::table
        .filter(drive_items::owner_id.eq(owner))
        .filter(drive_items::name.eq(name))
        .select(Item::as_select())
        .first(conn)
        .await
        .optional()?))
}

pub(super) async fn has_items(db: &Db, owner: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::select(diesel::dsl::exists(
        drive_items::table.filter(drive_items::owner_id.eq(owner))
    ))
    .get_result(conn)
    .await?))
}

/// The accounts a file is shared with, its owner among them, by id.
pub(super) async fn grants(db: &Db, item: &str) -> Result<Vec<String>, DbError> {
    with_conn!(db, |conn| Ok(drive_grants::table
        .filter(drive_grants::item_id.eq(item))
        .order(drive_grants::user_id.asc())
        .select(drive_grants::user_id)
        .load(conn)
        .await?))
}

pub(super) async fn grant(db: &Db, item: &str, users: &[String]) -> Result<(), DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            for user in users {
                diesel::insert_into(drive_grants::table)
                    .values((drive_grants::item_id.eq(item), drive_grants::user_id.eq(user)))
                    .on_conflict_do_nothing()
                    .execute(conn)
                    .await?;
            }
            Ok(())
        })
        .await)
}

pub(super) async fn revoke(db: &Db, item: &str, user: &str) -> Result<bool, DbError> {
    with_conn!(db, |conn| Ok(diesel::delete(drive_grants::table.find((item, user)))
        .execute(conn)
        .await?
        == 1))
}
