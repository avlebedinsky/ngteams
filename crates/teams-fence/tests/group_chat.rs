//! Group and meeting chats (`docs/protocol/fence-targets.md`): the live source reads any of them, one the native client
//! created too, with one fresh read of its member list, complete only in the shape the service gave live, and the
//! fence allows a write to it only when every member is a lab account or an approved service. `chats.toml` plays no
//! part in it.
//!
//! The answer is the synthetic fixture `tests/fixtures/chatsvc/thread-group-members.json`, with made-up GUIDs filled
//! in. No real MRI or token appears here.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use teams_fence::lab::{LabConfig, record_abc_chat};
use teams_fence::{
    ChatCredential, CredentialFuture, CredentialSource, DenyReason, Fence, FenceLog, FetchFailure,
    LiveMembershipSource, Membership, MembershipSource, Mri, PartialReason, Roster, Scope, Target, WriteKind,
    parse_members,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GUID_ECHO: &str = "00000000-0000-4000-8000-0000000000e0";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
const OTHER_GROUP: &str = "fedcba9876543210fedcba9876543210";
const MEETING: &str = "19:meeting_MDAwMDAwMDAtMDAwMC00MDAwLTgwMDAtMDAwMDAwMDAwMGFh@thread.v2";
/// A token-shaped made-up credential.
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/emea";
const MEMBERS_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-group-members.json");

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn echo() -> Mri {
    Mri::new(format!("28:{GUID_ECHO}"))
}

fn abc() -> Vec<Mri> {
    vec![mri(GUID_A), mri(GUID_B), mri(GUID_C)]
}

fn group_id(group: &str) -> String {
    format!("19:{group}@thread.v2")
}

fn group_chat() -> Scope {
    Scope::Conversation { id: group_id(GROUP) }
}

fn meeting_chat() -> Scope {
    Scope::Conversation { id: MEETING.to_owned() }
}

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:c>", GUID_C)
        .replace("<group>", GROUP)
        .replace("<region>", "emea")
}

/// The fixture's member list of the a–b–c chat.
fn members_body() -> TestResult<Value> {
    let members: Value = serde_json::from_str(&fill(MEMBERS_FIXTURE))?;
    Ok(members["response"]["body"].clone())
}

/// A `lab.toml` whose accounts a, b and c have the MRIs of [`abc`], the data dir `data_dir`.
fn lab_toml(data_dir: &Path) -> String {
    let accounts = ["a", "b", "c"]
        .into_iter()
        .zip(abc())
        .fold(String::new(), |mut out, (alias, mri)| {
            let _ = write!(
                out,
                "\n[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\nmri = \"{mri}\"\n"
            );
            out
        });
    format!("data_dir = \"{}\"\n{accounts}", data_dir.display())
}

/// The live source reading from `server` with a page's bearer token in `emea`.
fn source(server: &MockServer) -> TestResult<LiveMembershipSource> {
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(ChatCredential::bearer(BASE, TOKEN).ok_or("no credential")?);
    Ok(source)
}

fn members_path(id: &str) -> String {
    format!("/api/chatsvc/emea/v1/threads/{id}/members")
}

/// Serves `members` as chat `id`'s member list, to the bearer `token` only, in the form it was read in live.
async fn serve_to(server: &MockServer, id: &str, token: &str, members: &Value) {
    Mock::given(method("GET"))
        .and(path(members_path(id)))
        .and(query_param("view", "msnp24Equivalent"))
        .and(query_param("pageSize", "100"))
        .and(header("Authorization", format!("Bearer {token}").as_str()))
        .and(header("X-MS-Migration", "True"))
        .and(header("BehaviorOverride", "redirectAs404"))
        .respond_with(ResponseTemplate::new(200).set_body_json(members))
        .mount(server)
        .await;
}

async fn serve(server: &MockServer, members: &Value) {
    serve_to(server, &group_id(GROUP), TOKEN, members).await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

fn partial(reason: PartialReason) -> Membership {
    Membership::Partial(reason)
}

fn malformed() -> Membership {
    partial(PartialReason::FetchFailed(FetchFailure::Malformed))
}

fn lab_fence() -> Fence {
    Fence::new(Roster::new(abc(), [echo()]), FenceLog::from_writer(std::io::sink()))
}

#[tokio::test]
async fn a_lab_only_group_chat_is_complete_from_one_fresh_member_list_read() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await;
    let source = source(&server)?;

    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(paths, [members_path(&group_id(GROUP))], "the member list alone, once");

    // every check reads afresh
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    assert_eq!(requests(&server).await.len(), 2);
    Ok(())
}

#[tokio::test]
async fn a_lab_only_meeting_chat_is_complete_too() -> TestResult {
    let server = MockServer::start().await;
    serve_to(&server, MEETING, TOKEN, &members_body()?).await;
    let source = source(&server)?;
    assert_eq!(source.members(&meeting_chat()).await, Membership::Complete(abc()));
    let permit = lab_fence()
        .check(Target::new(WriteKind::Message, meeting_chat()), &source)
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    Ok(())
}

#[tokio::test]
async fn any_lab_only_group_chat_is_read_whatever_chats_toml_records() -> TestResult {
    // the fence doesn't read chats.toml: a chat it doesn't record (another recorded, none recorded, an unreadable
    // file) is read like the recorded one, a chat the native client created included
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
    let fence = Fence::open(&config)?;
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await;
    let source = source(&server)?;
    let chats = config.paths().chats_file();
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    record_abc_chat(config.paths(), &group_id(OTHER_GROUP))?;
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    std::fs::write(&chats, "not toml")?;
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    let permit = fence
        .check(Target::new(WriteKind::Message, group_chat()), &source)
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    Ok(())
}

#[test]
fn the_source_of_a_lab_fence_reads_every_group_and_meeting_chat() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
    let source = LiveMembershipSource::for_fence(&Fence::open(&config)?)?;
    for scope in [
        group_chat(),
        Scope::Conversation {
            id: group_id(OTHER_GROUP),
        },
        meeting_chat(),
    ] {
        assert!(source.reads_in_full(&scope), "{scope}");
    }
    Ok(())
}

#[tokio::test]
async fn the_fence_allows_writes_to_a_lab_only_group_chat_and_judges_what_they_add() -> TestResult {
    let server = MockServer::start().await;
    let mut members = members_body()?;
    // an approved service is admitted like a lab account
    let mut service = members["members"][2].clone();
    service["id"] = json!(echo().as_str());
    members["members"].as_array_mut().ok_or("no members")?.push(service);
    members["totalMemberCount"] = json!(4);
    serve(&server, &members).await;
    let source = source(&server)?;
    let fence = lab_fence();
    for kind in [WriteKind::Message, WriteKind::Reaction, WriteKind::Edit] {
        let permit = fence.check(Target::new(kind, group_chat()), &source).await;
        assert!(permit.is_ok(), "{kind:?}: {permit:?}");
    }
    let mention = Target::new(WriteKind::Message, group_chat()).with_added([mri(GUID_X)]);
    let denied = fence.check(mention, &source).await.err().ok_or("allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Outsiders { count: 1 });
    Ok(())
}

#[tokio::test]
async fn a_group_chat_with_a_member_outside_the_lab_is_denied() -> TestResult {
    let server = MockServer::start().await;
    let mut members = members_body()?;
    members["members"][2]["id"] = json!(mri(GUID_X).as_str());
    serve(&server, &members).await;
    let source = source(&server)?;
    assert_eq!(
        source.members(&group_chat()).await,
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B), mri(GUID_X)])
    );
    let denied = lab_fence()
        .check(Target::new(WriteKind::Message, group_chat()), &source)
        .await
        .err()
        .ok_or("allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Outsiders { count: 1 });
    Ok(())
}

#[tokio::test]
async fn a_failed_member_list_read_is_partial_with_its_status() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(members_path(&group_id(GROUP))))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    assert_eq!(
        source(&server)?.members(&group_chat()).await,
        partial(PartialReason::FetchFailed(FetchFailure::Status(403)))
    );
    Ok(())
}

/// A native client's credential source: `token` first, `renewed` once the service refuses it; `None` when `token`
/// is empty.
struct Renewing {
    token: Mutex<String>,
    renewed: String,
    refused: AtomicUsize,
}

impl Renewing {
    fn new(token: &str, renewed: &str) -> Self {
        Self {
            token: Mutex::new(token.to_owned()),
            renewed: renewed.to_owned(),
            refused: AtomicUsize::new(0),
        }
    }
}

impl CredentialSource for Renewing {
    fn credential(&self) -> CredentialFuture<'_> {
        let token = self.token.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(async move { ChatCredential::bearer(BASE, &token) })
    }

    fn credential_rejected<'a>(&'a self, _rejected: &'a ChatCredential) -> CredentialFuture<'a> {
        self.refused.fetch_add(1, Ordering::SeqCst);
        self.token
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(&self.renewed);
        Box::pin(async move { ChatCredential::bearer(BASE, &self.renewed) })
    }
}

#[tokio::test]
async fn a_refused_read_renews_the_token_once_and_reads_again_with_it() -> TestResult {
    let server = MockServer::start().await;
    let renewed = format!("{TOKEN}-renewed");
    serve_to(&server, &group_id(GROUP), &renewed, &members_body()?).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(10)
        .mount(&server)
        .await;
    let tokens = Arc::new(Renewing::new(TOKEN, &renewed));
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential_source(tokens.clone());

    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    assert_eq!(tokens.refused.load(Ordering::SeqCst), 1);
    let sent: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(sent, [format!("Bearer {TOKEN}"), format!("Bearer {renewed}")]);

    // a renewed token refused too is denied after those two reads
    let refusing = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&refusing)
        .await;
    let source = LiveMembershipSource::with_mock_origin(&refusing.uri())?;
    source.set_credential_source(Arc::new(Renewing::new(TOKEN, &renewed)));
    assert_eq!(
        source.members(&group_chat()).await,
        partial(PartialReason::FetchFailed(FetchFailure::Status(401)))
    );
    assert_eq!(requests(&refusing).await.len(), 2);
    Ok(())
}

#[tokio::test]
async fn without_a_credential_no_member_list_is_read() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await;
    let bare = LiveMembershipSource::with_mock_origin(&server.uri())?;
    assert_eq!(bare.members(&group_chat()).await, partial(PartialReason::NoCredential));
    let signed_out = LiveMembershipSource::with_mock_origin(&server.uri())?;
    signed_out.set_credential_source(Arc::new(Renewing::new("", "")));
    assert_eq!(
        signed_out.members(&meeting_chat()).await,
        partial(PartialReason::NoCredential)
    );
    assert!(requests(&server).await.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_slow_read_times_out_and_is_denied() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(members_path(&group_id(GROUP))))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(members_body()?)
                .set_delay(Duration::from_millis(700)),
        )
        .mount(&server)
        .await;
    let source = source(&server)?.with_deadline(Duration::from_millis(300));
    let timed_out = partial(PartialReason::FetchFailed(FetchFailure::Timeout));
    assert_eq!(source.members(&group_chat()).await, timed_out);
    let denied = lab_fence()
        .check(Target::new(WriteKind::Message, group_chat()), &source)
        .await
        .err()
        .ok_or("allowed")?;
    assert_eq!(
        *denied.reason(),
        DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Timeout))
    );
    // with time for it, the same answer is complete
    let source = source.with_deadline(Duration::from_secs(5));
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    Ok(())
}

/// One way the answer can differ from what the rule accepts: a name, the change, and the answer it must get.
type Case = (&'static str, fn(&mut Value), Membership);

fn remove(body: &mut Value, pointer: &str) {
    let (parent, key) = pointer.rsplit_once('/').unwrap_or_default();
    if let Some(object) = body.pointer_mut(parent).and_then(Value::as_object_mut) {
        object.remove(key);
    }
}

fn members_of(body: &mut Value) -> &mut Vec<Value> {
    body["members"]
        .as_array_mut()
        .unwrap_or_else(|| unreachable!("the fixture lists members"))
}

/// Every case: the answers that must stay partial, each by one change to the fixture's answer.
fn cases() -> Vec<Case> {
    [paging_cases(), count_cases(), member_cases(), shape_cases()].concat()
}

/// Paging, at any depth.
fn paging_cases() -> Vec<Case> {
    vec![
        (
            "a next link",
            |m| m["nextLink"] = json!("https://x.example.test/next"),
            partial(PartialReason::Paginated),
        ),
        (
            "a sync state under _metadata",
            |m| m["_metadata"] = json!({"syncState": "x"}),
            partial(PartialReason::Paginated),
        ),
        (
            "a continuation token in a listed member",
            |m| m["members"][1]["continuationToken"] = json!("x"),
            partial(PartialReason::Paginated),
        ),
        (
            "an empty next page marker",
            |m| m["@odata.nextLink"] = json!(null),
            partial(PartialReason::Paginated),
        ),
    ]
}

/// The counts.
fn count_cases() -> Vec<Case> {
    vec![
        (
            "a count above the list",
            |m| m["totalMemberCount"] = json!(4),
            partial(PartialReason::Truncated),
        ),
        (
            "a count below the list",
            |m| m["totalMemberCount"] = json!(2),
            partial(PartialReason::Truncated),
        ),
        (
            "another member count above the list",
            |m| m["memberCount"] = json!(4),
            partial(PartialReason::Truncated),
        ),
        ("no count", |m| remove(m, "/totalMemberCount"), malformed()),
        (
            "a count that isn't a number",
            |m| m["totalMemberCount"] = json!("3"),
            malformed(),
        ),
    ]
}

/// The members.
fn member_cases() -> Vec<Case> {
    vec![
        (
            "a member twice, counted",
            |m| {
                let first = m["members"][0].clone();
                members_of(m).push(first);
                m["totalMemberCount"] = json!(4);
            },
            malformed(),
        ),
        (
            "a member twice in another case",
            |m| m["members"][2]["id"] = json!(mri(GUID_A).as_str().to_uppercase()),
            malformed(),
        ),
        (
            "a hidden member",
            |m| m["members"][1]["hidden"] = json!(true),
            malformed(),
        ),
        (
            "a member without hidden",
            |m| remove(m, "/members/0/hidden"),
            malformed(),
        ),
        (
            "a deleted member",
            |m| m["members"][1]["isDeleted"] = json!(true),
            malformed(),
        ),
        (
            "a member deleted in another form",
            |m| m["members"][1]["isDeleted"] = json!("false"),
            malformed(),
        ),
        ("a member without an id", |m| remove(m, "/members/1/id"), malformed()),
        (
            "a member id that isn't text",
            |m| m["members"][1]["id"] = json!(8),
            malformed(),
        ),
        (
            "a consumer account's MRI",
            |m| m["members"][1]["id"] = json!("8:live:someone"),
            malformed(),
        ),
        (
            "a work MRI without a GUID",
            |m| m["members"][1]["id"] = json!("8:orgid:someone"),
            malformed(),
        ),
        (
            "a thread as a member",
            |m| m["members"][1]["id"] = json!(group_id(OTHER_GROUP)),
            malformed(),
        ),
        (
            "a bot MRI without a GUID",
            |m| m["members"][1]["id"] = json!("28:some-bot"),
            malformed(),
        ),
    ]
}

/// The rest of the shape.
fn shape_cases() -> Vec<Case> {
    vec![
        ("a deleted chat", |m| m["isDeleted"] = json!(true), malformed()),
        ("no isDeleted", |m| remove(m, "/isDeleted"), malformed()),
        ("no members", |m| remove(m, "/members"), malformed()),
        ("members that aren't a list", |m| m["members"] = json!({}), malformed()),
        ("a list that isn't an object", |m| *m = json!([]), malformed()),
    ]
}

#[test]
fn every_condition_of_the_rule_keeps_the_chat_partial_when_it_fails() -> TestResult {
    let members = members_body()?;
    assert_eq!(parse_members(&members), Membership::Complete(abc()));
    for (case, change, expected) in cases() {
        let mut members = members.clone();
        change(&mut members);
        assert_eq!(parse_members(&members), expected, "{case}");
    }
    Ok(())
}

#[test]
fn what_the_rule_doesn_t_judge_leaves_the_list_complete() -> TestResult {
    // the roster version, the eTag and who made the chat aren't judged: the member list carries everything the fence
    // judges; a bot's MRI is a form the reads showed, and an empty list is complete (the fence denies it: no members)
    let mut members = members_body()?;
    members["rosterVersion"] = json!(1_760_000_000_000_u64);
    remove(&mut members, "/eTag");
    members["members"][0]["isDeleted"] = json!(false);
    members["members"][2]["id"] = json!(echo().as_str());
    assert_eq!(
        parse_members(&members),
        Membership::Complete(vec![mri(GUID_A), mri(GUID_B), echo()])
    );
    members_of(&mut members).clear();
    members["totalMemberCount"] = json!(0);
    assert_eq!(parse_members(&members), Membership::Complete(Vec::new()));
    Ok(())
}

#[tokio::test]
async fn every_condition_holds_over_the_wire_and_the_fence_denies_with_its_reason() -> TestResult {
    let members = members_body()?;
    let fence = lab_fence();
    for (case, change, expected) in cases() {
        let server = MockServer::start().await;
        let mut members = members.clone();
        change(&mut members);
        serve(&server, &members).await;
        let source = source(&server)?;
        assert_eq!(source.members(&group_chat()).await, expected, "{case}");
        let denied = fence
            .check(Target::new(WriteKind::Message, group_chat()), &source)
            .await
            .err()
            .ok_or(case)?;
        let Membership::Partial(reason) = expected else {
            unreachable!("every case is partial")
        };
        assert_eq!(*denied.reason(), DenyReason::Partial(reason), "{case}");
    }
    Ok(())
}
