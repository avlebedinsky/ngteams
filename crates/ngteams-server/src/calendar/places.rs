//! The room finder's reads (`docs/protocol/calendar.md`, "Rooms"): the rooms the server's configuration lists
//! (`[[rooms]]`), and the room lists they are in. A room is a place an event names, not an account.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::{Me, Params};
use crate::app::App;
use crate::config::RoomConfig;

/// A room list's mailbox: its name in lower case, what isn't a letter or digit a `-`, at the server's host.
fn list_address(app: &App, name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let host = app.config.public_origin.host_str().unwrap_or("localhost");
    format!("{slug}@{host}")
}

fn room(room: &RoomConfig) -> Value {
    let mut place = json!({"displayName": room.name, "emailAddress": room.address});
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            place[key] = value;
        }
    };
    put("capacity", room.capacity.map(Value::from));
    put("building", room.list.clone().map(Value::from));
    put("floorLabel", room.floor.clone().map(Value::from));
    put("address", room.street.clone().map(Value::from));
    put("audioDeviceName", room.audio_device.clone().map(Value::from));
    put("videoDeviceName", room.video_device.clone().map(Value::from));
    put("displayDeviceName", room.display_device.clone().map(Value::from));
    put("isWheelChairAccessible", room.wheelchair.then_some(Value::Bool(true)));
    place
}

/// The page of `places` a read's `$skip` and `$top` ask for.
fn page(places: Vec<Value>, query: &Params) -> Vec<Value> {
    let number = |name: &str, default: usize| query.get(name).and_then(|value| value.parse().ok()).unwrap_or(default);
    places
        .into_iter()
        .skip(number("$skip", 0))
        .take(number("$top", 100).min(1000))
        .collect()
}

/// `GET v2.0/places/roomLists`.
pub(super) async fn lists(State(app): State<Arc<App>>, _: Me, Query(query): Query<Params>) -> Json<Value> {
    let mut names: Vec<&str> = Vec::new();
    for room in &app.config.rooms {
        if let Some(list) = room.list.as_deref()
            && !names.contains(&list)
        {
            names.push(list);
        }
    }
    let lists = names
        .into_iter()
        .map(|name| json!({"displayName": name, "emailAddress": list_address(&app, name)}))
        .collect();
    Json(json!({"roomLists": page(lists, &query)}))
}

/// `GET v2.0/places/rooms`.
pub(super) async fn rooms(State(app): State<Arc<App>>, _: Me, Query(query): Query<Params>) -> Json<Value> {
    let rooms = app.config.rooms.iter().map(room).collect();
    Json(json!({"rooms": page(rooms, &query)}))
}

/// `GET v2.0/places/<list address>/roomList/rooms`.
pub(super) async fn rooms_of(
    State(app): State<Arc<App>>,
    _: Me,
    Path(list): Path<String>,
    Query(query): Query<Params>,
) -> Json<Value> {
    let rooms = app
        .config
        .rooms
        .iter()
        .filter(|room| {
            room.list
                .as_deref()
                .is_some_and(|name| list_address(&app, name).eq_ignore_ascii_case(&list))
        })
        .map(room)
        .collect();
    Json(json!({"rooms": page(rooms, &query)}))
}

/// Whether `address` is a room's mailbox.
pub(super) fn is_room(app: &App, address: &str) -> bool {
    app.config
        .rooms
        .iter()
        .any(|room| room.address.eq_ignore_ascii_case(address))
}
