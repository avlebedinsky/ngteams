//! The media service's object creation, mapped once (M2 Task 21, `docs/backlog/ams-image-sending.md` step 1): only
//! the exact shape `ab-image` captured, for the one conversation the classifier is given, maps to a
//! `WriteKind::MediaObject` on that conversation, which the browser fence judges by its live members; every other
//! shape, the upload and a forward stay unknown and denied. The request is written from the capture's shape with
//! made-up ids; the thread answer is `tests/fixtures/chatsvc/thread-one-to-one.json`.

use std::sync::Arc;

use proto_lab::classify::{Classification, Classifier};
use proto_lab::fence_http::{DenyCause, HttpFence, PausedRequest, Verdict};
use proto_lab::media::{CREATION_URL, HEADER_VALUES_READ, object_creation};
use serde_json::{Value, json};
use teams_fence::{DenyReason, Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri, Roster, Scope, WriteKind};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type Headers = Vec<(String, Option<String>)>;
type BodyChange = Box<dyn Fn(&mut Value)>;
type HeaderChange = Box<dyn Fn(&mut Headers)>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const THREAD: &str = include_str!("../../../tests/fixtures/chatsvc/thread-one-to-one.json");

fn chat(one: &str, other: &str) -> String {
    format!("19:{one}_{other}@unq.gbl.spaces")
}

fn ab() -> String {
    chat(GUID_A, GUID_B)
}

/// The headers of a captured creation as the fence pauses it (values only for those it keeps), the first try's
/// client hints included.
fn captured_headers() -> Vec<(String, Option<String>)> {
    [
        ("Origin", Some("https://teams.microsoft.com")),
        ("Referer", None),
        ("User-Agent", None),
        ("accept", None),
        ("authorization", None),
        ("content-type", Some("application/json")),
        ("ms-cv", None),
        ("sec-ch-ua", None),
        ("sec-ch-ua-mobile", None),
        ("sec-ch-ua-platform", None),
        ("x-ams-post-sharing-mode", Some("Inline")),
        ("x-ms-client-version", None),
        ("x-ms-migration", Some("True")),
        ("x-ms-test-user", Some("False")),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.map(str::to_owned)))
    .collect()
}

fn captured_body(chat: &str) -> Value {
    json!({"type": "pish/image", "permissions": {chat: ["read"]}, "sharingMode": "Inline", "filename": "MicrosoftTeams-image.png"})
}

fn classify(
    classifier: &Classifier,
    method: &str,
    url: &str,
    headers: &[(String, Option<String>)],
    body: &Value,
) -> Classification {
    classifier.classify_with_headers(method, url, headers, Some(body.to_string().as_bytes()))
}

fn creating() -> Classifier {
    Classifier::mapped().with_media_object_creation(&ab())
}

#[test]
fn the_captured_creation_maps_to_a_media_object_on_the_given_chat_alone() {
    let target = teams_fence::Target::new(WriteKind::MediaObject, Scope::Conversation { id: ab() });
    assert_eq!(
        classify(
            &creating(),
            "POST",
            CREATION_URL,
            &captured_headers(),
            &captured_body(&ab())
        ),
        Classification::Write(target.clone())
    );
    // without the client hints, as every later try sent it
    let later: Vec<_> = captured_headers()
        .into_iter()
        .filter(|(name, _)| !name.starts_with("sec-ch-ua"))
        .collect();
    assert_eq!(
        classify(&creating(), "POST", CREATION_URL, &later, &captured_body(&ab())),
        Classification::Write(target)
    );
    // nothing maps it without the experiment's classifier, nor for another chat
    for classifier in [
        Classifier::mapped(),
        Classifier::seeded(),
        Classifier::mapped().with_media_object_creation(&chat(GUID_A, GUID_C)),
        Classifier::mapped().with_media_object_creation("19:channel0001@thread.tacv2"),
    ] {
        assert_eq!(
            classify(
                &classifier,
                "POST",
                CREATION_URL,
                &captured_headers(),
                &captured_body(&ab())
            ),
            Classification::Unknown
        );
    }
}

#[test]
fn any_other_url_or_method_is_unknown() {
    let headers = captured_headers();
    let body = captured_body(&ab());
    for (method, url) in [
        ("PUT", CREATION_URL),
        ("PATCH", CREATION_URL),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/?x=1"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v1/objects/#x"),
        ("POST", "https://u@us-prod.asyncgw.teams.microsoft.com/v1/objects/"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com:8443/v1/objects/"),
        ("POST", "http://us-prod.asyncgw.teams.microsoft.com/v1/objects/"),
        ("POST", "https://us-api.asm.skype.com/v1/objects/"),
        ("POST", "https://teams.microsoft.com/asm/amer/v1/objects/"),
        ("POST", "https://teams.cloud.microsoft/ams/v1/objects/"),
        ("POST", "https://eu-prod.asyncgw.teams.microsoft.com/v1/objects/"),
        ("POST", "https://us-prod.asyncgw.teams.microsoft.com/v2/objects/"),
    ] {
        assert_eq!(
            classify(&creating(), method, url, &headers, &body),
            Classification::Unknown,
            "{method} {url}"
        );
    }
}

#[test]
fn the_upload_and_a_forward_stay_unknown() {
    let object = "0-wus-d1-0123456789abcdef0123456789abcdef";
    let upload = format!("https://us-prod.asyncgw.teams.microsoft.com/v1/objects/{object}/content/imgpsh");
    let forward = format!("https://us-prod.asyncgw.teams.microsoft.com/v1/objects/{object}/forward");
    let classifier = creating();
    let headers = captured_headers();
    assert_eq!(
        classifier.classify_with_headers("PUT", &upload, &headers, Some(b"\x89PNG")),
        Classification::Unknown
    );
    let readers = json!({"permissions": {ab(): ["read"]}});
    assert_eq!(
        classify(&classifier, "POST", &forward, &headers, &readers),
        Classification::Unknown
    );
    assert_eq!(
        classify(
            &classifier,
            "DELETE",
            &format!("https://us-prod.asyncgw.teams.microsoft.com/v1/objects/{object}"),
            &headers,
            &json!({})
        ),
        Classification::Unknown
    );
}

#[test]
fn a_body_in_any_other_shape_is_unknown() {
    let headers = captured_headers();
    let with = |change: &dyn Fn(&mut Value)| {
        let mut body = captured_body(&ab());
        change(&mut body);
        classify(&creating(), "POST", CREATION_URL, &headers, &body)
    };
    let cases: Vec<(&str, BodyChange)> = vec![
        (
            "another chat",
            Box::new(|body| body["permissions"] = json!({chat(GUID_A, GUID_C): ["read"]})),
        ),
        (
            "a second reader",
            Box::new(|body| body["permissions"][chat(GUID_A, GUID_C)] = json!(["read"])),
        ),
        (
            "the composer key",
            Box::new(|body| body["permissions"]["@:composer"] = json!(["read"])),
        ),
        ("no reader", Box::new(|body| body["permissions"] = json!({}))),
        (
            "more rights",
            Box::new(|body| body["permissions"][ab()] = json!(["read", "write"])),
        ),
        (
            "rights as a string",
            Box::new(|body| body["permissions"][ab()] = json!("read")),
        ),
        ("a file", Box::new(|body| body["type"] = json!("sharing/file"))),
        (
            "a video",
            Box::new(|body| body["type"] = json!("sharing/videototranscode")),
        ),
        ("another mode", Box::new(|body| body["sharingMode"] = json!("Shared"))),
        (
            "another name",
            Box::new(|body| body["filename"] = json!("lab-image.png")),
        ),
        ("an extra key", Box::new(|body| body["expiry"] = json!(1))),
        (
            "a key left out",
            Box::new(|body| {
                body.as_object_mut().map(|object| object.remove("filename"));
            }),
        ),
        (
            "a key in another case",
            Box::new(|body| {
                if let Some(object) = body.as_object_mut() {
                    let value = object.remove("type").unwrap_or_default();
                    object.insert("Type".to_owned(), value);
                }
            }),
        ),
    ];
    for (case, change) in cases {
        assert_eq!(with(&*change), Classification::Unknown, "{case}");
    }
    // a key twice, or twice in two cases, is ambiguous
    let chat = ab();
    for raw in [
        format!(
            r#"{{"type":"pish/image","type":"pish/image","permissions":{{"{chat}":["read"]}},"sharingMode":"Inline","filename":"MicrosoftTeams-image.png"}}"#
        ),
        format!(
            r#"{{"type":"pish/image","permissions":{{"{chat}":["read"]}},"Permissions":{{}},"sharingMode":"Inline","filename":"MicrosoftTeams-image.png"}}"#
        ),
    ] {
        assert_eq!(
            creating().classify_with_headers("POST", CREATION_URL, &headers, Some(raw.as_bytes())),
            Classification::Unknown
        );
    }
    assert_eq!(
        creating().classify_with_headers("POST", CREATION_URL, &headers, None),
        Classification::Unknown
    );
}

#[test]
fn headers_in_any_other_shape_are_unknown() {
    let body = captured_body(&ab());
    let with = |change: &dyn Fn(&mut Headers)| {
        let mut headers = captured_headers();
        change(&mut headers);
        classify(&creating(), "POST", CREATION_URL, &headers, &body)
    };
    let set = |name: &'static str, value: Option<&'static str>| {
        move |headers: &mut Vec<(String, Option<String>)>| {
            for header in headers
                .iter_mut()
                .filter(|(header, _)| header.eq_ignore_ascii_case(name))
            {
                header.1 = value.map(str::to_owned);
            }
        }
    };
    let remove = |name: &'static str| {
        move |headers: &mut Vec<(String, Option<String>)>| {
            headers.retain(|(header, _)| !header.eq_ignore_ascii_case(name));
        }
    };
    let cases: Vec<(&str, HeaderChange)> = vec![
        (
            "another sharing mode",
            Box::new(set("x-ams-post-sharing-mode", Some("Shared"))),
        ),
        ("no sharing mode", Box::new(remove("x-ams-post-sharing-mode"))),
        ("an unread sharing mode", Box::new(set("x-ams-post-sharing-mode", None))),
        (
            "another content type",
            Box::new(set("content-type", Some("text/plain"))),
        ),
        ("no content type", Box::new(remove("content-type"))),
        ("no migration flag", Box::new(remove("x-ms-migration"))),
        ("another migration flag", Box::new(set("x-ms-migration", Some("False")))),
        ("a test user", Box::new(set("x-ms-test-user", Some("True")))),
        ("another origin", Box::new(set("Origin", Some("https://evil.example")))),
        ("an unread origin", Box::new(set("Origin", None))),
        (
            "a header no capture showed",
            Box::new(|headers: &mut Vec<_>| headers.push(("x-ms-forward-to".to_owned(), None))),
        ),
        (
            "a header twice",
            Box::new(|headers: &mut Vec<_>| {
                headers.push(("Content-Type".to_owned(), Some("application/json".to_owned())));
            }),
        ),
    ];
    for (case, change) in cases {
        assert_eq!(with(&*change), Classification::Unknown, "{case}");
    }
    // the other origin of the web client, or none, is the same request
    assert!(matches!(
        with(&set("Origin", Some("https://teams.cloud.microsoft"))),
        Classification::Write(_)
    ));
    assert!(matches!(with(&remove("Origin")), Classification::Write(_)));
}

#[test]
fn a_paused_creation_keeps_the_pinned_values_and_never_the_credential() -> TestResult {
    let params = json!({"requestId": "interception-1", "networkId": "1.1", "request": {
        "method": "POST",
        "url": CREATION_URL,
        "headers": {"authorization": format!("Bearer {TOKEN}"), "x-ams-post-sharing-mode": "Inline",
            "x-ms-migration": "True", "x-ms-test-user": "False", "content-type": "application/json",
            "Origin": "https://teams.microsoft.com", "ms-cv": "made-up-cv"},
        "postData": captured_body(&ab()).to_string(),
    }});
    let paused = PausedRequest::from_params(&params)?;
    let value = |name: &str| {
        paused
            .headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .and_then(|(_, value)| value.clone())
    };
    for name in HEADER_VALUES_READ {
        assert!(value(name).is_some(), "{name}");
    }
    assert_eq!(value("authorization"), None);
    assert_eq!(value("ms-cv"), None);
    assert!(!format!("{paused:?}").contains(TOKEN));
    assert!(matches!(
        object_creation(
            "POST",
            &Url::parse(&paused.url)?,
            &paused.headers,
            paused.body.as_deref(),
            &ab()
        ),
        Some(target) if target.kind() == WriteKind::MediaObject
    ));
    Ok(())
}

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<region>", "emea")
}

fn fence(lab: &[&str]) -> Fence {
    Fence::new(
        Roster::new(lab.iter().map(|guid| Mri::new(format!("8:orgid:{guid}"))), []),
        FenceLog::from_writer(std::io::sink()),
    )
}

#[tokio::test]
async fn the_fence_judges_the_creation_by_the_chats_live_members() -> TestResult {
    let server = MockServer::start().await;
    let fixture: Value = serde_json::from_str(&fill(THREAD))?;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", ab())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture["response"]["body"]))
        .mount(&server)
        .await;
    let live = Arc::new(LiveMembershipSource::with_mock_origin(&server.uri())?);
    let members: Arc<dyn MembershipSource + Send> = live.clone();
    let credentials =
        HttpFence::new(creating(), Arc::new(fence(&[])), Arc::clone(&members)).with_credentials(live.clone());
    credentials.offer_credential(&json!({"requestId": "interception-0", "request": {
        "method": "GET",
        "url": "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations?view=msnp24Equivalent",
        "headers": {"Authentication": format!("skypetoken={TOKEN}")},
    }}));
    let creation = PausedRequest {
        request_id: "interception-1".to_owned(),
        method: "POST".to_owned(),
        url: CREATION_URL.to_owned(),
        headers: captured_headers(),
        body: Some(captured_body(&ab()).to_string().into_bytes()),
        network_id: Some("1.1".to_owned()),
    };
    let decide = async |lab: &[&str]| {
        HttpFence::new(creating(), Arc::new(fence(lab)), Arc::clone(&members))
            .decide(&creation)
            .await
    };
    assert_eq!(decide(&[GUID_A, GUID_B]).await, Verdict::Allow);
    assert_eq!(
        decide(&[GUID_A]).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    // the mapped classifier alone doesn't know it: unknown, denied
    let unmapped = HttpFence::new(
        Classifier::mapped(),
        Arc::new(fence(&[GUID_A, GUID_B])),
        Arc::clone(&members),
    );
    assert_eq!(unmapped.decide(&creation).await, Verdict::Deny(DenyCause::Unclassified));
    Ok(())
}
