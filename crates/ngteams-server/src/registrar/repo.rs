//! The registrations, one row per account, endpoint (`registrationId`) and app.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};

use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};

diesel::table! {
    push_registrations (user_id, registration_id, app_id) {
        user_id -> Text,
        registration_id -> Text,
        app_id -> Text,
        path -> Text,
        expires_at -> BigInt,
    }
}

/// An endpoint a stack registered.
#[derive(Clone, Debug, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = push_registrations)]
pub struct Registration {
    pub user_id: String,
    /// The client's `epid`.
    pub registration_id: String,
    /// `TeamsCDLWebWorker` or `SkypeSpacesWeb`.
    pub app_id: String,
    /// The endpoint's push URL.
    pub path: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// Keeps `registration`, in place of the same endpoint's earlier one. `false`, and nothing kept, when the account
/// already holds `most` others that haven't run out.
///
/// # Errors
/// A database failure.
pub(super) async fn put(db: &Db, registration: &Registration, most: i64) -> Result<bool, DbError> {
    use push_registrations::dsl;
    let now = crate::db::now();
    with_conn!(db, |conn| conn
        .transaction::<_, DbError, _>(async |conn| {
            // holds the account's row, so two nodes can't both count the last free place
            diesel::update(users::table.find(&registration.user_id))
                .set(users::disabled.eq(users::disabled))
                .execute(conn)
                .await?;
            diesel::delete(
                dsl::push_registrations
                    .filter(dsl::user_id.eq(&registration.user_id))
                    .filter(dsl::expires_at.le(now)),
            )
            .execute(conn)
            .await?;
            let others: i64 = dsl::push_registrations
                .filter(dsl::user_id.eq(&registration.user_id))
                .filter(
                    dsl::registration_id
                        .ne(&registration.registration_id)
                        .or(dsl::app_id.ne(&registration.app_id)),
                )
                .count()
                .get_result(conn)
                .await?;
            if others >= most {
                return Ok(false);
            }
            diesel::insert_into(dsl::push_registrations)
                .values(registration)
                .on_conflict((dsl::user_id, dsl::registration_id, dsl::app_id))
                .do_update()
                .set((
                    dsl::path.eq(&registration.path),
                    dsl::expires_at.eq(registration.expires_at),
                ))
                .execute(conn)
                .await?;
            Ok(true)
        })
        .await)
}

/// Until when (Unix seconds) the registration of `user`'s endpoint `registration_id` by `app_id` holds; `None`
/// without one that still does.
///
/// # Errors
/// A database failure.
pub async fn until(db: &Db, user: &str, registration_id: &str, app_id: &str) -> Result<Option<i64>, DbError> {
    use push_registrations::dsl;
    let now = crate::db::now();
    with_conn!(db, |conn| Ok(dsl::push_registrations
        .find((user, registration_id, app_id))
        .filter(dsl::expires_at.gt(now))
        .select(dsl::expires_at)
        .first::<i64>(conn)
        .await
        .optional()?))
}

/// Drops the registrations that ran out before `before` (Unix seconds); how many.
///
/// # Errors
/// A database failure.
pub async fn purge_expired(db: &Db, before: i64) -> Result<usize, DbError> {
    use push_registrations::dsl;
    with_conn!(db, |conn| Ok(diesel::delete(
        dsl::push_registrations.filter(dsl::expires_at.lt(before))
    )
    .execute(conn)
    .await?))
}

/// The endpoints (`registrationId`s) `user` holds a live registration of `app_id` for.
///
/// # Errors
/// A database failure.
pub async fn registered(db: &Db, user: &str, app_id: &str) -> Result<Vec<String>, DbError> {
    use push_registrations::dsl;
    let now = crate::db::now();
    with_conn!(db, |conn| Ok(dsl::push_registrations
        .filter(dsl::user_id.eq(user))
        .filter(dsl::app_id.eq(app_id))
        .filter(dsl::expires_at.gt(now))
        .order(dsl::registration_id.asc())
        .select(dsl::registration_id)
        .load::<String>(conn)
        .await?))
}
