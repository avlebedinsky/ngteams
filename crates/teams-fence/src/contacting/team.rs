//! The writes that manage a team (`docs/protocol/teams.md`), as the web client's team pages sent them
//! (`proto-lab run team-manage`). A new private team names nobody ([`WriteKind::TeamCreate`] on [`Scope::New`]), a new
//! team open to the organization has nobody to judge ([`WriteKind::OpenTeamCreate`]), and everything else reaches the
//! team, so it is a write on its [`Scope::Team`], whose members the fence reads fresh; the people a write adds are named
//! on it, judged too. What only changes the account's own view of a team (its channels hidden or shown for the account)
//! reaches nobody. Anything else on these paths stays the service's general rule ([`Contact::Unreadable`]).

use serde_json::Value;

use super::json::strict_json;
use super::{Contact, Parsed, decoded_segments};
use crate::{Mri, Scope, Target, WriteKind};

/// The keys of a new team's body.
const CREATE_KEYS: [&str; 9] = [
    "accessType",
    "description",
    "displayName",
    "displayNamePrefix",
    "displayNameSuffix",
    "isTenantWide",
    "sensitivityLabelId",
    "topicThreadTopic",
    "validationRequired",
];

/// `accessType` of a private team.
const PRIVATE: u64 = 1;

/// The middle tier's row a mutating request matches, if any (after its partition: `/api/mt/part/<partition>/…`).
pub(super) fn middle_tier(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let rest = match segments.as_slice() {
        [api, mt, part, _partition, rest @ ..] if api == "api" && mt == "mt" && part == "part" => rest,
        _ => return None,
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    if let Some(team) = changed_team(&request.method, &rest) {
        return Some(on_team(WriteKind::TeamChange, team));
    }
    match (request.method.as_str(), rest.as_slice()) {
        ("POST", ["beta", "teams", "create"]) if request.query.is_empty() => create(request),
        ("PUT", ["beta", "teams", team, "bulkUpdateRoledMembers"]) => members_added(request, team),
        // reads sent as a `POST`: the people asking to join
        ("POST", ["beta", "teams", "findJoinRequests"]) => Some(Contact::None),
        ("POST", ["beta", "teams", "joinRequest", group]) => Some(on_team(WriteKind::MemberChange, group)),
        ("POST", ["beta", "teams", "joinByCode"]) => {
            let code = strict_json(request.body?)?;
            Some(on_team(WriteKind::MemberChange, code.as_str()?))
        }
        ("POST", ["beta", "teams", "joinRequestAdminAction"]) => join_answer(request),
        ("POST", ["beta", "teams", "sponsorJoinRequest", group]) => members_requested(request, group),
        ("PUT", ["beta", "teams", team, "members"]) => Some(on_team(WriteKind::MemberChange, team)),
        _ => None,
    }
}

/// The team a middle tier write changes for everyone on it: its name and description, deleted, its code.
fn changed_team<'a>(method: &str, rest: &[&'a str]) -> Option<&'a str> {
    match rest {
        ["beta", "teams", team] if method == "PATCH" => Some(team),
        ["beta", "teams", team, "delete"] if method == "DELETE" => Some(team),
        ["beta", "teams", team, "joinCode"] if matches!(method, "PUT" | "DELETE") => Some(team),
        _ => None,
    }
}

/// The provisioning service's row a mutating request matches, if any (after its base: `/fabric/<region>/templates/api`).
pub(super) fn provisioning(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let rest = match segments.as_slice() {
        [fabric, _region, templates, api, rest @ ..]
            if fabric == "fabric" && templates == "templates" && api == "api" =>
        {
            rest
        }
        _ => return None,
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match (request.method.as_str(), rest.as_slice()) {
        // the team's channels hidden for the account alone
        ("PUT", ["team", _, "hide"]) if request.query.is_empty() && request.body.is_none_or(<[u8]>::is_empty) => {
            Some(Contact::None)
        }
        ("PUT", ["conversations", "show" | "hide"]) => Some(Contact::None),
        ("POST", ["groups", group, "archive" | "unarchive"]) => Some(on_team(WriteKind::TeamChange, group)),
        _ => None,
    }
}

/// The chat service's row a mutating request matches, if any: a team's settings, on its thread, and how a channel stands
/// in the account's own list, its interest alone, which reaches nobody.
pub(super) fn chat_service(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let at = segments.iter().position(|segment| segment == "v1")?;
    let rest: Vec<&str> = segments[at + 1..].iter().map(String::as_str).collect();
    match (request.method.as_str(), rest.as_slice()) {
        ("PUT", ["threads", team, "properties"]) if is_team_id(team) && named_settings(request) => {
            Some(on_team(WriteKind::TeamChange, team))
        }
        ("PUT", ["threads", channel, "members", _]) if is_team_id(channel) => {
            let body = strict_json(request.body?)?;
            let object = body.as_object()?;
            (object.len() == 1 && object.contains_key("interest")).then_some(Contact::None)
        }
        _ => None,
    }
}

/// CSA's row a mutating request matches, if any (after its base): a team's tags and who may change them.
pub(super) fn csa(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let at = segments
        .windows(2)
        .position(|pair| pair[0] == "api" && pair[1] == "v1")?;
    let rest: Vec<&str> = segments[at + 2..].iter().map(String::as_str).collect();
    match (request.method.as_str(), rest.as_slice()) {
        ("PUT", ["teams", team, "tagSettings"]) | ("PATCH" | "DELETE", ["teams", team, "tags", _]) => {
            Some(on_team(WriteKind::TeamChange, team))
        }
        ("POST", ["teams", team, "tags"]) => tag_created(request, team),
        _ => None,
    }
}

/// A team's thread id, the only channel kind whose interest the account changes from Manage team.
fn is_team_id(id: &str) -> bool {
    id.starts_with("19:") && id.ends_with("@thread.tacv2") && !id.contains(';')
}

/// Whether the query is `name=spaceAdminSettings` alone.
fn named_settings(request: &Parsed<'_>) -> bool {
    matches!(request.query.as_slice(), [(key, value)] if key == "name" && value == "spaceAdminSettings")
}

/// A new tag names its people, each judged too: `{"tagName", "tagDescription", "memberIds"}`.
fn tag_created(request: &Parsed<'_>, team: &str) -> Option<Contact> {
    let body = strict_json(request.body?)?;
    let object = body.as_object()?;
    let members = object
        .get("memberIds")?
        .as_array()?
        .iter()
        .map(|member| Some(Mri::new(member.as_str()?)))
        .collect::<Option<Vec<_>>>()?;
    Some(Contact::Write(
        Target::new(WriteKind::TeamChange, Scope::Team { id: team.to_owned() }).with_added(members),
    ))
}

/// Someone's request to join answered: `{"GroupId", "UserMri", "ActionType"}`, reaching them.
fn join_answer(request: &Parsed<'_>) -> Option<Contact> {
    let body = strict_json(request.body?)?;
    let object = body.as_object()?;
    let group = object.get("GroupId")?.as_str()?;
    let member = object.get("UserMri")?.as_str()?;
    Some(Contact::Write(
        Target::new(WriteKind::MemberChange, Scope::Team { id: group.to_owned() }).with_added([Mri::new(member)]),
    ))
}

/// A member's request to let people in: their MRIs.
fn members_requested(request: &Parsed<'_>, group: &str) -> Option<Contact> {
    let body = strict_json(request.body?)?;
    let added = body
        .as_array()?
        .iter()
        .map(|member| Some(Mri::new(member.as_str()?)))
        .collect::<Option<Vec<_>>>()?;
    Some(Contact::Write(
        Target::new(WriteKind::MemberChange, Scope::Team { id: group.to_owned() }).with_added(added),
    ))
}

/// A new team: private, or open to the organization when it says anything else.
fn create(request: &Parsed<'_>) -> Option<Contact> {
    let body = strict_json(request.body?)?;
    let object = body.as_object()?;
    object
        .keys()
        .all(|key| CREATE_KEYS.contains(&key.as_str()))
        .then_some(())?;
    let private = object.get("accessType").and_then(Value::as_u64) == Some(PRIVATE)
        && object.get("isTenantWide").and_then(Value::as_bool) != Some(true);
    let kind = if private {
        WriteKind::TeamCreate
    } else {
        WriteKind::OpenTeamCreate
    };
    Some(Contact::Write(Target::new(kind, Scope::New)))
}

/// A write on the team `team`.
fn on_team(kind: WriteKind, team: &str) -> Contact {
    Contact::Write(Target::new(kind, Scope::Team { id: team.to_owned() }))
}

/// People added to `team` or their roles changed: `{"users": [{"mri", "role"}], "groupId"}`.
fn members_added(request: &Parsed<'_>, team: &str) -> Option<Contact> {
    let body = strict_json(request.body?)?;
    let object = body.as_object()?;
    object
        .keys()
        .all(|key| matches!(key.as_str(), "users" | "groupId"))
        .then_some(())?;
    let added = object
        .get("users")?
        .as_array()
        .filter(|users| !users.is_empty())?
        .iter()
        .map(|user| {
            let user = user.as_object()?;
            user.keys()
                .all(|key| matches!(key.as_str(), "mri" | "role"))
                .then_some(())?;
            Some(Mri::new(user.get("mri")?.as_str()?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Contact::Write(
        Target::new(WriteKind::MemberChange, Scope::Team { id: team.to_owned() }).with_added(added),
    ))
}
