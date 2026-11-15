//! A call delivery as a calling endpoint's socket is told of it (`docs/protocol/calling.md`): what the call
//! controller published on the endpoint's topic ([`crate::calling::topic`]), `POST`ed at the endpoint's root (an
//! invitation, as JSON) or at a call agent's callback under it (gzip'd in base64, as Teams' call controller sends
//! them).

use std::collections::VecDeque;
use std::io::Write as _;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

/// How many deliveries a socket remembers the call of, for their answers.
const REMEMBERED: usize = 64;

/// What the call controller publishes for an endpoint.
#[derive(Debug, Deserialize)]
pub(super) struct Published {
    /// The call it is of.
    pub call: String,
    /// The path under the endpoint's push URL: empty for its root.
    pub path: String,
    /// The delivery's JSON, as text.
    pub body: String,
}

/// A socket's call deliveries: their numbers, and the call each was of.
#[derive(Debug, Default)]
pub(super) struct Sent {
    next: i64,
    calls: VecDeque<(i64, String)>,
}

impl Sent {
    /// The number of the next delivery, of `call`.
    pub(super) fn number(&mut self, call: &str) -> i64 {
        self.next += 1;
        if self.calls.len() == REMEMBERED {
            self.calls.pop_front();
        }
        self.calls.push_back((self.next, call.to_owned()));
        self.next
    }

    /// The call the delivery `id` was of.
    pub(super) fn call_of(&self, id: i64) -> Option<&str> {
        self.calls
            .iter()
            .find(|(sent, _)| *sent == id)
            .map(|(_, call)| call.as_str())
    }
}

fn gzip_base64(text: &str) -> String {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let compressed = encoder
        .write_all(text.as_bytes())
        .and_then(|()| encoder.finish())
        .unwrap_or_default();
    base64::engine::general_purpose::STANDARD.encode(compressed)
}

/// The delivery (`3:::`'s JSON) number `id` of `published` to `endpoint`, on the origin's `host`.
pub(super) fn delivery(host: &str, endpoint: &str, id: i64, published: &Published) -> Value {
    let guid = uuid::Uuid::new_v4();
    let request = json!({"id": uuid::Uuid::new_v4().to_string(), "src": "127.0.0.1", "port": 443});
    let mut headers = json!({
        "Host": host,
        "User-Agent": "ngteams-server",
        "Content-Type": "application/json; charset=utf-8",
        "trouter-request": request.to_string(),
        "X-Microsoft-Skype-Chain-ID": published.call,
        "X-Microsoft-Skype-Message-ID": guid.to_string(),
    });
    let body = if published.path.starts_with("callAgent/") {
        headers["X-Microsoft-Skype-Content-Encoding"] = json!("gzip");
        gzip_base64(&published.body)
    } else {
        published.body.clone()
    };
    headers["Content-Length"] = json!(body.len().to_string());
    json!({
        "id": id,
        "method": "POST",
        "url": format!("/trouter/v4/f/{endpoint}/{}", published.path),
        "headers": headers,
        "body": body,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn published(path: &str) -> Published {
        Published {
            call: "call-1".to_owned(),
            path: path.to_owned(),
            body: r#"{"evt":107}"#.to_owned(),
        }
    }

    #[test]
    fn an_invitation_goes_to_the_root_as_json_and_a_callback_gzipped() {
        let root = delivery("own.example.test", "Endpoint", 1, &published(""));
        assert_eq!(root["url"], "/trouter/v4/f/Endpoint/");
        assert_eq!(root["body"], r#"{"evt":107}"#);
        assert!(root["headers"].get("X-Microsoft-Skype-Content-Encoding").is_none());
        let callback = delivery("own.example.test", "Endpoint", 2, &published("callAgent/a/1/call/end/"));
        assert_eq!(callback["url"], "/trouter/v4/f/Endpoint/callAgent/a/1/call/end/");
        assert_eq!(callback["headers"]["X-Microsoft-Skype-Content-Encoding"], "gzip");
        let packed = base64::engine::general_purpose::STANDARD
            .decode(callback["body"].as_str().unwrap())
            .unwrap();
        let mut text = String::new();
        flate2::read::GzDecoder::new(packed.as_slice())
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, r#"{"evt":107}"#);
    }

    #[test]
    fn a_socket_remembers_the_call_of_its_latest_deliveries() {
        let mut sent = Sent::default();
        let first = sent.number("a");
        assert_eq!(sent.call_of(first), Some("a"));
        for _ in 0..REMEMBERED {
            sent.number("b");
        }
        assert_eq!(sent.call_of(first), None);
        assert_eq!(sent.call_of(first + 1), Some("b"));
    }
}
