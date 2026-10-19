//! The fixture guard: a planted secret is found, placeholders and prose are not, and the repo's committed fixtures and
//! docs are clean.

use std::fs;
use std::path::{Path, PathBuf};

use fixture_guard::{Guard, Kind};

// Synthetic, unsigned: header {"alg":"none","typ":"JWT"}, payload {"sub":"planted"}.
const JWT: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJwbGFudGVkIn0.c2lnbmF0dXJlLXBsYW50ZWQ";

fn kinds(guard: &Guard, text: &str) -> Vec<Kind> {
    guard.scan_text(text).into_iter().map(|(_, kind)| kind).collect()
}

#[test]
fn planted_jwt_in_a_temp_dir_is_detected() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("auth");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("token.json"),
        format!("{{\n  \"note\": \"ok\",\n  \"value\": \"{JWT}\"\n}}\n"),
    )
    .unwrap();
    fs::write(dir.path().join("clean.json"), "{\"user\": \"<user:a>\"}\n").unwrap();

    let findings = Guard::new().unwrap().scan_paths(&[dir.path()]).unwrap();

    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].path, nested.join("token.json"));
    assert_eq!(findings[0].line, 3);
    assert_eq!(findings[0].kind, Kind::Jwt);
}

#[test]
fn a_finding_never_prints_the_secret() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("leak.txt"), format!("Authorization: Bearer {JWT}\n")).unwrap();

    let findings = Guard::new().unwrap().scan_paths(&[dir.path()]).unwrap();

    assert!(!findings.is_empty());
    for finding in &findings {
        let shown = format!("{finding} {finding:?}");
        assert!(!shown.contains("eyJ"), "{shown}");
        assert!(shown.contains("leak.txt:1"), "{shown}");
    }
}

#[test]
fn every_kind_of_secret_is_detected() {
    let cases = [
        (JWT, Kind::Jwt),
        (
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123456789",
            Kind::Bearer,
        ),
        ("X-Skypetoken: abcdefghijklmnopqrstuvwxyz0123456789", Kind::Skypetoken),
        (
            r#"{"skypetoken":"abcdefghijklmnopqrstuvwxyz0123456789"}"#,
            Kind::Skypetoken,
        ),
        (
            r#"{"refresh_token": "0.AXYZabcdefghijklmnopqrstuvwxyz0123456789"}"#,
            Kind::OauthToken,
        ),
        ("access_token=abcdefghijklmnopqrstuvwxyz0123456789", Kind::OauthToken),
        ("from someone@contoso.com today", Kind::Email),
        ("8:orgid:1fec8e78-bce4-4aaf-ab1b-5451cc387264", Kind::Mri),
        ("8:live:somebody", Kind::Mri),
        ("28:1fec8e78-bce4-4aaf-ab1b-5451cc387264", Kind::Mri),
        // camelCase and prefixed keys, and the MSAL cache shape, hold opaque (non-JWT) refresh tokens.
        (
            r#"{"refreshToken": "0.AXYZabcdefghijklmnopqrstuvwxyz0123456789"}"#,
            Kind::OauthToken,
        ),
        (
            "x_refresh_token=0.AXYZabcdefghijklmnopqrstuvwxyz0123456789",
            Kind::OauthToken,
        ),
        (
            r#"{"credentialType":"RefreshToken","secret":"1.AUoabcdefghijklmnopqrstuvwxyz0123456789"}"#,
            Kind::OauthToken,
        ),
        // Percent-encoded, as in request URLs.
        (
            "/v1/users/8%3Aorgid%3A1fec8e78-bce4-4aaf-ab1b-5451cc387264/properties",
            Kind::Mri,
        ),
        ("/v1/users/8%3aorgid%3a1fec8e78-bce4-4aaf-ab1b-5451cc387264", Kind::Mri),
        ("?mri=28%253A1fec8e78-bce4-4aaf-ab1b-5451cc387264", Kind::Mri),
        ("authorize?login_hint=someone%40contoso.com&x=1", Kind::Email),
        // A response body serialized into a JSON string.
        (
            r#"{"body":"{\"refresh_token\":\"0.AXYZabcdefghijklmnopqrstuvwxyz0123456789\"}"}"#,
            Kind::OauthToken,
        ),
        (
            r#"{"body":"{\"skypetoken\":\"abcdefghijklmnopqrstuvwxyz0123456789\"}"}"#,
            Kind::Skypetoken,
        ),
        (
            r#"{"body":"line one\n8:orgid:1fec8e78-bce4-4aaf-ab1b-5451cc387264"}"#,
            Kind::Mri,
        ),
    ];
    let guard = Guard::new().unwrap();
    for (text, kind) in cases {
        assert!(
            kinds(&guard, text).contains(&kind),
            "{text:?} should be {kind:?}, got {:?}",
            kinds(&guard, text)
        );
    }
}

#[test]
fn placeholders_and_prose_pass() {
    let clean = [
        "<user:a> wrote to <mri:b> in <guid:1>",
        r#"{"skypetoken": "<skypetoken:a>", "refresh_token": "<token:1>"}"#,
        "Authorization: Bearer <token:1>",
        "a test scans for JWTs, `Bearer`, skypetokens, email addresses and real MRIs",
        "Skypetoken resource: `authzResource = https://api.spaces.skype.com`",
        "mail to someone@example.com or ops@lab.example.org",
        "the meeting is at 8:30, see 28:fixed-note",
        "client ID `1fec8e78-bce4-4aaf-ab1b-5451cc387264`",
        r#"{"credentialType":"RefreshToken","secret":"<token:1>"}"#,
        "/v1/users/%3Cmri%3Ab%3E/properties?login_hint=%3Cuser%3Aa%3E",
        r#"{"body":"{\"refreshToken\":\"<token:1>\",\"to\":\"ops%40example.com\"}"}"#,
        "100% done, a stray backslash \\ and 50%3",
    ];
    let guard = Guard::new().unwrap();
    for text in clean {
        assert_eq!(kinds(&guard, text), Vec::<Kind>::new(), "{text:?}");
    }
}

#[test]
fn line_numbers_are_one_based_per_line() {
    let found = Guard::new()
        .unwrap()
        .scan_text("clean\nsomeone@contoso.com\nclean\n8:orgid:1fec8e78-bce4-4aaf-ab1b-5451cc387264\n");
    assert_eq!(found, vec![(2, Kind::Email), (4, Kind::Mri)]);
}

#[test]
fn a_missing_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent");
    let error = Guard::new().unwrap().scan_paths(&[&missing]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(
        error.to_string().contains(&missing.display().to_string()),
        "the error should name the path: {error}"
    );
}

#[test]
fn a_single_file_root_is_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("one.md");
    fs::write(&file, "someone@contoso.com\n").unwrap();
    let findings = Guard::new().unwrap().scan_paths(&[&file]).unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, Kind::Email);
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn committed_fixtures_and_docs_are_clean() {
    let root = repo_root();
    let findings = Guard::new()
        .unwrap()
        .scan_paths(&[&root.join("tests/fixtures"), &root.join("docs")])
        .unwrap();
    let report: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert!(
        findings.is_empty(),
        "secrets in committed files:\n{}",
        report.join("\n")
    );
}
