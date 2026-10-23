//! The browser fence's own-availability check (`proto_lab::availability`, M2 Task 18): UPS's presence endpoint `PUT`
//! and the manual status pass only in their captured shape, only with a classifier that opts in
//! (`Classifier::with_own_availability`), and only for an endpoint the session's fence let this account register in
//! this run (the messaging stack's `registrationId`), logged to `fence.log` as own state.
//!
//! The allowed shapes are the synthetic fixtures `tests/fixtures/ups/endpoint-requests.json` and
//! `tests/fixtures/ups/manual-status-requests.json`; the registration is the messaging stack's of
//! `tests/fixtures/trouter/registration.json`. Every variant here changes one thing. Made-up GUIDs
//! fill in their placeholders; no real id or token appears here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use proto_lab::availability::OwnEndpoints;
use proto_lab::classify::{Classification, Classifier, OwnKind};
use proto_lab::fence_http::{DenyCause, HttpFence, MalformedPause, PausedRequest, Verdict};
use proto_lab::post_read::HEADER_VALUES_READ;
use serde_json::{Value, json};
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ENDPOINT: &str = "00000000-0000-4000-8000-0000000000e1";
const OTHER_ENDPOINT: &str = "00000000-0000-4000-8000-0000000000e2";
const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJ1cG4iOiJwbGFudGVkIn0.cGxhbnRlZC1zaWduYXR1cmU";
const REQUESTS: &str = include_str!("../../../tests/fixtures/ups/endpoint-requests.json");
const MANUAL: &str = include_str!("../../../tests/fixtures/ups/manual-status-requests.json");
const REGISTRATIONS: &str = include_str!("../../../tests/fixtures/trouter/registration.json");

/// One request of a fixture.
#[derive(Clone, Debug)]
struct Put {
    name: String,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
    /// Whether the event carries no `postData` at all (the manual status's reset); `body` is then unused.
    no_body: bool,
    /// More keys of the event's `request`, as Chrome may give them (`hasPostData`, `postDataEntries`).
    extra: Vec<(&'static str, Value)>,
}

impl Put {
    fn paused(&self) -> Result<PausedRequest, MalformedPause> {
        let headers: serde_json::Map<String, Value> = self
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), Value::from(value.as_str())))
            .collect();
        let mut request = json!({"method": self.method, "url": self.url, "headers": headers});
        if !self.no_body {
            request["postData"] = json!(self.body);
        }
        for (key, value) in &self.extra {
            request[*key] = value.clone();
        }
        PausedRequest::from_params(&json!({"requestId": "interception-1", "request": request}))
    }

    /// Classified with its headers as the browser fence keeps them. A pause the fence couldn't read is a read, which
    /// no test expects, so it fails every assertion.
    fn classify(&self, classifier: &Classifier) -> Classification {
        let Ok(paused) = self.paused() else {
            return Classification::Read;
        };
        classifier.classify_with_headers(&self.method, &self.url, &paused.headers, paused.body.as_deref())
    }

    fn set_header(&mut self, name: &str, value: &str) {
        self.headers.retain(|(header, _)| !header.eq_ignore_ascii_case(name));
        self.headers.push((name.to_owned(), value.to_owned()));
    }

    fn json_body(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

fn paused_event(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<PausedRequest, MalformedPause> {
    let headers: serde_json::Map<String, Value> = headers
        .iter()
        .map(|(name, value)| (name.clone(), Value::from(value.as_str())))
        .collect();
    let params = json!({
        "requestId": "interception-1",
        "request": {"method": method, "url": url, "headers": headers, "postData": body},
    });
    PausedRequest::from_params(&params)
}

/// The fixture's endpoint `PUT`s, for `endpoint`.
fn puts_for(endpoint: &str) -> TestResult<Vec<Put>> {
    load(REQUESTS, endpoint)
}

/// The manual status fixture's requests, for `endpoint`.
fn manual_for(endpoint: &str) -> TestResult<Vec<Put>> {
    load(MANUAL, endpoint)
}

fn manual() -> TestResult<Vec<Put>> {
    manual_for(ENDPOINT)
}

/// A fixture's requests, for `endpoint`: its headers, each request's `caller` as `x-ms-client-caller` when it has one,
/// and its body (`null`: none at all).
fn load(fixture: &str, endpoint: &str) -> TestResult<Vec<Put>> {
    let fixture: Value = serde_json::from_str(&fixture.replace("<endpoint>", endpoint))?;
    let headers = fixture["headers"].as_object().ok_or("no headers")?;
    let mut puts = Vec::new();
    for request in fixture["requests"].as_array().ok_or("no requests")? {
        let origin = request["origin"].as_str().ok_or("no origin")?;
        let path = request["path"].as_str().ok_or("no path")?;
        let mut put = Put {
            name: request["name"].as_str().ok_or("no name")?.to_owned(),
            method: "PUT".to_owned(),
            url: format!("{origin}{path}"),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    let value = value.as_str().unwrap_or_default();
                    (name.clone(), value.replace("<the request's own origin>", origin))
                })
                .collect(),
            body: request["body"].to_string(),
            no_body: request["body"].is_null(),
            extra: Vec::new(),
        };
        if let Some(caller) = request["caller"].as_str() {
            put.set_header("x-ms-client-caller", caller);
        }
        puts.push(put);
    }
    Ok(puts)
}

fn puts() -> TestResult<Vec<Put>> {
    puts_for(ENDPOINT)
}

/// The messaging stack's registration of the fixture, registering `endpoint`, as the fence pauses it.
fn registration(endpoint: &str, case: usize) -> TestResult<PausedRequest> {
    let fixture: Value = serde_json::from_str(REGISTRATIONS)?;
    let case = &fixture["cases"][case];
    let mut body = case["body"].clone();
    body["registrationId"] = json!(endpoint);
    let url = case["url"].as_str().ok_or("no url")?;
    let headers = [("content-type".to_owned(), "application/json".to_owned())];
    Ok(paused_event("POST", url, &headers, &body.to_string())?)
}

/// The fixture's case of the messaging stack's registration, and the calling stack's.
const MESSAGING: usize = 0;
const CALLING: usize = 2;

fn opted_in() -> Classifier {
    Classifier::messaging().with_own_availability()
}

fn availability(endpoint: &str) -> Classification {
    Classification::OwnAvailability {
        endpoint: endpoint.to_owned(),
    }
}

#[test]
fn each_captured_put_names_its_endpoint() -> TestResult {
    let puts = puts()?;
    assert_eq!(puts.len(), 3);
    for put in &puts {
        assert_eq!(put.classify(&opted_in()), availability(ENDPOINT), "{}", put.name);
    }
    Ok(())
}

#[test]
fn only_a_classifier_that_opts_in_recognizes_it() -> TestResult {
    for put in puts()? {
        for classifier in [
            Classifier::seeded(),
            Classifier::mapped(),
            Classifier::messaging(),
            Classifier::calling(),
        ] {
            assert_eq!(put.classify(&classifier), Classification::Unknown, "{}", put.name);
        }
    }
    Ok(())
}

/// One change to a `PUT`, and what it is.
type Change = (&'static str, fn(&mut Put));

fn assert_unknown_after(changes: &[Change]) -> TestResult {
    for (what, change) in changes {
        for mut put in puts()? {
            change(&mut put);
            assert_eq!(
                put.classify(&opted_in()),
                Classification::Unknown,
                "{}: {what}",
                put.name
            );
        }
    }
    Ok(())
}

#[test]
fn another_method_host_path_or_query_is_unknown() -> TestResult {
    assert_unknown_after(&[
        ("POST", |put| put.method = "POST".to_owned()),
        ("DELETE", |put| put.method = "DELETE".to_owned()),
        ("PATCH", |put| put.method = "PATCH".to_owned()),
        ("another host", |put| {
            put.url = put
                .url
                .replace("https://teams.microsoft.com", "https://teams.example.com");
            put.url = put
                .url
                .replace("https://teams.cloud.microsoft", "https://teams.example.com");
        }),
        ("http", |put| put.url = put.url.replace("https://", "http://")),
        ("user info", |put| put.url = put.url.replace("https://", "https://lab@")),
        ("a query", |put| put.url.push_str("?endpoint=1")),
        ("an empty query", |put| put.url.push('?')),
        ("a fragment", |put| put.url.push_str("#x")),
        ("no trailing slash", |put| {
            put.url = put.url.trim_end_matches('/').to_owned();
        }),
        ("a segment more", |put| put.url.push_str("extra/")),
        ("an upper-case geo", |put| put.url = put.url.replace("/noam/", "/NOAM/")),
        ("no geo", |put| put.url = put.url.replace("/noam/", "/")),
        ("the manual status", |put| {
            put.url = put.url.replace("/me/endpoints/", "/me/forceavailability/");
        }),
        ("a status note", |put| {
            put.url = put.url.replace("/me/endpoints/", "/me/publishnote");
        }),
        ("a work location", |put| {
            put.url = put.url.replace("/me/endpoints/", "/me/workLocation/");
        }),
        ("another user's endpoints", |put| {
            put.url = put.url.replace(
                "/me/endpoints/",
                "/users/8:orgid:00000000-0000-4000-8000-0000000000ff/endpoints/",
            );
        }),
    ])
}

#[test]
fn another_header_or_value_is_unknown() -> TestResult {
    assert_unknown_after(&[
        ("a header not captured", |put| put.set_header("x-ms-test-user", "False")),
        ("another content type", |put| {
            put.set_header("content-type", "text/plain");
        }),
        ("no content type", |put| {
            put.headers
                .retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
        }),
        ("another behavioroverride", |put| {
            put.set_header("behavioroverride", "none");
        }),
        ("another client type", |put| put.set_header("x-ms-client-type", "web")),
        ("another client user agent", |put| {
            put.set_header("x-ms-client-user-agent", "Teams-V2-Desktop");
        }),
        ("an object id", |put| put.set_header("x-ms-object-id", GUID_B)),
        ("a request id", |put| put.set_header("x-ms-request-id", "made-up")),
        ("another origin header", |put| {
            put.set_header("Origin", "https://teams.example.com");
        }),
        ("a caller carrying an id", |put| {
            put.set_header("x-ms-client-caller", GUID_B);
        }),
        ("no endpoint id header", |put| {
            put.headers
                .retain(|(name, _)| !name.eq_ignore_ascii_case("x-ms-endpoint-id"));
        }),
        ("another endpoint in the header", |put| {
            put.set_header("x-ms-endpoint-id", OTHER_ENDPOINT);
        }),
        ("a header twice", |put| {
            put.headers
                .push(("Behavioroverride".to_owned(), "redirectAs404".to_owned()));
        }),
    ])
}

fn with_body(put: &mut Put, change: impl FnOnce(&mut serde_json::Map<String, Value>)) {
    let mut body = put.json_body();
    if let Some(object) = body.as_object_mut() {
        change(object);
    }
    put.body = body.to_string();
}

#[test]
fn a_body_with_other_fields_is_unknown() -> TestResult {
    assert_unknown_after(&[
        ("a status note", |put| {
            with_body(put, |body| {
                body.insert("note".to_owned(), json!("made up"));
            });
        }),
        ("out of office", |put| {
            with_body(put, |body| {
                body.insert("outOfOffice".to_owned(), json!(true));
            });
        }),
        ("no device type", |put| {
            with_body(put, |body| {
                body.remove("deviceType");
            });
        }),
        ("no id", |put| {
            with_body(put, |body| {
                body.remove("id");
            });
        }),
        ("another device type", |put| {
            with_body(put, |body| {
                body.insert("deviceType".to_owned(), json!("Desktop"));
            });
        }),
        ("another activity reporting", |put| {
            with_body(put, |body| {
                body.insert("activityReporting".to_owned(), json!("Manual"));
            });
        }),
    ])
}

#[test]
fn availability_values_no_capture_showed_are_unknown() -> TestResult {
    assert_unknown_after(&[
        ("away", |put| {
            with_body(put, |body| {
                body.insert("availability".to_owned(), json!("Away"));
                body.insert("activity".to_owned(), json!("Away"));
            });
        }),
        ("do not disturb", |put| {
            with_body(put, |body| {
                body.insert("availability".to_owned(), json!("DoNotDisturb"));
                body.insert("activity".to_owned(), json!("Presenting"));
            });
        }),
        ("offline", |put| {
            with_body(put, |body| {
                body.insert("availability".to_owned(), json!("Offline"));
                body.insert("activity".to_owned(), json!("Offline"));
            });
        }),
        ("a pair not captured", |put| {
            with_body(put, |body| {
                body.insert("availability".to_owned(), json!("Available"));
                body.insert("activity".to_owned(), json!("InACall"));
            });
        }),
    ])
}

#[test]
fn another_id_or_an_unreadable_body_is_unknown() -> TestResult {
    assert_unknown_after(&[
        ("an id in upper case", |put| {
            let upper = ENDPOINT.to_uppercase();
            with_body(put, |body| {
                body.insert("id".to_owned(), json!(upper));
            });
            put.set_header("x-ms-endpoint-id", &upper);
        }),
        ("an id that isn't a GUID", |put| {
            with_body(put, |body| {
                body.insert("id".to_owned(), json!("8:orgid:me"));
            });
            put.set_header("x-ms-endpoint-id", "8:orgid:me");
        }),
        ("a number for availability", |put| {
            with_body(put, |body| {
                body.insert("availability".to_owned(), json!(1));
            });
        }),
        ("a key in another case beside it", |put| {
            put.body = put.body.replacen('{', r#"{"Availability":"Busy","#, 1);
        }),
        ("a key twice", |put| {
            put.body = put.body.replacen('{', r#"{"activity":"Busy","#, 1);
        }),
        ("a list", |put| put.body = format!("[{}]", put.body)),
        ("trailing data", |put| put.body.push_str(" {}")),
        ("no body", |put| put.body = String::new()),
    ])
}

struct Lab {
    _dir: tempfile::TempDir,
    log_path: PathBuf,
    http: HttpFence,
}

/// An HTTP fence of `classifier` over a roster of a, b and c, with a log on disk.
fn lab(classifier: Classifier) -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let roster = Roster::new(
        [GUID_A, GUID_B, GUID_C].map(|guid| Mri::new(format!("8:orgid:{guid}"))),
        [],
    );
    let fence = Arc::new(Fence::new(roster, FenceLog::open(&log_path)?));
    let http = HttpFence::new(classifier, fence, Arc::new(FailClosed));
    Ok(Lab {
        _dir: dir,
        log_path,
        http,
    })
}

fn log_entries(path: &Path) -> TestResult<Vec<Value>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?)
}

#[tokio::test]
async fn a_put_before_its_endpoint_is_registered_is_denied() -> TestResult {
    let lab = lab(opted_in())?;
    for put in puts()? {
        assert_eq!(
            lab.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
    }
    let entries = log_entries(&lab.log_path)?;
    assert!(
        entries
            .iter()
            .all(|entry| entry["verdict"] == "deny" && entry["reason"] == "unclassified"),
        "{entries:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_put_on_the_endpoint_this_session_registered_passes_logged_as_own() -> TestResult {
    let lab = lab(opted_in())?;
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Allow
    );
    let puts = puts()?;
    for put in &puts {
        assert_eq!(lab.http.decide(&put.paused()?).await, Verdict::Allow, "{}", put.name);
    }
    assert!(lab.http.denials().is_empty());
    let entries = log_entries(&lab.log_path)?;
    assert_eq!(entries.len(), 1 + puts.len());
    assert_eq!(entries[0]["own"], OwnKind::EndpointRegistration.as_str());
    for (entry, put) in entries[1..].iter().zip(&puts) {
        assert_eq!(entry["verdict"], "allow");
        assert_eq!(entry["method"], "PUT");
        assert_eq!(entry["own"], "own_availability");
        assert_eq!(entry["url"], put.url);
        assert!(entry.get("kind").is_none(), "not a write: {entry}");
        assert!(entry.get("post_read").is_none(), "not a read: {entry}");
    }
    Ok(())
}

#[tokio::test]
async fn a_put_on_any_other_endpoint_is_denied() -> TestResult {
    let lab = lab(opted_in())?;
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Allow
    );
    for put in puts_for(OTHER_ENDPOINT)? {
        assert_eq!(
            lab.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_endpoint_another_session_registered_does_not_count() -> TestResult {
    let a = lab(opted_in())?;
    let b = lab(opted_in())?;
    assert_eq!(a.http.decide(&registration(ENDPOINT, MESSAGING)?).await, Verdict::Allow);
    for put in puts()? {
        assert_eq!(
            b.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
        assert_eq!(a.http.decide(&put.paused()?).await, Verdict::Allow, "{}", put.name);
    }
    Ok(())
}

#[tokio::test]
async fn the_calling_stacks_registration_does_not_count() -> TestResult {
    // the calling classifier lets the calling stack's registration out; it registers no endpoint the PUT may name
    let lab = lab(Classifier::calling().with_own_availability())?;
    assert_eq!(lab.http.decide(&registration(ENDPOINT, CALLING)?).await, Verdict::Allow);
    for put in puts()? {
        assert_eq!(
            lab.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_denied_registration_registers_nothing() -> TestResult {
    // without the messaging set the registration is unknown and denied, so it notes no endpoint
    let lab = lab(Classifier::mapped().with_own_availability())?;
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Deny(DenyCause::Unclassified)
    );
    for put in puts()? {
        assert_eq!(
            lab.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
    }
    Ok(())
}

#[tokio::test]
async fn without_opting_in_a_registered_endpoint_lets_nothing_through() -> TestResult {
    let lab = lab(Classifier::messaging())?;
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Allow
    );
    for put in puts()? {
        assert_eq!(
            lab.http.decide(&put.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}",
            put.name
        );
    }
    Ok(())
}

#[test]
fn a_paused_put_keeps_no_credential() -> TestResult {
    for mut put in puts()? {
        put.set_header("authorization", &format!("Bearer {TOKEN}"));
        put.set_header("Cookie", &format!("authtoken={TOKEN}"));
        let paused = put.paused()?;
        for (name, value) in &paused.headers {
            let read = HEADER_VALUES_READ.contains(&name.to_ascii_lowercase().as_str());
            assert_eq!(value.is_some(), read, "{}: {name}", put.name);
        }
        assert!(!format!("{paused:?}").contains(TOKEN), "{}", put.name);
        assert_eq!(put.classify(&opted_in()), availability(ENDPOINT), "{}", put.name);
    }
    Ok(())
}

#[test]
fn another_caller_or_a_missing_captured_header_is_unknown() -> TestResult {
    assert_unknown_after(&[
        ("a caller no capture showed", |put| {
            put.set_header("x-ms-client-caller", "presence-sync-strategy:onInterval");
        }),
        ("the manual status's caller", |put| {
            put.set_header("x-ms-client-caller", "TeamsClient:setPresenceAvailability");
        }),
        ("no caller", |put| remove_header(put, "x-ms-client-caller")),
        ("no behavioroverride", |put| remove_header(put, "behavioroverride")),
        ("no client type", |put| remove_header(put, "x-ms-client-type")),
        ("no client user agent", |put| {
            remove_header(put, "x-ms-client-user-agent");
        }),
        ("no object id", |put| remove_header(put, "x-ms-object-id")),
        ("no request id", |put| remove_header(put, "x-ms-request-id")),
    ])
}

fn remove_header(put: &mut Put, name: &str) {
    put.headers.retain(|(header, _)| !header.eq_ignore_ascii_case(name));
}

// the manual status

#[test]
fn each_captured_manual_status_names_its_endpoint() -> TestResult {
    let manual = manual()?;
    assert_eq!(manual.len(), 5);
    for request in &manual {
        assert_eq!(
            request.classify(&opted_in()),
            availability(ENDPOINT),
            "{}",
            request.name
        );
        for classifier in [Classifier::mapped(), Classifier::messaging(), Classifier::calling()] {
            assert_eq!(
                request.classify(&classifier),
                Classification::Unknown,
                "{}",
                request.name
            );
        }
    }
    Ok(())
}

fn assert_manual_unknown_after(changes: &[Change]) -> TestResult {
    assert_unknown_among(changes, &manual()?);
    Ok(())
}

/// Each change applied to each of the manual status's sets (not the reset) makes it unknown.
fn assert_sets_unknown_after(changes: &[Change]) -> TestResult {
    let sets: Vec<Put> = manual()?.into_iter().filter(|request| !request.no_body).collect();
    assert_unknown_among(changes, &sets);
    Ok(())
}

fn assert_unknown_among(changes: &[Change], requests: &[Put]) {
    for (what, change) in changes {
        for mut request in requests.iter().cloned() {
            change(&mut request);
            assert_eq!(
                request.classify(&opted_in()),
                Classification::Unknown,
                "{}: {what}",
                request.name
            );
        }
    }
}

fn swap_caller(request: &mut Put) {
    let caller = if request.no_body {
        "TeamsClient:setPresenceAvailability"
    } else {
        "TeamsClient:setPresenceAvailability-reset"
    };
    request.set_header("x-ms-client-caller", caller);
}

#[test]
fn a_manual_status_off_its_shape_is_unknown() -> TestResult {
    assert_manual_unknown_after(&[
        ("POST", |request| request.method = "POST".to_owned()),
        ("DELETE", |request| request.method = "DELETE".to_owned()),
        ("a query", |request| request.url.push_str("?expiry=1")),
        ("no trailing slash", |request| {
            request.url = request.url.trim_end_matches('/').to_owned();
        }),
        ("the other caller", swap_caller),
        ("a caller no capture showed", |request| {
            request.set_header("x-ms-client-caller", "presence-sync-strategy-client");
        }),
        ("no endpoint id header", |request| {
            remove_header(request, "x-ms-endpoint-id");
        }),
        ("an endpoint id in upper case", |request| {
            request.set_header("x-ms-endpoint-id", &ENDPOINT.to_uppercase());
        }),
        ("an endpoint id that isn't a GUID", |request| {
            request.set_header("x-ms-endpoint-id", "8:orgid:me");
        }),
        ("a header not captured", |request| {
            request.set_header("x-ms-test-user", "False");
        }),
        ("no content type", |request| remove_header(request, "content-type")),
    ])
}

/// A set (not a reset) with its body as `body`; a reset stays as it is, unless `body` is `None` (then a set loses its
/// body too).
fn set_with(request: &mut Put, body: Option<&str>) {
    if request.no_body {
        return;
    }
    match body {
        Some(body) => body.clone_into(&mut request.body),
        None => request.no_body = true,
    }
}

#[test]
fn a_manual_status_with_another_body_is_unknown() -> TestResult {
    assert_sets_unknown_after(&[
        ("be right back", |request| {
            set_with(request, Some(r#"{"availability":"BeRightBack"}"#));
        }),
        ("appear offline", |request| {
            set_with(request, Some(r#"{"availability":"Offline"}"#));
        }),
        ("in another case", |request| {
            set_with(request, Some(r#"{"availability":"busy"}"#));
        }),
        ("an activity beside it", |request| {
            set_with(request, Some(r#"{"availability":"Busy","activity":"Busy"}"#));
        }),
        ("a duration", |request| {
            set_with(
                request,
                Some(r#"{"availability":"Busy","desiredExpirationTime":"2026-10-23T12:00:00Z"}"#),
            );
        }),
        ("a note", |request| {
            set_with(request, Some(r#"{"availability":"Busy","note":"made up"}"#));
        }),
        ("the key in another case", |request| {
            set_with(request, Some(r#"{"Availability":"Busy"}"#));
        }),
        ("the key twice", |request| {
            set_with(request, Some(r#"{"availability":"Busy","availability":"Away"}"#));
        }),
        ("a list", |request| {
            set_with(request, Some(r#"[{"availability":"Busy"}]"#));
        }),
        ("an empty body", |request| set_with(request, Some(""))),
        ("an empty object", |request| set_with(request, Some("{}"))),
        ("no body", |request| set_with(request, None)),
    ])
}

#[test]
fn a_reset_with_any_body_is_unknown() -> TestResult {
    for body in ["", " ", "{}", "null", r#"{"availability":"Busy"}"#] {
        for mut request in manual()?.into_iter().filter(|request| request.no_body) {
            request.no_body = false;
            body.clone_into(&mut request.body);
            assert_eq!(
                request.classify(&opted_in()),
                Classification::Unknown,
                "{}: {body:?}",
                request.name
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_manual_status_on_the_endpoint_this_session_registered_passes_logged_as_own() -> TestResult {
    let lab = lab(opted_in())?;
    for request in manual()? {
        assert_eq!(
            lab.http.decide(&request.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}: before the registration",
            request.name
        );
    }
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Allow
    );
    for request in manual()? {
        assert_eq!(
            lab.http.decide(&request.paused()?).await,
            Verdict::Allow,
            "{}",
            request.name
        );
    }
    for request in manual_for(OTHER_ENDPOINT)? {
        assert_eq!(
            lab.http.decide(&request.paused()?).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{}: another endpoint",
            request.name
        );
    }
    let allowed: Vec<Value> = log_entries(&lab.log_path)?
        .into_iter()
        .filter(|entry| entry["verdict"] == "allow" && entry["own"] == "own_availability")
        .collect();
    assert_eq!(allowed.len(), 5);
    assert!(allowed.iter().all(|entry| entry["method"] == "PUT"), "{allowed:?}");
    Ok(())
}

#[tokio::test]
async fn a_reset_whose_body_the_event_hides_is_denied() -> TestResult {
    let lab = lab(opted_in())?;
    assert_eq!(
        lab.http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Allow
    );
    let hidden = [
        ("hasPostData", json!(true)),
        ("hasPostData", json!(false)),
        ("hasPostData", Value::Null),
        ("hasPostData", json!("true")),
        ("postDataEntries", json!([{"bytes": "e30="}])),
        ("postDataEntries", json!([])),
        ("postData", Value::Null),
        ("postData", json!(1)),
    ];
    for (key, value) in hidden {
        for mut request in manual()?.into_iter().filter(|request| request.no_body) {
            request.extra.push((key, value.clone()));
            let paused = request.paused()?;
            assert!(paused.body_unread, "{key}: {value}");
            assert_eq!(
                lab.http.decide(&paused).await,
                Verdict::Deny(DenyCause::Unclassified),
                "{}: {key}: {value}",
                request.name
            );
        }
    }
    Ok(())
}

#[test]
fn a_body_the_event_inlines_is_read() -> TestResult {
    for request in puts()? {
        let mut with_flag = request.clone();
        with_flag.extra.push(("hasPostData", json!(true)));
        let paused = with_flag.paused()?;
        assert!(!paused.body_unread, "{}", request.name);
        assert_eq!(
            with_flag.classify(&opted_in()),
            availability(ENDPOINT),
            "{}",
            request.name
        );
    }
    Ok(())
}

/// A log sink that refuses while its switch is on.
struct Switched(Arc<AtomicBool>);

impl std::io::Write for Switched {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.load(Ordering::SeqCst) {
            Err(std::io::Error::other("disk full"))
        } else {
            Ok(buf.len())
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_registration_the_log_refused_lets_nothing_through() -> TestResult {
    let refusing = Arc::new(AtomicBool::new(true));
    let roster = Roster::new([GUID_A].map(|guid| Mri::new(format!("8:orgid:{guid}"))), []);
    let fence = Arc::new(Fence::new(
        roster,
        FenceLog::from_writer(Switched(Arc::clone(&refusing))),
    ));
    let http = HttpFence::new(opted_in(), fence, Arc::new(FailClosed));
    assert_eq!(
        http.decide(&registration(ENDPOINT, MESSAGING)?).await,
        Verdict::Deny(DenyCause::LogUnavailable)
    );
    refusing.store(false, Ordering::SeqCst);
    for request in puts()?.into_iter().chain(manual()?) {
        assert_ne!(
            http.decide(&request.paused()?).await,
            Verdict::Allow,
            "{}",
            request.name
        );
    }
    Ok(())
}

#[test]
fn an_endpoint_counts_only_once_enrolled() -> TestResult {
    let endpoints = OwnEndpoints::new();
    let registration = registration(ENDPOINT, MESSAGING)?;
    let classification = || Classification::OwnAvailability {
        endpoint: ENDPOINT.to_owned(),
    };
    assert_eq!(endpoints.resolve(classification(), false), Classification::Unknown);
    // what the fence does with a registration whose verdict was Allow
    endpoints.enrol(&registration.method, &registration.url, registration.body.as_deref());
    assert_eq!(
        endpoints.resolve(classification(), false),
        Classification::Own(OwnKind::OwnAvailability)
    );
    assert_eq!(endpoints.resolve(classification(), true), Classification::Unknown);
    // the calling stack's registration enrols nothing
    let other = OwnEndpoints::new();
    let calling = self::registration(ENDPOINT, CALLING)?;
    other.enrol(&calling.method, &calling.url, calling.body.as_deref());
    assert_eq!(other.resolve(classification(), false), Classification::Unknown);
    // other classifications pass as they are
    assert_eq!(endpoints.resolve(Classification::Read, true), Classification::Read);
    Ok(())
}
