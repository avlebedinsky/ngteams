//! The browser fence's reviewed reads by `POST` (`proto_lab::post_read`): the middle tier's profile lookups the mention
//! picker needs, allowed only in their captured shape and only when every MRI they name is a lab account.
//!
//! The allowed shapes are the synthetic fixture `tests/fixtures/mt/profile-lookups.json`; every variant here is made
//! from it by changing one thing. Made-up GUIDs fill in its placeholders; no real MRI or token appears here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_http::{DenyCause, HttpFence, MalformedPause, PausedRequest, Verdict};
use proto_lab::post_read::{HEADER_VALUES_READ, POST_READS, PostReadKind};
use serde_json::{Value, json};
use teams_fence::{FailClosed, Fence, FenceLog, Mri, Roster};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJ1cG4iOiJwbGFudGVkIn0.cGxhbnRlZC1zaWduYXR1cmU";
const LOOKUPS: &str = include_str!("../../../tests/fixtures/mt/profile-lookups.json");

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:c>", GUID_C)
}

fn lab_mri(guid: &str) -> String {
    format!("8:orgid:{guid}")
}

/// One lookup of the fixture.
#[derive(Clone, Debug)]
struct Lookup {
    name: String,
    kind: PostReadKind,
    method: String,
    origin: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Value,
}

impl Lookup {
    fn url(&self) -> String {
        format!("{}{}?{}", self.origin, self.path, self.query)
    }

    fn body_bytes(&self) -> Vec<u8> {
        self.body.to_string().into_bytes()
    }

    /// Classified with its headers as the browser fence keeps them ([`PausedRequest::from_params`]). A pause the fence
    /// couldn't read (never, here: every field it needs is given) is a read, which no test expects of a lookup by
    /// `POST`, so it fails every assertion.
    fn classify(&self, classifier: &Classifier) -> Classification {
        let Ok(paused) = self.paused() else {
            return Classification::Read;
        };
        classifier.classify_with_headers(&self.method, &self.url(), &paused.headers, Some(&self.body_bytes()))
    }

    fn names(&self) -> Vec<Mri> {
        self.body
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).map(Mri::new).collect())
            .unwrap_or_default()
    }

    fn allowed(&self) -> Classification {
        Classification::PostRead {
            kind: self.kind,
            names: self.names(),
        }
    }

    /// The lookup as the browser fence reads it from a `Fetch.requestPaused` event.
    fn paused(&self) -> Result<PausedRequest, MalformedPause> {
        paused_event(&self.method, &self.url(), &self.headers, &self.body.to_string())
    }
}

/// A request read from a `Fetch.requestPaused` event, as the browser fence reads it.
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

fn lookups() -> TestResult<Vec<Lookup>> {
    let fixture: Value = serde_json::from_str(&fill(LOOKUPS))?;
    let query = fixture["query"].as_str().ok_or("no query")?;
    let headers: Vec<(String, String)> = fixture["headers"]
        .as_object()
        .ok_or("no headers")?
        .iter()
        .map(|(name, value)| (name.clone(), value.as_str().unwrap_or_default().to_owned()))
        .collect();
    let mut lookups = Vec::new();
    for lookup in fixture["lookups"].as_array().ok_or("no lookups")? {
        let kind = match lookup["kind"].as_str() {
            Some("short_profile") => PostReadKind::ShortProfile,
            Some("user_fetch") => PostReadKind::UserFetch,
            other => return Err(format!("an unknown kind {other:?}").into()),
        };
        let text = |key: &str| lookup[key].as_str().map(str::to_owned).ok_or(format!("no {key}"));
        lookups.push(Lookup {
            name: text("name")?,
            kind,
            method: text("method")?,
            origin: text("origin")?,
            path: text("path")?,
            query: query.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    let value = value.replace(
                        "<the request's own origin>",
                        lookup["origin"].as_str().unwrap_or_default(),
                    );
                    (name.clone(), value)
                })
                .collect(),
            body: lookup["body"].clone(),
        });
    }
    Ok(lookups)
}

/// Each lookup of the fixture with one thing changed, and what was changed.
fn variants(change: impl Fn(&mut Lookup) -> String) -> TestResult<Vec<(String, Lookup)>> {
    Ok(lookups()?
        .into_iter()
        .map(|mut lookup| {
            let what = change(&mut lookup);
            (format!("{}: {what}", lookup.name), lookup)
        })
        .collect())
}

fn assert_unknown(classifier: &Classifier, variants: &[(String, Lookup)]) {
    for (what, lookup) in variants {
        assert_eq!(lookup.classify(classifier), Classification::Unknown, "{what}");
    }
}

#[test]
fn each_listed_lookup_is_a_post_read_naming_its_mris() -> TestResult {
    let lookups = lookups()?;
    assert_eq!(lookups.len(), 4);
    for lookup in &lookups {
        assert_eq!(
            lookup.classify(&Classifier::mapped()),
            lookup.allowed(),
            "{}",
            lookup.name
        );
        // the calling classifier is the mapped one and more
        assert_eq!(
            lookup.classify(&Classifier::calling()),
            lookup.allowed(),
            "{}",
            lookup.name
        );
    }
    Ok(())
}

#[test]
fn every_entry_of_the_list_says_why() {
    assert_eq!(POST_READS.len(), 2);
    for entry in POST_READS {
        assert!(entry.reason.len() > 40, "{:?}", entry.kind);
    }
}

#[test]
fn the_classifiers_that_write_nothing_keep_the_lookups_unknown() -> TestResult {
    for lookup in lookups()? {
        for classifier in [Classifier::seeded(), Classifier::sign_in()] {
            assert_eq!(lookup.classify(&classifier), Classification::Unknown, "{}", lookup.name);
        }
    }
    Ok(())
}

#[test]
fn lookalike_origins_are_unknown() -> TestResult {
    let origins = [
        "https://teams.microsoft.com.example.net",
        "https://eteams.microsoft.com",
        "https://teams.microsoft.us",
        "https://teams.live.com",
        "https://microsoft.com",
        "http://teams.microsoft.com",
        "https://teams.microsoft.com:8443",
        "https://teams.cloud.microsoft.example.net",
        "https://cloud.microsoft",
        "https://user@teams.microsoft.com",
    ];
    for origin in origins {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                lookup.origin = origin.to_owned();
                origin.to_owned()
            })?,
        );
    }
    Ok(())
}

#[test]
fn other_paths_are_unknown() -> TestResult {
    let paths = [
        "/api/mt/part/amer-01/beta/users/fetchShortProfiles",
        "/api/mt/part/amer-01/beta/users/FetchShortProfile",
        "/api/mt/part/amer-01/beta/users/fetchshortprofile",
        "/api/mt/part/amer-01/beta/users/fetch/",
        "/api/mt/part/amer-01/beta/users/fetch/8:orgid:x",
        "/api/mt/part/amer-01/beta/users/fetchfeaturesettings",
        "/api/mt/part/amer-01/v1/users/fetch",
        "/api/mt/part/amer-01/beta/teams/fetch",
        "/api/mt/part/AMER-01/beta/users/fetch",
        "/api/mt/part/amer/beta/users/fetch",
        "/api/mt/part/-01/beta/users/fetch",
        "/api/mt/part//beta/users/fetch",
        "/api/mt/beta/users/fetch",
        "/api/mt/part/amer-01/amer-01/beta/users/fetch",
        "/api/mt/part/amer-01/beta/users/%66etch",
        "/api/mt/part/amer%2D01/beta/users/fetch",
    ];
    for path in paths {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                lookup.path = path.to_owned();
                path.to_owned()
            })?,
        );
    }
    Ok(())
}

#[test]
fn other_methods_are_unknown_and_reading_ones_stay_reads() -> TestResult {
    for method in ["PUT", "PATCH", "DELETE", "PROPFIND"] {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                lookup.method = method.to_owned();
                method.to_owned()
            })?,
        );
    }
    for lookup in lookups()? {
        let get = Lookup {
            method: "GET".to_owned(),
            ..lookup.clone()
        };
        assert_eq!(
            get.classify(&Classifier::mapped()),
            Classification::Read,
            "{}",
            lookup.name
        );
    }
    Ok(())
}

#[test]
fn a_query_other_than_the_captured_one_is_unknown() -> TestResult {
    let queries = [
        "",
        "isMailAddress=true&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true",
        "isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=true&includeIBBarredUsers=true&includeDisabledAccounts=true",
        // one left out, one added, one twice, one in another case
        "isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true",
        "isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true&top=50",
        "isMailAddress=false&isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true",
        "IsMailAddress=false&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true",
        "isMailAddress=False&enableGuest=true&skypeTeamsInfo=true&canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true",
    ];
    for query in queries {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                lookup.query = query.to_owned();
                format!("query {query:?}")
            })?,
        );
    }
    // the order of the captured pairs plays no part
    for lookup in lookups()? {
        let reordered = Lookup {
            query: "includeDisabledAccounts=true&includeIBBarredUsers=true&canBeSmtpAddress=false&skypeTeamsInfo=true\
                    &enableGuest=true&isMailAddress=false"
                .to_owned(),
            ..lookup.clone()
        };
        assert_eq!(
            reordered.classify(&Classifier::mapped()),
            lookup.allowed(),
            "{}",
            lookup.name
        );
    }
    Ok(())
}

/// A change to a lookup's headers.
type Change = fn(&mut Vec<(String, String)>);

/// Sets header `name` (as the fixture spells it) to `value`.
fn set(headers: &mut [(String, String)], name: &str, value: &str) {
    for (header, old) in headers.iter_mut() {
        if header == name {
            value.clone_into(old);
        }
    }
}

/// Asserts that each change makes every lookup unknown.
fn assert_unknown_after(changes: &[(&str, Change)]) -> TestResult {
    for &(what, change) in changes {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                change(&mut lookup.headers);
                what.to_owned()
            })?,
        );
    }
    Ok(())
}

#[test]
fn headers_other_than_the_captured_ones_are_unknown() -> TestResult {
    let content_type = |value: &str| ("content-type".to_owned(), value.to_owned());
    assert_unknown_after(&[
        ("another origin", |headers| {
            headers.retain(|(name, _)| name != "Origin");
            headers.push(("Origin".to_owned(), "https://example.com".to_owned()));
        }),
        ("Origin twice", |headers| {
            headers.push(("origin".to_owned(), "https://example.com".to_owned()));
        }),
        ("no headers", |headers| headers.clear()),
        ("no content type", |headers| {
            headers.retain(|(name, _)| name != "content-type");
        }),
        ("a method override", |headers| {
            headers.push(("X-HTTP-Method-Override".to_owned(), "DELETE".to_owned()));
        }),
        ("a header never seen", |headers| {
            headers.push(("x-ms-scenario".to_owned(), "write".to_owned()));
        }),
        ("content type twice", |headers| {
            headers.push(("Content-Type".to_owned(), "application/json;charset=UTF-8".to_owned()));
        }),
        ("another content type", |headers| {
            headers.retain(|(name, _)| name != "content-type");
            headers.push(("content-type".to_owned(), "text/plain".to_owned()));
        }),
        ("JSON without the charset", |headers| {
            headers.retain(|(name, _)| name != "content-type");
            headers.push(("content-type".to_owned(), "application/json".to_owned()));
        }),
    ])?;
    // a header's name in another case is the same header
    for lookup in lookups()? {
        let mut renamed = lookup.clone();
        renamed.headers.retain(|(name, _)| name != "content-type");
        renamed.headers.push(content_type("application/json;charset=UTF-8"));
        renamed
            .headers
            .iter_mut()
            .for_each(|(name, _)| *name = name.to_ascii_uppercase());
        assert_eq!(
            renamed.classify(&Classifier::mapped()),
            lookup.allowed(),
            "{}",
            lookup.name
        );
        // and without the headers (the plain `classify`) no lookup is a read by POST
        let classified = Classifier::mapped().classify(&lookup.method, &lookup.url(), Some(&lookup.body_bytes()));
        assert_eq!(classified, Classification::Unknown, "{}", lookup.name);
    }
    Ok(())
}

#[test]
fn pinned_header_values_other_than_the_captured_ones_are_unknown() -> TestResult {
    assert_unknown_after(&[
        // the object id the captures always left empty, naming a lab account, an outsider or anything
        ("a lab account's object id", |headers| {
            set(headers, "x-ms-object-id", GUID_A);
        }),
        ("an outsider's object id", |headers| {
            set(headers, "x-ms-object-id", GUID_X);
        }),
        ("an object id's MRI", |headers| {
            set(headers, "x-ms-object-id", &lab_mri(GUID_B));
        }),
        ("a blank object id", |headers| {
            set(headers, "x-ms-object-id", " ");
        }),
        // the client's flags, off their captured values
        ("x-ms-migration False", |headers| {
            set(headers, "x-ms-migration", "False");
        }),
        ("x-ms-migration true", |headers| {
            set(headers, "x-ms-migration", "true");
        }),
        ("x-ms-test-user True", |headers| {
            set(headers, "x-ms-test-user", "True");
        }),
        ("x-ms-client-request-type 1", |headers| {
            set(headers, "x-ms-client-request-type", "1");
        }),
        ("x-ms-client-type web", |headers| {
            set(headers, "x-ms-client-type", "web");
        }),
        // a flag left out plays no part (below), but one given twice, in another case, does
        ("x-ms-test-user twice", |headers| {
            headers.push(("X-MS-Test-User".to_owned(), "False".to_owned()));
        }),
    ])
}

#[test]
fn callers_that_could_carry_an_id_are_unknown() -> TestResult {
    assert_unknown_after(&[
        ("a caller with an object id", |headers| {
            set(headers, "x-ms-client-caller", &format!("use-chat-hook:{GUID_X}"));
        }),
        ("a caller with a hex run", |headers| {
            set(headers, "x-ms-client-caller", "chat-deadbeef");
        }),
        ("an empty caller", |headers| {
            set(headers, "x-ms-client-caller", "");
        }),
        ("a caller with a space", |headers| {
            set(headers, "x-ms-client-caller", "use chat hook");
        }),
        ("a caller with a slash", |headers| {
            set(headers, "x-ms-client-caller", "users/fetch");
        }),
        ("a caller too long", |headers| {
            set(headers, "x-ms-client-caller", &"x".repeat(129));
        }),
    ])
}

#[test]
fn callers_of_the_captured_kind_and_unpinned_values_pass() -> TestResult {
    let passing: [(&str, Change); 7] = [
        ("a caller with a colon", |headers| {
            set(headers, "x-ms-client-caller", "startup-job:update-sync");
        }),
        ("a caller with a dot", |headers| {
            set(headers, "x-ms-client-caller", "User.avatarUrl");
        }),
        ("a caller of 128 bytes", |headers| {
            set(headers, "x-ms-client-caller", &"x".repeat(128));
        }),
        ("no caller", |headers| {
            headers.retain(|(name, _)| name != "x-ms-client-caller");
        }),
        ("no object id", |headers| {
            headers.retain(|(name, _)| name != "x-ms-object-id");
        }),
        ("no migration flag", |headers| {
            headers.retain(|(name, _)| name != "x-ms-migration");
        }),
        ("another session id", |headers| {
            set(headers, "x-ms-session-id", "another-made-up-session");
        }),
    ];
    for (what, change) in passing {
        let lookups = variants(|lookup| {
            change(&mut lookup.headers);
            what.to_owned()
        })?;
        for (variant, lookup) in lookups {
            assert_eq!(lookup.classify(&Classifier::mapped()), lookup.allowed(), "{variant}");
        }
    }
    Ok(())
}

#[test]
fn a_body_other_than_a_list_of_mris_is_unknown() -> TestResult {
    let mri = lab_mri(GUID_B);
    let bodies = [
        json!([]),
        json!({"mris": [mri]}),
        json!({"members": [{"id": mri, "role": "Admin"}]}),
        json!([{"mri": mri}]),
        json!([mri, 7]),
        json!([mri, null]),
        json!([[mri]]),
        json!(mri),
        json!(null),
        json!([mri, {"id": mri, "role": "Admin"}]),
    ];
    for body in &bodies {
        assert_unknown(
            &Classifier::mapped(),
            &variants(|lookup| {
                lookup.body = body.clone();
                format!("body {body}")
            })?,
        );
    }
    for lookup in lookups()? {
        let url = lookup.url();
        let classifier = Classifier::mapped();
        let headers = lookup.paused()?.headers;
        let classify = |body| classifier.classify_with_headers("POST", &url, &headers, body);
        for raw in [&b""[..], b"[", br#"["8:orgid:x"] ["8:orgid:y"]"#, b"not json"] {
            assert_eq!(classify(Some(raw)), Classification::Unknown, "{}", lookup.name);
        }
        assert_eq!(classify(None), Classification::Unknown, "{}", lookup.name);
    }
    Ok(())
}

#[test]
fn a_paused_lookup_keeps_no_credential() -> TestResult {
    for lookup in lookups()? {
        let mut with_token = lookup.clone();
        set(&mut with_token.headers, "authorization", &format!("Bearer {TOKEN}"));
        set(&mut with_token.headers, "Cookie", &format!("authtoken={TOKEN}"));
        let paused = with_token.paused()?;
        assert_eq!(paused.headers.len(), lookup.headers.len(), "every name is kept");
        for (name, value) in &paused.headers {
            let read = HEADER_VALUES_READ.contains(&name.to_ascii_lowercase().as_str());
            assert_eq!(value.is_some(), read, "{}: {name}", lookup.name);
        }
        for credential in ["authorization", "Cookie"] {
            assert!(
                paused.headers.contains(&(credential.to_owned(), None)),
                "{}: {credential}",
                lookup.name
            );
        }
        assert!(!format!("{paused:?}").contains(TOKEN), "{}", lookup.name);
        // and the lookup still passes: the list reads no credential
        assert_eq!(
            with_token.classify(&Classifier::mapped()),
            lookup.allowed(),
            "{}",
            lookup.name
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_bot_invokes_stay_unknown() -> TestResult {
    let lab = lab()?;
    let lookup = lookups()?.remove(0);
    assert_eq!(lookup.origin, "https://teams.microsoft.com");
    let body = json!({
        "appId": "00000000-0000-4000-8000-0000000000b0",
        "conversation": {"id": format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")},
        "imdisplayname": "Lab A",
        "name": "composeExtension/fetchCommands",
        "userAadToken": TOKEN,
        "value": {"commandContext": "compose"},
    })
    .to_string();
    for origin in ["https://teams.microsoft.com", "https://teams.cloud.microsoft"] {
        let url = format!("{origin}/api/chatsvc/amer/v1/agents/28:00000000-0000-4000-8000-0000000000b0/invoke");
        // with the headers a listed lookup carries, its own origin among them, as the fence pauses it
        let headers: Vec<(String, String)> = lookup
            .headers
            .iter()
            .map(|(name, value)| (name.clone(), value.replace(&lookup.origin, origin)))
            .collect();
        let paused = paused_event("POST", &url, &headers, &body)?;
        for classifier in [Classifier::mapped(), Classifier::calling()] {
            assert_eq!(
                classifier.classify_with_headers("POST", &url, &paused.headers, Some(body.as_bytes())),
                Classification::Unknown,
                "{url}"
            );
        }
        assert_eq!(
            lab.http.decide(&paused).await,
            Verdict::Deny(DenyCause::Unclassified),
            "{url}"
        );
    }
    assert_eq!(lab.http.denials().len(), 2);
    let entries = log_entries(&lab.log_path)?;
    assert!(
        entries
            .iter()
            .all(|entry| entry["reason"] == "unclassified" && entry.get("post_read").is_none()),
        "{entries:?}"
    );
    Ok(())
}

struct Lab {
    _dir: tempfile::TempDir,
    log_path: PathBuf,
    http: HttpFence,
}

/// An HTTP fence of the mapped classifier over a roster of a, b and c (and the echo service), with a log on disk.
fn lab() -> TestResult<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let roster = Roster::new(
        [GUID_A, GUID_B, GUID_C].map(|guid| Mri::new(lab_mri(guid))),
        [Mri::new(ECHO)],
    );
    let fence = Arc::new(Fence::new(roster, FenceLog::open(&log_path)?));
    let http = HttpFence::new(Classifier::mapped(), fence, Arc::new(FailClosed));
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
async fn a_lookup_of_lab_accounts_passes_logged_as_a_post_read() -> TestResult {
    let lab = lab()?;
    let lookups = lookups()?;
    for lookup in &lookups {
        assert_eq!(
            lab.http.decide(&lookup.paused()?).await,
            Verdict::Allow,
            "{}",
            lookup.name
        );
    }
    assert!(lab.http.denials().is_empty());
    let entries = log_entries(&lab.log_path)?;
    assert_eq!(entries.len(), lookups.len());
    for (entry, lookup) in entries.iter().zip(&lookups) {
        assert_eq!(entry["verdict"], "allow");
        assert_eq!(entry["method"], "POST");
        assert_eq!(entry["post_read"], lookup.kind.as_str());
        assert_eq!(entry["url"], format!("{}{}", lookup.origin, lookup.path), "no query");
        assert!(entry.get("own").is_none(), "not own-state: {entry}");
        assert!(entry.get("kind").is_none(), "not a write: {entry}");
    }
    Ok(())
}

#[tokio::test]
async fn a_lookup_naming_anyone_but_lab_accounts_is_denied() -> TestResult {
    let lab = lab()?;
    let outsider = lab_mri(GUID_X);
    let bodies = [
        json!([outsider]),
        json!([lab_mri(GUID_A), outsider]),
        // an approved service participant isn't a lab account
        json!([ECHO]),
        json!([ECHO, lab_mri(GUID_A)]),
        // an account's MRI in another case, or by its address
        json!([lab_mri(GUID_A).to_uppercase()]),
        json!([format!("8:orgid:{}", GUID_A.to_uppercase())]),
        json!(["lab-a@example.com"]),
        json!([format!("28:{GUID_A}")]),
    ];
    let mut denied = 0;
    for lookup in lookups()? {
        for body in &bodies {
            let lookup = Lookup {
                body: body.clone(),
                ..lookup.clone()
            };
            assert_eq!(
                lab.http.decide(&lookup.paused()?).await,
                Verdict::Deny(DenyCause::Unclassified),
                "{}: {body}",
                lookup.name
            );
            denied += 1;
        }
    }
    assert_eq!(lab.http.denials().len(), denied);
    let entries = log_entries(&lab.log_path)?;
    assert_eq!(entries.len(), denied);
    assert!(
        entries
            .iter()
            .all(|entry| entry["verdict"] == "deny" && entry["reason"] == "unclassified"),
        "{entries:?}"
    );
    Ok(())
}
