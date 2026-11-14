//! The admin operations' database work. Each change is one transaction that also writes its audit entry, so a
//! refused or failed change leaves neither. Changes that could take the last admin away first take the admin lock
//! (a row every node updates), then count the other admins: two nodes can't each remove "the other" admin.

use diesel::prelude::*;
use diesel_async::{AsyncConnection as _, RunQueryDsl};
use serde_json::{Map, Value, json};

use super::{Actor, AdminError, Changes};
use crate::db::schema::{admin_audit, refresh_tokens, server_meta, users};
use crate::db::users::User;
use crate::db::{Db, DbError, conflict, now, with_conn};

/// The `server_meta` row the guarded changes lock (made by the migration).
const ADMIN_LOCK: &str = "admin_lock";
/// The `server_meta` row holding the pending setup code's digest.
const SETUP_CODE: &str = "setup_code";
/// The name a deleted account keeps for what it authored.
pub const DELETED_NAME: &str = "Deleted user";

diesel::define_sql_function! {
    /// SQL `lower`.
    fn lower(text: diesel::sql_types::Text) -> diesel::sql_types::Text;
}

/// An account to add, checked and with its password hashed.
#[derive(Clone)]
pub struct NewUser {
    pub upn: String,
    pub display_name: String,
    pub password_hash: String,
    pub admin: bool,
    pub must_change_password: bool,
}

impl NewUser {
    fn row(self) -> User {
        let at = now();
        User {
            id: uuid::Uuid::new_v4().to_string(),
            upn: self.upn,
            display_name: self.display_name,
            password_hash: self.password_hash,
            disabled: false,
            created_at: at,
            updated_at: at,
            is_admin: self.admin,
            must_change_password: self.must_change_password,
            deleted_at: None,
        }
    }
}

/// An audit entry as the database keeps it; `detail` is a JSON object, never holding a password.
#[derive(Clone, Debug, PartialEq, Eq, Queryable, Selectable)]
#[diesel(table_name = admin_audit)]
pub struct Entry {
    pub id: i64,
    pub at: i64,
    pub actor_id: Option<String>,
    pub actor: String,
    pub action: String,
    pub target_id: Option<String>,
    pub target: String,
    pub detail: String,
}

#[derive(Insertable)]
#[diesel(table_name = admin_audit)]
struct NewEntry<'a> {
    at: i64,
    actor_id: Option<&'a str>,
    actor: &'a str,
    action: &'a str,
    target_id: Option<&'a str>,
    target: &'a str,
    detail: String,
}

impl<'a> NewEntry<'a> {
    fn new(actor: Actor<'a>, action: &'a str, target_id: &'a str, target: &'a str, detail: &Value) -> Self {
        Self {
            at: now(),
            actor_id: actor.id,
            actor: actor.name,
            action,
            target_id: Some(target_id),
            target,
            detail: detail.to_string(),
        }
    }
}

/// A taken UPN as [`AdminError::Exists`].
fn taken(error: diesel::result::Error) -> AdminError {
    match conflict(error) {
        DbError::Conflict => AdminError::Exists,
        error => AdminError::Db(error),
    }
}

/// Takes the admin lock for the rest of the transaction.
macro_rules! lock {
    ($conn:ident) => {
        diesel::update(server_meta::table.find(ADMIN_LOCK))
            .set(server_meta::value.eq(server_meta::value))
            .execute($conn)
            .await?
    };
}

/// How many enabled admins there are besides the account `$id`.
macro_rules! other_admins {
    ($conn:ident, $id:expr) => {
        users::table
            .filter(users::is_admin.eq(true))
            .filter(users::disabled.eq(false))
            .filter(users::id.ne($id))
            .count()
            .get_result::<i64>($conn)
            .await?
    };
}

/// The account `$id` unless it was deleted.
macro_rules! account {
    ($conn:ident, $id:expr) => {
        users::table
            .find($id)
            .filter(users::deleted_at.is_null())
            .select(User::as_select())
            .first($conn)
            .await
            .optional()?
            .ok_or(AdminError::NoSuchUser)?
    };
}

/// Revokes the refresh tokens of the account `$id`.
macro_rules! revoke {
    ($conn:ident, $id:expr) => {
        diesel::update(
            refresh_tokens::table
                .filter(refresh_tokens::user_id.eq($id))
                .filter(refresh_tokens::revoked_at.is_null()),
        )
        .set(refresh_tokens::revoked_at.eq(Some(now())))
        .execute($conn)
        .await?
    };
}

macro_rules! record {
    ($conn:ident, $entry:expr) => {
        diesel::insert_into(admin_audit::table)
            .values($entry)
            .execute($conn)
            .await?
    };
}

fn trace(actor: Actor<'_>, action: &str, target: &str) {
    tracing::info!(actor = actor.name, actor_id = actor.id, action, target, "admin action");
}

/// Adds an account.
///
/// # Errors
/// [`AdminError::Exists`] for a taken UPN; a database failure.
pub async fn create(db: &Db, actor: Actor<'_>, new: NewUser) -> Result<User, AdminError> {
    let user = new.row();
    let detail = json!({"admin": user.is_admin, "must_change_password": user.must_change_password});
    let entry = NewEntry::new(actor, "user.add", &user.id, &user.upn, &detail);
    with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            diesel::insert_into(users::table)
                .values(&user)
                .execute(conn)
                .await
                .map_err(taken)?;
            record!(conn, &entry);
            Ok(())
        })
        .await)?;
    trace(actor, "user.add", &user.upn);
    Ok(user)
}

/// How many enabled admins the server has.
///
/// # Errors
/// A database failure.
pub async fn admins(db: &Db) -> Result<i64, DbError> {
    with_conn!(db, |conn| Ok(other_admins!(conn, "")))
}

/// The pending setup code's digest.
///
/// # Errors
/// A database failure.
pub async fn setup_code(db: &Db) -> Result<Option<String>, DbError> {
    with_conn!(db, |conn| Ok(server_meta::table
        .find(SETUP_CODE)
        .select(server_meta::value)
        .first::<String>(conn)
        .await
        .optional()?))
}

/// Stores a setup code's digest unless one is pending; whether it did.
///
/// # Errors
/// A database failure.
pub async fn offer_setup_code(db: &Db, digest: &str) -> Result<bool, DbError> {
    let stored = with_conn!(db, |conn| diesel::insert_into(server_meta::table)
        .values((server_meta::name.eq(SETUP_CODE), server_meta::value.eq(digest)))
        .on_conflict_do_nothing()
        .execute(conn)
        .await?);
    Ok(stored == 1)
}

/// Replaces the pending setup code's digest.
///
/// # Errors
/// [`AdminError::AdminExists`] when no setup is needed; a database failure.
pub async fn replace_setup_code(db: &Db, digest: &str) -> Result<(), AdminError> {
    with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            lock!(conn);
            if other_admins!(conn, "") > 0 {
                return Err(AdminError::AdminExists);
            }
            diesel::delete(server_meta::table.find(SETUP_CODE))
                .execute(conn)
                .await?;
            diesel::insert_into(server_meta::table)
                .values((server_meta::name.eq(SETUP_CODE), server_meta::value.eq(digest)))
                .execute(conn)
                .await?;
            Ok(())
        })
        .await)
}

/// Whether `stored` is the digest `given`, compared in constant time.
fn same_digest(stored: Option<&str>, given: &str) -> bool {
    stored.is_some_and(|stored| {
        aws_lc_rs::constant_time::verify_slices_are_equal(stored.as_bytes(), given.as_bytes()).is_ok()
    })
}

/// Whether a first admin may be created with this code's digest (`None` when the server asks for no code): the
/// cheap check before the password is hashed. [`create_first_admin`] checks again under the lock.
///
/// # Errors
/// [`AdminError::AdminExists`], [`AdminError::SetupCode`]; a database failure.
pub async fn check_setup(db: &Db, code_digest: Option<&str>) -> Result<(), AdminError> {
    if admins(db).await? > 0 {
        return Err(AdminError::AdminExists);
    }
    if let Some(given) = code_digest
        && !same_digest(setup_code(db).await?.as_deref(), given)
    {
        return Err(AdminError::SetupCode);
    }
    Ok(())
}

/// Creates the first admin and spends the setup code, when the server has no admin and the code matches.
///
/// # Errors
/// [`AdminError::AdminExists`], [`AdminError::SetupCode`], [`AdminError::Exists`]; a database failure.
pub async fn create_first_admin(db: &Db, new: NewUser, code_digest: Option<&str>) -> Result<User, AdminError> {
    let user = new.row();
    let entry = NewEntry::new(Actor::SETUP, "setup.first_admin", &user.id, &user.upn, &json!({}));
    with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            lock!(conn);
            if other_admins!(conn, "") > 0 {
                return Err(AdminError::AdminExists);
            }
            if let Some(given) = code_digest {
                let stored = server_meta::table
                    .find(SETUP_CODE)
                    .select(server_meta::value)
                    .first::<String>(conn)
                    .await
                    .optional()?;
                if !same_digest(stored.as_deref(), given) {
                    return Err(AdminError::SetupCode);
                }
            }
            diesel::insert_into(users::table)
                .values(&user)
                .execute(conn)
                .await
                .map_err(taken)?;
            diesel::delete(server_meta::table.find(SETUP_CODE))
                .execute(conn)
                .await?;
            record!(conn, &entry);
            Ok(())
        })
        .await)?;
    trace(Actor::SETUP, "setup.first_admin", &user.upn);
    Ok(user)
}

/// Applies `changes` to `user`; what changed, as the audit keeps it.
fn apply(user: &mut User, changes: &Changes) -> Map<String, Value> {
    let mut detail = Map::new();
    if let Some(name) = changes.display_name.as_ref().filter(|name| **name != user.display_name) {
        detail.insert("display_name".to_owned(), json!(name));
        user.display_name.clone_from(name);
    }
    if let Some(upn) = changes.upn.as_ref().filter(|upn| **upn != user.upn) {
        detail.insert("upn".to_owned(), json!(upn));
        user.upn.clone_from(upn);
    }
    if let Some(enabled) = changes.enabled.filter(|enabled| *enabled == user.disabled) {
        detail.insert("enabled".to_owned(), json!(enabled));
        user.disabled = !enabled;
    }
    if let Some(admin) = changes.admin.filter(|admin| *admin != user.is_admin) {
        detail.insert("admin".to_owned(), json!(admin));
        user.is_admin = admin;
    }
    detail
}

/// Changes an account's name, UPN, enabled state or admin role. Disabling revokes its refresh tokens. A change that
/// changes nothing is not audited.
///
/// # Errors
/// [`AdminError::NoSuchUser`], [`AdminError::LastAdmin`], [`AdminError::Exists`]; a database failure.
pub async fn update(db: &Db, actor: Actor<'_>, id: &str, changes: &Changes) -> Result<User, AdminError> {
    let (user, target) = with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            lock!(conn);
            let mut user: User = account!(conn, id);
            let target = user.upn.clone();
            let was_admin = user.is_admin && !user.disabled;
            let detail = apply(&mut user, changes);
            if detail.is_empty() {
                return Ok((user, None));
            }
            let stays_admin = user.is_admin && !user.disabled;
            if was_admin && !stays_admin && other_admins!(conn, id) == 0 {
                return Err(AdminError::LastAdmin);
            }
            user.updated_at = now();
            diesel::update(users::table.find(id))
                .set((
                    users::display_name.eq(&user.display_name),
                    users::upn.eq(&user.upn),
                    users::disabled.eq(user.disabled),
                    users::is_admin.eq(user.is_admin),
                    users::updated_at.eq(user.updated_at),
                ))
                .execute(conn)
                .await
                .map_err(taken)?;
            if user.disabled && detail.contains_key("enabled") {
                revoke!(conn, id);
            }
            record!(
                conn,
                &NewEntry::new(actor, "user.update", id, &target, &Value::Object(detail))
            );
            Ok((user, Some(target)))
        })
        .await)?;
    if let Some(target) = target {
        trace(actor, "user.update", &target);
    }
    Ok(user)
}

/// Deletes an account: its sign-in (UPN, password, role, refresh tokens) goes, and its id stays as a disabled
/// "Deleted user", so what it authored keeps an author. The UPN is free for a new account.
///
/// # Errors
/// [`AdminError::NoSuchUser`], [`AdminError::LastAdmin`]; a database failure.
pub async fn delete(db: &Db, actor: Actor<'_>, id: &str) -> Result<(), AdminError> {
    let target = with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            lock!(conn);
            let user: User = account!(conn, id);
            if user.is_admin && !user.disabled && other_admins!(conn, id) == 0 {
                return Err(AdminError::LastAdmin);
            }
            let at = now();
            diesel::update(users::table.find(id))
                .set((
                    users::upn.eq(format!("deleted:{id}")),
                    users::display_name.eq(DELETED_NAME),
                    users::password_hash.eq(""),
                    users::disabled.eq(true),
                    users::is_admin.eq(false),
                    users::must_change_password.eq(false),
                    users::deleted_at.eq(Some(at)),
                    users::updated_at.eq(at),
                ))
                .execute(conn)
                .await?;
            diesel::delete(refresh_tokens::table.filter(refresh_tokens::user_id.eq(id)))
                .execute(conn)
                .await?;
            record!(conn, &NewEntry::new(actor, "user.delete", id, &user.upn, &json!({})));
            Ok(user.upn)
        })
        .await)?;
    trace(actor, "user.delete", &target);
    Ok(())
}

/// A new password for an account, as [`set_password`] takes it.
#[derive(Clone, Copy)]
pub struct NewPassword<'a> {
    pub hash: &'a str,
    pub must_change: bool,
    /// The hash the current password was checked against (a self-service change): the change is refused when the
    /// account's password is no longer that one.
    pub verified: Option<&'a str>,
    /// The audit action.
    pub action: &'a str,
}

/// Replaces an account's password and revokes its refresh tokens.
///
/// # Errors
/// [`AdminError::NoSuchUser`], [`AdminError::Credentials`]; a database failure.
pub async fn set_password(db: &Db, actor: Actor<'_>, id: &str, new: NewPassword<'_>) -> Result<(), AdminError> {
    let detail = if new.verified.is_some() {
        json!({})
    } else {
        json!({"must_change_password": new.must_change})
    };
    let target = with_conn!(db, |conn| conn
        .transaction::<_, AdminError, _>(async |conn| {
            // the account's row lock, as the refresh-token paths take it
            diesel::update(users::table.find(id))
                .set(users::updated_at.eq(users::updated_at))
                .execute(conn)
                .await?;
            let user: User = account!(conn, id);
            if new.verified.is_some_and(|verified| verified != user.password_hash) {
                return Err(AdminError::Credentials);
            }
            diesel::update(users::table.find(id))
                .set((
                    users::password_hash.eq(new.hash),
                    users::must_change_password.eq(new.must_change),
                    users::updated_at.eq(now()),
                ))
                .execute(conn)
                .await?;
            revoke!(conn, id);
            let entry = NewEntry::new(actor, new.action, id, &user.upn, &detail);
            record!(conn, &entry);
            Ok(user.upn)
        })
        .await)?;
    trace(actor, new.action, &target);
    Ok(())
}

/// The account with this id, unless deleted.
///
/// # Errors
/// A database failure.
pub async fn get(db: &Db, id: &str) -> Result<Option<User>, DbError> {
    with_conn!(db, |conn| Ok(users::table
        .find(id)
        .filter(users::deleted_at.is_null())
        .select(User::as_select())
        .first(conn)
        .await
        .optional()?))
}

/// `%`, `_` and `\` as plain characters of a `LIKE` pattern.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Up to `limit` accounts by UPN, after the UPN `after`, whose UPN or name contains `query` (any case; all for an
/// empty one).
///
/// # Errors
/// A database failure.
pub async fn list(db: &Db, query: &str, after: &str, limit: i64) -> Result<Vec<User>, DbError> {
    let pattern = format!("%{}%", escape_like(&query.to_lowercase()));
    with_conn!(db, |conn| Ok(users::table
        .filter(users::deleted_at.is_null())
        .filter(users::upn.gt(after))
        .filter(
            users::upn
                .like(&pattern)
                .escape('\\')
                .or(lower(users::display_name).like(&pattern).escape('\\')),
        )
        .order(users::upn.asc())
        .limit(limit)
        .select(User::as_select())
        .load(conn)
        .await?))
}

/// Up to `limit` audit entries older than the entry `before`, newest first.
///
/// # Errors
/// A database failure.
pub async fn audit(db: &Db, before: i64, limit: i64) -> Result<Vec<Entry>, DbError> {
    with_conn!(db, |conn| Ok(admin_audit::table
        .filter(admin_audit::id.lt(before))
        .order(admin_audit::id.desc())
        .limit(limit)
        .select(Entry::as_select())
        .load(conn)
        .await?))
}
