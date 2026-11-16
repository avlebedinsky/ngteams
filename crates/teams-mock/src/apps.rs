//! The apps added for the account, to chats and to teams (`docs/protocol/apps.md`), made up and kept: the Apps page's
//! reads of the account's apps and a chat's apps, and the writes that add an app (its definition as the body, `201`) or
//! remove one (`200`). A team's apps read stays the fixed one of `team_apps.rs`; a team's writes are only recorded.

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

/// The apps added, by place, each in the order it was added.
#[derive(Debug, Default)]
pub(crate) struct Installs {
    pub(crate) added: BTreeMap<AppPlace, Vec<String>>,
}

/// The made-up account's object id, which its entitlements are listed under.
const OWNER: &str = "00000000-0000-4000-8000-00000000000a";

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
    ResponseTemplate::new(200)
}

fn definition(id: &str) -> Option<Value> {
    crate::team_apps::definitions()
        .into_iter()
        .find(|definition| definition.get("id").and_then(Value::as_str) == Some(id))
}

/// Each added app as `Installed`, after one Teams keeps for everyone.
fn entitlements(added: &[String]) -> Vec<Value> {
    let mut listed = vec![json!({ "id": "00000000-0000-4000-8000-00000000a003", "state": "InstalledAndPermanent" })];
    listed.extend(added.iter().map(|id| json!({ "id": id, "state": "Installed" })));
    listed
}

fn user_entitlements(state: &State) -> Value {
    let added = state.apps.added.get(&AppPlace::Personal).cloned().unwrap_or_default();
    let listed = entitlements(&added);
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
    let added = state
        .apps
        .added
        .get(&AppPlace::Chat(chat.to_owned()))
        .cloned()
        .unwrap_or_default();
    let listed = entitlements(&added);
    let definitions: Vec<Value> = listed
        .iter()
        .filter_map(|entitlement| definition(entitlement.get("id")?.as_str()?))
        .collect();
    json!({ "value": { "entitlements": listed, "definitions": definitions } })
}
