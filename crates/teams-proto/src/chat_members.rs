//! Complete exact-target participant lists; this parser applies no roster admission policy.
use crate::Mri;
use serde_json::Value;
use std::collections::BTreeSet;

/// Reads an exact thread or its complete members response, preserving work, personal and bot identities.
/// A partial, hidden, deleted, duplicate or malformed list is not a complete recipient observation.
#[must_use]
pub fn chat_members(expected: &str, answer: &Value, members_endpoint: bool) -> Option<Vec<Mri>> {
    let object = answer.as_object()?;
    if expected.is_empty()
        || object.get("isDeleted").is_some_and(|value| value != false)
        || object.get("hidden").is_some_and(|value| value != false)
        || page_marker(answer)
        || (!members_endpoint && object.get("id").and_then(Value::as_str) != Some(expected))
    {
        return None;
    }
    let listed = object.get("members")?.as_array()?;
    let count = u64::try_from(listed.len()).ok()?;
    if members_endpoint
        && (object.get("isDeleted") != Some(&Value::Bool(false))
            || object.get("totalMemberCount").and_then(Value::as_u64) != Some(count))
    {
        return None;
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
            return None;
        }
    }
    let members = listed
        .iter()
        .map(|member| {
            let id = member.get("id")?.as_str()?;
            if !valid_mri(id)
                || member.get("hidden").is_some_and(|v| v != false)
                || member.get("isDeleted").is_some_and(|v| v != false)
            {
                return None;
            }
            Some(Mri::new(id))
        })
        .collect::<Option<Vec<_>>>()?;
    if members.iter().collect::<BTreeSet<_>>().len() != members.len() {
        return None;
    }
    if expected.ends_with("@unq.gbl.spaces") {
        let pair = crate::one_to_one_pair(expected)?;
        if members.len() != 2 || !pair.iter().all(|person| members.contains(person)) {
            return None;
        }
    }
    Some(members)
}
fn valid_mri(id: &str) -> bool {
    let Some((kind, identity)) = id.split_once(':') else {
        return false;
    };
    !kind.is_empty()
        && kind.bytes().all(|b| b.is_ascii_digit())
        && !identity.is_empty()
        && !id.chars().any(char::is_whitespace)
        && !id.chars().any(char::is_control)
        && (!id.starts_with("8:orgid:") || crate::object_id(id).is_some())
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
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn complete_observations_preserve_personal_and_service_people_without_a_roster() {
        let mut body = json!({"id":"thread","members":[{"id":"8:live:personal"},{"id":crate::TEAMS_ECHO_BOT_MRI}]});
        assert_eq!(chat_members("thread", &body, false).unwrap().len(), 2);
        body["members"][1]["hidden"] = json!(true);
        assert!(chat_members("thread", &body, false).is_none());
    }
    #[test]
    fn completeness_and_exact_thread_are_required() {
        let base = json!({"id":"thread","members":[{"id":"8:live:personal"}],"isDeleted":false,"totalMemberCount":1});
        assert!(chat_members("thread", &base, true).is_some());
        assert!(chat_members("other", &base, false).is_none());
        for field in ["nextLink", "continuationToken", "syncState"] {
            let mut partial = base.clone();
            partial[field] = json!("more");
            assert!(chat_members("thread", &partial, true).is_none());
        }
        let mut duplicate = base.clone();
        duplicate["members"] = json!([{"id":"8:live:personal"},{"id":"8:live:personal"}]);
        duplicate["totalMemberCount"] = json!(2);
        assert!(chat_members("thread", &duplicate, true).is_none());
    }
}
