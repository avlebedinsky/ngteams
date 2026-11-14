//! Accounts: an object id (the `oid` and MRI `8:orgid:<id>`), a UPN (kept lowercase, unique), a name, an Argon2
//! password hash, whether the account may sign in, whether it is an admin, and whether it must change its password
//! before it signs in. A deleted account stays as a disabled row without a sign-in (`deleted_at`), so its id keeps
//! naming what it authored (`crate::admin::repo::delete`).

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use super::schema::users;
use super::{Db, DbError, with_conn};

/// An account as the database keeps it.
#[derive(Clone, PartialEq, Eq, Queryable, Selectable, Insertable)]
#[diesel(table_name = users)]
pub struct User {
    pub id: String,
    pub upn: String,
    pub display_name: String,
    pub password_hash: String,
    pub disabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub is_admin: bool,
    pub must_change_password: bool,
    /// When the account was deleted; a deleted account is also disabled.
    pub deleted_at: Option<i64>,
}

impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("upn", &self.upn)
            .field("disabled", &self.disabled)
            .field("is_admin", &self.is_admin)
            .finish_non_exhaustive()
    }
}

/// A UPN as it is stored and looked up: trimmed and lowercase.
#[must_use]
pub fn normalize_upn(upn: &str) -> String {
    upn.trim().to_lowercase()
}

/// The account with this UPN (any case).
///
/// # Errors
/// A database failure.
pub async fn by_upn(db: &Db, upn: &str) -> Result<Option<User>, DbError> {
    let upn = normalize_upn(upn);
    with_conn!(db, |conn| Ok(users::table
        .filter(users::upn.eq(&upn))
        .select(User::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// The account with this object id.
///
/// # Errors
/// A database failure.
pub async fn by_id(db: &Db, id: &str) -> Result<Option<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .find(id)
        .select(User::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// Every account that isn't deleted, by UPN.
///
/// # Errors
/// A database failure.
pub async fn list(db: &Db) -> Result<Vec<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .filter(users::deleted_at.is_null())
        .order(users::upn.asc())
        .select(User::as_select())
        .load(conn)
        .await?))
}
