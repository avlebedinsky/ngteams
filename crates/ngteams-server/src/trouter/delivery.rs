//! A chat event as an endpoint is told of it: the fan-out's envelope (`{"resourceType":…,"resource":…}`, published on
//! `push:<mri>` by the service that made the event) wrapped as Teams' notification hub wraps it, an `EventMessage`
//! `POST`ed to the endpoint's `messaging` path (`docs/protocol/trouter.md`, "Deliveries").

use serde_json::{Map, Value, json};

use crate::fanout::journal::EntryId;

/// What the push channel does with an envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Delivered, and kept for replay.
    Kept,
    /// Delivered to the sockets open now and never again: typing and other controls.
    Transient,
    /// Others' presence, for the endpoints the envelope names: delivered to the sockets open now, at the endpoint's
    /// presence path (`crate::presence`).
    Presence,
    /// Not a delivery Teams makes: stays inside the server.
    Internal,
}

/// The resource types Teams delivers to an account's messaging endpoints.
const DELIVERED: [&str; 5] = [
    "NewMessage",
    "MessageUpdate",
    "ConversationUpdate",
    "ThreadUpdate",
    "CustomUserProperties",
];

/// What ends the id of a team's or a channel's thread (`crate::teams`).
const CHANNEL_THREAD: &str = "@thread.tacv2";

pub(crate) fn kind(envelope: &Value) -> Kind {
    let resource_type = envelope["resourceType"].as_str().unwrap_or_default();
    if resource_type == crate::presence::RESOURCE_TYPE {
        return Kind::Presence;
    }
    if !DELIVERED.contains(&resource_type) {
        return Kind::Internal;
    }
    let control = envelope["resource"]["messagetype"]
        .as_str()
        .is_some_and(|kind| kind.starts_with("Control/"));
    if control { Kind::Transient } else { Kind::Kept }
}

/// The fan-out's rule ([`crate::fanout::journal::Keep`]): an account's deliverable, non-transient events are kept.
#[must_use]
pub fn keep(topic: &str, payload: &[u8]) -> bool {
    topic.starts_with("push:")
        && serde_json::from_slice::<Value>(payload).is_ok_and(|envelope| kind(&envelope) == Kind::Kept)
}

/// Where a delivery goes and what its links are under.
pub(super) struct Target<'a> {
    /// The endpoint id.
    pub endpoint: &'a str,
    /// The chat service's base (`<origin>/api/chatsvc/<region>`).
    pub chat_base: &'a str,
    /// The public origin's host, with its port.
    pub host: &'a str,
}

/// Which delivery of an envelope this is.
pub(super) enum Which {
    /// The journal's entry: its id is the same on every replay.
    Entry { id: EntryId, stored: bool },
    /// A transient one.
    Once,
}

/// The delivery (`3:::`'s JSON) of `envelope` to `target`.
pub(super) fn delivery(target: &Target<'_>, envelope: &Value, which: &Which) -> Value {
    let body = event_message(target.chat_base, envelope).to_string();
    let (id, guid, request) = match which {
        Which::Entry { id, .. } => (
            number(*id),
            uuid::Uuid::from_u64_pair(id.ms, id.seq),
            uuid::Uuid::from_u64_pair(!id.ms, id.seq),
        ),
        Which::Once => {
            let guid = uuid::Uuid::new_v4();
            (i64::from(guid.as_fields().0.cast_signed()), guid, uuid::Uuid::new_v4())
        }
    };
    let mut request = json!({"id": request.to_string(), "src": "127.0.0.1", "port": 443});
    let mut control = "async; ttl=900; flow=messaging; prio=normal".to_owned();
    let mut timeout = "117000";
    let mut headers = Map::new();
    match which {
        Which::Entry { id, stored: true } => {
            request["mstore"] = json!(true);
            request["flow"] = json!("messaging");
            request["seq"] = json!(id.seq);
            timeout = "7500";
            headers.insert("Connection".into(), json!("keep-alive"));
        }
        Which::Entry { .. } => {}
        Which::Once => control.push_str("; resilience=nostorenoloss"),
    }
    headers.insert("Host".into(), json!(target.host));
    headers.insert("User-Agent".into(), json!("ngteams-server"));
    headers.insert("Content-Type".into(), json!("application/json"));
    headers.insert("Content-Length".into(), json!(body.len().to_string()));
    headers.insert("Trouter-Timeout".into(), json!(timeout));
    headers.insert("X-Trouter-Delivery-Control".into(), json!(control));
    headers.insert("X-Microsoft-Skype-Message-ID".into(), json!(guid.to_string()));
    headers.insert("trouter-request".into(), json!(request.to_string()));
    json!({
        "id": id,
        "method": "POST",
        "url": format!("/trouter/v4/f/{}/messaging", target.endpoint),
        "headers": headers,
        "body": body,
    })
}

/// The presence delivery (`3:::`'s JSON) of `envelope` to `target`, when the envelope names that endpoint: UPS's
/// `POST` to the endpoint's `unifiedPresenceService` path, its body the envelope's resource
/// (`docs/protocol/trouter.md`, "The subscription and its deliveries").
pub(super) fn presence(target: &Target<'_>, envelope: &Value) -> Option<Value> {
    let named = envelope["endpoints"]
        .as_array()?
        .iter()
        .any(|endpoint| endpoint.as_str() == Some(target.endpoint));
    if !named {
        return None;
    }
    let body = envelope["resource"].to_string();
    let guid = uuid::Uuid::new_v4();
    let request = json!({"id": uuid::Uuid::new_v4().to_string(), "src": "127.0.0.1", "port": 443});
    Some(json!({
        "id": i64::from(guid.as_fields().0.cast_signed()),
        "method": "POST",
        "url": format!("/trouter/v4/f/{}/unifiedPresenceService", target.endpoint),
        "headers": {
            "Host": target.host,
            "User-Agent": "Microsoft.Skype.Presence.App/1.0",
            "Content-Type": "application/json",
            "Content-Length": body.len().to_string(),
            "Trouter-Timeout": "117000",
            "X-Trouter-Delivery-Control": "async; ttl=600",
            "X-Microsoft-Skype-Message-ID": "0",
            "trouter-request": request.to_string(),
        },
        "body": body,
    }))
}

/// A delivery's number: a signed 32-bit one, as Teams', the same for an entry every time.
fn number(id: EntryId) -> i64 {
    let mixed = id.ms.wrapping_mul(1000).wrapping_add(id.seq);
    let low = u32::try_from(mixed & u64::from(u32::MAX)).unwrap_or_default();
    i64::from(low.cast_signed())
}

/// A time as Teams writes it: seven fractional digits.
pub(super) fn iso(time: chrono::DateTime<chrono::Utc>) -> String {
    format!("{}0Z", time.format("%Y-%m-%dT%H:%M:%S%.6f"))
}

/// The `EventMessage` around an envelope's resource.
fn event_message(base: &str, envelope: &Value) -> Value {
    let resource_type = envelope["resourceType"].as_str().unwrap_or_default();
    let mut resource = envelope["resource"].clone();
    let id = |resource: &Value| resource["id"].as_str().unwrap_or_default().to_owned();
    let link = match resource_type {
        "NewMessage" | "MessageUpdate" => {
            let chat = resource["conversationid"]
                .as_str()
                .or_else(|| resource["conversationLink"].as_str()?.rsplit('/').next())
                .unwrap_or_default()
                .to_owned();
            if let Value::Object(fields) = &mut resource {
                fields.entry("to").or_insert_with(|| json!(chat));
                // a channel's thread is a `space` or a `topic`, which its message doesn't say
                if !chat.contains(CHANNEL_THREAD) {
                    fields.entry("threadtype").or_insert_with(|| json!("chat"));
                }
            }
            format!("{base}/v1/users/ME/conversations/{chat}/messages/{}", id(&resource))
        }
        "ConversationUpdate" => format!("{base}/v1/users/ME/conversations/{}", id(&resource)),
        "ThreadUpdate" => format!("{base}/v1/threads/{}", id(&resource)),
        _ => format!("{base}/v1/users/ME/properties"),
    };
    json!({
        "time": iso(chrono::Utc::now()),
        "type": "EventMessage",
        "resourceLink": link,
        "resourceType": resource_type,
        "resource": resource,
        "isactive": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_are_kept_transient_or_internal_by_what_teams_delivers() {
        let message = json!({"resourceType": "NewMessage", "resource": {"messagetype": "RichText/Html"}});
        let typing = json!({"resourceType": "NewMessage", "resource": {"messagetype": "Control/Typing"}});
        let receipt = json!({"resourceType": "ReadReceipt", "resource": {}});
        assert_eq!(kind(&message), Kind::Kept);
        assert_eq!(kind(&typing), Kind::Transient);
        assert_eq!(kind(&receipt), Kind::Internal);
        assert_eq!(kind(&json!({})), Kind::Internal);
        let presence = json!({"resourceType": "Presence", "endpoints": ["Endpoint"], "resource": {"presence": []}});
        assert_eq!(kind(&presence), Kind::Presence);
        assert!(!keep("push:8:orgid:a", presence.to_string().as_bytes()));
        let target = |endpoint: &'static str| Target {
            endpoint,
            chat_base: "https://own.example.test/api/chatsvc/own",
            host: "own.example.test",
        };
        let delivery = super::presence(&target("Endpoint"), &presence).unwrap();
        assert_eq!(delivery["url"], "/trouter/v4/f/Endpoint/unifiedPresenceService");
        assert_eq!(delivery["body"], r#"{"presence":[]}"#);
        assert!(super::presence(&target("Other"), &presence).is_none());
        assert!(super::presence(&target("Endpoint"), &json!({"resourceType": "Presence"})).is_none());
        assert!(keep("push:8:orgid:a", message.to_string().as_bytes()));
        assert!(!keep("push:8:orgid:a", typing.to_string().as_bytes()));
        assert!(!keep("push:8:orgid:a", receipt.to_string().as_bytes()));
        assert!(!keep("push:8:orgid:a", b"not json"));
        assert!(!keep("trouter:8:orgid:a", message.to_string().as_bytes()));
    }

    #[test]
    fn a_chats_message_is_delivered_as_a_chats_and_a_channels_names_no_thread_type() {
        let base = "https://own.example.test/api/chatsvc/own";
        let message = |conversation: &str| {
            let link = format!("{base}/v1/users/ME/conversations/{conversation}");
            let envelope = json!({"resourceType": "NewMessage", "resource": {"id": "7", "conversationLink": link}});
            event_message(base, &envelope)
        };
        let chat = message("19:x@thread.v2");
        assert_eq!(chat["resource"]["threadtype"], "chat");
        assert_eq!(chat["resource"]["to"], "19:x@thread.v2");
        let reply = message("19:y@thread.tacv2;messageid=5");
        assert_eq!(reply["resource"].get("threadtype"), None);
        assert_eq!(reply["resource"]["to"], "19:y@thread.tacv2;messageid=5");
        assert_eq!(
            reply["resourceLink"],
            format!("{base}/v1/users/ME/conversations/19:y@thread.tacv2;messageid=5/messages/7")
        );
    }

    #[test]
    fn a_stored_delivery_keeps_its_number_and_says_it_is_stored() {
        let target = Target {
            endpoint: "Endpoint",
            chat_base: "https://own.example.test/api/chatsvc/own",
            host: "own.example.test",
        };
        let envelope = json!({"resourceType": "ThreadUpdate", "resource": {"id": "19:x@thread.v2"}});
        let id = EntryId {
            ms: 1_700_000_000_000,
            seq: 2,
        };
        let live = delivery(&target, &envelope, &Which::Entry { id, stored: false });
        let stored = delivery(&target, &envelope, &Which::Entry { id, stored: true });
        assert_eq!(live["id"], stored["id"]);
        assert!(i32::try_from(live["id"].as_i64().unwrap()).is_ok());
        assert_eq!(live["url"], "/trouter/v4/f/Endpoint/messaging");
        let request = |delivery: &Value| {
            serde_json::from_str::<Value>(delivery["headers"]["trouter-request"].as_str().unwrap()).unwrap()
        };
        assert_eq!(request(&stored)["mstore"], true);
        assert!(request(&live).get("mstore").is_none());
        let body: Value = serde_json::from_str(stored["body"].as_str().unwrap()).unwrap();
        assert_eq!(
            body["resourceLink"],
            "https://own.example.test/api/chatsvc/own/v1/threads/19:x@thread.v2"
        );
        let once = delivery(&target, &envelope, &Which::Once);
        assert!(
            once["headers"]["X-Trouter-Delivery-Control"]
                .as_str()
                .unwrap()
                .ends_with("nostorenoloss")
        );
    }
}
