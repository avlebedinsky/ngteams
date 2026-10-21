//! The guard's decisions, offline: as proto-lab's `sign_in` classifier for an armed page, every one in `fence.log`
//! before the answer, the redirect caught and never let through, an unarmed page or an unlogged decision denied; and
//! the guard's socket, as the extension talks to it.

use std::fs;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, mpsc};

use serde_json::Value;
use teams_auth::redirect::RedirectUri;
use teams_auth::token::NATIVE_CLIENT_REDIRECT;
use teams_fence::FenceLog;
use teams_signin::guard::{Decision, Guard, GuardEvent, GuardServer};
use teams_signin_extension::client::GuardClient;
use teams_signin_extension::protocol::{ALLOW, DENY, OK};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PAGE: u64 = 5;
const CODE: &str = "made-up-code-0123456789";
const TOKEN_URL: &str = "https://login.microsoftonline.com/organizations/oauth2/v2.0/token?client-request-id=\
     0f8b6a3e-1c2d-4e5f-8a9b-0c1d2e3f4a5b";

struct Lab {
    _dir: tempfile::TempDir,
    log_path: std::path::PathBuf,
    guard: Guard,
    events: mpsc::Receiver<GuardEvent>,
}

fn lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let (sender, events) = mpsc::channel();
    let guard = Guard::new(
        FenceLog::open(&log_path)?,
        RedirectUri::parse(NATIVE_CLIENT_REDIRECT)?,
        sender,
    );
    Ok(Lab {
        _dir: dir,
        log_path,
        guard,
        events,
    })
}

fn entries(path: &Path) -> TestResult<Vec<Value>> {
    let text = fs::read_to_string(path)?;
    Ok(text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?)
}

fn armed(lab: &Lab) {
    assert_eq!(lab.guard.answer(&format!("armed {PAGE} 1234")), OK);
}

#[test]
fn an_armed_page_is_decided_as_the_sign_in_classifier_decides() -> TestResult {
    let lab = lab()?;
    armed(&lab);
    let cases = [
        ("GET", "https://aadcdn.example.test/script.js?v=1", Decision::Allow),
        ("HEAD", "https://anywhere.example.test/", Decision::Allow),
        ("POST", TOKEN_URL, Decision::Allow),
        (
            "POST",
            "https://login.microsoftonline.com/common/GetCredentialType?mkt=en-US",
            Decision::Allow,
        ),
        (
            "POST",
            "https://login.microsoftonline.com/common/login",
            Decision::Allow,
        ),
        ("POST", "https://login.microsoftonline.com/kmsi", Decision::Allow),
        // the token endpoint with a query no capture showed, MFA, and any other host's POST
        (
            "POST",
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/token?x=1",
            Decision::Deny,
        ),
        (
            "POST",
            "https://login.microsoftonline.com/common/SAS/BeginAuth",
            Decision::Deny,
        ),
        (
            "POST",
            "https://login.microsoftonline.com.evil.test/common/login",
            Decision::Deny,
        ),
        ("POST", "http://127.0.0.1:8080/common/login", Decision::Deny),
        ("PUT", "https://login.microsoftonline.com/common/login", Decision::Deny),
        ("POST", "not a url", Decision::Deny),
    ];
    for (method, url, expected) in cases {
        assert_eq!(lab.guard.decide(PAGE, method, url), expected, "{method} {url}");
    }
    let logged = entries(&lab.log_path)?;
    assert_eq!(logged.len(), cases.len(), "one line per decision: {logged:?}");
    for ((method, _, expected), entry) in cases.iter().zip(&logged) {
        assert_eq!(entry["method"], *method);
        let verdict = if *expected == Decision::Allow { "allow" } else { "deny" };
        assert_eq!(entry["verdict"], verdict, "{entry}");
    }
    assert_eq!(logged[0]["read"], true);
    assert_eq!(logged[2]["own"], "entra_token");
    assert_eq!(logged[3]["own"], "entra_sign_in");
    assert_eq!(logged[6]["reason"], "unclassified");
    assert!(
        logged
            .iter()
            .all(|entry| !entry["url"].as_str().unwrap_or("").contains('?')),
        "no query is logged"
    );
    Ok(())
}

#[test]
fn an_unarmed_page_gets_nothing() -> TestResult {
    let lab = lab()?;
    assert_eq!(lab.guard.decide(PAGE, "GET", "https://example.test/"), Decision::Deny);
    armed(&lab);
    assert_eq!(
        lab.guard.decide(PAGE + 1, "GET", "https://example.test/"),
        Decision::Deny
    );
    assert_eq!(lab.guard.decide(PAGE, "GET", "https://example.test/"), Decision::Allow);
    let logged = entries(&lab.log_path)?;
    assert_eq!(logged.len(), 3);
    assert_eq!(logged[0]["verdict"], "deny");
    assert_eq!(logged[1]["verdict"], "deny");
    match lab.events.try_recv()? {
        GuardEvent::Armed { page } => assert_eq!(page, PAGE),
        other @ GuardEvent::Redirect(_) => panic!("{other:?}"),
    }
    Ok(())
}

#[test]
fn the_redirect_is_caught_logged_without_its_code_and_never_allowed() -> TestResult {
    let lab = lab()?;
    armed(&lab);
    let _ = lab.events.try_recv()?;
    let url = format!("{NATIVE_CLIENT_REDIRECT}?code={CODE}&state=made-up");
    assert_eq!(lab.guard.decide(PAGE, "GET", &url), Decision::Deny);
    match lab.events.try_recv()? {
        GuardEvent::Redirect(caught) => assert_eq!(caught.expose(), url),
        other @ GuardEvent::Armed { .. } => panic!("{other:?}"),
    }
    let logged = entries(&lab.log_path)?;
    assert_eq!(logged[0]["reason"], "sign_in_redirect");
    assert_eq!(logged[0]["url"], NATIVE_CLIENT_REDIRECT);
    assert!(!fs::read_to_string(&lab.log_path)?.contains(CODE));
    Ok(())
}

#[test]
fn a_loopback_only_guard_denies_everything_off_loopback() -> TestResult {
    let mut lab = lab()?;
    lab.guard = lab.guard.local_only();
    armed(&lab);
    let _ = lab.events.try_recv()?;
    let cases = [
        ("GET", "http://127.0.0.1:8080/next?x=1", Decision::Allow),
        ("GET", "http://[::1]:8080/next", Decision::Allow),
        ("HEAD", "https://127.9.9.9/a", Decision::Allow),
        (
            "GET",
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?client_id=x",
            Decision::Deny,
        ),
        ("GET", "https://aadcdn.example.test/script.js?v=1", Decision::Deny),
        ("GET", "http://192.0.2.1/img.png", Decision::Deny),
        ("GET", "http://localhost:8080/next", Decision::Deny),
        ("POST", TOKEN_URL, Decision::Deny),
        ("POST", "https://login.microsoftonline.com/common/login", Decision::Deny),
    ];
    for (method, url, expected) in cases {
        assert_eq!(lab.guard.decide(PAGE, method, url), expected, "{method} {url}");
    }
    // the redirect is still caught first, and logged as such
    let url = format!("{NATIVE_CLIENT_REDIRECT}?code={CODE}&state=made-up");
    assert_eq!(lab.guard.decide(PAGE, "GET", &url), Decision::Deny);
    assert!(matches!(lab.events.try_recv()?, GuardEvent::Redirect(_)));
    lab.guard.refuse("CONNECT", "192.0.2.1:443");
    let logged = entries(&lab.log_path)?;
    assert_eq!(logged.len(), cases.len() + 2, "one line per decision: {logged:?}");
    for ((_, _, expected), entry) in cases.iter().zip(&logged) {
        let verdict = if *expected == Decision::Allow { "allow" } else { "deny" };
        assert_eq!(entry["verdict"], verdict, "{entry}");
    }
    assert_eq!(logged[cases.len()]["reason"], "sign_in_redirect");
    let refused = &logged[cases.len() + 1];
    assert_eq!(
        (&refused["method"], &refused["url"]),
        (&"CONNECT".into(), &"192.0.2.1:443".into())
    );
    assert_eq!(refused["verdict"], "deny");
    Ok(())
}

/// A log that refuses every append.
struct Refusing;

impl std::io::Write for Refusing {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("full"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_decision_that_can_t_be_logged_is_a_deny() -> TestResult {
    let (sender, _events) = mpsc::channel();
    let guard = Guard::new(
        FenceLog::from_writer(Refusing),
        RedirectUri::parse(NATIVE_CLIENT_REDIRECT)?,
        sender,
    );
    assert_eq!(guard.answer(&format!("armed {PAGE} 1")), OK);
    assert_eq!(guard.decide(PAGE, "GET", "https://example.test/"), Decision::Deny);
    assert_eq!(guard.decide(PAGE, "POST", TOKEN_URL), Decision::Deny);
    Ok(())
}

#[test]
fn the_socket_answers_the_extension_line_by_line() -> TestResult {
    let lab = lab()?;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("guard.sock");
    let _server = GuardServer::start(&path, Arc::new(lab.guard))?;

    let mut client = GuardClient::connect(&path);
    assert!(!client.allows(PAGE, None, "https://example.test/"), "not armed yet");
    assert!(client.armed(PAGE, 99));
    assert!(client.allows(PAGE, None, "https://example.test/"));
    assert!(client.allows(PAGE, Some("POST"), TOKEN_URL));
    assert!(!client.allows(PAGE, Some("POST"), "https://example.test/"));

    // a malformed line is a deny, an overlong one ends the connection
    let mut raw = UnixStream::connect(&path)?;
    let mut reader = BufReader::new(raw.try_clone()?);
    let mut answer = String::new();
    raw.write_all(b"request 5 GET\n")?;
    reader.read_line(&mut answer)?;
    assert_eq!(answer.trim_end(), DENY);
    raw.write_all(format!("request 5 GET https://example.test/{}\n", "x".repeat(70_000)).as_bytes())?;
    answer.clear();
    reader.read_line(&mut answer)?;
    assert_eq!(answer.trim_end(), DENY);
    answer.clear();
    assert_eq!(reader.read_line(&mut answer)?, 0, "the connection is closed");
    assert_ne!(ALLOW, DENY);

    let events: Vec<GuardEvent> = lab.events.try_iter().collect();
    assert!(matches!(events.as_slice(), [GuardEvent::Armed { page: PAGE }]));
    // the four requests; a line that isn't one is answered, not logged
    assert_eq!(entries(&lab.log_path)?.len(), 4);
    Ok(())
}
