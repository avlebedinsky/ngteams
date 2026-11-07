//! Strict JSON decoding for recorded protocol shapes.
use std::collections::HashSet;
use std::fmt;

use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
/// A body as one JSON value: `None` for anything but a single JSON value, and for a body in which any object has two keys a
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

#[cfg(test)]
mod tests {
    use serde::de::DeserializeSeed as _;
    use serde::de::value::{BytesDeserializer, Error, StringDeserializer};
    use serde_json::json;

    use super::{Strict, strict_json};

    #[test]
    fn every_json_value_kind_reads_as_itself() {
        let body = br#"{"t":true,"f":false,"n":null,"i":-1,"u":1,"x":1.5,"s":"a\"b","a":[{"k":"v"}]}"#;
        assert_eq!(
            strict_json(body),
            Some(json!({"t":true,"f":false,"n":null,"i":-1,"u":1,"x":1.5,"s":"a\"b","a":[{"k":"v"}]}))
        );
    }

    #[test]
    fn keys_alike_in_any_case_or_trailing_text_refuse_the_body() {
        for body in [
            &br#"{"key":1,"KEY":2}"#[..],
            br#"{"outer":{"\u017f":1,"s":2}}"#,
            br#"[{"k":1,"\u212a":2}]"#,
            br#"{"key":1} {"key":2}"#,
            b"",
        ] {
            assert_eq!(strict_json(body), None, "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn an_owned_string_reads_as_a_string_and_bytes_are_refused_by_name() {
        let owned = Strict.deserialize(StringDeserializer::<Error>::new("synthetic".to_owned()));
        assert_eq!(owned.unwrap(), json!("synthetic"));
        let refused = Strict
            .deserialize(BytesDeserializer::<Error>::new(b"synthetic"))
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains("JSON whose objects have no two keys alike in any case"),
            "{refused}"
        );
    }
}
