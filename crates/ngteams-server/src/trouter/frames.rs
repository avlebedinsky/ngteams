//! The socket.io 0.9 packets of a Trouter socket (`docs/protocol/trouter.md`), from the service's side: what the
//! client sends, read, and what the service sends, written. A packet is `<type>:<id>:<endpoint>[:<data>]`.

use serde_json::{Value, json};

/// A packet from the client.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum FromClient {
    /// `0::`: the client leaves.
    Disconnect,
    /// `5:[<id>+]::{"name":…,"args":[…]}`: an event; `ack` when it asks for an acknowledgement.
    Event {
        ack: Option<u64>,
        name: String,
        args: Vec<Value>,
    },
    /// `3:::{"id":…,"status":…,…}`: the client's answer to a delivery.
    Response { id: i64, status: u16 },
    /// Anything else: left alone.
    Other,
}

pub(super) fn read(text: &str) -> FromClient {
    let mut parts = text.splitn(4, ':');
    let (Some(kind), Some(id), Some(_endpoint)) = (parts.next(), parts.next(), parts.next()) else {
        return FromClient::Other;
    };
    let data = parts.next().unwrap_or_default();
    match kind {
        "0" => FromClient::Disconnect,
        "5" => event(id, data).unwrap_or(FromClient::Other),
        "3" => response(data).unwrap_or(FromClient::Other),
        _ => FromClient::Other,
    }
}

fn event(id: &str, data: &str) -> Option<FromClient> {
    let ack = match id.strip_suffix('+') {
        Some(id) => Some(id.parse().ok()?),
        None => None,
    };
    let value: Value = serde_json::from_str(data).ok()?;
    let name = value["name"].as_str()?.to_owned();
    let args = match &value["args"] {
        Value::Array(args) => args.clone(),
        Value::Null => Vec::new(),
        _ => return None,
    };
    Some(FromClient::Event { ack, name, args })
}

fn response(data: &str) -> Option<FromClient> {
    let value: Value = serde_json::from_str(data).ok()?;
    Some(FromClient::Response {
        id: value["id"].as_i64()?,
        status: u16::try_from(value["status"].as_u64()?).ok()?,
    })
}

/// `1::`: the socket is open; the client authenticates now.
pub(super) const CONNECT: &str = "1::";

/// `0::` or `0:::{"reason":…}`: the service closes the socket.
pub(super) fn disconnect(reason: Option<&str>) -> String {
    reason.map_or_else(
        || "0::".to_owned(),
        |reason| format!("0:::{}", json!({"reason": reason})),
    )
}

/// `5:<id>::{"name":…,"args":[…]}`: the service's event number `id`.
pub(super) fn event_out(id: u64, name: &str, argument: &Value) -> String {
    format!("5:{id}::{}", json!({"name": name, "args": [argument]}))
}

/// `6:::<id>+[…]`: the acknowledgement of the client's event `id`.
pub(super) fn ack(id: u64, args: &Value) -> String {
    format!("6:::{id}+{args}")
}

/// `3:::{…}`: a delivery.
pub(super) fn delivery(delivery: &Value) -> String {
    format!("3:::{delivery}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_packets_are_read_by_type() {
        assert_eq!(read("0::"), FromClient::Disconnect);
        assert_eq!(
            read(r#"5:3+::{"name":"ping"}"#),
            FromClient::Event {
                ack: Some(3),
                name: "ping".into(),
                args: vec![]
            }
        );
        assert_eq!(
            read(r#"5:::{"name":"user.authenticate","args":[{"a":":"}]}"#),
            FromClient::Event {
                ack: None,
                name: "user.authenticate".into(),
                args: vec![json!({"a": ":"})]
            }
        );
        assert_eq!(
            read(r#"3:::{"id":-7,"status":200,"headers":{},"body":""}"#),
            FromClient::Response { id: -7, status: 200 }
        );
        for other in [
            "",
            "5",
            "2::",
            "5:x+::{}",
            "5:::not json",
            r#"5:::{"name":1}"#,
            r#"5:::{"name":"x","args":{}}"#,
            r#"3:::{"id":"x"}"#,
            r#"3:::{"id":1,"status":99999}"#,
        ] {
            assert_eq!(read(other), FromClient::Other, "{other}");
        }
    }

    #[test]
    fn service_packets_are_written_as_the_client_reads_them() {
        assert_eq!(disconnect(None), "0::");
        assert_eq!(disconnect(Some("unauthorized")), r#"0:::{"reason":"unauthorized"}"#);
        // The client reads the object by key, and the order of the keys follows the build's `serde_json` features.
        let event = event_out(2, "trouter.message_loss", &json!({"droppedIndicators": []}));
        let data = event
            .strip_prefix("5:2::")
            .and_then(|data| serde_json::from_str::<Value>(data).ok());
        assert_eq!(
            data,
            Some(json!({"name": "trouter.message_loss", "args": [{"droppedIndicators": []}]}))
        );
        assert_eq!(ack(4, &json!(["pong"])), r#"6:::4+["pong"]"#);
        assert_eq!(delivery(&json!({"id": 1})), r#"3:::{"id":1}"#);
    }
}
