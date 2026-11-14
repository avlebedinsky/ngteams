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

/// What an object held before it was filled.
pub(super) struct Held {
    pub content_type: Option<String>,
    pub size: i64,
}

/// Records what `owner`'s object `id` holds now (`content_type`, `size`), when the owner's stored bytes stay within
/// `quota` (0: no limit): what it held before, or `None` when they wouldn't. The owner is charged the difference
/// from the size the row has inside the transaction, so fills of one object at once count its bytes once.
pub(super) async fn fill_object(
    db: &Db,
    id: &str,
    owner: &str,
    content_type: Option<&str>,
    size: i64,
    quota: i64,
) -> Result<Option<Held>, DbError> {
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            // changes nothing, but holds the row: another fill reads the size only after this one ends
            diesel::update(media_objects::table.find(id))
                .set(media_objects::size.eq(media_objects::size))
                .execute(conn)
                .await?;
            let (held_type, held_size): (Option<String>, i64) = media_objects::table
                .find(id)
                .select((media_objects::content_type, media_objects::size))
                .first(conn)
                .await?;
            if !charge!(conn, owner, size - held_size, quota) {
                return Ok(None);
            }
            diesel::update(media_objects::table.find(id))
                .set((
                    media_objects::content_type.eq(content_type),
                    media_objects::size.eq(size),
                ))
                .execute(conn)
                .await?;
            Ok(Some(Held {
                content_type: held_type,
                size: held_size,
            }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::users;
    use crate::db::users::User;

    async fn add_owner(db: &Db) -> Result<String, DbError> {
        let owner = User {
            id: "owner".to_owned(),
            upn: "owner@own.test".to_owned(),
            display_name: "Owner".to_owned(),
            password_hash: String::new(),
            disabled: false,
            created_at: 0,
            updated_at: 0,
            is_admin: false,
            must_change_password: false,
            deleted_at: None,
        };
        with_conn!(db, |conn| diesel::insert_into(users::table)
            .values(&owner)
            .execute(conn)
            .await?);
        Ok(owner.id)
    }

    async fn object_of(db: &Db) -> Object {
        let object = Object {
            id: "object".to_owned(),
            owner_id: add_owner(db).await.unwrap(),
            kind: "pish/image".to_owned(),
            filename: "picture.png".to_owned(),
            content_type: None,
            size: 0,
            created_at: 0,
        };
        create_object(db, &object, &[]).await.unwrap();
        object
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fills_that_started_from_the_same_size_count_the_objects_bytes_once() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}", dir.path().join("server.sqlite").display());
        let db = Db::connect(&url, 1).await.unwrap();
        let object = object_of(&db).await;
        let fill = async |content_type: Option<&str>, size: i64, quota: i64| {
            fill_object(&db, &object.id, &object.owner_id, content_type, size, quota)
                .await
                .unwrap()
                .map(|held| (held.content_type, held.size))
        };

        // two first fills, both begun while the object was empty
        assert_eq!(fill(Some("image/png"), 10, 10).await, Some((None, 0)));
        assert_eq!(
            fill(Some("image/png"), 10, 10).await,
            Some((Some("image/png".to_owned()), 10))
        );
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 10);
        // two replacements, both begun while it held 10 bytes
        assert!(fill(Some("image/gif"), 1, 10).await.is_some());
        assert!(fill(Some("image/gif"), 1, 10).await.is_some());
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 1);

        assert_eq!(fill(Some("image/png"), 11, 10).await, None);
        let held = fill(Some("image/png"), 7, 10).await.unwrap();
        assert_eq!(held, (Some("image/gif".to_owned()), 1));
        // what a fill whose bytes never reached the store puts back
        assert!(fill(held.0.as_deref(), held.1, 0).await.is_some());
        let now = super::object(&db, &object.id).await.unwrap().unwrap();
        assert_eq!((now.content_type.as_deref(), now.size), (Some("image/gif"), 1));
        assert_eq!(used(&db, &object.owner_id).await.unwrap(), 1);
    }
}
