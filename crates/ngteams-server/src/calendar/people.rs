//! Whom an event invites. The server takes no mail, so each attendee must be one of its accounts, named by its
//! MRI or by its address (its sign-in name).

use serde_json::Value;

use super::{Answer, Error};
use crate::db::users::{self, User};
use crate::db::{Db, DbError};

/// The most people one event invites.
const MOST: usize = 250;

pub(super) struct Invitee {
    pub user: User,
    pub optional: bool,
}

/// The accounts the `attendees` of a sent event name, in order, each once: not the organizer `me`, not an entry
/// that names the organizer's own part, and not the mailbox `skip` (a channel meeting's team).
pub(super) async fn invitees(db: &Db, me: &str, attendees: &Value, skip: Option<&str>) -> Answer<Vec<Invitee>> {
    let entries = match attendees {
        Value::Null => return Ok(Vec::new()),
        Value::Array(entries) if entries.len() <= MOST => entries,
        _ => return Err(Error::BAD_REQUEST),
    };
    let mut invited: Vec<Invitee> = Vec::new();
    for entry in entries {
        let address = entry["address"].as_str().map(str::trim).unwrap_or_default();
        if entry["type"].as_str() == Some("Organizer") || skip.is_some_and(|skip| skip.eq_ignore_ascii_case(address)) {
            continue;
        }
        let user = account(db, entry["mri"].as_str(), address)
            .await?
            .filter(|user| !user.disabled && user.deleted_at.is_none())
            .ok_or(Error::UNKNOWN_ATTENDEE)?;
        if user.id != me && !invited.iter().any(|known| known.user.id == user.id) {
            invited.push(Invitee {
                user,
                optional: entry["type"].as_str() == Some("Optional"),
            });
        }
    }
    Ok(invited)
}

async fn account(db: &Db, mri: Option<&str>, address: &str) -> Result<Option<User>, DbError> {
    match mri.and_then(|mri| mri.strip_prefix("8:orgid:")) {
        Some(id) => users::by_id(db, id).await,
        None if address.is_empty() => Ok(None),
        None => users::by_upn(db, address).await,
    }
}
