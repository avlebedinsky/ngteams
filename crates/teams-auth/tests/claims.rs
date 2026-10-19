//! What may be shown of an access token: audience, expiry, scopes and tenant, read from an unsigned JWT built at run
//! time, and nothing for a token that isn't a JWT.

use std::time::{Duration, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::claims::TokenSummary;
use teams_auth::secrets::Secret;

fn jwt(claims: &Value) -> Secret {
    Secret::new(format!(
        "{}.{}.c2ln",
        URL_SAFE_NO_PAD.encode(r#"{"typ":"JWT","alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    ))
}

#[test]
fn the_summary_has_audience_expiry_scopes_and_tenant_only() {
    let token = jwt(&json!({
        "aud": "https://api.spaces.skype.com",
        "exp": 2_000_000_000_u64,
        "scp": "Authorization.ReadWrite Region.ReadWrite user_impersonation",
        "tid": "00000000-0000-4000-8000-0000000000ff",
        "upn": "a@lab.example.test",
    }));
    let summary = TokenSummary::read(&token).expect("a JWT");
    assert_eq!(summary.audience, ["https://api.spaces.skype.com"]);
    assert_eq!(
        summary.expires_at,
        Some(UNIX_EPOCH + Duration::from_secs(2_000_000_000))
    );
    assert_eq!(
        summary.scopes,
        ["Authorization.ReadWrite", "Region.ReadWrite", "user_impersonation"]
    );
    assert_eq!(summary.tenant.as_deref(), Some("00000000-0000-4000-8000-0000000000ff"));
    assert!(!format!("{summary:?}").contains("lab.example.test"), "no other claim");
}

#[test]
fn missing_claims_are_empty_and_a_list_audience_is_kept() {
    let summary = TokenSummary::read(&jwt(&json!({"aud": ["x", "y"]}))).expect("a JWT");
    assert_eq!(summary.audience, ["x", "y"]);
    assert_eq!(summary.expires_at, None);
    assert!(summary.scopes.is_empty());
    assert_eq!(summary.tenant, None);
}

#[test]
fn an_opaque_token_has_no_summary() {
    for token in ["opaque", "a.b", "a.b.c.d", "e30.bm90IGpzb24.c2ln", "e30.WzFd.c2ln"] {
        assert_eq!(TokenSummary::read(&Secret::new(token)), None, "{token}");
    }
}
