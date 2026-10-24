//! What a browser request is, as far as the fence is concerned: the lab rule, **lab accounts contact only each other;
//! everything else is allowed** (CLAUDE.md).
//!
//! [`Classifier::classify`] asks [`teams_fence::contacting`], the one classifier the browser fence and the native
//! transport share, and sorts a request into:
//! - [`Classification::Read`]: a request that can't change anything (`GET`, `HEAD` or `OPTIONS` with no body and no
//!   method override): let through, unlogged;
//! - [`Classification::Allow`]: a mutating request that reaches nobody ([`Contact::None`]: every request to a host that
//!   isn't a contacting service, and the named carve-outs on one): let through, logged to `fence.log` as `allowed`;
//! - [`Classification::Write`]: a mapped write with its [`Target`] ([`Contact::Write`]), judged by
//!   [`teams_fence::Fence::check`] against the target's fresh member list;
//! - [`Classification::Unknown`]: a mutating request on a contacting service that no row maps or carves out
//!   ([`Contact::Unreadable`]): denied.
//!
//! The headers [`teams_fence::contacting`] reads are the method overrides ([`teams_fence::OVERRIDE_HEADERS`]); every
//! other header is passed by name alone. The web client's test call start is a write only with the echo service's
//! MRI ([`Classifier::with_test_call_callee`]); without it, it is unknown.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
/// The chat service path of a request URL after its base, from `v1` on, each segment percent-decoded; `None` for
/// anything but the chat service. It is teams-fence's, so the mapping and the live member read recognize the chat
/// service by one check.
pub use teams_fence::chat_service_path;
use teams_fence::{Contact, Mri, Policy, Request, Target, contacting};

/// What the fence makes of one request (or one WebSocket frame: [`crate::fence_ws::classify_frame`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// A request that can't change anything: let through.
    Read,
    /// A mutating request (or a frame) that reaches nobody: let through, logged as `allowed`.
    Allow,
    /// A mapped write: let through only with a permit from the fence.
    Write(Target),
    /// A mutating request (or a frame) that could reach people and that nothing reads: denied.
    Unknown,
}

/// The classifier: the lab rule's ([`teams_fence::contacting`]), with what it needs to know beyond the request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Classifier {
    policy: Policy,
}

impl Classifier {
    /// The lab rule with no echo callee: a test call start is unknown.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The participant the web client's test call is for, which its start doesn't name: the echo service `lab.toml`
    /// approves ([`Policy::with_echo_callee`]). Without one, a test call start is unknown.
    #[must_use]
    pub fn with_test_call_callee(mut self, callee: Mri) -> Self {
        self.policy = self.policy.with_echo_callee(callee);
        self
    }

    /// Classifies one request whose headers are known as the browser fence pauses it: each header's name, and its
    /// value when [`teams_fence::contacting`] reads it (a method override) and the event gave one as a string. A method
    /// override whose value isn't held reads as one naming another method. `body` is `Some` (even empty) for a request
    /// with a body: [`crate::fence_http::PausedRequest`] passes an empty one for a body Chrome didn't inline.
    #[must_use]
    pub fn classify(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, Option<String>)],
        body: Option<&[u8]>,
    ) -> Classification {
        let mut request = Request::new(method, url).with_body(body);
        for (name, value) in headers {
            request = request.with_header(name, value.as_deref().unwrap_or_default());
        }
        if !request.is_mutating() {
            return Classification::Read;
        }
        match contacting(&request, &self.policy) {
            Contact::None => Classification::Allow,
            Contact::Write(target) => Classification::Write(target),
            Contact::Unreadable => Classification::Unknown,
        }
    }
}

/// Classifies one request with no headers ([`Classifier::new`]).
#[must_use]
pub fn classify(method: &str, url: &str, body: Option<&[u8]>) -> Classification {
    Classifier::new().classify(method, url, &[], body)
}

/// A body as one JSON value, read as the fence's classifier reads one (`teams_fence::contacting`'s reader, which is
/// private to it): `None` for anything but a single JSON value, and for a body in which any object has two keys a
/// case-insensitive reader could take for one. The scenarios read what the web client sent with it.
pub(crate) fn strict_json(body: &[u8]) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_slice(body);
    let value = Strict.deserialize(&mut reader).ok()?;
    reader.end().ok()?;
    Some(value)
}

/// A key as a case-insensitive reader compares it: upper-cased, then lower-cased, so `ſ` and `K` (Kelvin) fold with
/// `s` and `k`. Where it folds more than such a reader would (`ß` and `ss`), a body is refused that didn't need to be.
fn fold(key: &str) -> String {
    key.to_uppercase().to_lowercase()
}

/// [`strict_json`]'s reader: a [`Value`], refusing an object with two keys of one [`fold`].
struct Strict;

impl<'de> DeserializeSeed<'de> for Strict {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON whose objects have no two keys alike in any case")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Strict)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut object = Map::new();
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(fold(&key)) {
                return Err(de::Error::custom("two keys alike in some case"));
            }
            let value = map.next_value_seed(Strict)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}
