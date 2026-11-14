//! Account administration: the operations behind the admin API ([`api`]), the command line and the first-admin setup
//! ([`setup`]). Each checks its input, then runs as one database transaction ([`repo`]) that also writes its audit
//! entry and holds the guards that keep the server with an enabled admin.

pub mod api;
pub mod repo;
pub mod setup;

use diesel_async::pooled_connection::bb8::RunError;

use self::repo::{NewPassword, NewUser};
use crate::auth::password::{self, PasswordError};
use crate::db::users::{self, User};
use crate::db::{Db, DbError};

/// The longest display name, in characters.
pub const MAX_NAME: usize = 256;
/// The longest UPN, in characters.
pub const MAX_UPN: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error(transparent)]
    Password(#[from] PasswordError),
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("an account with that UPN already exists")]
    Exists,
    #[error("no such account")]
    NoSuchUser,
    #[error("a UPN looks like name@domain")]
    Upn,
    #[error("a display name has 1 to {MAX_NAME} characters")]
    Name,
    #[error("the server must keep an enabled admin")]
    LastAdmin,
    #[error("the server has an admin account")]
    AdminExists,
    #[error("the setup code is wrong")]
    SetupCode,
    #[error("the current password is wrong")]
    Credentials,
    #[error("the new password must differ from the current one")]
    SamePassword,
    #[error("the system's random source failed")]
    Random,
}

impl From<diesel::result::Error> for AdminError {
    fn from(error: diesel::result::Error) -> Self {
        Self::Db(DbError::Query(error))
    }
}

impl From<RunError> for AdminError {
    fn from(error: RunError) -> Self {
        Self::Db(error.into())
    }
}

/// Who does an admin action, as the audit names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor<'a> {
    /// The account's id; none for the command line and the setup.
    pub id: Option<&'a str>,
    /// The account's UPN, `cli` or `setup`.
    pub name: &'a str,
}

impl Actor<'static> {
    /// The operator at the command line.
    pub const CLI: Self = Self { id: None, name: "cli" };
    /// The first-admin setup.
    pub const SETUP: Self = Self {
        id: None,
        name: "setup",
    };
}

impl<'a> Actor<'a> {
    #[must_use]
    pub fn account(user: &'a User) -> Self {
        Self {
            id: Some(&user.id),
            name: &user.upn,
        }
    }
}

/// An account to add.
#[derive(Clone, Copy)]
pub struct NewAccount<'a> {
    pub upn: &'a str,
    /// The name others see; the UPN when empty.
    pub display_name: &'a str,
    pub password: &'a str,
    pub admin: bool,
    pub must_change_password: bool,
}

/// What to change of an account; `None` leaves a value as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub display_name: Option<String>,
    pub upn: Option<String>,
    pub enabled: Option<bool>,
    pub admin: Option<bool>,
}

/// A UPN as it is stored, when it is `name@domain`.
fn checked_upn(upn: &str) -> Result<String, AdminError> {
    let upn = users::normalize_upn(upn);
    let well_formed = upn.split_once('@').is_some_and(|(name, domain)| {
        !name.is_empty() && domain.contains('.') && !upn.contains(char::is_whitespace) && !domain.contains('@')
    });
    if well_formed && upn.chars().count() <= MAX_UPN {
        Ok(upn)
    } else {
        Err(AdminError::Upn)
    }
}

/// A display name as it is stored.
fn checked_name(name: &str) -> Result<String, AdminError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        Err(AdminError::Name)
    } else {
        Ok(name.to_owned())
    }
}

/// `new` checked, with its password hashed.
async fn prepared(new: &NewAccount<'_>) -> Result<NewUser, AdminError> {
    let upn = checked_upn(new.upn)?;
    let display_name = if new.display_name.trim().is_empty() {
        upn.clone()
    } else {
        checked_name(new.display_name)?
    };
    Ok(NewUser {
        password_hash: password::hash(new.password).await?,
        upn,
        display_name,
        admin: new.admin,
        must_change_password: new.must_change_password,
    })
}

/// Adds an account.
///
/// # Errors
/// A UPN that isn't `name@domain` or is taken, a name too long, a short password, or a database failure.
pub async fn create_user(db: &Db, actor: Actor<'_>, new: &NewAccount<'_>) -> Result<User, AdminError> {
    repo::create(db, actor, prepared(new).await?).await
}

/// Creates the first admin: only while the server has no admin, and, when `require_code`, only with the pending
/// setup code, which it spends.
///
/// # Errors
/// [`AdminError::AdminExists`], [`AdminError::SetupCode`], what [`create_user`] refuses, or a database failure.
pub async fn create_first_admin(
    db: &Db,
    require_code: bool,
    code: Option<&str>,
    new: &NewAccount<'_>,
) -> Result<User, AdminError> {
    let digest = require_code.then(|| setup::digest(code.unwrap_or_default()));
    // before the password is hashed: a caller without the code costs the server no hashing
    repo::check_setup(db, digest.as_deref()).await?;
    let user = prepared(&NewAccount { admin: true, ..*new }).await?;
    repo::create_first_admin(db, user, digest.as_deref()).await
}

/// Changes an account's name, UPN, enabled state or admin role. Disabling signs it out everywhere.
///
/// # Errors
/// No such account, a value [`create_user`] would refuse, [`AdminError::LastAdmin`] when it would leave the server
/// without an enabled admin, or a database failure.
pub async fn update_user(db: &Db, actor: Actor<'_>, id: &str, changes: Changes) -> Result<User, AdminError> {
    let changes = Changes {
        display_name: changes.display_name.as_deref().map(checked_name).transpose()?,
        upn: changes.upn.as_deref().map(checked_upn).transpose()?,
        ..changes
    };
    repo::update(db, actor, id, &changes).await
}

/// Deletes an account ([`repo::delete`]).
///
/// # Errors
/// No such account, [`AdminError::LastAdmin`], or a database failure.
pub async fn delete_user(db: &Db, actor: Actor<'_>, id: &str) -> Result<(), AdminError> {
    repo::delete(db, actor, id).await
}

/// Gives an account a new password, signing it out everywhere; with `must_change`, the account must replace it
/// before it signs in.
///
/// # Errors
/// No such account, a short password, or a database failure.
pub async fn reset_user_password(
    db: &Db,
    actor: Actor<'_>,
    id: &str,
    password: &str,
    must_change: bool,
) -> Result<(), AdminError> {
    let hash = password::hash(password).await?;
    let new = NewPassword {
        hash: &hash,
        must_change,
        verified: None,
        action: "user.password_reset",
    };
    repo::set_password(db, actor, id, new).await
}

/// An account's own password change, once `current` was checked against `user`'s hash: sets `new` and signs the
/// account out everywhere.
///
/// # Errors
/// [`AdminError::SamePassword`], a short password, [`AdminError::Credentials`] when the password changed
/// meanwhile, or a database failure.
pub async fn change_password(db: &Db, user: &User, current: &str, new: &str) -> Result<(), AdminError> {
    if current == new {
        return Err(AdminError::SamePassword);
    }
    let hash = password::hash(new).await?;
    let new = NewPassword {
        hash: &hash,
        must_change: false,
        verified: Some(&user.password_hash),
        action: "user.password_change",
    };
    repo::set_password(db, Actor::account(user), &user.id, new).await
}

/// The id of the account with this UPN.
async fn id_of(db: &Db, upn: &str) -> Result<String, AdminError> {
    users::by_upn(db, upn)
        .await?
        .map(|user| user.id)
        .ok_or(AdminError::NoSuchUser)
}

/// Disables (signing it out everywhere) or enables the account with this UPN (the command line).
///
/// # Errors
/// As [`update_user`].
pub async fn set_disabled(db: &Db, upn: &str, disabled: bool) -> Result<(), AdminError> {
    let changes = Changes {
        enabled: Some(!disabled),
        ..Changes::default()
    };
    update_user(db, Actor::CLI, &id_of(db, upn).await?, changes).await?;
    Ok(())
}

/// Makes the account with this UPN an admin, or takes the role away (the command line).
///
/// # Errors
/// As [`update_user`].
pub async fn set_admin(db: &Db, upn: &str, admin: bool) -> Result<(), AdminError> {
    let changes = Changes {
        admin: Some(admin),
        ..Changes::default()
    };
    update_user(db, Actor::CLI, &id_of(db, upn).await?, changes).await?;
    Ok(())
}

/// Gives the account with this UPN a new password, signing it out everywhere (the command line).
///
/// # Errors
/// As [`reset_user_password`].
pub async fn reset_password(db: &Db, upn: &str, password: &str) -> Result<(), AdminError> {
    reset_user_password(db, Actor::CLI, &id_of(db, upn).await?, password, false).await
}

/// Deletes the account with this UPN (the command line).
///
/// # Errors
/// As [`delete_user`].
pub async fn delete_by_upn(db: &Db, upn: &str) -> Result<(), AdminError> {
    delete_user(db, Actor::CLI, &id_of(db, upn).await?).await
}
