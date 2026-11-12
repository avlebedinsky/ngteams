//! Synthetic malformed and boundary capture values; no real identity, credential or capture is read.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use proto_lab::config::{Alias, LabConfig};
use proto_lab::sanitize::{Identities, KnownAccount, KnownService, sanitize_events};
use serde_json::{Value, json};

#[test]
fn lab_identity_seeding_uses_only_recorded_services_and_preserves_account_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let text = r#"data_dir = "data"
[[accounts]]
alias = "a"
upn = "synthetic-a@lab.example.test"
mri = "8:live:synthetic-a"
[[accounts]]
alias = "b"
upn = "synthetic-b@lab.example.test"
[[accounts]]
alias = "c"
upn = "synthetic-c@lab.example.test"
[[service_participants]]
name = "echo"
mri = "28:00000000-0000-4000-8000-000000000001"
[[service_participants]]
name = "unrecorded"
"#;
    let text = text.replace(
        "data_dir = \"data\"",
        &format!("data_dir = {:?}", dir.path().join("data")),
    );
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    let identities = Identities::from_lab(&config);
    assert_eq!(identities.accounts.len(), 3);
    assert_eq!(identities.accounts[0].alias, Alias::A);
    assert_eq!(identities.services.len(), 1);
    assert_eq!(identities.services[0].name, "echo");
    assert_eq!(identities.tenant_domain.as_deref(), Some("lab.example.test"));
    let out = sanitize_events(
        &[json!({"from":"8:live:synthetic-a","to":"synthetic-b@lab.example.test"})],
        &identities,
    )
    .unwrap();
    assert_eq!(out[0], json!({"from":"<mri:a>","to":"<user:b>"}));
}

#[test]
fn invalid_service_aliases_cannot_impersonate_accounts_or_collide_with_numeric_placeholders() {
    for name in ["", "!!!", "123", "a", "B", "c"] {
        let identities = Identities {
            accounts: vec![KnownAccount {
                alias: Alias::A,
                upn: "synthetic@example.test".into(),
                mri: None,
            }],
            services: vec![KnownService {
                name: name.into(),
                mri: "8:live:synthetic-service".into(),
            }],
            tenant_domain: None,
        };
        let out = sanitize_events(&[json!({"from":"8:live:synthetic-service"})], &identities).unwrap();
        assert_eq!(out[0]["from"], "<mri:1>", "{name}");
    }
}

#[test]
fn bare_http_authentication_credentials_are_removed_without_rewriting_a_scheme_or_an_existing_placeholder() {
    let events = [
        json!({"text": "Bearer AbCdEf0123456789 basic QWxhZGRpbjpvcGVuU2VzYW1l negotiate AbCdEfGhIjKlMnOp"}),
        json!({"accessToken":"Bearer <token:7>", "authorization":"", "token_type":"Bearer", "state":"short", "text":"token=Basic code=123 state=short"}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[0]["text"], "Bearer <token:1> basic <token:2> negotiate <token:3>");
    assert_eq!(out[1], events[1]);
    assert_eq!(sanitize_events(&out, &Identities::default()).unwrap(), out);
}

#[test]
fn malformed_binary_bodies_cookie_edges_and_nonsecret_scalars_keep_their_shape() {
    let events = [
        json!({"base64Encoded":true, "postData":"not base64!"}),
        json!({"opcode":2,"payloadData":"<binary:5>"}),
        json!({"kind":"text","data":"plain", "opcode":1,"payloadData":"text"}),
        json!({"Cookie":"blank=;done=<token:8>; Secure; Path=/; SameSite=Lax\nnext=synthetic-cookie", "name":null,"title":"  ","displayName":false,"meetingId":3,"meetingCode":37}),
    ];
    let out = sanitize_events(&events, &Identities::default()).unwrap();
    assert_eq!(out[0], json!({"base64Encoded":false,"postData":"<binary:11>"}));
    assert_eq!(out[1], events[1]);
    assert_eq!(out[2], events[2]);
    assert_eq!(
        out[3]["Cookie"],
        "blank=;done=<token:8>; Secure; Path=/; SameSite=Lax\nnext=<token:1>"
    );
    assert_eq!(out[3]["name"], Value::Null);
    assert_eq!(out[3]["displayName"], false);
    assert_eq!(out[3]["meetingId"], 3);
    assert_eq!(out[3]["meetingCode"], "<meeting:1>");
    assert_eq!(sanitize_events(&out, &Identities::default()).unwrap(), out);
}

#[test]
fn address_and_field_boundaries_preserve_nonidentities_while_replacing_real_private_values() {
    let input = json!({"text":"localhost 127.0.0.1 0.0.0.0 ::1 :: 001.002.003.004 x:2001:db8::5 ..2001:db8::6.. 2001:db8:::broken user@example.com user@sub.example.org a@example.net; title=null; topic=true; city=false; displayName=\"\"; meetingId=not-a-number; jobTitle='synthetic role'"});
    let out = sanitize_events(&[input], &Identities::default()).unwrap();
    let text = out[0]["text"].as_str().unwrap();
    for kept in [
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "001.002.003.004",
        "user@example.com",
        "user@sub.example.org",
        "a@example.net",
        "title=null",
        "topic=true",
        "city=false",
        "meetingId=not-a-number",
    ] {
        assert!(text.contains(kept), "missing {kept}: {text}");
    }
    assert!(!text.contains("2001:db8::5"));
    assert!(!text.contains("synthetic role"));
    assert!(text.contains("<ip:") && text.contains("<pii:"));
    assert_eq!(sanitize_events(&out, &Identities::default()).unwrap(), out);
}
