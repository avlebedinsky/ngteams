//! Strict decoding of calling delivery objects.

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// A request body as one JSON value, read so that the fence and the service can't read it differently: `None` for
/// anything but a single JSON value, and for a body in which any object has two keys a case-insensitive reader could
/// take for one (an exact duplicate, `members` beside `Members`, `id` beside `ID`). A .NET service may bind keys in any
/// case and keep either duplicate, so such a body is ambiguous and stays unreadable. With every key unique in any case,
/// an exact lookup ([`Value::get`]) finds the one key the service binds, or nothing: a key the fence would miss only
/// because of its case (`Members` alone) reads as missing, which a row then refuses ([`optional`]).
pub(crate) fn strict_json(body: &[u8]) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_slice(body);
    let value = Strict.deserialize(&mut reader).ok()?;
    reader.end().ok()?;
    Some(value)
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
            if !seen.insert(fold(&key)) {
                return Err(de::Error::custom("two keys alike in some case"));
            }
            let value = map.next_value_seed(Strict)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}
