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

/// Trouter's `connectparams` signature and a SAS link's `sig`: a base64 HMAC-SHA256, 44 characters with `/`, `+` and
/// a closing `=`, far shorter than the last pass's opaque run and not JWT-shaped, so only its name catches it.
fn signature(seed: &str) -> String {
    let text: String = format!("{seed}q7/Zk+Lm3N9pR2tV/wY4bC+6dF8hJ0aS2eG5iK7oU1xW3")
        .chars()
        .take(43)
        .collect();
    format!("{text}=")
}

/// A `trouter.connected` event as the service sends it, its reconnect parameters signed with `sig`.
fn trouter_connected(sig: &str) -> String {
    format!(
        r#"5:::{{"name":"trouter.connected","args":[{{"id":"00000000-0000-4000-8000-0000000000e1","ccid":"e1",
"url":"https://pub-ent-euno-01-f.trouter.teams.microsoft.com:3443/v4/f/AbC-dE_1/","ttl":"604800",
"connectparams":{{"issuer":"prod-2","scae":"1","sig":"{sig}","sr":"AbC-dE_1","sp":"pub-ent-euno-01",
"se":"1793300000000","st":"1792700000000"}},"reconnectUrl":"wss://pub-ent-euno-01-t.trouter.teams.microsoft.com:443/v4/c"}}]}}"#
    )
    .replace('\n', "")
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
        "X-Ms-Signature",
        "X-Client-Secret",
        "X-Credential",
        "sig",
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
fn trouter_connected_s_signature_is_redacted_and_its_other_parameters_kept() {
    let sig = signature("s");
    let shown = Frame {
        direction: Direction::Received,
        text: &trouter_connected(&sig),
    }
    .to_string();
    assert!(!shown.contains(&sig), "{shown}");
    let data: serde_json::Value = serde_json::from_str(shown.strip_prefix("frame received 5:::").unwrap()).unwrap();
    let params = &data["args"][0]["connectparams"];
    assert_eq!(params["sig"], REDACTED, "{shown}");
    assert_eq!(params["sr"], "AbC-dE_1");
    assert_eq!(params["se"], "1793300000000");
    assert_eq!(params["issuer"], "prod-2");
    assert_eq!(
        data["args"][0]["reconnectUrl"],
        "wss://pub-ent-euno-01-t.trouter.teams.microsoft.com:443/v4/c"
    );
    // the same parameters sent back in `user.authenticate`: all its arguments go
    let authenticate = format!(
        r#"5:::{{"name":"user.authenticate","args":[{{"headers":{{}},"connectparams":{{"sig":"{sig}","sr":"AbC-dE_1"}}}}]}}"#
    );
    assert!(!redact::frame(&authenticate).contains(&sig));
}

#[test]
fn a_signed_link_s_sig_is_redacted_in_a_query_a_header_and_json() {
    let sig = signature("l");
    let link = format!(
        "https://example.blob.core.windows.net/c/f.png?sv=2024-01-01&se=2026-10-30&sp=r&sig={}",
        sig.replace('/', "%2F").replace('+', "%2B").replace('=', "%3D")
    );
    let shown = redact::url(&link);
    assert!(shown.ends_with(&format!("&sp=r&sig={REDACTED}")), "{shown}");
    assert_eq!(redact::header_value("Location", &link), redact::url(&link));
    assert_eq!(redact::header_value("Accept", "text/html; q=0.9"), "text/html; q=0.9");
    let body = format!(r#"{{"links":[{{"href":"{link}"}}],"signature":"{sig}"}}"#);
    let shown = redact::body(Some("application/json"), body.as_bytes());
    assert!(!shown.contains(&sig) && !shown.contains("sig=%"), "{shown}");
    assert!(
        shown.contains("sv=2024-01-01&se=2026-10-30&sp=r&sig=[redacted]"),
        "{shown}"
    );
}

#[test]
fn sign_in_s_names_and_redirect_are_redacted_in_json() {
    let (code, verifier) = (authorization_code("j"), code_verifier("k"));
    let secret = signature("c");
    let body = format!(
        r#"{{"code":"{code}","code_verifier":"{verifier}","client_secret":"{secret}","client_assertion":"{secret}",
            "password":"{secret}","clientDescription":{{"appId":"TeamsCDLWebWorker","aesKey":"{secret}"}}}}"#
    );
    let shown = redact::body(Some("application/json"), body.as_bytes());
    for value in [&code, &verifier, &secret] {
        assert!(!shown.contains(value.as_str()), "{shown}");
    }
    assert!(shown.contains(r#""appId":"TeamsCDLWebWorker""#), "{shown}");
    // the sign-in window's answer: the URL Entra redirected to, its code in the query
    let short_code = "0.AXEA-shortcode_1";
    let answer = format!(
        r#"{{"redirect":"https://login.microsoftonline.com/common/oauth2/nativeclient?code={short_code}&state=s1&session_state=x"}}"#
    );
    let shown = redact::body(None, answer.as_bytes());
    assert!(!shown.contains(short_code), "{shown}");
    assert!(shown.contains("nativeclient?code=[redacted]&state=s1"), "{shown}");
    // a string that isn't a URL keeps its text, but a credential's `<name>=` in it goes: the redactor can't tell a
    // link from prose, and redacts more, never less
    let text = r#"{"content":"a question? yes # and code=1, a decode=2"}"#;
    assert_eq!(
        redact::body(Some("application/json"), text.as_bytes()),
        r#"{"content":"a question? yes # and code=[redacted] a decode=2"}"#
    );
}

/// One way a SAS link's query is written: its separator, its `=` and its signature's encoding, as a message's HTML, a
/// link nested in another's query, or JSON escapes them.
struct Form {
    name: &'static str,
    separator: &'static str,
    equals: &'static str,
    encode: fn(&str) -> String,
}

fn percent(sig: &str) -> String {
    sig.replace('/', "%2F").replace('+', "%2B").replace('=', "%3D")
}

fn percent_twice(sig: &str) -> String {
    percent(sig).replace('%', "%25")
}

/// Every form a SAS link's `&sig=` takes in the text the log meets.
const FORMS: [Form; 10] = [
    Form {
        name: "html &amp;",
        separator: "&amp;",
        equals: "=",
        encode: str::to_owned,
    },
    Form {
        name: "html &AMP;",
        separator: "&AMP;",
        equals: "=",
        encode: str::to_owned,
    },
    Form {
        name: "html &#38;",
        separator: "&#38;",
        equals: "=",
        encode: str::to_owned,
    },
    Form {
        name: "html &#x26;",
        separator: "&#x26;",
        equals: "=",
        encode: str::to_owned,
    },
    Form {
        name: "html &amp; and &#61;",
        separator: "&amp;",
        equals: "&#61;",
        encode: str::to_owned,
    },
    Form {
        name: "percent %26",
        separator: "%26",
        equals: "%3D",
        encode: percent,
    },
    Form {
        name: "percent twice %2526",
        separator: "%2526",
        equals: "%253D",
        encode: percent_twice,
    },
    Form {
        name: "json \\u0026",
        separator: "\\u0026",
        equals: "=",
        encode: str::to_owned,
    },
    Form {
        name: "json \\u0026 and \\u003d",
        separator: "\\u0026",
        equals: "\\u003d",
        encode: str::to_owned,
    },
    Form {
        name: "plain &",
        separator: "&",
        equals: "=",
        encode: str::to_owned,
    },
];

impl Form {
    fn link(&self, sig: &str) -> String {
        format!(
            "https://example.blob.core.windows.net/c/f.png?sv=2024-01-01{0}se=2026-10-30{0}sp=r{0}sig{1}{2}",
            self.separator,
            self.equals,
            (self.encode)(sig)
        )
    }
}

/// `text` holds no form of `sig`: raw, percent-encoded once or twice, nor its first 20 characters.
fn holds_no_signature(text: &str, sig: &str) -> bool {
    [sig.to_owned(), percent(sig), percent_twice(sig), sig[..20].to_owned()]
        .iter()
        .all(|form| !text.contains(form.as_str()))
}

#[test]
fn a_signed_link_s_sig_is_redacted_in_every_escaped_form() {
    for (at, form) in FORMS.iter().enumerate() {
        let sig = signature(&format!("{at}f"));
        let link = form.link(&sig);
        let html = format!(r#"<p>the file: <a href="{link}" title="x">{link}</a> ok</p>"#);
        // a message's HTML in a post's JSON body, its answer, a page of a text type, a line of any record
        let body = serde_json::json!({ "content": html, "messagetype": "RichText/Html" }).to_string();
        let shown = [
            redact::body(Some("application/json"), body.as_bytes()),
            redact::body(Some("text/html"), html.as_bytes()),
            redact::line(&html),
            redact::line(&body),
            redact::url(&link),
            redact::header_value("Link", &format!("<{link}>; rel=next")).into_owned(),
        ];
        for shown in shown {
            assert!(holds_no_signature(&shown, &sig), "{}: {shown}", form.name);
            assert!(
                shown.contains("sv=2024-01-01"),
                "{}: the rest stays: {shown}",
                form.name
            );
            // in a JSON body's text, a `\` of the `=`'s escape is escaped once more
            let equals = [form.equals.to_owned(), form.equals.replace('\\', "\\\\")];
            assert!(
                equals
                    .iter()
                    .any(|equals| shown.contains(&format!("sig{equals}{REDACTED}"))),
                "{}: {shown}",
                form.name
            );
        }
    }
}

#[test]
fn a_link_nested_in_another_s_query_and_a_delivery_s_body_are_redacted() {
    let sig = signature("n");
    // a safe-links wrapper: the SAS link percent-encoded in its `url`
    let inner = FORMS[5]
        .link(&sig)
        .replace(':', "%3A")
        .replace('/', "%2F")
        .replace('?', "%3F");
    let wrapped = format!("https://safelinks.example.com/?url={inner}&data=05%7C02&reserved=0");
    let shown = redact::url(&wrapped);
    assert!(holds_no_signature(&shown, &sig), "{shown}");
    assert!(shown.contains("&data=05%7C02&reserved=0"), "{shown}");
    // a Trouter delivery: its body a string of JSON, the message's HTML in it with its `&` escaped as `\u0026`, and a
    // token field by name
    let token = skypetoken("d");
    let message = format!(
        r#"{{"content":"<a href=\"{}\">f</a>","skypetoken":"{token}"}}"#,
        FORMS[7].link(&sig)
    );
    let delivery = serde_json::json!({
        "id": 7, "method": "POST", "url": "/v4/f/AbC-dE_1/messaging", "headers": {}, "body": message,
    });
    let frame = format!("3:::{delivery}");
    let shown = redact::frame(&frame);
    assert!(holds_no_signature(&shown, &sig), "{shown}");
    assert!(!shown.contains(&token), "{shown}");
    assert!(
        shown.contains("sv=2024-01-01&se=2026-10-30&sp=r&sig=[redacted]"),
        "{shown}"
    );
    assert!(shown.contains(r#"\"skypetoken\":\"[redacted]\""#), "{shown}");
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
    sig: String,
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
            sig: signature("g"),
        }
    }

    fn all(&self) -> [&str; 7] {
        [
            &self.bearer,
            &self.skypetoken,
            &self.refresh,
            &self.code,
            &self.verifier,
            &self.cookie,
            &self.sig,
        ]
    }
}

/// A token grant with every credential in its place (its header, its cookie, its form, its query, its answer's
/// fields, cookie and `Location`), `user.authenticate`, `trouter.connected` with its signature, and a line of no record
/// type, at `debug`.
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
    let location = format!(
        "https://login.microsoftonline.com/common/oauth2/nativeclient?code={}&state=s1",
        secrets.code
    );
    let answer_headers = headers(&[
        ("content-type", "application/json"),
        ("set-cookie", &cookie_header),
        ("location", &location),
    ]);
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
    let connected = trouter_connected(&secrets.sig);
    let frame = Frame {
        direction: Direction::Received,
        text: &connected,
    };
    debug_record(log, "teams_wire::trouter", format_args!("{frame}"));
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
    assert!(
        files
            .iter()
            .any(|(_, text)| text.contains("frame received 5:::") && text.contains(r#""sr":"AbC-dE_1""#))
    );
}
