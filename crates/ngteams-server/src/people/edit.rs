//! The operator's edit of an account's profile: each field set or cleared, the others left as they are.

use super::repo::{self, Profile};
use crate::db::users::{self, User};
use crate::db::{Db, DbError};

/// The longest value of a field.
const MAX_FIELD: usize = 256;
/// How far up a reporting line is followed for a loop.
const MAX_CHAIN: usize = 64;

/// What to change: `None` leaves a field, an empty value clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileChanges {
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub office_location: Option<String>,
    pub company_name: Option<String>,
    pub mobile_phone: Option<String>,
    pub business_phone: Option<String>,
    /// The UPN of the account this one reports to.
    pub manager: Option<String>,
    pub out_of_office: Option<bool>,
}

/// Why a profile wasn't changed.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("no such account")]
    NoAccount,
    #[error("a profile field takes at most 256 characters and no control characters")]
    Field,
    #[error("no such account to report to")]
    NoManager,
    #[error("an account can't report to itself, directly or through the accounts that report to it")]
    Loop,
    #[error(transparent)]
    Database(#[from] DbError),
}

/// The account with the UPN `upn`, unless it was deleted.
///
/// # Errors
/// [`ProfileError::NoAccount`], or a database failure.
pub async fn account(db: &Db, upn: &str) -> Result<User, ProfileError> {
    users::by_upn(db, upn)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(ProfileError::NoAccount)
}

/// Sets `field` as `change` says.
fn apply(field: &mut Option<String>, change: Option<&String>) -> Result<(), ProfileError> {
    let Some(change) = change else { return Ok(()) };
    let text = change.trim();
    if text.chars().count() > MAX_FIELD || text.chars().any(char::is_control) {
        return Err(ProfileError::Field);
    }
    *field = (!text.is_empty()).then(|| text.to_owned());
    Ok(())
}

/// The account `upn` names as a manager of `id`: one that wasn't deleted and doesn't report, however far up, to
/// `id`.
async fn manager(db: &Db, id: &str, upn: &str) -> Result<String, ProfileError> {
    let manager = users::by_upn(db, upn)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(ProfileError::NoManager)?;
    let mut above = Some(manager.id.clone());
    for _ in 0..MAX_CHAIN {
        let Some(next) = above else { break };
        if next == id {
            return Err(ProfileError::Loop);
        }
        above = repo::profile(db, &next).await?.manager_id;
    }
    Ok(manager.id)
}

/// Changes the profile of the account with the UPN `upn`; the profile as it is then.
///
/// # Errors
/// See [`ProfileError`].
pub async fn update_profile(db: &Db, upn: &str, changes: &ProfileChanges) -> Result<Profile, ProfileError> {
    let user = account(db, upn).await?;
    let mut profile = repo::profile(db, &user.id).await?;
    apply(&mut profile.given_name, changes.given_name.as_ref())?;
    apply(&mut profile.surname, changes.surname.as_ref())?;
    apply(&mut profile.job_title, changes.job_title.as_ref())?;
    apply(&mut profile.department, changes.department.as_ref())?;
    apply(&mut profile.office_location, changes.office_location.as_ref())?;
    apply(&mut profile.company_name, changes.company_name.as_ref())?;
    apply(&mut profile.mobile_phone, changes.mobile_phone.as_ref())?;
    apply(&mut profile.business_phone, changes.business_phone.as_ref())?;
    match changes.manager.as_deref().map(str::trim) {
        Some("") => profile.manager_id = None,
        Some(upn) => profile.manager_id = Some(manager(db, &user.id, upn).await?),
        None => {}
    }
    if let Some(out_of_office) = changes.out_of_office {
        profile.out_of_office = out_of_office;
    }
    profile.updated_at = crate::db::now();
    repo::put_profile(db, &profile).await?;
    Ok(profile)
}
