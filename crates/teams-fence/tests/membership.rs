//! The live membership source: the credential it takes from a page's request, the read it makes (against a local mock,
//! `with_mock_origin`), and every way an answer falls short of a complete member list.
//!
//! The thread answer is the synthetic fixture `tests/fixtures/chatsvc/thread-one-to-one.json` (sanitized form), with
//! made-up GUIDs filled in for its placeholders. No real MRI or token appears here.

use std::sync::Arc;

use serde_json::{Value, json};
use teams_fence::{
    ChatCredential, DenyReason, Fence, FenceLog, FetchFailure, LiveMembershipSource, MAX_BODY, Membership,
    MembershipSource, Mri, PartialReason, Roster, Scope, Target, WriteKind, one_to_one_pair, parse_thread,
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
