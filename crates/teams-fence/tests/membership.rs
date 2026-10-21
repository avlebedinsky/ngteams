//! The live membership source: the credential it takes from a page's request, the read it makes (against a local mock,
//! `with_mock_origin`), and every way an answer falls short of a complete member list.
//!
//! The thread answer is the synthetic fixture `tests/fixtures/chatsvc/thread-one-to-one.json` (sanitized form), with
//! made-up GUIDs filled in for its placeholders; a group chat's answers are `tests/fixtures/chatsvc/thread-group*.json`.
//! No real MRI or token appears here.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_fence::{
    ChatCredential, CredentialFuture, CredentialSource, DenyReason, Fence, FenceLog, FetchFailure,
    LiveMembershipSource, MAX_BODY, Membership, MembershipSource, Mri, PartialReason, Roster, Scope, Target, WriteKind,
    chat_service_path, is_group_chat_id, one_to_one_id, one_to_one_pair, parse_thread,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
/// A token-shaped made-up credential; it must never show in a `Debug`, an error or the log.
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const AFD_REQUEST: &str =
    "https://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations?view=msnp24Equivalent";
const THREAD_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-one-to-one.json");

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:x>", GUID_X)
        .replace("<region>", "emea")
}

fn chat_id() -> String {
    format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")
}

fn chat() -> Scope {
    Scope::Conversation { id: chat_id() }
}

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

/// The fixture's thread answer.
fn thread() -> TestResult<Value> {
    let fixture: Value = serde_json::from_str(&fill(THREAD_FIXTURE))?;
    Ok(fixture["response"]["body"].clone())
}

fn credential(url: &str) -> TestResult<ChatCredential> {
    let auth = format!("skypetoken={TOKEN}");
    ChatCredential::from_request(url, [("Accept", "*/*"), ("Authentication", auth.as_str())])
        .ok_or_else(|| "no credential".into())
}

/// A source reading from `server`, with the credential of a page request to the AFD chat service in `emea`.
fn source(server: &MockServer) -> TestResult<LiveMembershipSource> {
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(credential(AFD_REQUEST)?);
    Ok(source)
}

/// Serves `body` (with `status`) for the 1:1 chat's thread read, as the source must make it.
async fn serve(server: &MockServer, status: u16, body: &Value) {
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", chat_id())))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authentication", format!("skypetoken={TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

fn malformed() -> Membership {
    Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed))
}

#[tokio::test]
async fn a_one_to_one_chat_of_the_two_members_its_id_names_is_complete() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, 200, &thread()?).await;
    let members = source(&server)?.members(&chat()).await;
    assert_eq!(members, Membership::Complete(vec![mri(GUID_A), mri(GUID_B)]));
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 1);
    Ok(())
}

#[tokio::test]
async fn every_check_reads_afresh() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, 200, &thread()?).await;
    let source = source(&server)?;
    for _ in 0..3 {
        assert!(matches!(source.members(&chat()).await, Membership::Complete(_)));
    }
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 3);
    Ok(())
}

#[tokio::test]
async fn without_a_credential_nothing_is_read() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, 200, &thread()?).await;
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    assert!(!source.has_credential());
    assert_eq!(
        source.members(&chat()).await,
        Membership::Partial(PartialReason::NoCredential)
    );
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[tokio::test]
async fn scopes_other_than_a_one_to_one_chat_are_unsupported_and_never_read() -> TestResult {
    let server = MockServer::start().await;
    let source = source(&server)?;
    let scopes = [
        Scope::Conversation {
            id: "19:abcdef0123456789@thread.v2".to_owned(),
        },
        Scope::Conversation {
            id: format!("19:{GUID_A}_{GUID_A}@unq.gbl.spaces"),
        },
        Scope::Conversation {
            id: format!("19:{GUID_A}_not-a-guid@unq.gbl.spaces"),
        },
        Scope::Conversation {
            id: "48:notes".to_owned(),
        },
        Scope::Channel {
            team: "19:team@thread.tacv2".to_owned(),
            channel: "19:channel@thread.tacv2".to_owned(),
        },
        Scope::Team {
            id: "19:team@thread.tacv2".to_owned(),
        },
        Scope::Call {
            id: "call-1".to_owned(),
        },
    ];
    for scope in &scopes {
        assert_eq!(
            source.members(scope).await,
            Membership::Partial(PartialReason::Unsupported),
            "{scope}"
        );
    }
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_refused_or_failed_read_is_partial_with_its_status() -> TestResult {
    for status in [401, 403, 404, 500] {
        let server = MockServer::start().await;
        serve(&server, status, &json!({"errorCode": "nope"})).await;
        assert_eq!(
            source(&server)?.members(&chat()).await,
            Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(status))),
            "{status}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_redirect_is_not_followed() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "https://elsewhere.example.com/members"))
        .mount(&server)
        .await;
    assert_eq!(
        source(&server)?.members(&chat()).await,
        Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(302)))
    );
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 1);
    Ok(())
}

#[tokio::test]
async fn an_unreachable_service_is_a_network_failure() -> TestResult {
    // a port nothing listens on (wiremock pools its servers, so a dropped one may serve another test)
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let origin = format!("http://{}", listener.local_addr()?);
    drop(listener);
    let source = LiveMembershipSource::with_mock_origin(&origin)?;
    source.set_credential(credential(AFD_REQUEST)?);
    assert_eq!(
        source.members(&chat()).await,
        Membership::Partial(PartialReason::FetchFailed(FetchFailure::Network))
    );
    Ok(())
}

#[tokio::test]
async fn a_body_that_isnt_json_is_malformed_and_one_over_the_cap_is_truncated() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>not a thread</html>"))
        .mount(&server)
        .await;
    assert_eq!(source(&server)?.members(&chat()).await, malformed());

    let server = MockServer::start().await;
    let mut big = thread()?;
    big["padding"] = json!("x".repeat(MAX_BODY));
    serve(&server, 200, &big).await;
    assert_eq!(
        source(&server)?.members(&chat()).await,
        Membership::Partial(PartialReason::Truncated)
    );
    Ok(())
}

#[tokio::test]
async fn an_answer_that_isnt_exactly_the_two_members_is_malformed() -> TestResult {
    let cases: [(&str, Value); 4] = [
        (
            "an outsider for b",
            json!([{"id": format!("8:orgid:{GUID_A}")}, {"id": format!("8:orgid:{GUID_X}")}]),
        ),
        ("one member only", json!([{"id": format!("8:orgid:{GUID_A}")}])),
        (
            "a third member",
            json!([{"id": format!("8:orgid:{GUID_A}")}, {"id": format!("8:orgid:{GUID_B}")}, {"id": format!("8:orgid:{GUID_X}")}]),
        ),
        (
            "a member twice",
            json!([{"id": format!("8:orgid:{GUID_A}")}, {"id": format!("8:orgid:{GUID_A}")}]),
        ),
    ];
    for (case, members) in cases {
        let server = MockServer::start().await;
        let mut body = thread()?;
        body["members"] = members;
        serve(&server, 200, &body).await;
        assert_eq!(source(&server)?.members(&chat()).await, malformed(), "{case}");
    }
    Ok(())
}

#[tokio::test]
async fn another_threads_answer_is_malformed() -> TestResult {
    let server = MockServer::start().await;
    let mut body = thread()?;
    body["id"] = json!(format!("19:{GUID_A}_{GUID_X}@unq.gbl.spaces"));
    serve(&server, 200, &body).await;
    assert_eq!(source(&server)?.members(&chat()).await, malformed());
    Ok(())
}

#[test]
fn parse_thread_finds_pagination_truncation_and_bad_shapes() -> TestResult {
    let id = chat_id();
    let good = thread()?;
    assert_eq!(
        parse_thread(&id, &good),
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B)])
    );
    let with = |change: &dyn Fn(&mut Value)| {
        let mut body = good.clone();
        change(&mut body);
        parse_thread(&id, &body)
    };
    let paginated = Membership::Partial(PartialReason::Paginated);
    let truncated = Membership::Partial(PartialReason::Truncated);
    assert_eq!(with(&|body| body["nextLink"] = json!("https://next")), paginated);
    assert_eq!(
        with(&|body| body["_metadata"] = json!({"backwardLink": "x", "syncState": "y"})),
        paginated
    );
    assert_eq!(with(&|body| body["continuationToken"] = json!("t")), paginated);
    assert_eq!(with(&|body| body["properties"]["membercount"] = json!("3")), truncated);
    assert_eq!(with(&|body| body["memberCount"] = json!(5)), truncated);
    assert_eq!(
        with(&|body| body["properties"]["memberCount"] = json!("many")),
        truncated
    );
    assert_eq!(
        with(&|body| body["properties"]["membercount"] = json!("2")),
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B)])
    );
    assert_eq!(with(&|body| body["id"] = json!("19:other@thread.v2")), malformed());
    assert_eq!(
        with(&|body| body.as_object_mut().map(|o| o.remove("id")).map_or((), drop)),
        malformed()
    );
    assert_eq!(with(&|body| body["members"] = json!({"id": "x"})), malformed());
    assert_eq!(with(&|body| body["members"][1] = json!({"mri": "x"})), malformed());
    assert_eq!(with(&|body| body["members"][0]["id"] = json!(7)), malformed());
    assert_eq!(parse_thread(&id, &json!([1, 2])), malformed());
    Ok(())
}

#[test]
fn a_one_to_one_chat_id_is_built_from_its_two_members_and_read_back_to_them() {
    let (a, b) = (
        format!("8:orgid:{GUID_A}"),
        format!("8:orgid:{}", GUID_B.to_ascii_uppercase()),
    );
    // the object ids sorted and lowercased, whichever account comes first
    for (first, second) in [(&a, &b), (&b, &a)] {
        let id = one_to_one_id(first, second);
        assert_eq!(id, Some(chat_id()), "{first} {second}");
        assert_eq!(
            one_to_one_pair(&id.unwrap_or_default()),
            Some([mri(GUID_A), mri(GUID_B)])
        );
    }
    let same_in_another_case = format!("8:orgid:{}", GUID_A.to_ascii_uppercase());
    for (first, second) in [
        (a.as_str(), a.as_str()),
        (a.as_str(), same_in_another_case.as_str()),
        (a.as_str(), ""),
        ("28:a-bot", b.as_str()),
        ("8:orgid:not-a-guid", b.as_str()),
        // 36 characters of hex digits and dashes, but not a GUID's groups
        ("8:orgid:----00000000000000000000000000000000", b.as_str()),
        (GUID_A, b.as_str()),
    ] {
        assert_eq!(one_to_one_id(first, second), None, "{first} {second}");
    }
}

#[test]
fn a_one_to_one_chat_id_names_its_two_members() {
    let upper = GUID_A.to_ascii_uppercase();
    assert_eq!(
        one_to_one_pair(&format!("19:{upper}_{GUID_B}@unq.gbl.spaces")),
        Some([mri(GUID_A), mri(GUID_B)])
    );
    for id in [
        format!("19:{GUID_A}_{GUID_B}@thread.v2"),
        format!("19:{GUID_A}{GUID_B}@unq.gbl.spaces"),
        format!("19:{GUID_A}_{GUID_B}_{GUID_X}@unq.gbl.spaces"),
        format!("{GUID_A}_{GUID_B}@unq.gbl.spaces"),
        format!("19:{GUID_A}_{GUID_A}@unq.gbl.spaces"),
        "19:meeting_abc@thread.v2".to_owned(),
    ] {
        assert_eq!(one_to_one_pair(&id), None, "{id}");
    }
}

#[test]
fn a_bearer_token_on_a_chat_service_request_is_a_credential_too() {
    let bearer = format!("Bearer {TOKEN}");
    let lower_bearer = format!("bearer {TOKEN}");
    let by_bearer = [("authorization", bearer.as_str())];
    let by_lower_bearer = [("Authorization", lower_bearer.as_str())];
    for url in [
        AFD_REQUEST,
        "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications",
    ] {
        assert!(ChatCredential::from_request(url, by_bearer).is_some(), "{url}");
        assert!(ChatCredential::from_request(url, by_lower_bearer).is_some(), "{url}");
    }
    // a bearer token on any other service isn't a chat service credential
    assert!(
        ChatCredential::from_request(
            "https://teams.cloud.microsoft/api/mt/part/amer-03/beta/users/",
            by_bearer
        )
        .is_none()
    );
    assert!(
        ChatCredential::from_request(
            "https://teams.cloud.microsoft/api/csa/amer/api/v3/teams/users/me",
            by_bearer
        )
        .is_none()
    );
}

#[test]
fn a_credential_comes_only_from_a_chat_service_request_with_a_token() {
    let auth = format!("skypetoken={TOKEN}");
    let by_auth = [("authentication", auth.as_str())];
    let by_header = [("X-Skypetoken", TOKEN)];
    let accepted = [
        AFD_REQUEST,
        "https://teams.cloud.microsoft/api/chatsvc/amer/v1/threads/19:x@thread.v2",
        "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations",
        "https://amer-02.msg.teams.microsoft.com/v1/users/ME/properties",
    ];
    for url in accepted {
        assert!(ChatCredential::from_request(url, by_auth).is_some(), "{url}");
        assert!(ChatCredential::from_request(url, by_header).is_some(), "{url}");
    }
    let refused = [
        "http://teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        "https://teams.microsoft.com:8443/api/chatsvc/emea/v1/users/ME/conversations",
        "https://user:pw@teams.microsoft.com/api/chatsvc/emea/v1/users/ME/conversations",
        "https://teams.microsoft.com/api/mt/emea/beta/users/",
        "https://teams.microsoft.com/api/chatsvc/EMEA/v1/users/ME/conversations",
        "https://teams.microsoft.com/api/chatsvc/emea/v2/users/ME/endpoints",
        "https://teams.microsoft.com.example.com/api/chatsvc/emea/v1/users/ME/conversations",
        "https://msg.teams.microsoft.com/v1/users/ME/conversations",
        "https://evilmsg.teams.microsoft.com/v1/users/ME/conversations",
        "https://emea.ng.msg.teams.microsoft.com/api/v1/users/ME/conversations",
        "https://chatsvcagg.teams.microsoft.com/api/v1/users/ME/conversations",
        "https://example.com/api/chatsvc/emea/v1/users",
        "not a url",
    ];
    for url in refused {
        assert!(ChatCredential::from_request(url, by_auth).is_none(), "{url}");
    }
    // the token has to be in a header this crate reads, in its scheme, and token-shaped
    let basic = format!("Basic {TOKEN}");
    let bare = TOKEN.to_owned();
    let bearer_in_authentication = format!("Bearer {TOKEN}");
    let short = "skypetoken=abc";
    let short_bearer = "Bearer abc";
    let injected = format!("skypetoken={TOKEN}\r\nX-Other: 1");
    let injected_bearer = format!("Bearer {TOKEN}\r\nX-Other: 1");
    let spaced = format!("skypetoken={TOKEN} extra");
    let spaced_bearer = format!("Bearer {TOKEN} extra");
    let skype_in_authorization = format!("skypetoken={TOKEN}");
    for headers in [
        vec![("Authorization", basic.as_str())],
        vec![("Authorization", bare.as_str())],
        vec![("Authorization", short_bearer)],
        vec![("Authorization", injected_bearer.as_str())],
        vec![("Authorization", spaced_bearer.as_str())],
        vec![("Authorization", skype_in_authorization.as_str())],
        vec![("Authentication", bearer_in_authentication.as_str())],
        vec![("Authentication", short)],
        vec![("Authentication", injected.as_str())],
        vec![("Authentication", spaced.as_str())],
        vec![("Authentication", TOKEN)],
        vec![],
    ] {
        assert!(
            ChatCredential::from_request(AFD_REQUEST, headers.clone()).is_none(),
            "{headers:?}"
        );
    }
}

#[tokio::test]
async fn the_read_goes_to_the_credentials_region_and_the_regional_host_has_no_prefix() -> TestResult {
    for (request, wanted) in [
        (
            "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations",
            format!("/api/chatsvc/amer/v1/threads/{}", chat_id()),
        ),
        (
            "https://emea.ng.msg.teams.microsoft.com/v1/users/ME/conversations",
            format!("/v1/threads/{}", chat_id()),
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(wanted.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(thread()?))
            .mount(&server)
            .await;
        let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
        source.set_credential(credential(request)?);
        assert!(
            matches!(source.members(&chat()).await, Membership::Complete(_)),
            "{request}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_bearer_credential_reads_with_the_bearer_token_and_never_shows() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{}", chat_id())))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread()?))
        .mount(&server)
        .await;
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    let bearer = format!("Bearer {TOKEN}");
    let credential = ChatCredential::from_request(
        "https://teams.cloud.microsoft/api/chatsvc/amer/v1/users/ME/conversations/48%3Anotifications",
        [("Accept", "*/*"), ("Authorization", bearer.as_str())],
    )
    .ok_or("no credential")?;
    let shown = format!("{credential:?}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("Bearer"), "{shown}");
    source.set_credential(credential);
    assert_eq!(
        source.members(&chat()).await,
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B)])
    );
    // the mock answers only a read that carries the bearer token: the request sent no skypetoken
    let requests = server.received_requests().await.ok_or("no recording")?;
    assert!(
        requests
            .iter()
            .all(|request| !request.headers.contains_key("authentication")),
        "a skypetoken header was sent"
    );
    Ok(())
}

#[tokio::test]
async fn the_credential_never_shows() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, 401, &json!({})).await;
    let source = source(&server)?;
    let credential = credential(AFD_REQUEST)?;
    let shown = format!("{credential:?} {source:?}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("<redacted>"));

    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let roster = Roster::new([mri(GUID_A), mri(GUID_B)], []);
    let fence = Fence::new(roster, FenceLog::open(&log_path)?);
    let denied = fence
        .check(Target::new(WriteKind::Message, chat()), &source)
        .await
        .err()
        .ok_or("allowed")?;
    let shown = format!("{denied} {denied:?} {}", std::fs::read_to_string(&log_path)?);
    assert!(!shown.contains(TOKEN), "{shown}");
    Ok(())
}

#[tokio::test]
async fn the_fence_allows_a_lab_only_one_to_one_chat_and_denies_one_with_an_outsider() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, 200, &thread()?).await;
    let source: Arc<dyn MembershipSource + Send> = Arc::new(source(&server)?);
    let fence = |lab: Vec<Mri>| Fence::new(Roster::new(lab, []), FenceLog::from_writer(std::io::sink()));
    let message = || Target::new(WriteKind::Message, chat());

    let lab = fence(vec![mri(GUID_A), mri(GUID_B)]);
    assert!(lab.check(message(), source.as_ref()).await.is_ok());

    let without_b = fence(vec![mri(GUID_A)]);
    let denied = without_b
        .check(message(), source.as_ref())
        .await
        .err()
        .ok_or("allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Outsiders { count: 1 });
    Ok(())
}

const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
const GROUP_FIXTURES: [(&str, &str); 3] = [
    ("", include_str!("../../../tests/fixtures/chatsvc/thread-group.json")),
    (
        "/members",
        include_str!("../../../tests/fixtures/chatsvc/thread-group-members.json"),
    ),
    (
        "/filtered",
        include_str!("../../../tests/fixtures/chatsvc/thread-group-filtered.json"),
    ),
];

fn group_chat() -> Scope {
    Scope::Conversation {
        id: format!("19:{GROUP}@thread.v2"),
    }
}

/// Serves the group chat fixtures' answers, as the chat service gave them live (M1 Task 8): the thread read and the
/// member list agree on a, b and c and the list counts them (`totalMemberCount`); the filtered view is refused.
async fn serve_group(server: &MockServer) -> TestResult {
    for (suffix, fixture) in GROUP_FIXTURES {
        let filled = fill(fixture)
            .replace("<guid:c>", GUID_C)
            .replace("<guid:tenant>", GUID_X)
            .replace("<group>", GROUP);
        let fixture: Value = serde_json::from_str(&filled)?;
        let status = u16::try_from(fixture["response"]["status"].as_u64().ok_or("no status")?)?;
        Mock::given(method("GET"))
            .and(path(format!(
                "/api/chatsvc/emea/v1/threads/19:{GROUP}@thread.v2{suffix}"
            )))
            .respond_with(ResponseTemplate::new(status).set_body_json(&fixture["response"]["body"]))
            .mount(server)
            .await;
    }
    Ok(())
}

#[tokio::test]
async fn a_group_chat_stays_unsupported_even_when_its_reads_look_complete() -> TestResult {
    // the answers agree and count their members, but nothing shows that neither is a page or a caller's view of the
    // chat (docs/backlog/group-chat-member-read-completeness.md): unsupported, and never read
    let server = MockServer::start().await;
    serve_group(&server).await?;
    let source = source(&server)?;
    assert_eq!(
        source.members(&group_chat()).await,
        Membership::Partial(PartialReason::Unsupported)
    );
    assert!(server.received_requests().await.unwrap_or_default().is_empty());

    // and so the fence denies a message to a chat of lab accounts only
    let lab = Fence::new(
        Roster::new(vec![mri(GUID_A), mri(GUID_B), mri(GUID_C)], []),
        FenceLog::from_writer(std::io::sink()),
    );
    let denied = lab
        .check(Target::new(WriteKind::Message, group_chat()), &source)
        .await
        .err()
        .ok_or("a group chat message was allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Partial(PartialReason::Unsupported));
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[test]
fn only_a_one_to_one_chat_can_be_read_in_full() {
    assert!(LiveMembershipSource::reads_in_full(&chat()));
    for scope in [
        group_chat(),
        Scope::Conversation {
            id: "19:meeting_0123456789abcdef@thread.v2".to_owned(),
        },
        Scope::Conversation {
            id: format!("19:{GUID_A}_{GUID_A}@unq.gbl.spaces"),
        },
        Scope::Channel {
            team: "19:team@thread.tacv2".to_owned(),
            channel: "19:channel@thread.tacv2".to_owned(),
        },
        Scope::Team {
            id: "19:team@thread.tacv2".to_owned(),
        },
        Scope::Call {
            id: "call-1".to_owned(),
        },
    ] {
        assert!(!LiveMembershipSource::reads_in_full(&scope), "{scope}");
    }
}

#[test]
fn a_group_chat_id_is_19_an_id_and_thread_v2_and_never_a_meeting_s() {
    assert!(is_group_chat_id(&format!("19:{GROUP}@thread.v2")));
    assert!(is_group_chat_id("19:a_b-C9@thread.v2"));
    for id in [
        "19:meeting_0123@thread.v2",
        "19:MEETING_0123@thread.v2",
        "19:@thread.v2",
        "19:01 23@thread.v2",
        "19:0123@thread.tacv2",
        "19:0123@thread.v2;messageid=1",
        "19:0123%40thread.v2",
        "0123@thread.v2",
        &format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces"),
        &format!("19:{}@thread.v2", "a".repeat(129)),
    ] {
        assert!(!is_group_chat_id(id), "{id}");
    }
}

#[test]
fn a_bearer_credential_from_a_base_takes_only_a_base_the_chat_service_path_check_takes() {
    let accepted = [
        "https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/chatsvc/amer/",
        "https://teams.cloud.microsoft/api/chatsvc/emea",
        "https://amer.ng.msg.teams.microsoft.com",
        "https://amer.ng.msg.teams.microsoft.com/",
    ];
    for base in accepted {
        // the same base under `v1` is a chat service URL to the write mapping's check too
        let request = format!("{}/v1/users/ME/conversations", base.trim_end_matches('/'));
        let parsed = url::Url::parse(&request).ok();
        assert!(parsed.as_ref().and_then(chat_service_path).is_some(), "{base}");
        assert!(ChatCredential::bearer(base, TOKEN).is_some(), "{base}");
    }
    let refused = [
        "http://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com:8443/api/chatsvc/amer",
        "https://user:pw@teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/chatsvc/AMER",
        "https://teams.microsoft.com/api/chatsvc",
        "https://teams.microsoft.com/api/mt/part/amer-03",
        "https://teams.microsoft.com/api/chatsvc/amer/v1",
        "https://teams.microsoft.com/api/chatsvc/amer/extra",
        "https://teams.microsoft.com/api/chatsvc/amer//",
        "https://teams.microsoft.com/api/chatsvc/amer?x=1",
        "https://teams.microsoft.com/api/chatsvc/amer#v1",
        "https://teams.microsoft.com.example.com/api/chatsvc/amer",
        "https://msg.teams.microsoft.com",
        "https://amer.ng.msg.teams.microsoft.com/api",
        "https://chatsvcagg.teams.microsoft.com/api/csa/amer",
        " https://teams.microsoft.com/api/chatsvc/amer",
        "https://teams.microsoft.com/api/chatsvc/amer ",
        "",
        "not a url",
    ];
    for base in refused {
        assert!(ChatCredential::bearer(base, TOKEN).is_none(), "{base:?}");
    }
    // the token has to be token-shaped, as one from a request
    for token in ["abc", "Bearer x", "a\r\nX-Other: 1aaaaaaaaaaaaaaaaaaaaaaa", ""] {
        assert!(
            ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", token).is_none(),
            "{token:?}"
        );
    }
}

#[tokio::test]
async fn a_bearer_credential_from_a_base_reads_there_with_the_token_and_never_shows() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{}", chat_id())))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread()?))
        .mount(&server)
        .await;
    let credential =
        ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", TOKEN).ok_or("no credential")?;
    let shown = format!("{credential:?}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(shown.contains("Bearer"), "{shown}");
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(credential);
    assert_eq!(
        source.members(&chat()).await,
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B)])
    );
    Ok(())
}

/// A native client's credential source: the token it gives now (none when signed out), how often it was asked, the
/// token it renews a refused one to (none: it has no renewed one), and the refused tokens it was told of.
struct Tokens {
    token: Mutex<Option<String>>,
    asked: AtomicUsize,
    when_refused: Mutex<Option<String>>,
    refused: Mutex<Vec<String>>,
}

impl Tokens {
    fn new(token: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            token: Mutex::new(token.map(str::to_owned)),
            asked: AtomicUsize::new(0),
            when_refused: Mutex::new(None),
            refused: Mutex::new(Vec::new()),
        })
    }

    /// A source that renews a refused token to `renewed`.
    fn renewing(token: &str, renewed: &str) -> Arc<Self> {
        let tokens = Self::new(Some(token));
        *tokens.when_refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(renewed.to_owned());
        tokens
    }

    fn refused(&self) -> Vec<String> {
        self.refused.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn renew(&self, token: &str) {
        *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some(token.to_owned());
    }

    fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

impl CredentialSource for Tokens {
    fn credential(&self) -> CredentialFuture<'_> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let token = self.token.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(async move { ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", &token?) })
    }

    fn credential_rejected<'a>(&'a self, rejected: &'a ChatCredential) -> CredentialFuture<'a> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(rejected.expose_token().to_owned());
        let renewed = self.when_refused.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(renewed) = &renewed {
            *self.token.lock().unwrap_or_else(PoisonError::into_inner) = Some(renewed.clone());
        }
        Box::pin(async move { ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", &renewed?) })
    }
}

/// Answers every other thread read in `amer` with a 401, as the service answers a token it doesn't take.
async fn refuse_the_rest(server: &MockServer) {
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(10)
        .mount(server)
        .await;
}

/// The bearer tokens the reads `server` received carried, in order.
async fn bearers_sent(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|request| request.headers.get("authorization")?.to_str().ok().map(str::to_owned))
        .collect()
}

fn refused() -> Membership {
    Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(401)))
}

#[tokio::test]
async fn a_refused_source_token_is_renewed_and_read_with_once_more() -> TestResult {
    let server = MockServer::start().await;
    let renewed = format!("{TOKEN}-renewed");
    serve_bearer(&server, &renewed).await?;
    refuse_the_rest(&server).await;
    let tokens = Tokens::renewing(TOKEN, &renewed);
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());
    let fence = Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)], []),
        FenceLog::from_writer(std::io::sink()),
    );

    let permit = fence.check(Target::new(WriteKind::Message, chat()), &source).await;

    assert!(permit.is_ok(), "{permit:?}");
    assert_eq!(tokens.refused(), [TOKEN]);
    assert_eq!(
        bearers_sent(&server).await,
        [format!("Bearer {TOKEN}"), format!("Bearer {renewed}")]
    );
    Ok(())
}

#[tokio::test]
async fn a_renewed_token_refused_too_is_denied_after_two_reads() -> TestResult {
    let server = MockServer::start().await;
    let renewed = format!("{TOKEN}-renewed");
    refuse_the_rest(&server).await;
    let tokens = Tokens::renewing(TOKEN, &renewed);
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());
    let fence = Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)], []),
        FenceLog::from_writer(std::io::sink()),
    );

    let denied = fence
        .check(Target::new(WriteKind::Message, chat()), &source)
        .await
        .unwrap_err();

    assert_eq!(
        denied.reason(),
        &DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Status(401)))
    );
    assert_eq!(
        tokens.refused(),
        [TOKEN],
        "told once: the second refusal isn't read again"
    );
    assert_eq!(
        bearers_sent(&server).await,
        [format!("Bearer {TOKEN}"), format!("Bearer {renewed}")]
    );
    Ok(())
}

#[tokio::test]
async fn a_refusal_without_a_renewed_credential_is_not_read_again() -> TestResult {
    // a source with no renewed token
    let server = MockServer::start().await;
    refuse_the_rest(&server).await;
    let tokens = Tokens::new(Some(TOKEN));
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());
    assert_eq!(source.members(&chat()).await, refused());
    assert_eq!(tokens.refused(), [TOKEN]);
    assert_eq!(bearers_sent(&server).await, [format!("Bearer {TOKEN}")]);

    // a page's credential, which the source can't renew
    let server = MockServer::start().await;
    refuse_the_rest(&server).await;
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(
        ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", TOKEN).ok_or("no credential")?,
    );
    assert_eq!(source.members(&chat()).await, refused());
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 1);

    // any other failure is not a refusal, and isn't read again either
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let tokens = Tokens::renewing(TOKEN, &format!("{TOKEN}-renewed"));
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());
    assert_eq!(
        source.members(&chat()).await,
        Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(403)))
    );
    assert!(tokens.refused().is_empty());
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 1);
    Ok(())
}

/// Serves the 1:1 chat's thread read in `amer` to the bearer `token` only.
async fn serve_bearer(server: &MockServer, token: &str) -> TestResult {
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{}", chat_id())))
        .and(header("Authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread()?))
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn a_credential_source_is_asked_on_every_read_and_its_newest_token_is_used() -> TestResult {
    let server = MockServer::start().await;
    let renewed = format!("{TOKEN}-renewed");
    serve_bearer(&server, TOKEN).await?;
    serve_bearer(&server, &renewed).await?;
    let tokens = Tokens::new(Some(TOKEN));
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());
    assert!(source.has_credential());
    let complete = Membership::Complete(vec![mri(GUID_A), mri(GUID_B)]);

    assert_eq!(source.members(&chat()).await, complete);
    tokens.renew(&renewed);
    assert_eq!(source.members(&chat()).await, complete);

    assert_eq!(tokens.asked(), 2, "asked on every read");
    let sent: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter_map(|request| request.headers.get("authorization")?.to_str().ok().map(str::to_owned))
        .collect();
    assert_eq!(sent, [format!("Bearer {TOKEN}"), format!("Bearer {renewed}")]);
    Ok(())
}

#[tokio::test]
async fn a_credential_source_without_a_token_denies_before_reading() -> TestResult {
    let server = MockServer::start().await;
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(Tokens::new(None));
    let fence = Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)], []),
        FenceLog::from_writer(std::io::sink()),
    );

    let denied = fence
        .check(Target::new(WriteKind::Message, chat()), &source)
        .await
        .unwrap_err();

    assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::NoCredential));
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_credential_set_later_replaces_the_source_and_the_other_way_round() -> TestResult {
    let server = MockServer::start().await;
    serve_bearer(&server, TOKEN).await?;
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    let signed_out = Tokens::new(None);
    source.set_credential_source(signed_out.clone());
    let credential =
        ChatCredential::bearer("https://teams.microsoft.com/api/chatsvc/amer", TOKEN).ok_or("no credential")?;
    source.set_credential(credential);
    assert_eq!(
        source.members(&chat()).await,
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B)])
    );
    assert_eq!(signed_out.asked(), 0, "the newest one wins");

    source.set_credential_source(signed_out.clone());
    assert_eq!(
        source.members(&chat()).await,
        Membership::Partial(PartialReason::NoCredential)
    );
    assert_eq!(signed_out.asked(), 1);
    Ok(())
}
