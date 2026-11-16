//! The apps added for the account, to chats and to teams (`docs/protocol/apps.md`), made up and kept: the reads of the
//! account's apps, a chat's apps and a team's apps, and the writes that add an app (its definition as the body, `201`)
//! or remove one (`200`). Each place starts with what Teams lists there before a write ([`listed_at_start`]).

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::json;
use crate::world::State;

/// Where an app was added.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppPlace {
    /// For the account itself.
    Personal,
    Chat(String),
    Team(String),
}

/// The apps added, by place, each in the order it was added, and the apps a remove took off a place.
#[derive(Debug, Default)]
pub(crate) struct Installs {
    pub(crate) added: BTreeMap<AppPlace, Vec<String>>,
    removed: BTreeMap<AppPlace, Vec<String>>,
}

/// The made-up account's object id, which its entitlements are listed under.
const OWNER: &str = "00000000-0000-4000-8000-00000000000a";
/// The app Teams keeps at each place for everyone.
const PERMANENT: &str = "00000000-0000-4000-8000-00000000a003";
/// The app the account has as `PreConsented` before a write, as the lab account has Approvals.
pub const PRE_CONSENTED: &str = "00000000-0000-4000-8000-00000000a006";

/// The answer to `request` when it is one of these; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let place = |kind: &str, id: &str| match kind {
        "chats" => AppPlace::Chat(id.to_owned()),
        _ => AppPlace::Team(id.to_owned()),
    };
    match (request.method.as_str(), segments.as_slice()) {
        ("POST", [.., "users", "apps", "aggregatedEntitlements"]) if is_empty_list(&request.body) => {
            Some(json(200, &user_entitlements(state)))
        }
        ("GET", [.., "chats", chat, "apps", "chatentitlements"]) => Some(json(200, &chat_entitlements(state, chat))),
        ("POST", [.., "teams", "apps", "aggregatedEntitlements"]) => {
            Some(json(200, &team_entitlements(state, &request.body)))
        }
        ("POST", [.., "users", "apps", "entitlements"]) => add(state, AppPlace::Personal, &request.body),
        ("DELETE", [.., "users", "apps", "entitlements", app]) => Some(remove(state, &AppPlace::Personal, app)),
        ("POST", [.., kind @ ("chats" | "teams"), id, "apps", "entitlements"]) => {
            add(state, place(kind, id), &request.body)
        }
        ("DELETE", [.., kind @ ("chats" | "teams"), id, "apps", "entitlements", app]) => {
            Some(remove(state, &place(kind, id), app))
        }
        _ => None,
    }
}

fn is_empty_list(body: &[u8]) -> bool {
    serde_json::from_slice::<Vec<Value>>(body).is_ok_and(|list| list.is_empty())
}

fn add(state: &mut State, place: AppPlace, body: &[u8]) -> Option<ResponseTemplate> {
    let definition: Value = serde_json::from_slice(body).ok()?;
    let Some(id) = definition.get("id").and_then(Value::as_str).map(str::to_owned) else {
        return Some(ResponseTemplate::new(400));
    };
    if let Some(removed) = state.apps.removed.get_mut(&place) {
        removed.retain(|gone| *gone != id);
    }
    let added = state.apps.added.entry(place).or_default();
    if !added.contains(&id) {
        added.push(id.clone());
    }
    Some(json(
        201,
        &json!({ "value": { "id": id, "state": "Installed", "installedBy": OWNER } }),
    ))
}

fn remove(state: &mut State, place: &AppPlace, app: &str) -> ResponseTemplate {
    state
        .apps
        .added
        .entry(place.clone())
        .or_default()
        .retain(|id| id != app);
    let removed = state.apps.removed.entry(place.clone()).or_default();
    if !removed.iter().any(|gone| gone == app) {
        removed.push(app.to_owned());
    }
    ResponseTemplate::new(200)
}

fn definition(id: &str) -> Option<Value> {
    crate::team_apps::definitions()
        .into_iter()
        .find(|definition| definition.get("id").and_then(Value::as_str) == Some(id))
}

/// What Teams lists at `place` before a write: the app it keeps for everyone; for the account an app it has as
/// `PreConsented`; for the made-up team ([`crate::CHANNEL`]) two apps added to it, one of them new for everyone.
fn listed_at_start(place: &AppPlace) -> Vec<Value> {
    let permanent = json!({ "id": PERMANENT, "state": "InstalledAndPermanent" });
    match place {
        AppPlace::Personal => vec![permanent, json!({ "id": PRE_CONSENTED, "state": "PreConsented" })],
        AppPlace::Chat(_) => vec![permanent],
        AppPlace::Team(team) if team == crate::CHANNEL => vec![
            json!({ "id": "00000000-0000-4000-8000-00000000a001", "state": "Installed" }),
            json!({ "id": "00000000-0000-4000-8000-00000000a002", "state": "Installed", "isNewGlobalPreInstalledApp": true }),
            permanent,
        ],
        AppPlace::Team(_) => Vec::new(),
    }
}

/// What `place` lists now: what Teams listed there less what a remove took off, then each added app as `Installed`.
fn entitlements(state: &State, place: &AppPlace) -> Vec<Value> {
    let removed = state.apps.removed.get(place);
    let gone = |id: &str| removed.is_some_and(|removed| removed.iter().any(|gone| gone == id));
    let mut listed: Vec<Value> = listed_at_start(place)
        .into_iter()
        .filter(|entitlement| !entitlement.get("id").and_then(Value::as_str).is_some_and(gone))
        .collect();
    for id in state.apps.added.get(place).into_iter().flatten() {
        if !listed
            .iter()
            .any(|entitlement| entitlement.get("id").and_then(Value::as_str) == Some(id))
        {
            listed.push(json!({ "id": id, "state": "Installed" }));
        }
    }
    listed
}

fn user_entitlements(state: &State) -> Value {
    let listed = entitlements(state, &AppPlace::Personal);
    let definitions: serde_json::Map<String, Value> = listed
        .iter()
        .filter_map(|entitlement| {
            let id = entitlement.get("id")?.as_str()?;
            Some((id.to_owned(), definition(id)?))
        })
        .collect();
    json!({ "value": { "userEntitlements": { OWNER: listed }, "definitions": definitions } })
}

fn chat_entitlements(state: &State, chat: &str) -> Value {
    let listed = entitlements(state, &AppPlace::Chat(chat.to_owned()));
    let definitions: Vec<Value> = listed
        .iter()
        .filter_map(|entitlement| definition(entitlement.get("id")?.as_str()?))
        .collect();
    json!({ "value": { "entitlements": listed, "definitions": definitions } })
}

/// The apps of each team the read's `body` names, with every made-up definition.
fn team_entitlements(state: &State, body: &[u8]) -> Value {
    let teams: Vec<String> = serde_json::from_slice(body).unwrap_or_default();
    let listed: serde_json::Map<String, Value> = teams
        .into_iter()
        .map(|team| {
            let listed = entitlements(state, &AppPlace::Team(team.clone()));
            (team, Value::Array(listed))
        })
        .collect();
    let definitions: serde_json::Map<String, Value> = crate::team_apps::definitions()
        .into_iter()
        .filter_map(|definition| Some((definition.get("id")?.as_str()?.to_owned(), definition)))
        .collect();
    json!({ "value": { "teamsEntitlements": listed, "definitions": definitions } })
}
