//! A team's new channel (`docs/protocol/channels.md`, "Creating a channel"): the middle tier's standard channel, the
//! provisioning service's private one (its status read and its people added), a channel shown to everyone, and the user
//! profile service's search of a team's people. Each takes the account's token alone; what was made is kept
//! ([`crate::ChatServiceMock::channel_writes`]) and listed by CSA's teams read from then on. A name the team has is
//! refused with Teams' code; a person named [`NOT_ADDED`] isn't added, told in `multipleStatus`.

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::{json, status};
use crate::world::State;

/// The object id of someone a private channel won't take.
pub const NOT_ADDED: &str = "00000000-0000-4000-8000-0000000000dd";

/// The people on the team, as the search finds them: (MRI, name, mail).
const TEAM_PEOPLE: [(&str, &str, &str); 3] = [
    (crate::LAB_B, "Lab B", "lab-b@lab.example.com"),
    (crate::LAB_C, "Lab C", "lab-c@lab.example.com"),
    (
        "8:orgid:00000000-0000-4000-8000-0000000000dd",
        "Lab D",
        "lab-d@lab.example.com",
    ),
];

/// A write the mock took: its method, its path after the service's base and its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelWrite {
    pub method: String,
    pub path: String,
    pub body: Value,
}

/// What was made, for CSA's read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MadeChannel {
    pub(crate) team: String,
    pub(crate) entry: Value,
    /// A private channel's group.
    group: Option<String>,
}

/// The mock's channels.
#[derive(Debug, Default)]
pub(crate) struct Channels {
    pub(crate) made: Vec<MadeChannel>,
    pub(crate) writes: Vec<ChannelWrite>,
}

/// The answer to `request` when it is one of these; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let method = request.method.as_str();
    let rest = match segments.as_slice() {
        ["api", "mt", "part", _, rest @ ..] if matches!(rest, ["beta", "teams", ..]) => rest,
        ["fabric", _, "templates", "api", rest @ ..] | ["api", "userprofilesvc", _, "api", "v1.0", rest @ ..] => rest,
        _ => return None,
    };
    if let (["api", "mt", ..], [_, _, _, last]) = (segments.as_slice(), rest)
        && !matches!(*last, "channels" | "setDefaultChannel")
    {
        return None;
    }
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    if token.and_then(|token| state.account_by_token(token)).is_none() {
        return Some(status(401));
    }
    let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
    Some(match (method, segments.first().copied(), rest) {
        ("POST", Some("api"), ["beta", "teams", team, "channels"]) => {
            let path = format!("beta/teams/{team}/channels");
            standard(state, team, &path, body)
        }
        ("PUT", Some("api"), ["beta", "teams", channel, "setDefaultChannel"]) => {
            record(state, method, format!("beta/teams/{channel}/setDefaultChannel"), body);
            status(200)
        }
        ("POST", Some("fabric"), ["teams", group, "channels"]) => {
            private(state, format!("teams/{group}/channels"), body)
        }
        ("GET", Some("fabric"), ["teams", _, "channels", channel]) => {
            let found = state
                .channels
                .made
                .iter()
                .find(|made| made.group.as_deref() == Some(*channel))
                .map(|made| made.entry["id"].clone());
            match found {
                Some(thread) => json(200, &json!({ "threadId": thread })),
                None => status(404),
            }
        }
        ("POST", Some("fabric"), ["teams", group, "channels", channel, "users"]) => {
            let failures: Vec<Value> = body["users"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|user| user["id"].as_str())
                .filter(|id| id.contains(NOT_ADDED))
                .map(|id| json!({ "id": id, "statusSubCode": "UserNotFound" }))
                .collect();
            record(state, method, format!("teams/{group}/channels/{channel}/users"), body);
            json(200, &json!({ "multipleStatus": failures }))
        }
        ("POST", Some("api"), ["teams", _, "users", "profilesearch"]) => {
            let keyword = body["keyword"].as_str().unwrap_or_default().to_lowercase();
            let found: Vec<Value> = TEAM_PEOPLE
                .iter()
                .filter(|(_, name, _)| name.to_lowercase().contains(&keyword))
                .map(|(mri, name, mail)| json!({ "mri": mri, "displayName": name, "email": mail }))
                .collect();
            json(200, &Value::Array(found))
        }
        _ => status(404),
    })
}

fn record(state: &mut State, method: &str, path: String, body: Value) {
    state.channels.writes.push(ChannelWrite {
        method: method.to_owned(),
        path,
        body,
    });
}

fn standard(state: &mut State, team: &str, path: &str, body: Value) -> ResponseTemplate {
    let name = body["displayName"].as_str().unwrap_or_default().to_owned();
    let taken = state
        .channels
        .made
        .iter()
        .any(|made| made.entry["displayName"] == name.as_str())
        || name == "A made-up channel";
    if taken {
        return json(409, &json!({ "errorCode": "ChannelNameAlreadyExist" }));
    }
    let thread = format!("19:made{}@thread.tacv2", state.channels.made.len());
    state.channels.made.push(MadeChannel {
        team: team.to_owned(),
        entry: json!({"id": thread, "displayName": name, "channelType": 0, "isGeneral": false, "isDeleted": false}),
        group: None,
    });
    record(state, "POST", path.to_owned(), body);
    json(201, &json!({ "value": { "objectId": thread } }))
}

fn private(state: &mut State, path: String, body: Value) -> ResponseTemplate {
    let name = body["displayName"].as_str().unwrap_or_default().to_owned();
    let team = body["spaceThreadId"].as_str().unwrap_or_default().to_owned();
    let at = state.channels.made.len();
    let thread = format!("19:made{at}@thread.tacv2");
    let group = format!("OID:00000000-0000-4000-8000-0000000000{at:02}@00000000-0000-4000-8000-0000000000fe");
    state.channels.made.push(MadeChannel {
        team,
        entry: json!({"id": thread, "displayName": name, "channelType": 1, "isGeneral": false, "isDeleted": false}),
        group: Some(group.clone()),
    });
    record(state, "POST", path, body);
    json(202, &json!({ "groupId": group }))
}
