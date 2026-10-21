//! The lab's group chat (the user's decision of 2026-10-21, `docs/protocol/fence-targets.md`): the live source finds
//! the a–b–c chat the lab recorded in `chats.toml` complete when its thread read and its member list, both read fresh,
//! agree on exactly a, b and c in the shape the service gave live, and every other group chat, shape or roster stays
//! `Partial`.
//!
//! The answers are the synthetic fixtures `tests/fixtures/chatsvc/thread-group.json` and `thread-group-members.json`,
//! with made-up GUIDs filled in. No real MRI or token appears here.

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
    parse_group,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GROUP: &str = "0123456789abcdef0123456789abcdef";
const OTHER_GROUP: &str = "fedcba9876543210fedcba9876543210";
/// A token-shaped made-up credential.
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/emea";
const THREAD_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-group.json");
const MEMBERS_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-group-members.json");

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
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

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:c>", GUID_C)
        .replace("<guid:tenant>", GUID_X)
        .replace("<group>", GROUP)
        .replace("<region>", "emea")
}

/// The fixtures' thread read and member list of the a–b–c chat.
fn bodies() -> TestResult<(Value, Value)> {
    let thread: Value = serde_json::from_str(&fill(THREAD_FIXTURE))?;
    let members: Value = serde_json::from_str(&fill(MEMBERS_FIXTURE))?;
    Ok((thread["response"]["body"].clone(), members["response"]["body"].clone()))
}

/// A `lab.toml` whose accounts a, b and c have the MRIs given (`None`: not known yet), the data dir `data_dir`.
fn lab_toml(data_dir: &Path, mris: [Option<String>; 3]) -> String {
    let accounts = ["a", "b", "c"]
        .into_iter()
        .zip(mris)
        .fold(String::new(), |mut out, (alias, mri)| {
            let mri = mri.map(|mri| format!("mri = \"{mri}\"\n")).unwrap_or_default();
            let _ = write!(
                out,
                "\n[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\n{mri}"
            );
            out
        });
    format!("data_dir = \"{}\"\n{accounts}", data_dir.display())
}

/// A lab in a temporary dir: a, b and c with their MRIs, and `recorded` as the a–b–c chat in its `chats.toml`.
struct Lab {
    dir: tempfile::TempDir,
    config: LabConfig,
}

impl Lab {
    fn new(recorded: Option<&str>) -> TestResult<Self> {
        Self::with_mris(
            [GUID_A, GUID_B, GUID_C].map(|guid| Some(mri(guid).as_str().to_owned())),
            recorded,
        )
    }

    fn with_mris(mris: [Option<String>; 3], recorded: Option<&str>) -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let config = LabConfig::parse(&lab_toml(dir.path(), mris), dir.path())?;
        if let Some(id) = recorded {
            record_abc_chat(config.paths(), id)?;
        }
        Ok(Self { dir, config })
    }

    /// The live source of this lab, reading from `server` with a page's bearer token in `emea`.
    fn source(&self, server: &MockServer) -> TestResult<LiveMembershipSource> {
        let source = LiveMembershipSource::with_mock_origin(&server.uri())?.with_lab(&self.config);
        source.set_credential(ChatCredential::bearer(BASE, TOKEN).ok_or("no credential")?);
        Ok(source)
    }
}

/// Serves `thread` and `members` as the group chat's thread read and member list, to the bearer `token` only.
async fn serve_to(server: &MockServer, token: &str, thread: &Value, members: &Value) {
    let thread_path = format!("/api/chatsvc/emea/v1/threads/{}", group_id(GROUP));
    Mock::given(method("GET"))
        .and(path(thread_path.as_str()))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {token}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{thread_path}/members")))
        .and(query_param("view", "msnp24Equivalent"))
        .and(query_param("pageSize", "100"))
        .and(header("Authorization", format!("Bearer {token}").as_str()))
        // the member list in the header form it was read in live
        .and(header("X-MS-Migration", "True"))
        .and(header("BehaviorOverride", "redirectAs404"))
        .respond_with(ResponseTemplate::new(200).set_body_json(members))
        .mount(server)
        .await;
}

async fn serve(server: &MockServer, thread: &Value, members: &Value) {
    serve_to(server, TOKEN, thread, members).await;
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
    Fence::new(Roster::new(abc(), []), FenceLog::from_writer(std::io::sink()))
}

#[tokio::test]
async fn the_lab_s_recorded_group_chat_of_a_b_and_c_is_complete() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    serve(&server, &thread, &members).await;
    let source = lab.source(&server)?;

    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(
        paths.len(),
        2,
        "the thread read and the member list, each once: {paths:?}"
    );
    assert!(paths[1].ends_with("/members"), "{paths:?}");

    // every check reads both afresh
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    assert_eq!(requests(&server).await.len(), 4);
    Ok(())
}

#[tokio::test]
async fn the_fence_allows_writes_to_the_lab_s_group_chat() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    serve(&server, &thread, &members).await;
    let source = lab.source(&server)?;
    let fence = lab_fence();
    for kind in [WriteKind::Message, WriteKind::Typing, WriteKind::ReadReceipt] {
        let permit = fence.check(Target::new(kind, group_chat()), &source).await;
        assert!(permit.is_ok(), "{kind:?}: {permit:?}");
    }
    // and judges what a write adds as for any chat: an outsider mentioned is denied
    let mention = Target::new(WriteKind::Message, group_chat()).with_added([mri(GUID_X)]);
    let denied = fence.check(mention, &source).await.err().ok_or("allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Outsiders { count: 1 });
    Ok(())
}

#[tokio::test]
async fn a_group_chat_the_lab_didn_t_record_is_never_read() -> TestResult {
    let (thread, members) = bodies()?;
    // another chat recorded, none recorded
    for recorded in [Some(group_id(OTHER_GROUP)), None] {
        let lab = Lab::new(recorded.as_deref())?;
        let server = MockServer::start().await;
        serve(&server, &thread, &members).await;
        let source = lab.source(&server)?;
        assert_eq!(
            source.members(&group_chat()).await,
            partial(PartialReason::UnrecordedGroupChat),
            "{recorded:?}"
        );
        assert!(requests(&server).await.is_empty(), "{recorded:?}");
        let denied = lab_fence()
            .check(Target::new(WriteKind::Message, group_chat()), &source)
            .await
            .err()
            .ok_or("allowed")?;
        assert_eq!(
            *denied.reason(),
            DenyReason::Partial(PartialReason::UnrecordedGroupChat)
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_unreadable_chats_file_is_never_taken_for_a_record() -> TestResult {
    let lab = Lab::new(None)?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    serve(&server, &thread, &members).await;
    let source = lab.source(&server)?;
    let chats = lab.config.paths().chats_file();
    for text in [
        format!("abc = \"{}\"\nother = 1\n", group_id(GROUP)),
        "abc = \"19:meeting_0123@thread.v2\"\n".to_owned(),
        "not toml".to_owned(),
    ] {
        std::fs::write(&chats, &text)?;
        assert_eq!(
            source.members(&group_chat()).await,
            partial(PartialReason::ChatsUnreadable),
            "{text}"
        );
    }
    // a directory where the file should be
    std::fs::remove_file(&chats)?;
    std::fs::create_dir(&chats)?;
    assert_eq!(
        source.members(&group_chat()).await,
        partial(PartialReason::ChatsUnreadable)
    );
    assert!(requests(&server).await.is_empty());
    drop(lab.dir);
    Ok(())
}

#[tokio::test]
async fn a_chat_recorded_after_the_source_was_built_is_read_from_then_on() -> TestResult {
    // the record is read when the fence decides, not when the source is built
    let lab = Lab::new(None)?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    serve(&server, &thread, &members).await;
    let source = lab.source(&server)?;
    assert!(!source.reads_in_full(&group_chat()));
    assert_eq!(
        source.members(&group_chat()).await,
        partial(PartialReason::UnrecordedGroupChat)
    );
    record_abc_chat(lab.config.paths(), &group_id(GROUP))?;
    assert!(source.reads_in_full(&group_chat()));
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    Ok(())
}

#[tokio::test]
async fn a_source_without_the_lab_admits_no_group_chat_and_reads_none() -> TestResult {
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    serve(&server, &thread, &members).await;

    // no lab at all: `with_mock_origin` alone, as `LiveMembershipSource::new` carries none
    let bare = LiveMembershipSource::with_mock_origin(&server.uri())?;
    bare.set_credential(ChatCredential::bearer(BASE, TOKEN).ok_or("no credential")?);
    assert_eq!(bare.members(&group_chat()).await, partial(PartialReason::Unsupported));
    assert!(!bare.reads_in_full(&group_chat()));
    let new = LiveMembershipSource::new()?;
    assert!(!new.reads_in_full(&group_chat()));

    // a lab whose a, b and c aren't three known, distinct MRIs
    let a = || Some(mri(GUID_A).as_str().to_owned());
    let b = || Some(mri(GUID_B).as_str().to_owned());
    let upper_a = Some(mri(GUID_A).as_str().to_uppercase().replace("8:ORGID:", "8:orgid:"));
    for mris in [[a(), b(), None], [a(), b(), upper_a]] {
        let lab = Lab::with_mris(mris.clone(), Some(&group_id(GROUP)))?;
        let source = lab.source(&server)?;
        assert_eq!(
            source.members(&group_chat()).await,
            partial(PartialReason::Unsupported),
            "{mris:?}"
        );
        assert!(!source.reads_in_full(&group_chat()), "{mris:?}");
    }
    assert!(requests(&server).await.is_empty());
    Ok(())
}

#[tokio::test]
async fn reads_in_full_says_yes_only_for_a_one_to_one_chat_and_the_recorded_group_chat() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let source = lab.source(&server)?;
    assert!(source.reads_in_full(&group_chat()));
    assert!(source.reads_in_full(&Scope::Conversation {
        id: format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces"),
    }));
    for scope in [
        Scope::Conversation {
            id: group_id(OTHER_GROUP),
        },
        Scope::Conversation {
            id: group_id(&GROUP.to_uppercase()),
        },
        Scope::Conversation {
            id: "19:meeting_0123456789abcdef@thread.v2".to_owned(),
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
        assert!(!source.reads_in_full(&scope), "{scope}");
    }
    Ok(())
}

#[tokio::test]
async fn a_group_chat_with_an_outsider_is_denied() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (mut thread, mut members) = bodies()?;
    for body in [&mut thread, &mut members] {
        let mut outsider = body["members"][2].clone();
        outsider["id"] = json!(mri(GUID_X).as_str());
        body["members"].as_array_mut().ok_or("no members")?.push(outsider);
    }
    members["totalMemberCount"] = json!(4);
    serve(&server, &thread, &members).await;
    let source = lab.source(&server)?;
    assert_eq!(
        source.members(&group_chat()).await,
        partial(PartialReason::UnexpectedRoster)
    );
    let denied = lab_fence()
        .check(Target::new(WriteKind::Message, group_chat()), &source)
        .await
        .err()
        .ok_or("allowed")?;
    assert_eq!(*denied.reason(), DenyReason::Partial(PartialReason::UnexpectedRoster));
    Ok(())
}

#[tokio::test]
async fn a_failed_member_list_read_is_partial_with_its_status() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (thread, _) = bodies()?;
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", group_id(GROUP))))
        .respond_with(ResponseTemplate::new(200).set_body_json(&thread))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/chatsvc/emea/v1/threads/{}/members",
            group_id(GROUP)
        )))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    assert_eq!(
        lab.source(&server)?.members(&group_chat()).await,
        partial(PartialReason::FetchFailed(FetchFailure::Status(403)))
    );
    Ok(())
}

/// A native client's credential source: `token` first, `renewed` once the service refuses it.
struct Renewing {
    token: Mutex<String>,
    renewed: String,
    refused: AtomicUsize,
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
async fn a_refusal_of_either_read_renews_the_token_once_and_reads_both_again_with_it() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    let renewed = format!("{TOKEN}-renewed");
    // the old token reads the thread, but the member list refuses it
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", group_id(GROUP))))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(&thread))
        .mount(&server)
        .await;
    serve_to(&server, &renewed, &thread, &members).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(10)
        .mount(&server)
        .await;
    let tokens = Arc::new(Renewing {
        token: Mutex::new(TOKEN.to_owned()),
        renewed: renewed.clone(),
        refused: AtomicUsize::new(0),
    });
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?.with_lab(&lab.config);
    source.set_credential_source(tokens.clone());

    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    assert_eq!(tokens.refused.load(Ordering::SeqCst), 1);
    let sent: Vec<(String, String)> = requests(&server)
        .await
        .iter()
        .map(|request| {
            let bearer = request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            (
                request.url.path().rsplit('/').next().unwrap_or_default().to_owned(),
                bearer,
            )
        })
        .collect();
    let thread_segment = group_id(GROUP);
    assert_eq!(
        sent,
        [
            (thread_segment.clone(), format!("Bearer {TOKEN}")),
            ("members".to_owned(), format!("Bearer {TOKEN}")),
            (thread_segment, format!("Bearer {renewed}")),
            ("members".to_owned(), format!("Bearer {renewed}")),
        ],
        "both reads again, with the renewed token"
    );
    Ok(())
}

#[tokio::test]
async fn both_reads_share_one_deadline() -> TestResult {
    // each read alone is well within the deadline, the two together aren't
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let server = MockServer::start().await;
    let (thread, members) = bodies()?;
    let delay = Duration::from_millis(700);
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/emea/v1/threads/{}", group_id(GROUP))))
        .respond_with(ResponseTemplate::new(200).set_body_json(&thread).set_delay(delay))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/api/chatsvc/emea/v1/threads/{}/members",
            group_id(GROUP)
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(&members).set_delay(delay))
        .mount(&server)
        .await;
    let source = lab.source(&server)?.with_deadline(Duration::from_secs(1));
    assert_eq!(
        source.members(&group_chat()).await,
        partial(PartialReason::FetchFailed(FetchFailure::Timeout))
    );
    // with time for both, the same answers are complete
    let source = lab.source(&server)?.with_deadline(Duration::from_secs(5));
    assert_eq!(source.members(&group_chat()).await, Membership::Complete(abc()));
    Ok(())
}

/// One way the answers can differ from what the rule accepts: a name, the change, and the answer it must get.
type Case = (&'static str, fn(&mut Value, &mut Value), Membership);

fn set(body: &mut Value, pointer: &str, value: Value) {
    if let Some(slot) = body.pointer_mut(pointer) {
        *slot = value;
    }
}

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

/// Every case: the answers that must stay partial, each by one change to the fixtures' answers.
fn cases() -> Vec<Case> {
    [
        paging_cases(),
        count_cases(),
        disagreement_cases(),
        roster_cases(),
        shape_cases(),
        more_shape_cases(),
    ]
    .concat()
}

/// Paging, at any depth of either answer.
fn paging_cases() -> Vec<Case> {
    vec![
        (
            "a next link on the list",
            |_, m| m["nextLink"] = json!("https://x.example.test/next"),
            partial(PartialReason::Paginated),
        ),
        (
            "a sync state under the list's _metadata",
            |_, m| m["_metadata"] = json!({"syncState": "x"}),
            partial(PartialReason::Paginated),
        ),
        (
            "a continuation token in a listed member",
            |_, m| m["members"][1]["continuationToken"] = json!("x"),
            partial(PartialReason::Paginated),
        ),
        (
            "a skip token in the thread's properties",
            |t, _| t["properties"]["skipToken"] = json!(""),
            partial(PartialReason::Paginated),
        ),
        (
            "an empty next page marker on the thread",
            |t, _| t["@odata.nextLink"] = json!(null),
            partial(PartialReason::Paginated),
        ),
    ]
}

/// The counts.
fn count_cases() -> Vec<Case> {
    vec![
        // the count
        (
            "a count above the list",
            |_, m| m["totalMemberCount"] = json!(4),
            partial(PartialReason::Truncated),
        ),
        (
            "a count below the list",
            |_, m| m["totalMemberCount"] = json!(2),
            partial(PartialReason::Truncated),
        ),
        ("no count", |_, m| remove(m, "/totalMemberCount"), malformed()),
        (
            "a count that isn't a number",
            |_, m| m["totalMemberCount"] = json!("3"),
            malformed(),
        ),
        (
            "a member count above the list on the thread",
            |t, _| t["properties"]["memberCount"] = json!(4),
            partial(PartialReason::Truncated),
        ),
        (
            "a member count above the list on the list",
            |_, m| m["memberCount"] = json!(4),
            partial(PartialReason::Truncated),
        ),
    ]
}

/// The two reads disagree.
fn disagreement_cases() -> Vec<Case> {
    vec![
        // the reads disagree
        (
            "another member on the list",
            |_, m| m["members"][2]["id"] = json!(mri(GUID_X).as_str()),
            partial(PartialReason::Inconsistent),
        ),
        (
            "a member missing from the list",
            |_, m| {
                members_of(m).pop();
                m["totalMemberCount"] = json!(2);
            },
            partial(PartialReason::Inconsistent),
        ),
        (
            "other eTags",
            |_, m| m["eTag"] = json!("\"1760000000001\""),
            partial(PartialReason::Inconsistent),
        ),
    ]
}

/// The roster both agree on isn't exactly a, b and c made by a.
fn roster_cases() -> Vec<Case> {
    vec![
        // the roster both agree on isn't exactly a, b and c
        (
            "both without c",
            |t, m| {
                members_of(t).pop();
                members_of(m).pop();
                m["totalMemberCount"] = json!(2);
            },
            partial(PartialReason::UnexpectedRoster),
        ),
        (
            "both with an outsider for c",
            |t, m| {
                t["members"][2]["id"] = json!(mri(GUID_X).as_str());
                m["members"][2]["id"] = json!(mri(GUID_X).as_str());
            },
            partial(PartialReason::UnexpectedRoster),
        ),
        (
            "made by b",
            |t, _| t["properties"]["creator"] = json!(mri(GUID_B).as_str()),
            partial(PartialReason::UnexpectedRoster),
        ),
    ]
}

/// The shape the service gave live: the members.
fn shape_cases() -> Vec<Case> {
    vec![
        // the shape the service gave live
        (
            "a member twice on the thread",
            |t, _| {
                let first = t["members"][0].clone();
                members_of(t).push(first);
            },
            malformed(),
        ),
        (
            "a member twice on the list, counted",
            |_, m| {
                let first = m["members"][0].clone();
                members_of(m).push(first);
                m["totalMemberCount"] = json!(4);
            },
            malformed(),
        ),
        (
            "a member twice in another case",
            |t, _| t["members"][2]["id"] = json!(mri(GUID_A).as_str().to_uppercase()),
            malformed(),
        ),
        (
            "a hidden member on the thread",
            |t, _| t["members"][1]["hidden"] = json!(true),
            malformed(),
        ),
        (
            "a hidden member on the list",
            |_, m| m["members"][1]["hidden"] = json!(true),
            malformed(),
        ),
        (
            "a member without hidden",
            |_, m| remove(m, "/members/0/hidden"),
            malformed(),
        ),
    ]
}

/// The shape the service gave live: the rest.
fn more_shape_cases() -> Vec<Case> {
    vec![
        ("a deleted chat", |_, m| m["isDeleted"] = json!(true), malformed()),
        ("no isDeleted", |_, m| remove(m, "/isDeleted"), malformed()),
        (
            "a list roster version other than 0",
            |_, m| m["rosterVersion"] = json!(1_760_000_000_000_u64),
            malformed(),
        ),
        (
            "no list roster version",
            |_, m| remove(m, "/rosterVersion"),
            malformed(),
        ),
        (
            "no thread roster version",
            |t, _| remove(t, "/rosterVersion"),
            malformed(),
        ),
        (
            "a thread roster version that isn't a number",
            |t, _| t["rosterVersion"] = json!("1760000000000"),
            malformed(),
        ),
        ("no thread eTag", |t, _| remove(t, "/eTag"), malformed()),
        (
            "empty eTags",
            |t, m| {
                t["eTag"] = json!("");
                m["eTag"] = json!("");
            },
            malformed(),
        ),
        (
            "a meeting's thread",
            |t, _| set(t, "/properties/threadType", json!("meeting")),
            malformed(),
        ),
        (
            "no thread type",
            |t, _| remove(t, "/properties/threadType"),
            malformed(),
        ),
        ("not a thread", |t, _| t["type"] = json!("Conversation"), malformed()),
        ("no creator", |t, _| remove(t, "/properties/creator"), malformed()),
        (
            "another thread's answer",
            |t, _| t["id"] = json!(group_id(OTHER_GROUP)),
            malformed(),
        ),
        (
            "a member without an id on the list",
            |_, m| remove(m, "/members/1/id"),
            malformed(),
        ),
        ("a list that isn't an object", |_, m| *m = json!([]), malformed()),
    ]
}

#[test]
fn every_condition_of_the_rule_keeps_the_chat_partial_when_it_fails() -> TestResult {
    let (thread, members) = bodies()?;
    let id = group_id(GROUP);
    let lab = abc();
    let lab: [Mri; 3] = [lab[0].clone(), lab[1].clone(), lab[2].clone()];
    assert_eq!(parse_group(&id, &thread, &members, &lab), Membership::Complete(abc()));
    for (case, change, expected) in cases() {
        let (mut thread, mut members) = (thread.clone(), members.clone());
        change(&mut thread, &mut members);
        assert_eq!(parse_group(&id, &thread, &members, &lab), expected, "{case}");
    }
    Ok(())
}

#[tokio::test]
async fn every_condition_holds_over_the_wire_too() -> TestResult {
    let lab = Lab::new(Some(&group_id(GROUP)))?;
    let (thread, members) = bodies()?;
    for (case, change, expected) in cases() {
        let server = MockServer::start().await;
        let (mut thread, mut members) = (thread.clone(), members.clone());
        change(&mut thread, &mut members);
        serve(&server, &thread, &members).await;
        assert_eq!(lab.source(&server)?.members(&group_chat()).await, expected, "{case}");
    }
    Ok(())
}
