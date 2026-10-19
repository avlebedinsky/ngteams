//! Reading an ID token's claims: what counts as a readable token, and that no error quotes it. The tokens are unsigned
//! and built at run time (the fixture guard keeps JWTs out of the repo); the code exchange's account check is in
//! `tests/token.rs`.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::id_token::{IdTokenClaims, IdTokenError};
use teams_auth::secrets::Secret;

const HEADER: &str = r#"{"typ":"JWT","alg":"RS256"}"#;

fn jwt(payload: &[u8]) -> Secret {
    Secret::new(format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(HEADER),
        URL_SAFE_NO_PAD.encode(payload)
    ))
}

fn claims() -> Value {
    json!({
        "aud": "client-1",
        "oid": "00000000-0000-4000-8000-00000000000a",
        "tid": "00000000-0000-4000-8000-0000000000ff",
        "preferred_username": "a@lab.example.test",
    })
}

fn parse(claims: &Value) -> Result<IdTokenClaims, IdTokenError> {
    IdTokenClaims::parse(&jwt(claims.to_string().as_bytes()))
}

#[test]
fn the_account_claims_are_read() {
    let parsed = parse(&claims()).expect("readable");
    assert_eq!(parsed.audiences, ["client-1"]);
    assert_eq!(parsed.object_id, "00000000-0000-4000-8000-00000000000a");
    assert_eq!(parsed.tenant, "00000000-0000-4000-8000-0000000000ff");
    assert_eq!(parsed.preferred_username.as_deref(), Some("a@lab.example.test"));
    assert_eq!(parsed.upn, None);
}

#[test]
fn aud_may_be_a_list() {
    let mut claims = claims();
    claims["aud"] = json!(["client-1", "client-2"]);
    assert_eq!(parse(&claims).expect("readable").audiences, ["client-1", "client-2"]);
}

#[test]
fn a_padded_payload_is_read() {
    let payload = base64::engine::general_purpose::URL_SAFE.encode(serde_json::to_vec(&claims()).expect("json"));
    let token = Secret::new(format!("{}.{payload}.c2ln", URL_SAFE_NO_PAD.encode(HEADER)));
    assert!(IdTokenClaims::parse(&token).is_ok());
}

#[test]
fn required_claims_must_be_non_empty_strings() {
    for (claim, value) in [
        ("aud", Value::Null),
        ("aud", json!([])),
        ("aud", json!("")),
        ("oid", json!("")),
        ("oid", json!(7)),
        ("tid", Value::Null),
    ] {
        let mut claims = claims();
        if value.is_null() {
            claims.as_object_mut().expect("object").remove(claim);
        } else {
            claims[claim] = value;
        }
        let error = parse(&claims).expect_err("refused");
        assert_eq!(error, IdTokenError::MissingClaim(claim), "{claim}");
    }
}

#[test]
fn what_is_not_a_jwt_is_malformed_and_never_quoted() {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims()).expect("json"));
    let not_tokens = [
        "planted-opaque-token-value".to_owned(),
        format!("e30.{payload}"),
        format!("e30.{payload}.c2ln.extra.part"),
        "e30.!!!not-base64!!!.c2ln".to_owned(),
        format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode("[1,2,3]")),
        format!("e30.{}.c2ln", URL_SAFE_NO_PAD.encode("not json at all")),
    ];
    for token in not_tokens {
        let error = IdTokenClaims::parse(&Secret::new(token.clone())).expect_err("malformed");
        assert_eq!(error, IdTokenError::Malformed, "{token}");
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains(&token) && !shown.contains(&payload), "{shown}");
    }
}
