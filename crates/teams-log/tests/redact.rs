//! Redaction: real-shaped credentials (made up here at run time, none a real one) put in every place the log could
//! meet one (a header, a JSON field, a form and a query field of sign-in, a redirect, a cookie, Trouter's
//! `user.authenticate`, a line of any record), then looked for in the formatted records and in every file of a log
//! rotated over them. A body or frame the redactor can't read is redacted whole.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::path::Path;
use std::time::Duration;

use log::{Level as LogLevel, Log as _, Record};
use teams_log::redact::{self, REDACTED, UNREADABLE};
use teams_log::wire::{Direction, Frame, Request, Response};
use teams_log::{Config, FileLog, Level, Rotation};

/// A JWT-shaped token: three base64url parts, the first two starting `eyJ` as an encoded JSON object does.
fn jwt(seed: &str) -> String {
    let body: String = format!("{seed}AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_").repeat(6);
    format!(
        "eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{}",
        body.chars().rev().collect::<String>()
    )
}

/// A skypetoken that isn't JWT-shaped, so only its name catches it.
fn skypetoken(seed: &str) -> String {
    format!("{seed}SkYpEtOkEn{}", "Zx9_".repeat(8))
}

/// An Entra refresh token's shape: `1.A…`, a long run of base64url with dots.
fn refresh_token(seed: &str) -> String {
    format!("1.AXEA{seed}{}", "qW3-eR4_tY5.uI6oP7".repeat(40))
}

/// An authorization code's shape: as long and as opaque as a refresh token.
fn authorization_code(seed: &str) -> String {
    format!("0.AXEA{seed}{}", "cOdE-8_vAl.Ue9".repeat(30))
}

/// A short cookie value: only the header's name catches it.
fn cookie(seed: &str) -> String {
    format!("{seed}CoOkIe{}", "k1L2m3".repeat(2))
}

/// A PKCE verifier's shape: 43 characters of base64url.
fn code_verifier(seed: &str) -> String {
    format!("{seed}{}", "vErIfIeR-_".repeat(5)).chars().take(43).collect()
}

fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn credential_headers_are_redacted_by_name_and_the_rest_kept() {
    for name in [
        "Authorization",
        "authorization",
        "Cookie",
        "Set-Cookie",
        "X-Skypetoken",
        "Authentication",
        "X-Ms-Some-Token-Header",
    ] {
        assert!(redact::secret_field(name), "{name}");
        assert_eq!(redact::header_value(name, "value"), REDACTED);
    }
    for name in [
        "Accept",
        "Content-Type",
        "x-ms-migration",
        "BehaviorOverride",
        "Retry-After",
    ] {
        assert!(!redact::secret_field(name), "{name}");
        assert_eq!(redact::header_value(name, "value"), "value");
    }
}

#[test]
fn token_fields_are_redacted_at_any_depth_and_others_kept() {
    let (access, refresh, id, sky) = (jwt("a"), refresh_token("r"), jwt("i"), skypetoken("s"));
    let answer = format!(
        r#"{{"token_type":"Bearer","access_token":"{access}","refresh_token":"{refresh}","id_token":"{id}",
            "expires_in":3599,"tokens":{{"skypeToken":"{sky}","expiresIn":86400}},"nested":[{{"token":"{sky}"}}]}}"#
    );
    let shown = redact::body(Some("application/json; charset=utf-8"), answer.as_bytes());
    for secret in [&access, &refresh, &id, &sky] {
        assert!(!shown.contains(secret.as_str()), "{shown}");
    }
    assert!(shown.contains(r#""expires_in":3599"#), "{shown}");
    // a name holding `token` is redacted whatever it holds (`token_type` too): over-redaction, never under
    assert!(shown.contains(r#""tokens":"[redacted]""#), "{shown}");
}

#[test]
fn sign_in_form_and_query_fields_are_redacted() {
    let (code, verifier, refresh) = (authorization_code("c"), code_verifier("v"), refresh_token("f"));
    let form = format!(
        "client_id=00000000-0000-4000-8000-0000000000c1&grant_type=authorization_code&code={code}\
         &code_verifier={verifier}&redirect_uri=https%3A%2F%2Flogin.microsoftonline.com%2Fnativeclient\
         &refresh_token={refresh}"
    );
    let shown = redact::body(Some("application/x-www-form-urlencoded"), form.as_bytes());
    for secret in [&code, &verifier, &refresh] {
        assert!(!shown.contains(secret.as_str()), "{shown}");
    }
    assert!(shown.contains("grant_type=authorization_code"), "{shown}");
    let redirect = format!("https://login.microsoftonline.com/common/oauth2/nativeclient?code={code}&state=xyz");
    let shown = redact::url(&redirect);
    assert!(!shown.contains(&code), "{shown}");
    assert!(shown.ends_with(&format!("?code={REDACTED}&state=xyz")), "{shown}");
    // a fragment's too (an implicit grant's answer)
    let fragment = format!("https://example.invalid/cb#access_token={}&expires_in=3599", jwt("x"));
    assert!(!redact::url(&fragment).contains("eyJ0"), "{}", redact::url(&fragment));
}

#[test]
fn user_authenticate_s_arguments_are_redacted_whole() {
    let token = jwt("t");
    let frame = format!(
        r#"5:::{{"name":"user.authenticate","args":[{{"headers":{{"X-Ms-Test-User":"False","Authorization":"Bearer {token}","X-MS-Migration":"True"}},"connectparams":{{"sr":"abc"}}}}]}}"#
    );
    let shown = redact::frame(&frame);
    assert!(!shown.contains(&token), "{shown}");
    // the fields' order is serde_json's (a workspace build may turn on its `preserve_order`)
    let data: serde_json::Value = serde_json::from_str(shown.strip_prefix("5:::").unwrap()).unwrap();
    assert_eq!(data, serde_json::json!({"name": "user.authenticate", "args": REDACTED}));
    // another event keeps its arguments, a credential field in them redacted
    let other = format!(r#"5:::{{"name":"trouter.connected","args":[{{"id":"e1","skypetoken":"{token}"}}]}}"#);
    let shown = redact::frame(&other);
    assert!(!shown.contains(&token) && shown.contains(r#""id":"e1""#), "{shown}");
}

#[test]
fn what_can_t_be_read_is_redacted_whole() {
    let secret = skypetoken("u");
    // says it is JSON and isn't
    let shown = redact::body(Some("application/json"), format!("{{not json {secret}").as_bytes());
    assert!(shown.starts_with(UNREADABLE) && !shown.contains(&secret), "{shown}");
    // no content type, neither JSON nor a form
    let shown = redact::body(None, format!("some text with {secret} in it").as_bytes());
    assert!(shown.starts_with(UNREADABLE), "{shown}");
    // a frame whose data should be JSON and isn't, and something that isn't a frame
    for frame in [format!("5:::{{{secret}"), format!("3:::{secret}"), secret.clone()] {
        let shown = redact::frame(&frame);
        assert!(shown.starts_with(UNREADABLE) && !shown.contains(&secret), "{shown}");
    }
    // the frames without data go as they are
    assert_eq!(redact::frame("2::"), "2::");
    assert_eq!(redact::frame("1::"), "1::");
}

#[test]
fn a_binary_body_is_its_type_and_size_only() {
    let picture = [0x89, b'P', b'N', b'G', 0, 1, 2, 3, 0xff];
    assert_eq!(redact::body(Some("image/png"), &picture), "[image/png, 9 bytes]");
    assert_eq!(
        redact::body(Some("application/octet-stream"), b"abc"),
        "[application/octet-stream, 3 bytes]"
    );
    assert_eq!(redact::body(None, &picture), "[no content type, 9 bytes]");
    assert_eq!(redact::body(Some("application/json"), b""), "(empty)");
}

#[test]
fn the_last_pass_catches_tokens_anywhere_in_a_line() {
    let (token, refresh, sky) = (jwt("p"), refresh_token("q"), skypetoken("k"));
    let text = format!(
        "error: a {token} here, Authorization: Bearer {sky}, skypetoken={sky}; then {refresh} and an id \
         19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces"
    );
    let shown = redact::line(&text);
    for secret in [&token, &refresh, &sky] {
        assert!(!shown.contains(secret.as_str()), "{shown}");
    }
    assert!(
        shown.contains("19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces"),
        "an id stays: {shown}"
    );
    assert!(shown.contains("Bearer [redacted]"), "{shown}");
}

/// Every file of the log in `dir` (the current one and the rotated ones), with its text.
fn log_files(dir: &Path) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, fs::read_to_string(entry.path()).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn debug_record(log: &FileLog, target: &str, args: std::fmt::Arguments<'_>) {
    log.log(
        &Record::builder()
            .level(LogLevel::Debug)
            .target(target)
            .args(args)
            .build(),
    );
}

/// Every kind of credential, real-shaped.
struct Secrets {
    bearer: String,
    skypetoken: String,
    refresh: String,
    code: String,
    verifier: String,
    cookie: String,
}

impl Secrets {
    fn new() -> Self {
        Self {
            bearer: jwt("b"),
            skypetoken: skypetoken("y"),
            refresh: refresh_token("z"),
            code: authorization_code("w"),
            verifier: code_verifier("v"),
            cookie: cookie("c"),
        }
    }

    fn all(&self) -> [&str; 6] {
        [
            &self.bearer,
            &self.skypetoken,
            &self.refresh,
            &self.code,
            &self.verifier,
            &self.cookie,
        ]
    }
}

/// A token grant with every credential in its place (its header, its cookie, its form, its query, its answer's
/// fields and cookie), `user.authenticate`, and a line of no record type, at `debug`.
fn write_exchange(log: &FileLog, secrets: &Secrets) {
    let auth_header = format!("Bearer {}", secrets.bearer);
    let cookie_header = format!("skypetoken_asm={0}; other={0}", secrets.cookie);
    let request_headers = headers(&[
        ("accept", "application/json"),
        ("authorization", &auth_header),
        ("x-skypetoken", &secrets.skypetoken),
        ("cookie", &cookie_header),
        ("content-type", "application/x-www-form-urlencoded"),
    ]);
    let form = format!(
        "grant_type=refresh_token&refresh_token={}&code={}&code_verifier={}",
        secrets.refresh, secrets.code, secrets.verifier
    );
    let url = format!(
        "https://login.microsoftonline.com/organizations/oauth2/v2.0/token?code={}",
        secrets.code
    );
    let request = Request {
        method: "POST",
        url: &url,
        headers: &request_headers,
        body: Some(form.as_bytes()),
    };
    debug_record(log, "teams_proto::http", format_args!("{request}"));
    let answer = format!(
        r#"{{"access_token":"{0}","refresh_token":"{1}","id_token":"{0}"}}"#,
        secrets.bearer, secrets.refresh
    );
    let answer_headers = headers(&[("content-type", "application/json"), ("set-cookie", &cookie_header)]);
    let response = Response {
        method: "POST",
        url: &url,
        status: 200,
        headers: &answer_headers,
        body: answer.as_bytes(),
        elapsed: Duration::from_millis(12),
    };
    debug_record(log, "teams_proto::http", format_args!("{response}"));
    let authenticate = format!(
        r#"5:::{{"name":"user.authenticate","args":[{{"headers":{{"Authorization":"Bearer {}"}}}}]}}"#,
        secrets.bearer
    );
    let frame = Frame {
        direction: Direction::Sent,
        text: &authenticate,
    };
    debug_record(log, "teams_trouter::socket", format_args!("{frame}"));
    // a line no record type formats: the last pass alone
    debug_record(
        log,
        "teams_core::engine",
        format_args!(
            "an error's text with {} and skypetoken={}",
            secrets.bearer, secrets.skypetoken
        ),
    );
}

#[test]
fn no_credential_reaches_any_rotated_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        dir: dir.path().join("logs"),
        setting: Level::Debug,
        env: None,
        rotation: Rotation {
            max_bytes: 2_048,
            keep: 3,
        },
    };
    let log = FileLog::open(&config).unwrap();
    let secrets = Secrets::new();
    for _ in 0..12 {
        write_exchange(&log, &secrets);
    }
    assert!(log.flush_written());
    let files = log_files(&config.dir);
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["teams-app.log", "teams-app.log.1", "teams-app.log.2", "teams-app.log.3"],
        "rotated, 3 older kept"
    );
    for (name, text) in &files {
        for secret in secrets.all() {
            assert!(!text.contains(secret), "{name} holds a credential:\n{text}");
        }
    }
    // the rest of the requests is there
    assert!(files.iter().any(|(_, text)| text.contains("grant_type=refresh_token")));
    assert!(files.iter().any(|(_, text)| text.contains("frame sent 5:::")));
}
