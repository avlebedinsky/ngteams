//! The tenant's meeting rooms, as the scheduling form's room finder reads them from the middle tier
//! (`docs/protocol/calendar.md`, "Rooms"): its room lists (buildings) and the rooms of the tenant or of one list. The
//! paths and the answers' keys are the web client's (its calendar service's `GET_ROOM_LISTS`, `GET_ALL_ROOMS` and
//! `GET_ROOMS_IN_ROOM_LIST`, read from the lab's copy of its code); the lab tenant has no rooms, so no answer with one
//! was captured, and a room's other fields are read by the names Microsoft Graph documents for a room (`capacity`,
//! `building`, `floorNumber`, `floorLabel`, `isWheelChairAccessible`, `audioDeviceName`, `videoDeviceName`,
//! `displayDeviceName`), leniently.
//!
//! - [`RoomLists`]: `GET v2.0/places/roomLists?$count=true&$top=<n>&$skip=0`, answered `{"roomLists": [<place>]}`.
//! - [`Rooms`]: `GET v2.0/places/rooms?$count=true&$top=<n>&$skip=0`, or a list's
//!   `GET v2.0/places/<list address>/roomList/rooms?…`, answered `{"rooms": [<place>]}`.
//!
//! A place is `{"displayName", "emailAddress", "address"?, …}`: its name, its mailbox and its street address.

use serde_json::Value;

use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// How many places a read asks for, as the web client asks when its settings name no other number.
pub const PLACES_PAGE: u32 = 100;

/// A room, or a room list (a building), as the answer gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    /// Its name (`displayName`).
    pub name: String,
    /// Its mailbox (`emailAddress`): a room's is what an event invites to book it.
    pub address: String,
    /// Its street address, as text, when the answer gives one.
    pub street: Option<String>,
    /// How many people it seats.
    pub capacity: Option<u32>,
    /// Its building, by name.
    pub building: Option<String>,
    /// Its floor, by label or number.
    pub floor: Option<String>,
    /// What it is equipped with (audio, video, a display, wheelchair access), in its own words.
    pub features: Vec<String>,
}

impl Place {
    /// A place of the answer; `None` without a name or an address.
    fn of(entry: &Value) -> Option<Self> {
        let text = |key: &str| {
            entry
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let address = text("emailAddress")?;
        let name = text("displayName").unwrap_or_else(|| address.clone());
        let capacity = entry
            .get("capacity")
            .and_then(|capacity| {
                capacity
                    .as_u64()
                    .or_else(|| capacity.as_str().and_then(|text| text.trim().parse().ok()))
            })
            .and_then(|capacity| u32::try_from(capacity).ok())
            .filter(|capacity| *capacity > 0);
        let floor = text("floorLabel").or_else(|| {
            entry
                .get("floorNumber")
                .and_then(Value::as_i64)
                .map(|floor| floor.to_string())
        });
        let mut features: Vec<String> = ["audioDeviceName", "videoDeviceName", "displayDeviceName"]
            .iter()
            .filter_map(|key| text(key))
            .collect();
        if entry.get("isWheelChairAccessible").and_then(Value::as_bool) == Some(true) {
            features.push("Wheelchair accessible".to_owned());
        }
        Some(Self {
            name,
            address,
            street: street(entry.get("address")),
            capacity,
            building: text("building"),
            floor,
            features,
        })
    }
}

/// A place's street address: text as it came, or a Graph `physicalAddress` object's parts joined.
fn street(address: Option<&Value>) -> Option<String> {
    let address = address?;
    if let Some(text) = address.as_str() {
        return Some(text.trim().to_owned()).filter(|text| !text.is_empty());
    }
    let parts: Vec<&str> = ["street", "city", "state", "postalCode", "countryOrRegion"]
        .iter()
        .filter_map(|key| address.get(key)?.as_str())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The places under `key` in the answer `body`; `None` unless it is an object (an absent or empty list is none).
fn places(body: &[u8], key: &str) -> Option<Vec<Place>> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    let entries = answer.as_object()?.get(key).and_then(Value::as_array);
    Some(
        entries
            .map(|entries| entries.iter().filter_map(Place::of).collect())
            .unwrap_or_default(),
    )
}

fn paged(endpoint: Endpoint) -> Endpoint {
    endpoint
        .with_query("$count", "true")
        .with_query("$top", PLACES_PAGE.to_string())
        .with_query("$skip", "0")
}

/// `GET v2.0/places/roomLists` (see the module docs): the tenant's room lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RoomLists;

impl RoomLists {
    /// The room lists the answer `body` gives.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Vec<Place>> {
        places(body, "roomLists")
    }
}

impl Read for RoomLists {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        paged(Endpoint::new(["v2.0", "places", "roomLists"]))
    }
}

/// `GET v2.0/places/rooms`, or one room list's rooms (see the module docs).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rooms {
    /// The room list's address, or `None` for every room of the tenant.
    pub list: Option<String>,
}

impl Rooms {
    /// The rooms the answer `body` gives.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Vec<Place>> {
        places(body, "rooms")
    }
}

impl Read for Rooms {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        paged(match &self.list {
            Some(list) => Endpoint::new(["v2.0", "places", list.as_str(), "roomList", "rooms"]),
            None => Endpoint::new(["v2.0", "places", "rooms"]),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn query(endpoint: &Endpoint) -> Vec<(&str, &str)> {
        endpoint
            .query()
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
    }

    #[test]
    fn the_reads_ask_for_a_page_as_the_web_client_does() {
        let lists = RoomLists.endpoint();
        assert_eq!(lists.segments(), ["v2.0", "places", "roomLists"]);
        assert_eq!(query(&lists), [("$count", "true"), ("$top", "100"), ("$skip", "0")]);
        assert_eq!(Rooms::default().endpoint().segments(), ["v2.0", "places", "rooms"]);
        let listed = Rooms {
            list: Some("building-1@example.com".to_owned()),
        }
        .endpoint();
        assert_eq!(
            listed.segments(),
            ["v2.0", "places", "building-1@example.com", "roomList", "rooms"]
        );
        assert_eq!(RoomLists.service(), Service::MiddleTier);
        assert_eq!(Rooms::default().service(), Service::MiddleTier);
    }

    #[test]
    fn a_room_reads_by_its_name_mailbox_and_what_graph_names_beside() {
        let body = json!({"rooms": [
            {"displayName": "Room 1", "emailAddress": "room-1@example.com", "capacity": 8, "building": "North",
             "floorNumber": 2, "isWheelChairAccessible": true, "videoDeviceName": "Screen",
             "address": {"street": "1 Example Way", "city": "Exampleton"}},
            {"displayName": "Room 2", "emailAddress": "room-2@example.com", "capacity": "4", "floorLabel": "Ground",
             "address": "2 Example Way"},
            {"displayName": "No mailbox"},
            {"emailAddress": "nameless@example.com", "capacity": 0},
        ]})
        .to_string();
        let rooms = Rooms::read_answer(body.as_bytes()).unwrap();
        assert_eq!(rooms.len(), 3);
        assert_eq!(rooms[0].name, "Room 1");
        assert_eq!(rooms[0].capacity, Some(8));
        assert_eq!(rooms[0].building.as_deref(), Some("North"));
        assert_eq!(rooms[0].floor.as_deref(), Some("2"));
        assert_eq!(rooms[0].features, ["Screen", "Wheelchair accessible"]);
        assert_eq!(rooms[0].street.as_deref(), Some("1 Example Way, Exampleton"));
        assert_eq!(rooms[1].capacity, Some(4));
        assert_eq!(rooms[1].floor.as_deref(), Some("Ground"));
        assert_eq!(rooms[1].street.as_deref(), Some("2 Example Way"));
        assert_eq!(rooms[2].name, "nameless@example.com");
        assert_eq!(rooms[2].capacity, None);
    }

    #[test]
    fn an_empty_or_odd_answer_reads_as_no_places_or_none() {
        assert_eq!(RoomLists::read_answer(br#"{"roomLists": []}"#), Some(Vec::new()));
        assert_eq!(RoomLists::read_answer(b"{}"), Some(Vec::new()));
        assert_eq!(Rooms::read_answer(b"[]"), None);
        assert_eq!(Rooms::read_answer(b"not json"), None);
        let lists = RoomLists::read_answer(
            br#"{"roomLists": [{"displayName": "North", "emailAddress": "north@example.com", "address": {}}]}"#,
        )
        .unwrap();
        assert_eq!(lists[0].name, "North");
        assert_eq!(lists[0].street, None);
    }
}
