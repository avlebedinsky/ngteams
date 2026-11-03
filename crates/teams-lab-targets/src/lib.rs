//! Implementation runs may contact only the three dedicated lab accounts. These preflights are independent
//! of transport authorization and never apply to the user's personal client. Callers read the exact target's
//! members fresh and in full immediately before the action; a cached roster or a chat title is not evidence.

use std::collections::BTreeSet;

use serde_json::Value;

/// One of the three dedicated implementation accounts, never a personal contact or service entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabAccount {
    pub mri: String,
    pub upn: String,
}

/// Only the three explicit implementation identities. Configuration contacts/services do not belong here.
#[derive(Clone, Debug)]
pub struct LabRoster {
    accounts: [LabAccount; 3],
}

/// A failed preflight contains no recipient identifiers or credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TargetError {
    #[error("the lab roster must contain three distinct work accounts with known identities")]
    InvalidRoster,
    #[error("the active account is not the expected lab identity")]
    Identity,
    #[error("the selected conversation is not the intended conversation")]
    Conversation,
    #[error("the complete recipient list is missing, ambiguous, or includes a non-lab recipient")]
    Recipients,
    #[error("the server did not return the exact target's full membership")]
    Membership,
}

impl LabRoster {
    /// Builds the explicit roster. Unknown MRIs fail closed before a run can contact anybody.
    ///
    /// # Errors
    /// [`TargetError::InvalidRoster`] for missing, duplicate or non-work identities.
    pub fn new(accounts: [LabAccount; 3]) -> Result<Self, TargetError> {
        let mris: BTreeSet<_> = accounts.iter().map(|account| account.mri.as_str()).collect();
        let upns: BTreeSet<_> = accounts.iter().map(|account| account.upn.to_lowercase()).collect();
        if mris.len() != 3
            || upns.len() != 3
            || accounts.iter().any(|account| {
                !work_mri(&account.mri) || !account.upn.contains('@') || account.upn.trim() != account.upn
            })
        {
            return Err(TargetError::InvalidRoster);
        }
        Ok(Self { accounts })
    }

    /// The exact configured identity for `mri`, for matching a share recipient's freshly resolved email.
    #[must_use]
    pub fn account(&self, mri: &str) -> Option<&LabAccount> {
        self.accounts.iter().find(|account| account.mri == mri)
    }

    /// Checks the active browser/native identity against the expected dedicated account.
    ///
    /// # Errors
    /// [`TargetError::Identity`] on a mismatch or an unknown identity.
    pub fn verify_identity(&self, expected: &str, mri: &str, upn: &str) -> Result<(), TargetError> {
        self.accounts
            .iter()
            .any(|account| account.mri == expected && account.mri == mri && account.upn.eq_ignore_ascii_case(upn))
            .then_some(())
            .ok_or(TargetError::Identity)
    }

    /// Checks a freshly read full recipient list. It must include the acting lab account and at least one peer.
    ///
    /// # Errors
    /// [`TargetError::Recipients`] on any outsider (including a bot/echo/personal contact), duplicate or missing self.
    pub fn verify_recipients(&self, own: &str, recipients: &[String]) -> Result<(), TargetError> {
        let members: BTreeSet<_> = recipients.iter().map(String::as_str).collect();
        if recipients.len() < 2
            || members.len() != recipients.len()
            || !members.contains(own)
            || members
                .iter()
                .any(|mri| !self.accounts.iter().any(|account| account.mri == *mri))
        {
            return Err(TargetError::Recipients);
        }
        Ok(())
    }

    /// Checks the intended/selected conversation and its fresh full members before a call, message or share.
    ///
    /// # Errors
    /// [`TargetError::Conversation`] on a missing/mismatched thread; otherwise [`TargetError::Recipients`].
    pub fn verify_chat(
        &self,
        own: &str,
        expected: &str,
        selected: &str,
        recipients: &[String],
    ) -> Result<(), TargetError> {
        if expected.is_empty() || expected != selected {
            return Err(TargetError::Conversation);
        }
        self.verify_recipients(own, recipients)
    }
}

/// The deterministic 1:1 thread of two distinct work accounts.
#[must_use]
pub fn one_to_one_id(a: &str, b: &str) -> Option<String> {
    if !work_mri(a) || !work_mri(b) || a == b {
        return None;
    }
    let mut ids = [a.strip_prefix("8:orgid:")?, b.strip_prefix("8:orgid:")?];
    ids.sort_unstable();
    Some(format!("19:{}_{}@unq.gbl.spaces", ids[0], ids[1]))
}

fn work_mri(mri: &str) -> bool {
    mri.strip_prefix("8:orgid:").is_some_and(|id| {
        let groups: Vec<_> = id.split('-').collect();
        groups.len() == 5
            && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, count)| {
                group.len() == count
                    && group
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
    })
}

/// Reads a fresh response from `GET /v1/threads/<expected>` or its `/members` endpoint. No page marker,
/// hidden/deleted member, conflicting count, missing id, duplicate or malformed work MRI is accepted.
///
/// # Errors
/// [`TargetError::Membership`] whenever the response cannot prove complete exact-target membership.
pub fn full_members(expected: &str, answer: &Value, members_endpoint: bool) -> Result<Vec<String>, TargetError> {
    let object = answer.as_object().ok_or(TargetError::Membership)?;
    if expected.is_empty()
        || object.get("isDeleted").is_some_and(|value| value != false)
        || object.get("hidden").is_some_and(|value| value != false)
        || page_marker(answer)
        || (!members_endpoint && object.get("id").and_then(Value::as_str) != Some(expected))
    {
        return Err(TargetError::Membership);
    }
    let listed = object
        .get("members")
        .and_then(Value::as_array)
        .ok_or(TargetError::Membership)?;
    let count = u64::try_from(listed.len()).map_err(|_| TargetError::Membership)?;
    if members_endpoint
        && (object.get("isDeleted") != Some(&Value::Bool(false))
            || object.get("totalMemberCount").and_then(Value::as_u64) != Some(count))
    {
        return Err(TargetError::Membership);
    }
    for (key, value) in object.iter().chain(
        object
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten(),
    ) {
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "membercount" | "memberscount" | "totalmembercount"
        ) && value.as_u64().or_else(|| value.as_str()?.parse().ok()) != Some(count)
        {
            return Err(TargetError::Membership);
        }
    }
    let members: Vec<String> = listed
        .iter()
        .map(|member| {
            let id = member
                .get("id")
                .and_then(Value::as_str)
                .ok_or(TargetError::Membership)?;
            if !work_mri(id)
                || member.get("hidden").is_some_and(|value| value != false)
                || member.get("isDeleted").is_some_and(|value| value != false)
            {
                return Err(TargetError::Membership);
            }
            Ok(id.to_owned())
        })
        .collect::<Result<_, _>>()?;
    if members.iter().collect::<BTreeSet<_>>().len() != members.len() {
        return Err(TargetError::Membership);
    }
    if expected.ends_with("@unq.gbl.spaces") {
        let raw = expected
            .strip_prefix("19:")
            .and_then(|id| id.strip_suffix("@unq.gbl.spaces"))
            .and_then(|pair| pair.split_once('_'))
            .ok_or(TargetError::Membership)?;
        let a = format!("8:orgid:{}", raw.0);
        let b = format!("8:orgid:{}", raw.1);
        if one_to_one_id(&a, &b).as_deref() != Some(expected)
            || members.len() != 2
            || !members.contains(&a)
            || !members.contains(&b)
        {
            return Err(TargetError::Membership);
        }
    }
    Ok(members)
}

fn page_marker(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            key.contains("next")
                || key.contains("continuation")
                || key.contains("skiptoken")
                || key.contains("syncstate")
                || page_marker(value)
        }),
        Value::Array(values) => values.iter().any(page_marker),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
