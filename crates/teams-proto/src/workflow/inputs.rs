//! The same trigger-input validation is applied in the form and again against a fresh service definition.
use serde_json::Value;

/// Whether an input value satisfies the declared trigger schema. Unresolved references and unsupported compound
/// schemas are refused; they are never treated as an empty schema. Unknown descriptive metadata is preserved.
#[must_use]
pub fn valid_trigger_inputs(schema: &Value, input: &Value) -> bool {
    validate(schema, input, 0)
}

fn validate(schema: &Value, value: &Value, depth: usize) -> bool {
    if depth > 24
        || !schema.is_object()
        || ["$ref", "oneOf", "anyOf", "allOf", "not", "pattern"]
            .iter()
            .any(|key| schema.get(key).is_some())
    {
        return false;
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.contains(value))
        || schema.get("const").is_some_and(|constant| constant != value)
    {
        return false;
    }
    if let Some(kind) = schema.get("type") {
        let valid = match kind {
            Value::String(kind) => matches_type(kind, value),
            Value::Array(kinds) => kinds
                .iter()
                .filter_map(Value::as_str)
                .any(|kind| matches_type(kind, value)),
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    match value {
        Value::Object(values) => object_valid(schema, values, depth),
        Value::Array(values) => {
            length_valid(schema, "minItems", "maxItems", values.len())
                && schema
                    .get("items")
                    .is_none_or(|item| values.iter().all(|value| validate(item, value, depth + 1)))
                && (!schema.get("uniqueItems").and_then(Value::as_bool).unwrap_or(false)
                    || values.iter().enumerate().all(|(i, value)| !values[..i].contains(value)))
        }
        Value::String(value) => length_valid(schema, "minLength", "maxLength", value.chars().count()),
        Value::Number(number) => number.as_f64().is_some_and(|value| {
            schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_none_or(|min| value >= min)
                && schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_none_or(|max| value <= max)
        }),
        _ => true,
    }
}
fn length_valid(schema: &Value, min: &str, max: &str, length: usize) -> bool {
    schema
        .get(min)
        .and_then(Value::as_u64)
        .is_none_or(|min| length as u64 >= min)
        && schema
            .get(max)
            .and_then(Value::as_u64)
            .is_none_or(|max| length as u64 <= max)
}
fn matches_type(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    }
}
fn object_valid(schema: &Value, values: &serde_json::Map<String, Value>, depth: usize) -> bool {
    if !length_valid(schema, "minProperties", "maxProperties", values.len()) {
        return false;
    }
    if schema
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|required| {
            required
                .iter()
                .any(|key| key.as_str().is_none_or(|key| !values.contains_key(key)))
        })
    {
        return false;
    }
    let properties = schema.get("properties").and_then(Value::as_object);
    values.iter().all(|(key, value)| {
        if let Some(property) = properties.and_then(|properties| properties.get(key)) {
            return validate(property, value, depth + 1);
        }
        match schema.get("additionalProperties") {
            Some(Value::Bool(false)) => false,
            Some(additional @ Value::Object(_)) => validate(additional, value, depth + 1),
            _ => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn required_values_nested_types_and_enums_are_checked() {
        let schema = json!({"type":"object","required":["text","count"],"additionalProperties":false,"properties":{"text":{"type":"string","minLength":1},"count":{"type":"integer","minimum":1},"choice":{"enum":["a","b"]},"items":{"type":"array","items":{"type":"boolean"}}}});
        assert!(valid_trigger_inputs(
            &schema,
            &json!({"text":"Synthetic","count":1,"choice":"a","items":[true,false]})
        ));
        for input in [
            json!({"text":"missing count"}),
            json!({"text":"","count":1}),
            json!({"text":"Synthetic","count":"1"}),
            json!({"text":"Synthetic","count":1,"choice":"c"}),
            json!({"text":"Synthetic","count":1,"extra":true}),
            json!({"text":"Synthetic","count":1,"items":["false"]}),
        ] {
            assert!(!valid_trigger_inputs(&schema, &input));
        }
        assert!(!valid_trigger_inputs(
            &json!({"$ref":"#/definitions/unknown"}),
            &json!({})
        ));
    }
}
