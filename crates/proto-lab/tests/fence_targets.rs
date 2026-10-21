//! The fence target mapping (`docs/protocol/fence-targets.md`): which chat service writes map to which `Target`, how
//! the browser fence decides them with the live membership source, where the source's credential comes from, and the
//! forced deny `ab-chat`'s live enforcement test uses.
//!
//! The requests are the synthetic fixture `tests/fixtures/chatsvc/writes.json` (sanitized form) and the writes of the
//! capture-shaped fixtures beside it (`edit.json`, `delete.json`, `reaction.json`, `reply.json`, and the bundle-shaped
//! `mention.json`); the thread answer is `tests/fixtures/chatsvc/thread-one-to-one.json`. Made-up GUIDs fill in their placeholders; no real MRI or token
//! appears here. The forced-deny test runs a throwaway armed browser on a local page.

mod common;
#[path = "common/page.rs"]
mod page;

use std::sync::Arc;
use std::time::Duration;

use proto_lab::arming::{ArmOptions, ArmedBrowser};
use proto_lab::browser::{Browser, LaunchOptions};
use proto_lab::cdp::SessionId;
use proto_lab::classify::{Classification, Classifier, OwnKind, OwnRule, chat_service_path};
use proto_lab::fence_http::{DenyCause, ForcedDeny, HttpFence, PausedRequest, Verdict};
use proto_lab::fence_ws::{FrameClassifier, GuardScript, WsFence};
use proto_lab::scenario::dsl::{Driver, RequestPattern, SeenRequest, Selector};
use serde_json::{Value, json};
use teams_fence::{
    ChatCredential, DenyReason, FailClosed, Fence, FenceLog, LiveMembershipSource, MembershipSource, Mri,
    PartialReason, Roster, Scope, Target, WriteKind,
};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const WRITES: &str = include_str!("../../../tests/fixtures/chatsvc/writes.json");
const THREAD: &str = include_str!("../../../tests/fixtures/chatsvc/thread-one-to-one.json");

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:x>", GUID_X)
        .replace("<region>", "emea")
}

fn lab_mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn one_to_one() -> String {
    format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")
}

/// One fixture case: the request, and the target it should map to (`None`: unknown).
struct Case {
    name: String,
    method: String,
    url: String,
    body: Option<Vec<u8>>,
    expect: Option<Target>,
}

fn cases() -> TestResult<Vec<Case>> {
    let fixture: Value = serde_json::from_str(&fill(WRITES))?;
    let base = fixture["base"].as_str().ok_or("no base")?;
    let mut cases = Vec::new();
    for case in fixture["cases"].as_array().ok_or("no cases")? {
        let url = match case["url"].as_str() {
            Some(url) => url.to_owned(),
            None => format!("{base}{}", case["path"].as_str().ok_or("no path")?),
        };
        let body = match &case["body"] {
            Value::Null => None,
            body => Some(serde_json::to_vec(body)?),
        };
        cases.push(Case {
            name: case["name"].as_str().unwrap_or_default().to_owned(),
            method: case["method"].as_str().ok_or("no method")?.to_owned(),
            url,
            body,
            expect: expected(&case["expect"])?,
        });
    }
    Ok(cases)
}

fn expected(expect: &Value) -> TestResult<Option<Target>> {
    if expect == "unknown" {
        return Ok(None);
    }
    let kind = WriteKind::ALL
        .into_iter()
        .find(|kind| expect["kind"] == kind.as_str())
        .ok_or("an unknown kind")?;
    let scope = match expect["conversation"].as_str() {
        Some(id) => Scope::Conversation { id: id.to_owned() },
        None if expect["new"] == true => Scope::New,
        None => return Err("no scope".into()),
    };
    let added: Vec<Mri> = expect["added"]
        .as_array()
        .map(|added| added.iter().filter_map(Value::as_str).map(Mri::new).collect())
        .unwrap_or_default();
    Ok(Some(Target::new(kind, scope).with_added(added)))
}

#[test]
fn the_mapped_classifier_maps_each_chat_write_to_its_target() -> TestResult {
    let classifier = Classifier::mapped();
    let cases = cases()?;
    assert!(cases.len() > 20);
    for case in &cases {
        let got = classifier.classify(&case.method, &case.url, case.body.as_deref());
        let want = case
            .expect
            .clone()
            .map_or(Classification::Unknown, Classification::Write);
        assert_eq!(got, want, "{}", case.name);
    }
    Ok(())
}

/// The writes of the capture-shaped fixtures (`docs/protocol/chat.md`): each fixture's requests, by where they sit in
/// it, with the write they must map to on the lab chat.
const SHAPED: [(&str, &str, WriteKind); 6] = [
    ("edit.json", "/request", WriteKind::Edit),
    ("delete.json", "/request", WriteKind::Delete),
    ("reply.json", "/request", WriteKind::Message),
    ("mention.json", "/request", WriteKind::Message),
    ("reaction.json", "/add/request", WriteKind::Reaction),
    ("reaction.json", "/remove/request", WriteKind::Reaction),
];

#[test]
fn the_capture_shaped_fixtures_map_to_their_writes_on_the_lab_chat() -> TestResult {
    let classifier = Classifier::mapped();
    for (name, at, kind) in SHAPED {
        let path = format!("{}/../../tests/fixtures/chatsvc/{name}", env!("CARGO_MANIFEST_DIR"));
        let fixture: Value = serde_json::from_str(&fill(&std::fs::read_to_string(path)?))?;
        let request = fixture.pointer(at).ok_or("no request")?;
        let mut url = format!(
            "https://teams.microsoft.com{}",
            request["path"].as_str().ok_or("no path")?
        );
        if let Some(query) = request["query"].as_str() {
            url = format!("{url}?{query}");
        }
        let body = match &request["body"] {
            Value::Null => None,
            body => Some(serde_json::to_vec(body)?),
        };
        let method = request["method"].as_str().ok_or("no method")?;
        // only the mention fixture mentions anyone: the chat's other member
        let mentioned = request["body"]["properties"]["mentions"]
            .as_str()
            .is_some_and(|mentions| mentions != "[]");
        let target =
            Target::new(kind, Scope::Conversation { id: one_to_one() }).with_added(mentioned.then(|| lab_mri(GUID_B)));
        assert_eq!(
            classifier.classify(method, &url, body.as_deref()),
            Classification::Write(target),
            "{name} {at}"
        );
    }
    Ok(())
}

#[test]
fn the_seeded_classifier_maps_no_write() -> TestResult {
    for case in cases()? {
        assert_eq!(
            Classifier::seeded().classify(&case.method, &case.url, case.body.as_deref()),
            Classification::Unknown,
            "{}",
            case.name
        );
    }
    Ok(())
}

#[test]
fn reads_and_own_state_stay_as_they_were_with_the_mapping() {
    let classifier = Classifier::mapped();
    let messages = format!(
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{}/messages",
        one_to_one()
    );
    assert_eq!(classifier.classify("GET", &messages, None), Classification::Read);
    assert_eq!(
        classifier.classify("POST", "https://teams.microsoft.com/api/authsvc/v1.0/authz", None),
        Classification::Own(OwnKind::Authz)
    );
    // the start-a-trial variant of authz stays denied
    assert_eq!(
        classifier.classify(
            "POST",
            "https://teams.microsoft.com/api/authsvc/v1.0/authz/signuptrial",
            None
        ),
        Classification::Unknown
    );
}

#[test]
fn a_write_is_mapped_only_on_the_chat_service_hosts() {
    let classifier = Classifier::mapped();
    let body = br#"{"messagetype": "Text", "content": "hi"}"#;
    let chat = format!("19%3A{GUID_A}_{GUID_B}%40unq.gbl.spaces");
    let path = format!("v1/users/ME/conversations/{chat}/messages");
    let mapped = [
        format!("https://teams.microsoft.com/api/chatsvc/emea/{path}"),
        format!("https://teams.cloud.microsoft/api/chatsvc/amer/{path}"),
        format!("https://emea.ng.msg.teams.microsoft.com/{path}"),
    ];
    for url in &mapped {
        assert!(
            matches!(classifier.classify("POST", url, Some(body)), Classification::Write(_)),
            "{url}"
        );
    }
    let unmapped = [
        format!("http://teams.microsoft.com/api/chatsvc/emea/{path}"),
        format!("https://teams.microsoft.com:444/api/chatsvc/emea/{path}"),
        format!("https://teams.microsoft.com/api/mt/emea/{path}"),
        format!("https://msg.teams.microsoft.com/{path}"),
        format!("https://teams.microsoft.com.example.com/api/chatsvc/emea/{path}"),
        format!("https://emea.ng.msg.teams.microsoft.com/api/{path}"),
        format!("https://chatsvcagg.teams.microsoft.com/{path}"),
        format!("https://graph.microsoft.com/v1.0/chats/{chat}/messages"),
        // the live read's checks: a region of lowercase letters, digits and `-`, every regional host label sound
        format!("https://teams.microsoft.com/api/chatsvc/EMEA/{path}"),
        format!("https://teams.microsoft.com/api/chatsvc/x%20y/{path}"),
        format!("https://teams.microsoft.com/api/chatsvc//{path}"),
        format!("https://a_b.msg.teams.microsoft.com/{path}"),
        format!("https://a..b.msg.teams.microsoft.com/{path}"),
    ];
    for url in &unmapped {
        assert_eq!(
            classifier.classify("POST", url, Some(body)),
            Classification::Unknown,
            "{url}"
        );
    }
}

#[test]
fn the_chat_service_path_is_decoded_segment_by_segment() -> TestResult {
    let url = Url::parse(&format!(
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/19%3A{GUID_A}_{GUID_B}%40unq.gbl.spaces/messages?view=x"
    ))?;
    assert_eq!(
        chat_service_path(&url),
        Some(vec![
            "users".to_owned(),
            "ME".to_owned(),
            "conversations".to_owned(),
            one_to_one(),
            "messages".to_owned()
        ])
    );
    // an encoded slash stays inside its segment, and a chat id holding one is no chat
    let sneaky = Url::parse(
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/19%3Aa%2F..%2Fb%40thread.v2/messages",
    )?;
    assert_eq!(
        Classifier::mapped().classify("POST", sneaky.as_str(), Some(br#"{"messagetype":"Text"}"#)),
        Classification::Unknown
    );
    Ok(())
}

#[test]
fn the_mapping_and_the_live_read_accept_the_same_chat_service_urls() -> TestResult {
    let auth = format!("skypetoken={TOKEN}");
    let urls = [
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        "https://teams.cloud.microsoft/api/chatsvc/amer-2/v1/threads",
        "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations",
        "https://teams.microsoft.com/api/chatsvc/EMEA/v1/users/ME/conversations",
        "https://teams.microsoft.com/api/chatsvc/x%20y/v1/users/ME/conversations",
        "https://teams.microsoft.com/api/chatsvc/emea/v2/users/ME/endpoints",
        "https://a_b.msg.teams.microsoft.com/v1/users/ME/conversations",
        "https://msg.teams.microsoft.com/v1/users/ME/conversations",
        "https://teams.microsoft.com:8443/api/chatsvc/emea/v1/users/ME/conversations",
        "http://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
    ];
    for url in urls {
        let mapped = chat_service_path(&Url::parse(url)?).is_some();
        let read = ChatCredential::from_request(url, [("Authentication", auth.as_str())]).is_some();
        assert_eq!(mapped, read, "{url}");
    }
    Ok(())
}

/// The mapped classifier's verdict on a body: the target, or `None` for unknown.
fn mapped_target(method: &str, url: &str, body: &str) -> Option<Target> {
    match Classifier::mapped().classify(method, url, Some(body.as_bytes())) {
        Classification::Write(target) => Some(target),
        _ => None,
    }
}

#[test]
fn a_body_the_chat_service_could_read_differently_is_unknown() {
    let lab = format!("8:orgid:{GUID_A}");
    let outsider = format!("8:orgid:{GUID_X}");
    let threads = "https://teams.microsoft.com/api/chatsvc/emea/v1/threads";
    let members = "https://teams.microsoft.com/api/chatsvc/emea/v1/threads/19%3Agroup%40thread.v2/members";
    let messages = format!(
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{}/messages",
        one_to_one()
    );
    // the clean shapes map
    let clean = format!(r#"{{"members":[{{"id":"{lab}","role":"Admin"}}]}}"#);
    assert!(mapped_target("POST", threads, &clean).is_some());
    assert!(mapped_target("PUT", members, &clean).is_some());
    assert!(mapped_target("POST", &messages, r#"{"messagetype":"Text","content":"hi"}"#).is_some());

    let ambiguous_members = [
        // a key in another case beside it: a case-insensitive reader could bind either
        format!(r#"{{"members":[{{"id":"{lab}"}}],"Members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"Members":[{{"id":"{outsider}"}}],"members":[{{"id":"{lab}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}","Id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"ID":"{outsider}","id":"{lab}"}}]}}"#),
        // the same key twice: a reader could keep either
        format!(r#"{{"members":[{{"id":"{lab}"}}],"members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{outsider}","id":"{lab}"}}]}}"#),
        // a case-insensitive key alone, which the fence would miss
        format!(r#"{{"Members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"ID":"{outsider}"}}]}}"#),
        // `ſ` folds to `s` in a case-insensitive comparison
        format!(r#"{{"members":[{{"id":"{lab}"}}],"memberſ":[{{"id":"{outsider}"}}]}}"#),
        // a duplicate anywhere, even off the read path
        format!(r#"{{"members":[{{"id":"{lab}","properties":{{"x":1,"X":2}}}}]}}"#),
        // unexpected shapes
        format!(r#"{{"members":{{"id":"{lab}"}}}}"#),
        format!(r#"{{"members":["{lab}"]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}"}},null]}}"#),
        format!(r#"{{"members":[{{"id":["{lab}"]}}]}}"#),
        format!(r#"[{{"members":[{{"id":"{lab}"}}]}}]"#),
        format!(r#"{{"members":[{{"id":"{lab}"}}]}} {{"members":[{{"id":"{outsider}"}}]}}"#),
        format!(r#"{{"members":[{{"id":"{lab}"}}],}}"#),
        r#"{"members":[]}"#.to_owned(),
    ];
    for body in &ambiguous_members {
        assert_eq!(mapped_target("POST", threads, body), None, "{body}");
        assert_eq!(mapped_target("PUT", members, body), None, "{body}");
    }
    let ambiguous_messages = [
        r#"{"messagetype":"Text","MessageType":"Control/Typing"}"#,
        r#"{"messagetype":"Control/Typing","messagetype":"Text"}"#,
        r#"{"MessageType":"Text"}"#,
        r#"{"messagetype":"Text","content":"hi","properties":{"a":1,"A":2}}"#,
        r#"{"messagetype":"Text"} {"messagetype":"Text"}"#,
        r#"["messagetype","Text"]"#,
    ];
    for body in ambiguous_messages {
        assert_eq!(mapped_target("POST", &messages, body), None, "{body}");
    }
}

/// A source reading the fixture's thread answer from `server`, with a credential taken from a page request.
async fn live_source(server: &MockServer) -> TestResult<Arc<LiveMembershipSource>> {
    let fixture: Value = serde_json::from_str(&fill(THREAD))?;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", one_to_one())))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture["response"]["body"]))
        .mount(server)
        .await;
    let live = Arc::new(LiveMembershipSource::with_mock_origin(&server.uri())?);
    let fence = HttpFence::new(Classifier::mapped(), Arc::new(fence(&[])), live.clone()).with_credentials(live.clone());
    fence.offer_credential(&paused_params(
        "GET",
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations?view=msnp24Equivalent",
        &json!({"Authentication": format!("skypetoken={TOKEN}")}),
    ));
    assert!(live.has_credential());
    Ok(live)
}

fn fence(lab: &[&str]) -> Fence {
    Fence::new(
        Roster::new(lab.iter().copied().map(lab_mri), []),
        FenceLog::from_writer(std::io::sink()),
    )
}

fn paused_params(method: &str, url: &str, headers: &Value) -> Value {
    json!({"requestId": "interception-1", "request": {"method": method, "url": url, "headers": headers}})
}

fn message_to(chat: &str) -> PausedRequest {
    let chat: String = url::form_urlencoded::byte_serialize(chat.as_bytes()).collect();
    PausedRequest {
        request_id: "interception-1".to_owned(),
        method: "POST".to_owned(),
        url: format!("https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations/{chat}/messages"),
        body: Some(br#"{"messagetype":"RichText/Html","content":"<p>hi</p>"}"#.to_vec()),
        network_id: None,
    }
}

#[tokio::test]
async fn a_message_in_the_lab_chat_passes_with_live_members_and_nothing_else_does() -> TestResult {
    let server = MockServer::start().await;
    let live = live_source(&server).await?;
    let members: Arc<dyn MembershipSource + Send> = live.clone();
    let decide = async |lab: &[&str], request: &PausedRequest| {
        HttpFence::new(Classifier::mapped(), Arc::new(fence(lab)), Arc::clone(&members))
            .decide(request)
            .await
    };
    let lab_chat = message_to(&one_to_one());
    assert_eq!(decide(&[GUID_A, GUID_B, GUID_X], &lab_chat).await, Verdict::Allow);
    assert_eq!(
        decide(&[GUID_A], &lab_chat).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    // a group chat's members can't be confirmed in full yet
    assert_eq!(
        decide(&[GUID_A, GUID_B], &message_to("19:groupchat0001@thread.v2")).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::Unsupported)))
    );
    // a channel post isn't mapped at all
    assert_eq!(
        decide(&[GUID_A, GUID_B], &message_to("19:channel0001@thread.tacv2")).await,
        Verdict::Deny(DenyCause::Unclassified)
    );
    Ok(())
}

#[tokio::test]
async fn a_mention_of_an_outsider_in_the_lab_chat_is_denied() -> TestResult {
    let server = MockServer::start().await;
    let live = live_source(&server).await?;
    let members: Arc<dyn MembershipSource + Send> = live.clone();
    let http = HttpFence::new(Classifier::mapped(), Arc::new(fence(&[GUID_A, GUID_B])), members);
    let mentioning = |guid: &str| {
        let mentions = json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": format!("8:orgid:{guid}"),
            "mentionType": "person", "displayName": "Someone"}]);
        let mut request = message_to(&one_to_one());
        request.body = Some(
            json!({"messagetype": "RichText/Html", "content": "<p>hi</p>", "properties": {"mentions": mentions.to_string()}})
                .to_string()
                .into_bytes(),
        );
        request
    };
    // the chat's two members are lab accounts; whom the message mentions decides
    assert_eq!(http.decide(&mentioning(GUID_B)).await, Verdict::Allow);
    assert_eq!(
        http.decide(&mentioning(GUID_X)).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    Ok(())
}

#[tokio::test]
async fn without_live_members_or_a_credential_a_mapped_write_is_still_denied() -> TestResult {
    let lab = [GUID_A, GUID_B];
    let fail_closed = HttpFence::new(Classifier::mapped(), Arc::new(fence(&lab)), Arc::new(FailClosed));
    assert_eq!(
        fail_closed.decide(&message_to(&one_to_one())).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::NoSource)))
    );
    let no_credential = HttpFence::new(
        Classifier::mapped(),
        Arc::new(fence(&lab)),
        Arc::new(LiveMembershipSource::new()?),
    );
    assert_eq!(
        no_credential.decide(&message_to(&one_to_one())).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Partial(PartialReason::NoCredential)))
    );
    Ok(())
}

#[tokio::test]
async fn a_new_chat_is_judged_by_its_members_without_a_read() -> TestResult {
    let server = MockServer::start().await;
    let live = Arc::new(LiveMembershipSource::with_mock_origin(&server.uri())?);
    let http = |lab: &[&str]| HttpFence::new(Classifier::mapped(), Arc::new(fence(lab)), live.clone());
    let create = |members: &[&str]| {
        PausedRequest {
        request_id: "interception-1".to_owned(),
        method: "POST".to_owned(),
        url: "https://teams.microsoft.com/api/chatsvc/emea/v1/threads".to_owned(),
        body: Some(
            json!({"members": members.iter().map(|guid| json!({"id": format!("8:orgid:{guid}"), "role": "Admin"})).collect::<Vec<_>>()})
                .to_string()
                .into_bytes(),
        ),
        network_id: None,
    }
    };
    assert_eq!(
        http(&[GUID_A, GUID_B]).decide(&create(&[GUID_A, GUID_B])).await,
        Verdict::Allow
    );
    assert_eq!(
        http(&[GUID_A, GUID_B]).decide(&create(&[GUID_A, GUID_X])).await,
        Verdict::Deny(DenyCause::Fenced(DenyReason::Outsiders { count: 1 }))
    );
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[test]
fn the_credential_is_taken_only_from_a_chat_service_request_that_carries_one() -> TestResult {
    let live = Arc::new(LiveMembershipSource::new()?);
    let http = HttpFence::new(Classifier::mapped(), Arc::new(fence(&[])), live.clone()).with_credentials(live.clone());
    let auth = json!({"Authentication": format!("skypetoken={TOKEN}")});
    // the web client sends the chat service an Entra bearer token (2026-10-20 capture), the middle tier one too
    let bearer = json!({"authorization": format!("Bearer {TOKEN}")});
    for (url, headers) in [
        ("https://teams.microsoft.com/api/mt/emea/beta/users/", &auth),
        (
            "https://teams.cloud.microsoft/api/mt/part/amer-03/beta/users/tenantsv2",
            &bearer,
        ),
        ("https://example.com/api/chatsvc/emea/v1/users/ME/conversations", &auth),
        (
            "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
            &json!({"Authorization": format!("Basic {TOKEN}")}),
        ),
    ] {
        http.offer_credential(&paused_params("GET", url, headers));
        assert!(!live.has_credential(), "{url} {headers}");
    }
    http.offer_credential(&paused_params(
        "GET",
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        &auth,
    ));
    assert!(live.has_credential());
    let by_bearer = Arc::new(LiveMembershipSource::new()?);
    HttpFence::new(Classifier::mapped(), Arc::new(fence(&[])), by_bearer.clone())
        .with_credentials(by_bearer.clone())
        .offer_credential(&paused_params(
            "GET",
            "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications",
            &bearer,
        ));
    assert!(by_bearer.has_credential());
    // a fence without `with_credentials` takes none
    let other = Arc::new(LiveMembershipSource::new()?);
    let plain = HttpFence::new(Classifier::mapped(), Arc::new(fence(&[])), other.clone());
    plain.offer_credential(&paused_params(
        "GET",
        "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        &auth,
    ));
    assert!(!other.has_credential());
    Ok(())
}

#[test]
fn a_forced_deny_matches_method_url_and_body_text() -> TestResult {
    let rule = ForcedDeny::new("post", r"/messages$")?.with_body_containing("probe-1");
    let request = |method: &str, url: &str, body: Option<&str>| PausedRequest {
        request_id: "r".to_owned(),
        method: method.to_owned(),
        url: url.to_owned(),
        body: body.map(|body| body.as_bytes().to_vec()),
        network_id: None,
    };
    assert!(rule.matches(&request(
        "POST",
        "https://h/c/messages",
        Some(r#"{"content":"probe-1"}"#)
    )));
    assert!(!rule.matches(&request("PUT", "https://h/c/messages", Some("probe-1"))));
    assert!(!rule.matches(&request("POST", "https://h/c/messages/2", Some("probe-1"))));
    assert!(!rule.matches(&request("POST", "https://h/c/messages", Some("other"))));
    assert!(!rule.matches(&request("POST", "https://h/c/messages", None)));
    Ok(())
}

#[tokio::test]
async fn a_forced_deny_comes_before_classification_and_is_logged() -> TestResult {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let fence = Arc::new(Fence::new(Roster::default(), FenceLog::open(&log_path)?));
    let own = Classifier::seeded().with_own(OwnRule::new(OwnKind::Authz, "POST", "http://127.0.0.1:9", "/api/send"));
    let http = HttpFence::new(own, fence, Arc::new(FailClosed));
    let send = |body: &str| PausedRequest {
        request_id: "r".to_owned(),
        method: "POST".to_owned(),
        url: "http://127.0.0.1:9/api/send?x=1".to_owned(),
        body: Some(body.as_bytes().to_vec()),
        network_id: Some("net-1".to_owned()),
    };
    assert_eq!(http.decide(&send("probe")).await, Verdict::Allow);
    http.force_deny(
        ForcedDeny::new("POST", "/api/send")?
            .with_body_containing("probe run-1")
            .labelled("this run"),
    );
    http.force_deny(ForcedDeny::new("POST", "/api/send")?.with_body_containing("probe"));
    assert_eq!(http.decide(&send("probe")).await, Verdict::Deny(DenyCause::Forced));
    assert_eq!(
        http.decide(&send("probe run-1")).await,
        Verdict::Deny(DenyCause::Forced)
    );
    assert_eq!(http.decide(&send("other")).await, Verdict::Allow);
    // each deny names the first rule that matched it, and the request by its Network id
    let labels: Vec<_> = http.denials().into_iter().map(|denial| denial.label).collect();
    assert_eq!(labels, [None, Some("this run".to_owned())]);
    assert!(
        http.denials()
            .iter()
            .all(|denial| denial.network_id.as_deref() == Some("net-1") && denial.url == "http://127.0.0.1:9/api/send")
    );
    http.clear_forced();
    assert_eq!(http.decide(&send("probe")).await, Verdict::Allow);
    let log = std::fs::read_to_string(&log_path)?;
    let forced: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["reason"] == "forced")
        .collect();
    assert_eq!(forced.len(), 2, "{log}");
    assert_eq!(forced[0]["verdict"], "deny");
    assert_eq!(http.denials().len(), 2);
    Ok(())
}

const SEND_PAGE: &str = r#"<!doctype html><html><body>
<button data-tid="send" id="send">send</button>
<script>
window.lab = {results: []};
document.getElementById('send').addEventListener('click', async () => {
  const text = new URLSearchParams(location.search).get('text') ?? 'hello';
  try {
    const response = await fetch('/api/send', {method: 'POST', body: JSON.stringify({content: text})});
    window.lab.results.push('ok ' + response.status);
  } catch (error) {
    window.lab.results.push('error ' + error.name);
  }
});
</script></body></html>"#;

/// The live enforcement test's mechanism on a local page: a forced deny on a post holding the probe text keeps it in
/// the browser (the server receives nothing), the page sees a network error, and the driver sees the post fail as
/// blocked; the same post without the probe goes through.
#[tokio::test]
async fn a_forced_deny_never_leaves_the_browser_and_the_page_sees_an_error() -> TestResult {
    let Some(chrome) = common::chrome_or_skip() else {
        return Ok(());
    };
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/send"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SEND_PAGE, "text/html"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/send"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir()?;
    let browser = Browser::launch(
        &chrome,
        &LaunchOptions::new(dir.path().join("profile")),
        Duration::from_secs(30),
    )
    .await?;
    let fence = Arc::new(Fence::new(
        Roster::default(),
        FenceLog::open(&dir.path().join("fence.log"))?,
    ));
    let own = Classifier::seeded().with_own(OwnRule::new(OwnKind::Authz, "POST", &server.uri(), "/api/send"));
    let members: Arc<dyn MembershipSource + Send> = Arc::new(FailClosed);
    let http = HttpFence::new(own, Arc::clone(&fence), Arc::clone(&members));
    let ws = WsFence::new(FrameClassifier::seeded(), fence, members, GuardScript::generate()?);
    let options = ArmOptions {
        service_worker_origins: vec![server.uri()],
    };
    let armed = ArmedBrowser::arm(browser, http, ws, &options).await?;
    let outcome = tokio::time::timeout(Duration::from_secs(60), forced_deny_run(&armed, &server)).await;
    armed.close(Duration::from_secs(10)).await?;
    outcome.map_err(|_| "the run took over 60 s")?
}

async fn forced_deny_run(armed: &ArmedBrowser, server: &MockServer) -> TestResult {
    let page = armed.open_page(Duration::from_secs(15)).await?.session;
    let mut driver = Driver::new(armed.cdp().clone(), page.clone(), armed.watch());
    let posts = RequestPattern::new(Some("POST"), "/api/send$")?;
    armed.force_deny(
        ForcedDeny::new("POST", "/api/send$")?
            .with_body_containing("probe-7")
            .labelled("probe-7"),
    );

    send_from_page(armed, &page, &mut driver, server, "probe-7").await?;
    let denied = driver
        .wait_for_failed_request("the probe fails", &posts, Duration::from_secs(10))
        .await?;
    let error = denied.failed.clone().unwrap_or_default();
    assert!(error.contains("ERR_BLOCKED_BY_CLIENT"), "{denied:?}");
    assert_eq!(page_results(armed, &page).await?, json!(["error TypeError"]));
    the_deny_names_the_request(armed, &mut driver, &denied).await?;
    assert!(!server_got_send(server).await, "the denied probe reached the server");

    // the same post without the probe goes out
    armed.clear_forced();
    send_from_page(armed, &page, &mut driver, server, "plain").await?;
    let sent = driver
        .wait_for_request("the plain post", &posts, Duration::from_secs(10))
        .await?;
    assert!(sent.url.ends_with("/api/send"));
    wait_for_send(server).await
}

/// The forced deny names its rule's label and the page's own request (`denied`) by its `Network` id.
async fn the_deny_names_the_request(armed: &ArmedBrowser, driver: &mut Driver, denied: &SeenRequest) -> TestResult {
    let forced = armed
        .denials()
        .into_iter()
        .find(|denial| denial.cause == DenyCause::Forced)
        .ok_or("no forced deny")?;
    assert_eq!(forced.label.as_deref(), Some("probe-7"));
    let network_id = forced.network_id.ok_or("the forced deny has no Network id")?;
    let failed = driver
        .wait_for_failure_of("the probe by its id", &network_id, Duration::from_secs(5))
        .await?;
    assert_eq!(failed.request_id, denied.request_id);
    Ok(())
}

/// Waits up to 5 s for the server to get a post.
async fn wait_for_send(server: &MockServer) -> TestResult {
    for _ in 0..50 {
        if server_got_send(server).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("the plain post never reached the server".into())
}

/// Opens the send page with `text` and, once it has loaded (its button does nothing before its script has run),
/// presses its button.
async fn send_from_page(
    armed: &ArmedBrowser,
    page: &SessionId,
    driver: &mut Driver,
    server: &MockServer,
    text: &str,
) -> TestResult {
    let url = format!("{}/send?text={text}", server.uri());
    page::navigate(armed.cdp(), page, &url, Duration::from_secs(10)).await?;
    driver.click("send", Selector::Tid("send")).await?;
    Ok(())
}

/// What the send page recorded, once it has recorded anything (within 10 s).
async fn page_results(armed: &ArmedBrowser, page: &SessionId) -> TestResult<Value> {
    let expression = "window.lab.results.length > 0 ? window.lab.results : null";
    page::evaluate_until(armed.cdp(), page, expression, Duration::from_secs(10)).await
}

async fn server_got_send(server: &MockServer) -> bool {
    let received = server.received_requests().await.unwrap_or_default();
    received.iter().any(|request| request.url.path() == "/api/send")
}
