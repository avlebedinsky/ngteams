//! A call delivery as a calling endpoint's socket is told of it (`docs/protocol/calling.md`): what the call
//! controller published on the endpoint's topic ([`crate::calling::topic`]), `POST`ed at the endpoint's root (an
//! invitation, or a group call's presence, as JSON) or at a call agent's callback under it (gzip'd in base64, as
//! Teams' call controller sends them). An invitation the endpoint doesn't answer is delivered again
//! ([`REDELIVERIES`] times, [`REDELIVER_AFTER`] apart), as Teams' service does: the socket that wrote it is the one
//! that sees its answer, or none.

use std::collections::VecDeque;
use std::io::Write as _;
use std::time::Duration;

use tokio::time::Instant;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::{Value, json};

/// How many deliveries a socket remembers the call of, for their answers.
const REMEMBERED: usize = 64;
/// How many times an unanswered invitation is delivered again.
pub(super) const REDELIVERIES: u8 = 3;
/// How long an invitation waits for its answer before it is delivered again.
pub(super) const REDELIVER_AFTER: Duration = Duration::from_secs(2);

/// What the call controller publishes for an endpoint.
#[derive(Clone, Debug, Deserialize)]
pub(super) struct Published {
    /// The call it is of.
    pub call: String,
    /// The path under the endpoint's push URL: empty for its root.
    pub path: String,
    /// The delivery's JSON, as text.
    pub body: String,
}

impl Published {
    /// An invitation (`evt` 107 of a 1:1 call, 109 of a group call) at the endpoint's root: the one delivery that
    /// waits for its answer. A group call's presence (`evt` 115, 116) goes to the root too, but nothing answers it.
    fn invitation(&self) -> bool {
        let evt = serde_json::from_str::<Value>(&self.body)
            .ok()
            .and_then(|body| body.get("evt").and_then(Value::as_u64));
        self.path.is_empty() && matches!(evt, Some(107 | 109))
    }
}

/// A socket's call deliveries: their numbers, the call each was of, and the invitations that wait for an answer.
#[derive(Debug, Default)]
pub(super) struct Sent {
    next: i64,
    calls: VecDeque<(i64, String)>,
    unanswered: Vec<Unanswered>,
}

/// An invitation no answer came for yet: the numbers it was delivered under, when it goes again and how often more.
#[derive(Debug)]
struct Unanswered {
    ids: Vec<i64>,
    published: Published,
    due: Instant,
    left: u8,
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

    /// The delivery `id` of `published` went out at `now`: an invitation waits for its answer.
    pub(super) fn sent(&mut self, id: i64, published: &Published, now: Instant) {
        if !published.invitation() {
            return;
        }
        if let Some(waiting) = self
            .unanswered
            .iter_mut()
            .find(|waiting| waiting.published.body == published.body)
        {
            waiting.ids.push(id);
            waiting.due = now + REDELIVER_AFTER;
            waiting.left = waiting.left.saturating_sub(1);
            if waiting.left == 0 {
                self.unanswered.retain(|waiting| waiting.left > 0);
            }
            return;
        }
        self.unanswered.push(Unanswered {
            ids: vec![id],
            published: published.clone(),
            due: now + REDELIVER_AFTER,
            left: REDELIVERIES,
        });
    }

    /// The delivery `id` was answered (whatever its status): its invitation goes no more.
    pub(super) fn answered(&mut self, id: i64) {
        self.unanswered.retain(|waiting| !waiting.ids.contains(&id));
    }

    /// When the next unanswered invitation is due again.
    pub(super) fn next_due(&self) -> Option<Instant> {
        self.unanswered.iter().map(|waiting| waiting.due).min()
    }

    /// The invitations due again at `now`.
    pub(super) fn due(&self, now: Instant) -> Vec<Published> {
        self.unanswered
            .iter()
            .filter(|waiting| waiting.due <= now)
            .map(|waiting| waiting.published.clone())
            .collect()
    }
}

/// `text` gzipped, in base64 (a delivery's `cp`).
pub(crate) fn gzip_base64(text: &str) -> String {
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
        at_root(path, 107)
    }

    fn at_root(path: &str, evt: u64) -> Published {
        Published {
            call: "call-1".to_owned(),
            path: path.to_owned(),
            body: json!({"evt": evt}).to_string(),
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
    fn an_invitation_goes_again_until_it_is_answered_three_times_at_most() {
        let mut sent = Sent::default();
        let start = Instant::now();
        let invitation = published("");
        sent.sent(1, &invitation, start);
        sent.sent(2, &published("callAgent/a/1/call/end/"), start);
        assert_eq!(
            sent.next_due(),
            Some(start + REDELIVER_AFTER),
            "a callback waits for nothing"
        );
        assert!(sent.due(start).is_empty());
        let mut later = start;
        for id in 3..=5 {
            later += REDELIVER_AFTER;
            assert_eq!(sent.due(later).len(), 1);
            sent.sent(id, &invitation, later);
        }
        assert_eq!(sent.next_due(), None, "three times again, no more");
        let mut group = Sent::default();
        group.sent(1, &at_root("", 109), start);
        group.sent(2, &at_root("", 115), start);
        group.sent(3, &at_root("", 116), start);
        assert_eq!(
            group.due(start + REDELIVER_AFTER).len(),
            1,
            "a group call's invitation goes again, its presence does not"
        );
        let mut answered = Sent::default();
        answered.sent(1, &invitation, start);
        answered.sent(2, &invitation, start + REDELIVER_AFTER);
        answered.answered(1);
        assert_eq!(answered.next_due(), None, "an answer to any of its deliveries ends it");
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
