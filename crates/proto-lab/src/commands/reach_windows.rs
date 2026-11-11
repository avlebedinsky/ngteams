//! Read-only operator evidence before a signed-in engine starts. This does not change the installed client's policy.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::authz::{AuthzClient, CHAT_BASE_GTM, Region};
use teams_auth::secrets::KeyringSecretStore;
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient};
use teams_proto::chat::{ChatReads, Field, MemberList};
use teams_proto::{Client, Credentials, HttpTransport};

use crate::commands::auth::{KEYRING_APPLICATION, NativeTarget};
use crate::config::{Alias, LabConfig};

type Error = Box<dyn std::error::Error>;

/// A data-free reason that operator reach evidence is incomplete.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Refused(&'static str);

fn refused(reason: &'static str) -> Error {
    Box::new(Refused(reason))
}

/// Lists every chat page and its actual members using only the account's typed GET paths.
///
/// # Errors
/// Identity, service reads, incomplete rosters, paging or private evidence publication fails.
pub async fn audit(config: &LabConfig, alias: Alias, target: &NativeTarget, selected: &[String]) -> Result<(), Error> {
    let client = native_client(config, alias, target).await?;
    let reads = ChatReads::new(&client);
    audit_reads(config, alias, selected, &reads).await
}

async fn audit_reads<C: Credentials>(
    config: &LabConfig,
    alias: Alias,
    selected: &[String],
    reads: &ChatReads<'_, C>,
) -> Result<(), Error> {
    let account = config.account(alias);
    let allowed = allowed(config);
    let mut page = reads.conversations().await?;
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    loop {
        if !page.unreadable.is_empty() {
            return Err(refused("conversation list contains unreadable entries"));
        }
        for chat in &page.conversations {
            // The documented 48: namespace addresses this account's own streams, not participant threads.
            if chat.id.starts_with("48:") {
                rows.push(json!({"chat":chat.id,"complete":true,"participants_allowed":true,"source":"account-private-stream","members":[{"mri":account.mri,"allowed_as":alias.as_str()}]}));
                continue;
            }
            crate::private::atomic(
                &config.paths().data_dir().join(format!("reach-{alias}-pending.json")),
                &serde_json::to_vec(&json!({"chat":chat.id,"stage":"members"}))?,
            )?;
            let members = reads.members(&chat.id).await?;
            let (complete, participants_allowed, people) = judge(&members, &allowed);
            rows.push(json!({"chat":chat.id,"complete":complete,"participants_allowed":participants_allowed,"members":people}));
        }
        let Some(link) = page.forward_link().map(str::to_owned) else {
            break;
        };
        if !seen.insert(link.clone()) || seen.len() > 20 {
            return Err(refused("conversation paging did not terminate"));
        }
        page = reads.conversations_from(&link).await?;
    }
    let pending = config.paths().data_dir().join(format!("reach-{alias}-pending.json"));
    if pending.exists() {
        std::fs::remove_file(pending)?;
    }
    let rosters_complete = rows.iter().all(|row| row["complete"] == true);
    let all_allowed = rows.iter().all(|row| row["participants_allowed"] == true);
    let selected_checked = selected_allowed(&rows, selected);
    let evidence = json!({"alias":alias.as_str(),"all_rosters_complete":rosters_complete,
        "all_participants_allowed":all_allowed,"selected":selected,"selected_targets_checked":selected_checked,"chats":rows});
    crate::private::atomic(
        &config.paths().data_dir().join(format!("reach-{alias}.json")),
        &serde_json::to_vec_pretty(&evidence)?,
    )?;
    println!(
        "reach: {} entries reviewed; all rosters complete: {rosters_complete}; all people allowed: {all_allowed}; {} selected targets checked: {selected_checked}",
        rows.len(),
        selected.len()
    );
    if !selected_checked {
        return Err(refused(
            "a selected target has incomplete or unlisted participants; do not use it",
        ));
    }
    Ok(())
}

type NativeClient = Client<HttpTransport<Session<KeyringSecretStore>>>;

async fn native_client(config: &LabConfig, alias: Alias, target: &NativeTarget) -> Result<NativeClient, Error> {
    let account = config.account(alias);
    let oid = target.account(&account.upn)?.object_id;
    let expected = account.mri.as_ref().and_then(|mri| mri.strip_prefix("8:orgid:"));
    if !expected.is_some_and(|expected| expected.eq_ignore_ascii_case(&oid)) {
        return Err(refused("native identity does not match roster MRI"));
    }
    let region = Region::load(&target.root().join(format!("regions/{oid}.json")))?;
    let base = region
        .base(CHAT_BASE_GTM)
        .ok_or_else(|| refused("missing chat service base"))?
        .to_owned();
    let store = KeyringSecretStore::new(KEYRING_APPLICATION).await?;
    let session = Arc::new(Session::new(
        &account.upn,
        store,
        TokenClient::new(EntraConfig::teams_native())?,
        AuthzClient::new()?,
        region,
    ));
    Ok(Client::new(HttpTransport::new(session, &base)?))
}

fn allowed(config: &LabConfig) -> BTreeMap<String, String> {
    let accounts = config
        .accounts()
        .iter()
        .filter_map(|a| a.mri.as_ref().map(|mri| (mri.to_lowercase(), a.alias.to_string())));
    let contacts = config
        .contacts()
        .iter()
        .map(|c| (c.mri.to_lowercase(), "own-account".into()));
    let services = config
        .service_participants()
        .iter()
        .filter_map(|s| s.mri.as_ref().map(|mri| (mri.to_lowercase(), s.name.clone())));
    accounts.chain(contacts).chain(services).collect()
}

fn judge(members: &MemberList, allowed: &BTreeMap<String, String>) -> (bool, bool, Vec<Value>) {
    let complete = matches!(members.total_member_count, Field::Known(n) if usize::try_from(n).ok() == Some(members.members.len()))
        && !members.members.is_empty();
    let mut participants_allowed = true;
    let people = members
        .members
        .iter()
        .map(|member| {
            let label = allowed
                .get(&member.id.to_lowercase())
                .map(String::as_str)
                .or_else(|| member.id.starts_with("28:").then_some("bot"));
            participants_allowed &= label.is_some();
            json!({"mri":member.id,"allowed_as":label})
        })
        .collect();
    (complete, participants_allowed, people)
}

fn selected_allowed(rows: &[Value], selected: &[String]) -> bool {
    selected.iter().all(|id| {
        rows.iter()
            .any(|row| row["chat"] == *id && row["complete"] == true && row["participants_allowed"] == true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_rosters_and_unlisted_people_remain_unchecked() {
        let allowed = BTreeMap::from([("8:orgid:synthetic-a".into(), "a".into())]);
        for (body, safe) in [
            (
                json!({"members":[{"id":"8:orgid:synthetic-a"},{"id":"28:synthetic-bot"}],"totalMemberCount":2}),
                true,
            ),
            (
                json!({"members":[{"id":"8:orgid:synthetic-other"}],"totalMemberCount":1}),
                false,
            ),
            (
                json!({"members":[{"id":"8:orgid:synthetic-a"}],"totalMemberCount":2}),
                false,
            ),
            (json!({"members":[{"id":"8:orgid:synthetic-a"}]}), false),
            (json!({"members":[],"totalMemberCount":0}), false),
        ] {
            let members: MemberList = serde_json::from_value(body).unwrap();
            let (complete, permitted, _) = judge(&members, &allowed);
            assert_eq!(complete && permitted, safe);
        }
    }
    #[test]
    fn visible_outside_channel_is_not_a_run_target_and_cannot_be_selected_as_one() {
        let rows = vec![
            json!({"chat":"synthetic-ab","complete":true,"participants_allowed":true}),
            json!({"chat":"synthetic-outside-channel","complete":true,"participants_allowed":false}),
        ];
        assert!(selected_allowed(&rows, &["synthetic-ab".into()]));
        assert!(!selected_allowed(&rows, &["synthetic-outside-channel".into()]));
        assert!(!selected_allowed(&rows, &["missing".into()]));
    }
}

#[cfg(test)]
#[path = "../../tests/support/commands/reach_windows.rs"]
mod orchestration_tests;
