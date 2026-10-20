//! The fixture guard: a planted secret is found, placeholders and prose are not, an unmarked fixture is flagged, and
//! the repo's committed fixtures and docs are clean and its fixtures all marked synthetic.

use std::fs;
use std::path::{Path, PathBuf};

use fixture_guard::synthetic;
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
        // A tenant or object id, as written, right after a percent escape, and encoded twice.
        ("tid=72f988bf-86f1-41af-91ab-2d7cd011db47", Kind::Guid),
        (
            "context=%7B%22Tid%22%3A%2272f988bf-86f1-41af-91ab-2d7cd011db47%22%7D",
            Kind::Guid,
        ),
        ("x=72F988BF%252D86F1%252D41AF%252D91AB%252D2D7CD011DB47", Kind::Guid),
        // A signed URL's signature: in a query, as a header, inside an encoded redirect, in JSON.
        (
            "blob?sv=2024-01-01&sig=abCD12%2BefGH34%2FijKL56%2BmnOP78%3D",
            Kind::Signature,
        ),
        (
            "X-Amz-Signature: 3f1a9c0b7e2d4f6a8b0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a",
            Kind::Signature,
        ),
        (
            "?target=https%3A%2F%2Fstorage.example.invalid%2Fblob%3Fsig%3DabCD12%252BefGH34%252FijKL56%252BmnOP78%253D",
            Kind::Signature,
        ),
        (r#"{"signature":"c2lnbmF0dXJlLXZhbHVlLXBsYW50ZWQ"}"#, Kind::Signature),
    ];
    assert_detected(&Guard::new().unwrap(), &cases);
}

fn assert_detected(guard: &Guard, cases: &[(&str, Kind)]) {
    for (text, kind) in cases {
        assert!(
            kinds(guard, text).contains(kind),
            "{text:?} should be {kind:?}, got {:?}",
            kinds(guard, text)
        );
    }
}

#[test]
fn a_meetings_join_credentials_are_detected() {
    let cases = [
        // A Teams meeting link with its passcode or its code, in every form a capture holds one.
        (
            "https://teams.microsoft.com/meet/1234567890123?p=Ab3dEf5gHi7jKl9mNo",
            Kind::MeetingLink,
        ),
        ("https://teams.live.com/meet/1234567890123", Kind::MeetingLink),
        (
            "https://teams.microsoft.com/meet/<meeting:1>?anon=true&p=Ab3dEf5gHi7jKl9mNo",
            Kind::MeetingLink,
        ),
        (
            "https://teams.microsoft.com/meet/<meeting:1>?x=<token:2>&amp;p=Ab3dEf",
            Kind::MeetingLink,
        ),
        (
            "/v2/?meetingjoin=true#/meet/<meeting:1>?p=Ab3dEf5gHi7jKl9mNo",
            Kind::MeetingLink,
        ),
        ("/lm/meet/1234567890123", Kind::MeetingLink),
        (
            "/l/meetup-join/19%3ameeting_x%40thread.v2/0?context=%7b%7d&p=Ab3dEf",
            Kind::MeetingLink,
        ),
        (
            "?next=https%3A%2F%2Fteams.microsoft.com%2Fmeet%2F%3Cmeeting%3A1%3E%3Fp%3DAb3dEf5gHi7jKl9mNo",
            Kind::MeetingLink,
        ),
        (
            "?next=https%253A%252F%252Fteams.microsoft.com%252Fmeet%252F1234567890123",
            Kind::MeetingLink,
        ),
        // A passcode: under a key, and as an invitation spells it out.
        (r#"{"passcode":"Qx7Rt2"}"#, Kind::Passcode),
        (r#"{"meetingPasscode": "246813"}"#, Kind::Passcode),
        ("meetingPw=Ab3dEf5gHi7jKl9mNo", Kind::Passcode),
        ("<p>Passcode: <b>Qx7Rt2</b></p>", Kind::Passcode),
        (
            "{\"body\":\"Passcode:\u{a0}<span class=\\\"x\\\">Qx7Rt2</span>\"}",
            Kind::Passcode,
        ),
        // A passcode of letters only, of either case.
        (r#"{"passcode":"aBcDeF"}"#, Kind::Passcode),
        ("<p>Passcode: <b>kwbxpq</b></p>", Kind::Passcode),
        // An invitation in another language, its labels as the bundle's locale strings spell them.
        ("Code secret\u{a0}: <span>hTqWmz</span>", Kind::Passcode),
        ("Секретный код собрания: pqrstu", Kind::Passcode),
        ("会議のパスコード: AbCdEf", Kind::Passcode),
        ("Kod dostępu: XyZwQp", Kind::Passcode),
        // A meeting's code after its label, in any of those languages.
        ("<p>Meeting ID: <span>355 123 456 789</span></p>", Kind::MeetingId),
        ("Besprechungs-ID: 355 123 456 789", Kind::MeetingId),
        ("ID de la réunion\u{a0}: 366222333444", Kind::MeetingId),
        ("Идентификатор собрания: 377 111 222 333", Kind::MeetingId),
        ("Conference ID: 987654321#", Kind::MeetingId),
        // A join link's passcode behind an escaped `&`, the code already a placeholder.
        (r"/meet/<meeting:1>?anon=true\u0026p=hQzRtW", Kind::MeetingLink),
        (r"/meet/<meeting:1>?anon=true\x26p=hQzRtW", Kind::MeetingLink),
        ("/meet/<meeting:1>?anon=true&#38;p=hQzRtW", Kind::MeetingLink),
        ("/meet/<meeting:1>?anon=true&#x26;p=hQzRtW", Kind::MeetingLink),
        (r"/meet/<meeting:1>?anon=true\\u0026p=hQzRtW", Kind::MeetingLink),
    ];
    assert_detected(&Guard::new().unwrap(), &cases);
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
        // Microsoft's first-party app ids, the same in every tenant, and the nil GUID.
        "web client `5e3ce6c0-2b1f-4285-8d4b-75ee78787346`, MSA client `8ec6bc83-69c8-4392-8f08-b3c986009232`",
        "meeting artifacts `6BC3B958-689B-49F5-9006-36D165F30E00`, nil 00000000-0000-0000-0000-000000000000",
        "sig=<token:1>&X-Amz-Signature=<token:2>, a `sig` parameter, signature=short",
        // A meeting link whose code and passcode are placeholders, and the routes that look like one.
        "https://teams.microsoft.com/meet/<meeting:1>?p=<token:1>&anon=true",
        "https%3A%2F%2Fteams.microsoft.com%2Fmeet%2F%3Cmeeting%3A1%3E%3Fp%3D%3Ctoken%3A1%3E",
        "/meet/meet-now, /v2/meet/meet-now, /l/meetup-join/19:<thread:1>@thread.v2/0?context=x",
        "a `/meet/` link's `p` parameter is its passcode",
        r#"{"passcode":"<token:1>","isPasscodeRequired":true,"passcodeLength":"short"}"#,
        "Passcode: <b><token:3></b>, the passcode: required",
        r#"{"passcodePolicy":"enabled","passcode":"none","meetingPw":"null"}"#,
        "Meeting ID: <meeting:1>, Besprechungs-ID: <meeting:2>, the meeting ID: 12345",
        r"/meet/<meeting:1>?anon=true\u0026p=<token:1>",
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

#[test]
fn committed_fixtures_are_marked_synthetic() {
    let unmarked = synthetic::unmarked(&repo_root().join("tests/fixtures")).unwrap();
    let report: Vec<String> = unmarked.iter().map(|path| path.display().to_string()).collect();
    assert!(
        unmarked.is_empty(),
        "fixtures that don't declare themselves synthetic (captures never enter the repo, not even sanitized; write \
         the fixture by hand with made-up values and a \"_synthetic\" note):\n{}",
        report.join("\n")
    );
}

#[test]
fn a_marked_fixture_passes_and_an_unmarked_one_is_flagged() {
    let marked = [
        (
            "a.json",
            "{\n  \"_synthetic\": \"made up, shaped like a chat thread read\",\n  \"id\": 1\n}\n",
        ),
        ("b.jsonl", "{\"_synthetic\": \"made up\"}\n{\"seq\": 1}\n{\"seq\": 2}\n"),
        (
            "c.jsonl",
            "\n{\"_synthetic\": \"made up\", \"seq\": 1}\n{\"_synthetic\": \"made up\", \"seq\": 2}\n",
        ),
        ("d.txt", "# _synthetic: made up\nrest\n"),
        ("e.html", "<!-- _synthetic: made up page -->\n<p>hi</p>\n"),
        ("sub/f.json", "{\"_synthetic\": \"made up\"}"),
    ];
    let unmarked = [
        // a sanitized export as it would have been committed before: no marker
        ("g.jsonl", "{\"seq\": 1, \"user\": \"<user:a>\"}\n"),
        ("h.json", "{\"id\": 1}\n"),
        // the marker only in a later line, nested, empty, not text, or in an array
        ("i.jsonl", "{\"seq\": 1}\n{\"_synthetic\": \"made up\"}\n"),
        ("j.json", "{\"inner\": {\"_synthetic\": \"made up\"}}\n"),
        ("k.json", "{\"_synthetic\": \"  \"}\n"),
        ("l.json", "{\"_synthetic\": true}\n"),
        ("m.json", "[{\"_synthetic\": \"made up\"}]\n"),
        ("n.json", "not json, _synthetic: made up\n"),
        // a text file whose header is not on the first line, or has no text
        ("o.txt", "rest\n# _synthetic: made up\n"),
        ("p.html", "<!-- _synthetic: -->\n"),
        ("q.bin", "\u{0}\u{1}"),
        ("r.txt", ""),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in marked.iter().chain(&unmarked) {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fs::write(dir.path().join("s.bin"), [0xff_u8, 0xfe, 0x00]).unwrap();
    // `.gitkeep` is not a fixture; a symlink is never a marked one
    fs::write(dir.path().join(".gitkeep"), "").unwrap();
    fs::create_dir_all(dir.path().join("empty")).unwrap();
    fs::write(dir.path().join("empty/.gitkeep"), "").unwrap();
    std::os::unix::fs::symlink(dir.path().join("a.json"), dir.path().join("t.json")).unwrap();

    let found = synthetic::unmarked(dir.path()).unwrap();

    let mut expected: Vec<PathBuf> = unmarked
        .iter()
        .map(|(name, _)| dir.path().join(name))
        .chain(["s.bin", "t.json"].map(|name| dir.path().join(name)))
        .collect();
    expected.sort();
    assert_eq!(found, expected);
    // one file as the root works too
    assert_eq!(
        synthetic::unmarked(&dir.path().join("a.json")).unwrap(),
        Vec::<PathBuf>::new()
    );
    assert!(synthetic::unmarked(&dir.path().join("missing")).is_err());
}
