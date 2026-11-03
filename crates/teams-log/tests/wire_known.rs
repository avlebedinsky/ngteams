//! Known exchange credentials are synthetic and short enough to escape the logger's opaque-run heuristic.

use std::time::Duration;

use serde_json::{Value, json};
use teams_log::redact::{REDACTED, UNREADABLE};
use teams_log::wire::{Request, Response};

fn headers(content_type: &str) -> Vec<(String, String)> {
    vec![("content-type".into(), content_type.into())]
}

fn response<'a>(headers: &'a [(String, String)], body: &'a [u8]) -> Response<'a> {
    Response {
        method: "POST",
        url: "https://auth.example.test/token",
        status: 400,
        headers,
        body,
        elapsed: Duration::from_millis(7),
    }
}

#[test]
fn raw_and_encoded_echoes_are_masked_in_every_request_component() {
    let secret = "short /+=%é";
    let encoded = "short+%2f%2B%3d%25%c3%a9";
    let mut headers = headers("text/plain");
    headers.push(("x-explanation".into(), format!("before {encoded} after")));
    headers.push(("authorization".into(), "unlisted-bearer".into()));
    let body = format!("visible before {secret} and {encoded} after");
    let url = format!("https://auth.example.test/token?explanation={encoded}&keep=yes");
    let record = Request {
        method: "POST",
        url: &url,
        headers: &headers,
        body: Some(body.as_bytes()),
    };
    let shown = record.with_secrets(&[secret]).to_string();
    for absent in [secret, encoded, "unlisted-bearer"] {
        assert!(!shown.contains(absent), "{shown}");
    }
    for kept in [
        "keep=yes",
        "x-explanation: before [redacted] after",
        "visible before",
        "after",
    ] {
        assert!(shown.contains(kept), "{shown}");
    }
    assert!(
        record.to_string().contains(secret),
        "ordinary formatting has no exchange context"
    );
}

#[test]
fn percent_escapes_and_form_spaces_can_be_mixed_with_raw_bytes() {
    let headers = headers("text/plain");
    let body = b"keep a+b a%20b %61+%62 %61%20b then c%2525 c%25%32%35 c%25 then end";
    let shown = response(&headers, body)
        .with_secrets(&["a b", "c%25", "c%", ""])
        .to_string();
    assert!(
        shown.ends_with(
            "body keep [redacted] [redacted] [redacted] [redacted] then [redacted] [redacted] [redacted] then end"
        ),
        "{shown}"
    );
}

#[test]
fn empty_values_do_not_match_and_longest_overlapping_value_wins() {
    let headers = headers("text/plain");
    let body = b"keep short-long and short; keep tail";
    let shown = response(&headers, body)
        .with_secrets(&["", "short", "short-long"])
        .to_string();
    assert!(
        shown.ends_with("body keep [redacted] and [redacted]; keep tail"),
        "{shown}"
    );
    assert_eq!(
        response(&headers, body).with_secrets(&[""]).to_string(),
        response(&headers, body).to_string()
    );
}

#[test]
fn json_strings_are_decoded_before_masking_including_nested_json() {
    let secret = "quote\" slash\\ newline\nend";
    let inner = json!({"description": format!("inner before {secret} after"), "keep": 3});
    let body = json!({
        "description": format!("outer before {secret} after"),
        "nested": [{"body": inner.to_string()}],
        "access_token": "a-different-named-credential",
        "visible": true,
    })
    .to_string();
    let headers = headers("application/json");
    let shown = response(&headers, body.as_bytes()).with_secrets(&[secret]).to_string();
    let output: Value = serde_json::from_str(shown.split_once(" body ").unwrap().1).unwrap();
    assert_eq!(output["description"], "outer before [redacted] after");
    let nested: Value = serde_json::from_str(output["nested"][0]["body"].as_str().unwrap()).unwrap();
    assert_eq!(nested["description"], "inner before [redacted] after");
    assert_eq!(nested["keep"], 3);
    assert_eq!(output["access_token"], REDACTED);
    assert_eq!(output["visible"], true);
}

#[test]
fn known_matching_precedes_name_redaction_that_would_split_a_credential() {
    let secret = "sensitive-prefix&code=sensitive-tail";
    let encoded = "sensitive-prefix%26code%3dsensitive-tail";
    let body = json!({
        "description": format!("before {secret} and {encoded} after"),
        "nested": json!({"description": format!("before {secret} after")}).to_string(),
    })
    .to_string();
    let mut headers = headers("application/json");
    headers.push(("x-description".into(), format!("before {secret} and {encoded} after")));
    let shown = response(&headers, body.as_bytes()).with_secrets(&[secret]).to_string();
    assert!(!shown.contains("sensitive-prefix"), "{shown}");
    assert!(!shown.contains("sensitive-tail"), "{shown}");
    assert!(
        shown.contains("x-description: before [redacted] and [redacted] after"),
        "{shown}"
    );
    let output: Value = serde_json::from_str(shown.split_once(" body ").unwrap().1).unwrap();
    assert_eq!(output["description"], "before [redacted] and [redacted] after");
    let nested: Value = serde_json::from_str(output["nested"].as_str().unwrap()).unwrap();
    assert_eq!(nested["description"], "before [redacted] after");
}

#[test]
fn invalid_json_and_binary_bodies_keep_the_existing_fail_closed_format() {
    let secret = "short-secret";
    let json = headers("application/json");
    let malformed = response(&json, b"not json: short-secret")
        .with_secrets(&[secret])
        .to_string();
    assert!(malformed.contains(UNREADABLE), "{malformed}");
    assert!(!malformed.contains(secret));
    let binary = headers("application/octet-stream");
    let shown = response(&binary, b"short-secret").with_secrets(&[secret]).to_string();
    assert!(shown.contains("[application/octet-stream, 12 bytes]"), "{shown}");
    assert!(!shown.contains(secret));
}

#[test]
fn form_bodies_mask_whole_known_values_before_structural_redaction() {
    let headers = headers("application/x-www-form-urlencoded");
    let secret = "sensitive-prefix&code=sensitive-tail";
    let body = b"message=before sensitive-prefix&code=sensitive-tail after&refresh_token=other";
    let shown = response(&headers, body).with_secrets(&[secret]).to_string();
    assert!(
        shown.ends_with("body message=before [redacted] after&refresh_token=[redacted]"),
        "{shown}"
    );
}
