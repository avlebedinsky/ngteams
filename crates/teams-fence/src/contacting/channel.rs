//! The writes that make a team's channel (`docs/protocol/channels.md`, "Creating a channel"), as the web client's
//! "Create a channel" dialog sent them (`channel-create`, held back): a standard channel on the middle tier, a private
//! one on the provisioning service, the people added to a private channel on the same service, and a channel shown to
//! the whole team by default. Each reaches the team, so each is a write on its [`Scope::Team`] or its [`Scope::Channel`],
//! among whose members are Teams' own apps (`docs/protocol/teams.md`, "The fence"): the lab's fence denies them all, the
//! personal build's lets them go, logged.
//! Anything else on these paths stays the service's general rule ([`Contact::Unreadable`]).

use serde_json::Value;

use super::json::strict_json;
use super::{Contact, Parsed, decoded_segments};
use crate::{Mri, Scope, Target, WriteKind};

/// The keys of the middle tier's creation body: `displayName`, `description`, `groupId`, `channelType`,
/// `chatModalityType`, and the bundle's `useOneDriveStorage` for a private channel made there.
const MIDDLE_TIER_KEYS: [&str; 6] = [
    "displayName",
    "description",
    "groupId",
    "channelType",
    "chatModalityType",
    "useOneDriveStorage",
];

/// The keys of the provisioning service's creation body.
const PROVISIONING_KEYS: [&str; 8] = [
    "spaceThreadId",
    "displayName",
    "description",
    "channelType",
    "sharedWithParentTeam",
    "sessionKey",
    "chatModalityType",
    "isNGC",
];

/// The middle tier's row a mutating request matches, if any (after its partition: `/api/mt/part/<partition>/…`).
pub(super) fn middle_tier(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let rest = match segments.as_slice() {
        [api, mt, part, _partition, rest @ ..] if api == "api" && mt == "mt" && part == "part" => rest,
        _ => return None,
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match (request.method.as_str(), rest.as_slice()) {
        ("POST", ["beta", "teams", team, "channels"]) if is_team_thread(team) && request.query.is_empty() => {
            let body = strict_body(request)?;
            let Value::Object(object) = &body else { return None };
            only_keys(object, &MIDDLE_TIER_KEYS)?;
            standard_or_private(object.get("channelType"))?;
            Some(write(
                WriteKind::ChannelChange,
                Scope::Team { id: (*team).to_owned() },
                [],
            ))
        }
        ("PUT", ["beta", "teams", channel, "setDefaultChannel"])
            if is_team_thread(channel) && request.query.is_empty() && request.body.is_none_or(<[u8]>::is_empty) =>
        {
            Some(write(
                WriteKind::ChannelChange,
                Scope::Team {
                    id: (*channel).to_owned(),
                },
                [],
            ))
        }
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
        // the group's warm-up before a creation: it holds nothing back for anyone and names nobody
        ("POST", ["teams", _, "channelsWarmup"])
            if request.query.is_empty() && request.body.is_none_or(<[u8]>::is_empty) =>
        {
            Some(Contact::None)
        }
        ("POST", ["teams", _, "channels"]) if force_sync_only(request) => {
            let body = strict_body(request)?;
            let Value::Object(object) = &body else { return None };
            only_keys(object, &PROVISIONING_KEYS)?;
            standard_or_private(object.get("channelType"))?;
            let team = object
                .get("spaceThreadId")?
                .as_str()
                .filter(|team| is_team_thread(team))?;
            Some(write(WriteKind::ChannelChange, Scope::Team { id: team.to_owned() }, []))
        }
        ("POST", ["teams", team, "channels", channel, "users"]) if force_sync_only(request) => {
            let body = strict_body(request)?;
            let Value::Object(object) = &body else { return None };
            only_keys(object, &["users"])?;
            let added = users(object.get("users")?)?;
            Some(write(
                WriteKind::MemberChange,
                Scope::Channel {
                    team: (*team).to_owned(),
                    channel: (*channel).to_owned(),
                },
                added,
            ))
        }
        _ => None,
    }
}

fn write(kind: WriteKind, scope: Scope, added: impl IntoIterator<Item = Mri>) -> Contact {
    Contact::Write(Target::new(kind, scope).with_added(added))
}

fn strict_body(request: &Parsed<'_>) -> Option<Value> {
    strict_json(request.body?)
}

/// A team's or a channel's thread id: `19:<id>@thread.tacv2` or `19:<id>@thread.skype`, no reply chain.
fn is_team_thread(id: &str) -> bool {
    id.starts_with("19:") && (id.ends_with("@thread.tacv2") || id.ends_with("@thread.skype")) && !id.contains(';')
}

/// Whether the query is `forceSync` alone, a boolean.
fn force_sync_only(request: &Parsed<'_>) -> bool {
    matches!(request.query.as_slice(), [(key, value)] if key == "forceSync" && matches!(value.as_str(), "true" | "false"))
}

fn only_keys(object: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<()> {
    object.keys().all(|key| keys.contains(&key.as_str())).then_some(())
}

/// A channel type the dialog offers: a shared channel names people and teams beyond its own and stays unread.
fn standard_or_private(kind: Option<&Value>) -> Option<()> {
    matches!(kind?.as_str()?, "Standard" | "Private").then_some(())
}

/// The people a members' body adds: each `{"id": "OID:<object id>@<tenant>", "role": <number>}`, by their work
/// account's MRI.
fn users(users: &Value) -> Option<Vec<Mri>> {
    users
        .as_array()
        .filter(|users| !users.is_empty())?
        .iter()
        .map(|user| {
            let user = user.as_object()?;
            only_keys(user, &["id", "role"])?;
            user.get("role")?.as_u64()?;
            let id = user.get("id")?.as_str()?;
            let (oid, tenant) = id.strip_prefix("OID:")?.split_once('@')?;
            (!oid.is_empty() && !tenant.is_empty()).then(|| Mri::new(format!("8:orgid:{oid}")))
        })
        .collect()
}
