//! Strict decoding of calling delivery objects.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// A calling body as one unambiguous JSON value for tracking and service bindings: `None` for
/// anything but a single JSON value, and for a body in which any object has two keys a case-insensitive reader could
/// take for one (an exact duplicate, `members` beside `Members`, `id` beside `ID`). A .NET service may bind keys in any
/// case and keep either duplicate, so such a body is ambiguous and stays unreadable. With every key unique in any case,
/// an exact lookup ([`Value::get`]) finds the one key the service binds, or nothing. An unsupported key spelling
/// (`Members` alone) remains unrecognized by the tracker.
pub(crate) fn strict_json(body: &[u8]) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_slice(body);
    let value = Strict.deserialize(&mut reader).ok()?;
    reader.end().ok()?;
    Some(value)
}

/// The one key a body may name twice, in the same spelling, its last value kept ([`Strict`]'s `visit_map`).
const LAST_WINS: &str = "customContext";

/// What [`strict_json`]'s refusal of a body that names a key twice starts with, the key after it.
const TWICE: &str = "two keys alike in some case: ";

/// The key `body` names twice (in some case), when that is why [`strict_json`] refuses it: a key's name, never a
/// value.
pub(crate) fn twice_named(body: &[u8]) -> Option<String> {
    let mut reader = serde_json::Deserializer::from_slice(body);
    let error = Strict.deserialize(&mut reader).err()?.to_string();
    let rest = error.strip_prefix(TWICE)?;
    // serde_json appends " at line L column C"
    Some(rest.rsplit_once(" at line ").map_or(rest, |(key, _)| key).to_owned())
}

/// A key as a case-insensitive reader compares it: upper-cased, then lower-cased, so `ſ` and `K` (Kelvin) fold with
/// `s` and `k`. Where it folds more than such a reader would (`ß` and `ss`), a body is refused that didn't need to be.
pub(crate) fn fold(key: &str) -> String {
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
            // a transferred call's invitation names `customContext` twice (seen 2026-11-06, native a → b transferred
            // to c): the service's own context, which nothing here reads, taken as the web client's `JSON.parse`
            // takes it (the last); any other key twice is ambiguous
            let again = key == LAST_WINS && object.contains_key(LAST_WINS);
            if !seen.insert(fold(&key)) && !again {
                return Err(de::Error::custom(format!("{TWICE}{key}")));
            }
            let value = map.next_value_seed(Strict)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}
