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
fn a_digit_prefixed_passcode_and_a_german_invitation_without_a_link_are_detected() {
    let cases = [
        // A passcode whose first six characters are digits is read whole, not as a meeting's code cut short.
        ("Code secret\u{a0}: <span>123456hTqWmz</span>", Kind::Passcode),
        ("Секретный код собрания: 9876543Xy", Kind::Passcode),
        ("Kod dostępu: 13579246AbC", Kind::Passcode),
        (r#"{"passcode":"2468135Qx"}"#, Kind::Passcode),
        // A German invitation's passcode, no join link in the text.
        ("Kenncode: Qx7Rt2", Kind::Passcode),
        ("<p>Besprechungskenncode: <b>hTqWmz</b></p>", Kind::Passcode),
        ("Besprechungskenncode:\u{a0}1234567Ab", Kind::Passcode),
    ];
    assert_detected(&Guard::new().unwrap(), &cases);
}

/// A made-up SHA-256 digest, as `a=fingerprint:` writes one.
const FINGERPRINT: &str =
    "3A:9F:01:C4:7B:22:E8:5D:90:16:AF:43:6C:D7:08:B1:5E:72:9A:0C:E3:48:F6:21:8D:B9:34:60:CA:17:F2:5B";

#[test]
fn a_calls_network_data_is_detected() {
    let sha1 = FINGERPRINT.split(':').take(20).collect::<Vec<_>>().join(":");
    let cases = [
        // a public address: an ICE candidate's, an SDP's, a relay's, in a URL, in JSON, at a sentence's end
        (
            "a=candidate:1 1 UDP 54788863 52.113.10.5 3478 typ relay raddr 52.113.10.5 rport 3478",
            Kind::IpAddress,
        ),
        ("c=IN IP4 20.202.1.7", Kind::IpAddress),
        (
            r"m=audio 3478 RTP/SAVP 111\r\nc=IN IP4 52.113.10.5\r\na=rtcp:3478",
            Kind::IpAddress,
        ),
        (r#"{"address":"13.107.64.2","port":3478}"#, Kind::IpAddress),
        ("the relay answered from 52.113.10.5.", Kind::IpAddress),
        ("http://52.113.10.5/x and //52.113.10.5:443", Kind::IpAddress),
        ("c=IN IP6 2a01:111:f100:3000::a83e:1a2b", Kind::IpAddress),
        ("turn:[2603:1063:2:f::5]:3478?transport=udp", Kind::IpAddress),
        (r#"{"ip":"2a01:111:f100::1"}"#, Kind::IpAddress),
        // ... glued to the word before it
        ("port:2a01:111:f100::1", Kind::IpAddress),
        (r#"{"urls":"turn:2a01:111:f100::1?transport=udp"}"#, Kind::IpAddress),
        ("host=2a01%3A111%3Af100%3A%3A1", Kind::IpAddress),
        ("mapped ::ffff:52.113.10.5 and ::ffff:3471:a05", Kind::IpAddress),
        ("translated 64:ff9b::3471:a05", Kind::IpAddress),
        // an ICE username fragment and password: in SDP, in SDP inside a JSON string, in a candidate string, under a
        // JSON key
        ("a=ice-ufrag:Xk3f", Kind::IceCredential),
        ("a=ice-pwd:Zq8hTn2vW5yR7mP1sD4fG6jK", Kind::IceCredential),
        (
            r#"{"blob":"v=0\r\na=ice-ufrag:Xk3f\r\na=ice-pwd:Zq8hTn2vW5yR7mP1sD4fG6jK\r\n"}"#,
            Kind::IceCredential,
        ),
        (
            r#"{"candidate":"candidate:1 1 udp 2122260223 127.0.0.1 54321 typ host generation 0 ufrag Qx7Zk network-id 1"}"#,
            Kind::IceCredential,
        ),
        (
            r#"{"candidate":"candidate:1 1 udp 1 192.0.2.1 9 typ host","usernameFragment":"Xk3f"}"#,
            Kind::IceCredential,
        ),
        // a DTLS fingerprint, SHA-256 and SHA-1, in SDP and inside a JSON string
        (
            &format!("a=fingerprint:sha-256 {FINGERPRINT}") as &str,
            Kind::DtlsFingerprint,
        ),
        (&format!("a=fingerprint:sha-1 {sha1}"), Kind::DtlsFingerprint),
        (
            &format!(r#"{{"blob":"a=setup:passive\r\na=fingerprint:sha-256 {FINGERPRINT}\r\n"}}"#),
            Kind::DtlsFingerprint,
        ),
        (
            &format!(r#"{{"fingerprint":"{}"}}"#, FINGERPRINT.to_lowercase()),
            Kind::DtlsFingerprint,
        ),
        // a TURN credential: a `trap/tokens` answer on one line and pretty-printed, an ICE server, a query
        (
            r#"{"tokens":[{"realm":"\"rtcmedia\"","username":"Ab12Cd34","password":"Zq8hTn2vW5yR7mP1"}],"expires":604800}"#,
            Kind::TurnCredential,
        ),
        (r#"      "password": "Zq8hTn2vW5yR7mP1","#, Kind::TurnCredential),
        (
            r#"      "username": "AgAAJAAAAAAAAAAAbCdEfGhIjKlMnOpQ","#,
            Kind::TurnCredential,
        ),
        (r#"{"realm":"rtcmedia","username":"ab12cd"}"#, Kind::TurnCredential),
        (
            r#"{"urls":"turn:relay.example.net:3478","username":"1700000000:laba","credential":"c2VjcmV0dmFsdWU="}"#,
            Kind::TurnCredential,
        ),
        ("?username=ab12cd&password=Zq8hTn2v", Kind::TurnCredential),
        (
            r#"{"body":"{\"tokens\":[{\"username\":\"Ab12Cd34\",\"password\":\"Zq8hTn2v\"}]}"}"#,
            Kind::TurnCredential,
        ),
    ];
    assert_detected(&Guard::new().unwrap(), &cases);
}

#[test]
fn made_up_network_data_and_what_only_looks_like_it_pass() {
    let made_up = "AB:".repeat(31) + "AB";
    let clean = [
        // the documentation ranges, IPv4 and IPv6
        "c=IN IP4 198.51.100.20, a=candidate:1 1 udp 2122194687 192.0.2.10 50000 typ host, 203.0.113.7",
        "c=IN IP6 2001:db8::1, [2001:db8:85a3::8a2e:370:7334]:443, 3fff:0:1::5, 2001:DB8:0:0:0:0:0:1",
        // ranges no public host has
        "10.0.0.1 172.16.5.4 172.31.255.254 192.168.1.20 127.0.0.1 169.254.1.1 100.64.0.1 0.0.0.0",
        "255.255.255.255 224.0.0.251 240.0.0.1 192.0.0.8 198.18.0.1 198.19.255.1",
        "::1 fe80::1 fd00::1 fc00:1::2 ff02::1 :: ::ffff:192.168.1.1 64:ff9b::c000:20a 1:2:3:4:5:6:7:8",
        // Rust paths, socket.io frames, a MAC address, times
        "teams_fence::contacting, a::b, Kind::Jwt, std::net::Ipv6Addr, add::cafe, beef::dead, <a as B>::c",
        // a word glued to an address no public host has, and a hex group that only makes an address of that kind
        "port:2001:db8::1, turn:fd00::1, face:2a01::1",
        r#"5:::{"name":"ping"}, 3:::{"id":1}, 0::, 1::, 2::"#,
        "aa:bb:cc:dd:ee:ff at 12:30:45 and 2026-10-24T08:00:00",
        // dotted versions and section numbers
        r#"<Identity Name="SyntheticTeams" Version="1.2.3.4" />, OpenID Connect Core 3.1.3.7"#,
        "Mozilla/5.0 (X11; Linux x86_64) Chrome/130.0.0.0 Safari/537.36",
        "OID 1.3.6.1.4.1.311, bundle 26260.1717.5229.7049, v10.20.30.40, 2.10.0.1.5",
        // placeholders and prose
        r"a=ice-ufrag:<token:3>\r\na=ice-pwd:<token:4>\r\na=fingerprint:sha-256 <token:5>",
        r#"{"tokens":[{"realm":"\"rtcmedia\"","username":"<token:1>","password":"<token:2>"}],"expires":604800}"#,
        r#"{"realm":"\"rtcmedia\"","username":"<user>","password":"<password>"}"#,
        "an ICE `a=ice-ufrag`/`a=ice-pwd` pair, a DTLS `a=fingerprint`, a TURN `username`/`password` pair",
        "the sign-in form: username lookup, password, \"stay signed in\"; Credential: the web client sends",
        // values that say they are made up, and a fingerprint of one byte repeated
        "a=ice-ufrag:SyntheticUfrag a=ice-pwd:SyntheticIcePassword0000000",
        "a=candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host generation 0 ufrag SyntheticUfrag network-id 1",
        "a=candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host generation 0 ufrag <token:3> network-id 1",
        &format!("a=fingerprint:sha-256 {made_up}"),
        &format!("a=fingerprint:sha-256 {}", made_up.to_lowercase()),
        r#"{"username":"SyntheticTurnUsername00000","password":"SyntheticTurnPassword","credential":""}"#,
        r#"{"username":"lab-a","displayName":"Lab A"}"#,
    ];
    let guard = Guard::new().unwrap();
    for text in clean {
        assert_eq!(kinds(&guard, text), Vec::<Kind>::new(), "{text:?}");
    }
}

#[test]
fn the_reserved_ranges_end_where_they_should() {
    use fixture_guard::network::{is_public_ipv4, is_public_ipv6};
    use std::net::{Ipv4Addr, Ipv6Addr};

    let v4 = |text: &str| is_public_ipv4(text.parse::<Ipv4Addr>().unwrap());
    let v6 = |text: &str| is_public_ipv6(text.parse::<Ipv6Addr>().unwrap());
    for public in [
        "1.0.0.1",
        "9.255.255.255",
        "11.0.0.0",
        "100.63.255.255",
        "100.128.0.0",
        "126.255.255.255",
        "128.0.0.0",
        "169.253.0.1",
        "172.15.255.255",
        "172.32.0.0",
        "192.0.1.1",
        "192.0.3.1",
        "192.169.0.1",
        "198.17.255.255",
        "198.20.0.0",
        "198.51.99.1",
        "198.51.101.1",
        "203.0.112.1",
        "203.0.114.1",
        "223.255.255.255",
    ] {
        assert!(v4(public), "{public} is public");
    }
    for public in [
        "2000::1",
        "2001:db7::1",
        "2001:db9::1",
        "3ffe::1",
        "3fff:1000::1",
        "3fff:ffff::1",
        "2002::1",
    ] {
        assert!(v6(public), "{public} is public");
    }
    for reserved in [
        "1fff:ffff::1",
        "4000::1",
        "2001:db8:ffff::1",
        "3fff:fff::1",
        "::ffff:10.1.2.3",
    ] {
        assert!(!v6(reserved), "{reserved} is not public");
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

fn repo_root() -> std::io::Result<PathBuf> {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Every fixture directory of the workspace (the top-level one, every crate's and spike's), as the repo-wide checks
/// see them; the top-level one and proto-lab's are always among them, so a discovery that finds nothing fails here.
fn workspace_fixture_dirs(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let dirs = synthetic::fixture_dirs(root)?;
    for expected in ["tests/fixtures", "crates/proto-lab/tests/fixtures"] {
        assert!(dirs.contains(&root.join(expected)), "{expected} not found in {dirs:?}");
    }
    Ok(dirs)
}

#[test]
fn committed_fixtures_and_docs_are_clean() {
    let root = repo_root().unwrap();
    let mut roots = workspace_fixture_dirs(&root).unwrap();
    roots.push(root.join("docs"));
    let roots: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let findings = Guard::new().unwrap().scan_paths(&roots).unwrap();
    let report: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert!(
        findings.is_empty(),
        "secrets in committed files:\n{}",
        report.join("\n")
    );
}

#[test]
fn committed_fixtures_are_marked_synthetic() {
    let root = repo_root().unwrap();
    workspace_fixture_dirs(&root).unwrap();
    // the exclusions name directories that exist, so a moved one doesn't leave a stale exception behind
    for dir in synthetic::TEST_INPUTS {
        assert!(root.join(dir).is_dir(), "TEST_INPUTS names a missing directory: {dir}");
    }
    let unmarked = synthetic::unmarked_in_workspace(&root).unwrap();
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
        ("f2.js", "  // _synthetic: made up worker\nself.onmessage = () => {};\n"),
        ("f3.txt", "_synthetic: made up\n"),
        ("f4.css", "/* _synthetic: made up */\n"),
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
        // `_synthetic:` inside the data or a longer word on the first line is not a header
        ("u.txt", "Bearer x, note_synthetic: made up\n"),
        ("v.yaml", "value: \"_synthetic: made up\"\n"),
        ("w.txt", "# note _synthetic: made up\n"),
        // a `.gitkeep` with content is a file like any other
        ("keep/.gitkeep", "captured: real data\n"),
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

/// A workspace with fixture dirs of every kind, and dirs that aren't fixture dirs: `(path, content)`.
const WORKSPACE: [(&str, &str); 14] = [
    // marked fixtures: top level, a spike's, a crate's
    ("tests/fixtures/auth/ok.json", "{\"_synthetic\": \"made up\"}\n"),
    ("tests/fixtures/.gitkeep", ""),
    (
        "spikes/echo-call/tests/fixtures/synthetic/end.json",
        "{\"_synthetic\": \"made up\"}\n",
    ),
    (
        "crates/teams-proto/tests/fixtures/chats.jsonl",
        "{\"_synthetic\": \"made up\"}\n",
    ),
    // unmarked ones in each kind of fixture dir, a crate's beside its test inputs included
    ("tests/fixtures/leak.json", "{\"id\": 1}\n"),
    (
        "spikes/echo-call/tests/fixtures/synthetic/planted.json",
        "{\"id\": 1}\n",
    ),
    ("spikes/other/tests/fixtures/capture.jsonl", "{\"seq\": 1}\n"),
    ("crates/proto-lab/tests/fixtures/stray.json", "{\"id\": 1}\n"),
    // test inputs listed in TEST_INPUTS carry no marker and are skipped
    ("crates/proto-lab/tests/fixtures/pages/blank.html", "<p>hi</p>\n"),
    (
        "crates/proto-lab/tests/fixtures/mini-bundle/AppxManifest.xml",
        "<Package/>\n",
    ),
    // build output, package caches and dot-directories are not walked
    ("target/debug/tests/fixtures/x.json", "{\"id\": 1}\n"),
    ("tools/x/node_modules/y/tests/fixtures/x.json", "{\"id\": 1}\n"),
    (".claude/worktrees/w/tests/fixtures/x.json", "{\"id\": 1}\n"),
    // a `fixtures` dir that isn't under `tests` is not a fixture dir
    ("crates/teams-proto/src/fixtures/x.json", "{\"id\": 1}\n"),
];

/// Writes `files` under `workspace`.
fn plant(workspace: &Path, files: &[(&str, &str)]) -> std::io::Result<()> {
    for (name, text) in files {
        let path = workspace.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, text)?;
    }
    Ok(())
}

#[test]
fn every_fixture_dir_of_a_workspace_is_checked_apart_from_test_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    plant(workspace, &WORKSPACE).unwrap();
    let joined = |names: &[&str]| -> Vec<PathBuf> { names.iter().map(|name| workspace.join(name)).collect() };

    let dirs = synthetic::fixture_dirs(workspace).unwrap();
    assert_eq!(
        dirs,
        joined(&[
            "crates/proto-lab/tests/fixtures",
            "crates/teams-proto/tests/fixtures",
            "spikes/echo-call/tests/fixtures",
            "spikes/other/tests/fixtures",
            "tests/fixtures",
        ])
    );
    assert_eq!(
        synthetic::unmarked_in_workspace(workspace).unwrap(),
        joined(&[
            "crates/proto-lab/tests/fixtures/stray.json",
            "spikes/echo-call/tests/fixtures/synthetic/planted.json",
            "spikes/other/tests/fixtures/capture.jsonl",
            "tests/fixtures/leak.json",
        ])
    );
}

#[test]
fn the_guard_scans_every_fixture_dir_test_inputs_included() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path();
    plant(workspace, &WORKSPACE).unwrap();
    let token = format!("{{\"_synthetic\": \"made up\", \"token\": \"{JWT}\"}}\n");
    plant(
        workspace,
        &[
            ("spikes/echo-call/tests/fixtures/synthetic/token.json", &token),
            (
                "crates/proto-lab/tests/fixtures/pages/leak.html",
                "<p>someone@contoso.com</p>\n",
            ),
            // outside every fixture dir: not scanned by this pass
            (
                "crates/teams-proto/src/fixtures/leak.json",
                "{\"to\": \"someone@contoso.com\"}\n",
            ),
        ],
    )
    .unwrap();
    let dirs = synthetic::fixture_dirs(workspace).unwrap();
    let roots: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let flagged: Vec<(PathBuf, Kind)> = Guard::new()
        .unwrap()
        .scan_paths(&roots)
        .unwrap()
        .into_iter()
        .map(|finding| (finding.path, finding.kind))
        .collect();
    assert_eq!(
        flagged,
        [
            (
                workspace.join("crates/proto-lab/tests/fixtures/pages/leak.html"),
                Kind::Email
            ),
            (
                workspace.join("spikes/echo-call/tests/fixtures/synthetic/token.json"),
                Kind::Jwt
            ),
        ]
    );
}
