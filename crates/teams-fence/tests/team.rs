//! Teams and their channels (`docs/protocol/teams.md`, "The fence"): the live source reads a team's members from its
//! root thread, fresh, and the fence lets a write on the team or one of its channels through unless a person outside the
//! lab (neither a lab account, an approved service nor a contact) is on it. Apps and bots, Teams' own or added, are no
//! people.
//!
//! The answers are the synthetic fixtures `tests/fixtures/chatsvc/thread-team.json` and `thread-team-members.json`, with
//! made-up GUIDs filled in. No real MRI or token appears here.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use teams_fence::lab::LabConfig;
use teams_fence::{
    ChatCredential, DenyReason, Fence, FenceLog, FetchFailure, LiveMembershipSource, Membership, MembershipSource, Mri,
    PartialReason, Roster, Scope, Target, WriteKind, is_team_thread_id, parse_team_thread,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
/// The user's own account, a contact.
const GUID_OWN: &str = "00000000-0000-4000-8000-0000000000e0";
const APPS: [&str; 3] = [
    "00000000-0000-4000-8000-0000000000a1",
    "00000000-0000-4000-8000-0000000000a2",
    "00000000-0000-4000-8000-0000000000a3",
];
/// An app someone added to the team: in the same shape as Teams' own.
const ADDED_APP: &str = "00000000-0000-4000-8000-0000000000b0";
const TEAM: &str = "lAbTeAm0123-_x";
const GROUP: &str = "00000000-0000-4000-8000-0000000000d0";
const OTHER_GROUP: &str = "00000000-0000-4000-8000-0000000000d1";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/emea";
const THREAD_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-team.json");
const MEMBERS_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-team-members.json");

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn app(guid: &str) -> Mri {
    Mri::new(format!("28:{guid}"))
}

fn apps() -> Vec<Mri> {
    APPS.iter().map(|guid| app(guid)).collect()
}

fn team_id() -> String {
    format!("19:{TEAM}@thread.tacv2")
}

fn team() -> Scope {
    Scope::Team { id: team_id() }
}

fn channel_of(team: &str) -> Scope {
    Scope::Channel {
        team: team.to_owned(),
        channel: format!("OID:{GUID_X}@00000000-0000-4000-8000-000000000001"),
    }
}

fn fill(text: &str) -> String {
    text.replace("<guid:a>", GUID_A)
        .replace("<app:1>", APPS[0])
        .replace("<app:2>", APPS[1])
        .replace("<app:3>", APPS[2])
        .replace("<team>", TEAM)
        .replace("<group>", GROUP)
        .replace("<region>", "emea")
}

fn body(fixture: &str) -> TestResult<Value> {
    let fixture: Value = serde_json::from_str(&fill(fixture))?;
    Ok(fixture["response"]["body"].clone())
}

fn thread_body() -> TestResult<Value> {
    body(THREAD_FIXTURE)
}

fn members_body() -> TestResult<Value> {
    body(MEMBERS_FIXTURE)
}

/// `members_body` with `who` on the team too.
fn members_with(who: &[Mri]) -> TestResult<Value> {
    let mut members = members_body()?;
    let list = members["members"].as_array_mut().ok_or("no members")?;
    let template = list[0].clone();
    for mri in who {
        let mut member = template.clone();
        member["id"] = json!(mri.as_str());
        list.push(member);
    }
    let count = list.len();
    members["totalMemberCount"] = json!(count);
    Ok(members)
}

fn source(server: &MockServer) -> TestResult<LiveMembershipSource> {
    let source = LiveMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(ChatCredential::bearer(BASE, TOKEN).ok_or("no credential")?);
    Ok(source)
}

fn thread_path(id: &str) -> String {
    format!("/api/chatsvc/emea/v1/threads/{id}")
}

/// Serves `thread` as the thread read of `id` and `members` as its member list, in the forms the source reads them.
async fn serve_team(server: &MockServer, id: &str, thread: &Value, members: &Value) {
    Mock::given(method("GET"))
        .and(path(thread_path(id)))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(thread))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{}/members", thread_path(id))))
        .and(query_param("view", "msnp24Equivalent"))
        .and(query_param("pageSize", "100"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .and(header("X-MS-Migration", "True"))
        .respond_with(ResponseTemplate::new(200).set_body_json(members))
        .mount(server)
        .await;
}

async fn serve(server: &MockServer, members: &Value) -> TestResult {
    serve_team(server, &team_id(), &thread_body()?, members).await;
    Ok(())
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

fn roster() -> Roster {
    Roster::new([mri(GUID_A), mri(GUID_B), mri(GUID_C)], [])
}

fn fence(roster: Roster) -> Fence {
    Fence::new(roster, FenceLog::from_writer(std::io::sink()))
}

fn malformed() -> Membership {
    Membership::Partial(PartialReason::FetchFailed(FetchFailure::Malformed))
}

async fn denied(fence: &Fence, target: Target, source: &dyn MembershipSource) -> TestResult<DenyReason> {
    let denied = fence.check(target, source).await.err().ok_or("allowed")?;
    Ok(*denied.reason())
}

#[tokio::test]
async fn a_team_s_members_are_read_from_its_root_thread_fresh_every_time() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await?;
    let source = source(&server)?;
    let mut everyone = vec![mri(GUID_A)];
    everyone.extend(apps());
    for _ in 0..2 {
        assert_eq!(source.members(&team()).await, Membership::Complete(everyone.clone()));
    }
    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    let thread = thread_path(&team_id());
    let members = format!("{thread}/members");
    assert_eq!(paths, [thread.clone(), members.clone(), thread, members]);
    Ok(())
}

#[tokio::test]
async fn a_write_on_a_team_of_the_lab_and_its_own_apps_is_allowed() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_with(&[mri(GUID_B)])?).await?;
    let source = source(&server)?;
    let fence = fence(roster());
    for target in [
        Target::new(WriteKind::TeamChange, team()),
        Target::new(WriteKind::ChannelChange, team()),
        Target::new(WriteKind::MemberChange, team()).with_added([mri(GUID_C)]),
    ] {
        let permit = fence.check(target.clone(), &source).await;
        assert!(permit.is_ok(), "{target:?}: {permit:?}");
    }
    Ok(())
}

#[tokio::test]
async fn an_app_or_a_bot_on_the_team_is_no_person_and_only_a_person_outside_the_lab_denies() -> TestResult {
    for (who, outsiders) in [
        (vec![app(ADDED_APP)], None),
        (vec![mri(GUID_X)], Some(1)),
        (vec![app(ADDED_APP), mri(GUID_X)], Some(1)),
    ] {
        let server = MockServer::start().await;
        serve(&server, &members_with(&who)?).await?;
        let source = source(&server)?;
        let verdict = fence(roster())
            .check(Target::new(WriteKind::TeamChange, team()), &source)
            .await;
        match outsiders {
            None => assert!(verdict.is_ok(), "{who:?}: {verdict:?}"),
            Some(count) => assert_eq!(
                verdict.err().map(|denied| *denied.reason()),
                Some(DenyReason::Outsiders { count }),
                "{who:?}"
            ),
        }
    }
    Ok(())
}

#[tokio::test]
async fn adding_an_app_reaches_no_person() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await?;
    let source = source(&server)?;
    let added = Target::new(WriteKind::MemberChange, team()).with_added([app(APPS[0])]);
    assert!(fence(roster()).check(added, &source).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn a_chat_holding_apps_is_no_chat_with_outsiders() -> TestResult {
    let server = MockServer::start().await;
    let chat = "19:0123456789abcdef0123456789abcdef@thread.v2";
    Mock::given(method("GET"))
        .and(path(format!("{}/members", thread_path(chat))))
        .respond_with(ResponseTemplate::new(200).set_body_json(members_body()?))
        .mount(&server)
        .await;
    let source = source(&server)?;
    let target = Target::new(WriteKind::Message, Scope::Conversation { id: chat.to_owned() });
    assert!(fence(roster()).check(target, &source).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn a_team_of_its_apps_alone_reaches_nobody() -> TestResult {
    let server = MockServer::start().await;
    let mut members = members_body()?;
    members["members"].as_array_mut().ok_or("no members")?.remove(0);
    members["totalMemberCount"] = json!(3);
    serve(&server, &members).await?;
    let source = source(&server)?;
    for kind in [WriteKind::TeamChange, WriteKind::ChannelChange] {
        let verdict = fence(roster()).check(Target::new(kind, team()), &source).await;
        assert!(verdict.is_ok(), "{kind:?}: {verdict:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_thread_that_isn_t_a_team_s_root_is_unsupported_and_its_members_never_read() -> TestResult {
    // a channel's thread (setDefaultChannel names one) is no team's member list
    let server = MockServer::start().await;
    let mut thread = thread_body()?;
    thread["properties"]["threadType"] = json!("topic");
    serve_team(&server, &team_id(), &thread, &members_body()?).await;
    let source = source(&server)?;
    assert_eq!(
        source.members(&team()).await,
        Membership::Partial(PartialReason::Unsupported)
    );
    assert_eq!(requests(&server).await.len(), 1, "the thread alone");
    Ok(())
}

#[tokio::test]
async fn a_thread_read_in_any_other_shape_is_malformed() -> TestResult {
    let mutations: [fn(&mut Value); 5] = [
        |thread| thread["id"] = json!("19:other@thread.tacv2"),
        |thread| thread["properties"]["groupId"] = json!("not-a-guid"),
        |thread| {
            thread["properties"].as_object_mut().map(|map| map.remove("groupId"));
        },
        |thread| {
            thread.as_object_mut().map(|map| map.remove("properties"));
        },
        |thread| *thread = json!([]),
    ];
    for mutate in mutations {
        let server = MockServer::start().await;
        let mut thread = thread_body()?;
        mutate(&mut thread);
        serve_team(&server, &team_id(), &thread, &members_body()?).await;
        assert_eq!(source(&server)?.members(&team()).await, malformed(), "{thread}");
    }
    let mut paged = thread_body()?;
    paged["_metadata"] = json!({});
    paged["nextLink"] = json!("x");
    assert_eq!(parse_team_thread(&team_id(), &paged), Err(PartialReason::Paginated));
    Ok(())
}

#[tokio::test]
async fn a_member_list_short_of_the_whole_team_is_partial() -> TestResult {
    let server = MockServer::start().await;
    let mut members = members_body()?;
    members["totalMemberCount"] = json!(87);
    serve(&server, &members).await?;
    let truncated = source(&server)?;
    assert_eq!(
        truncated.members(&team()).await,
        Membership::Partial(PartialReason::Truncated)
    );
    let server = MockServer::start().await;
    let mut members = members_body()?;
    members["nextLink"] = json!("https://example.test/next");
    serve(&server, &members).await?;
    assert_eq!(
        source(&server)?.members(&team()).await,
        Membership::Partial(PartialReason::Paginated)
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_thread_read_is_partial_and_the_members_are_never_read() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(thread_path(&team_id())))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    assert_eq!(
        source(&server)?.members(&team()).await,
        Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(403)))
    );
    assert_eq!(requests(&server).await.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_team_named_by_its_group_is_read_only_once_its_thread_named_that_group() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_with(&[mri(GUID_B)])?).await?;
    let source = source(&server)?;
    let fence = fence(roster());
    let add = |scope: Scope| Target::new(WriteKind::MemberChange, scope).with_added([mri(GUID_B)]);

    // before: nothing named the group, nothing is read
    assert!(!source.reads_in_full(&channel_of(GROUP)));
    assert_eq!(
        denied(&fence, add(channel_of(GROUP)), &source).await?,
        DenyReason::Partial(PartialReason::Unsupported)
    );
    assert!(requests(&server).await.is_empty());

    // a write on the team by its thread learns the group from the thread read
    assert!(
        fence
            .check(Target::new(WriteKind::ChannelChange, team()), &source)
            .await
            .is_ok()
    );
    assert_eq!(source.learned(&GROUP.to_ascii_uppercase()), Some(team_id()));
    assert!(source.reads_in_full(&channel_of(GROUP)));
    let permit = fence.check(add(channel_of(GROUP)), &source).await;
    assert!(permit.is_ok(), "{permit:?}");
    let permit = fence
        .check(
            Target::new(WriteKind::TeamChange, Scope::Team { id: GROUP.to_owned() }),
            &source,
        )
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    assert_eq!(
        requests(&server).await.len(),
        6,
        "each check read the thread and its members afresh"
    );

    // another group stays unknown
    assert_eq!(
        source.members(&channel_of(OTHER_GROUP)).await,
        Membership::Partial(PartialReason::Unsupported)
    );
    Ok(())
}

#[tokio::test]
async fn a_private_channel_s_thread_is_judged_by_its_own_members_and_never_learned_as_its_team() -> TestResult {
    // a private channel's thread is a space of its own naming the team's group, with only some of the team on it
    let server = MockServer::start().await;
    let mut thread = thread_body()?;
    thread["properties"]["productThreadType"] = json!("TeamsPrivateChannel");
    serve_team(&server, &team_id(), &thread, &members_with(&[mri(GUID_B)])?).await;
    let source = source(&server)?;
    let fence = fence(roster());
    let permit = fence
        .check(Target::new(WriteKind::ChannelChange, team()), &source)
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    assert_eq!(source.learned(GROUP), None, "the team's group isn't tied to it");
    assert!(!source.reads_in_full(&channel_of(GROUP)));
    Ok(())
}

#[tokio::test]
async fn a_learned_group_whose_thread_names_another_group_now_is_malformed() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_body()?).await?;
    let source = source(&server)?;
    assert!(matches!(source.members(&team()).await, Membership::Complete(_)));
    server.reset().await;
    let mut thread = thread_body()?;
    thread["properties"]["groupId"] = json!(OTHER_GROUP);
    serve_team(&server, &team_id(), &thread, &members_body()?).await;
    assert_eq!(source.members(&channel_of(GROUP)).await, malformed());
    assert_eq!(requests(&server).await.len(), 1, "the thread alone");
    Ok(())
}

#[tokio::test]
async fn a_channel_named_by_its_team_s_thread_is_judged_by_the_team() -> TestResult {
    let server = MockServer::start().await;
    serve(&server, &members_with(&[mri(GUID_X)])?).await?;
    let source = source(&server)?;
    let reason = denied(
        &fence(roster()),
        Target::new(WriteKind::MemberChange, channel_of(&team_id())).with_added([mri(GUID_B)]),
        &source,
    )
    .await?;
    assert_eq!(reason, DenyReason::Outsiders { count: 1 });
    Ok(())
}

/// A mutation of a channel's thread read, what the source makes of it and how many reads it took.
type Case = (fn(&mut Value), Membership, usize);

/// A standard channel's thread id, other than the team's own.
fn other_channel_id() -> String {
    "19:0123456789abcdef0123456789abcdef@thread.tacv2".to_owned()
}

/// The thread read of a standard channel of the team, as `channel-create` read one (2026-11-02): `topic`, naming the
/// team's root (`spaceId`) and its group.
fn channel_thread() -> TestResult<Value> {
    let mut thread = thread_body()?;
    thread["id"] = json!(other_channel_id());
    let properties = &mut thread["properties"];
    properties["threadType"] = json!("topic");
    properties["spaceId"] = json!(team_id());
    properties.as_object_mut().map(|map| map.remove("spaceType"));
    Ok(thread)
}

/// Serves the team as [`serve`] does and `channel` as the thread read of [`other_channel_id`]; its member list is never
/// served (a read of it fails the test's expectations).
async fn serve_channel(server: &MockServer, channel: &Value, members: &Value) -> TestResult {
    serve(server, members).await?;
    Mock::given(method("GET"))
        .and(path(thread_path(&other_channel_id())))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(channel))
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn another_channel_s_meeting_is_judged_by_the_team_its_thread_names() -> TestResult {
    let server = MockServer::start().await;
    serve_channel(&server, &channel_thread()?, &members_with(&[mri(GUID_B)])?).await?;
    let source = source(&server)?;
    let channel = Scope::Team { id: other_channel_id() };
    assert!(source.reads_in_full(&channel));
    let mut everyone = vec![mri(GUID_A)];
    everyone.extend(apps());
    everyone.push(mri(GUID_B));
    assert_eq!(source.members(&channel).await, Membership::Complete(everyone));
    let paths: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    let team = thread_path(&team_id());
    assert_eq!(
        paths,
        [
            thread_path(&other_channel_id()),
            team.clone(),
            format!("{team}/members")
        ],
        "the channel's thread, then its team's, then the team's members: never the channel's own"
    );
    assert_eq!(source.learned(GROUP), Some(team_id()));

    // Meet now in it: a call on the team, the sender its one added participant
    let meet_now = Target::new(WriteKind::Call, channel)
        .with_added([mri(GUID_A)])
        .with_sender(mri(GUID_A));
    let permit = fence(roster()).check(meet_now, &source).await;
    assert!(permit.is_ok(), "{permit:?}");
    Ok(())
}

#[tokio::test]
async fn another_channel_of_a_team_with_someone_outside_the_lab_denies() -> TestResult {
    let server = MockServer::start().await;
    serve_channel(&server, &channel_thread()?, &members_with(&[mri(GUID_X)])?).await?;
    let source = source(&server)?;
    let meet_now = Target::new(WriteKind::Call, Scope::Team { id: other_channel_id() })
        .with_added([mri(GUID_A)])
        .with_sender(mri(GUID_A));
    assert_eq!(
        denied(&fence(roster()), meet_now, &source).await?,
        DenyReason::Outsiders { count: 1 }
    );
    Ok(())
}

#[tokio::test]
async fn a_channel_thread_that_doesn_t_tie_to_its_team_is_never_judged_by_one() -> TestResult {
    let unsupported = Membership::Partial(PartialReason::Unsupported);
    let cases: [Case; 6] = [
        // no team named: the channel alone
        (
            |thread| {
                thread["properties"].as_object_mut().map(|map| map.remove("spaceId"));
            },
            unsupported.clone(),
            1,
        ),
        (
            |thread| thread["properties"]["spaceId"] = json!(other_channel_id()),
            malformed(),
            1,
        ),
        (
            |thread| thread["properties"]["spaceId"] = json!("19:x@thread.v2"),
            malformed(),
            1,
        ),
        (
            |thread| thread["properties"]["groupId"] = json!("not-a-guid"),
            malformed(),
            1,
        ),
        // the root it names belongs to another group
        (
            |thread| thread["properties"]["groupId"] = json!(OTHER_GROUP),
            malformed(),
            2,
        ),
        // a private channel's thread is a space of its own: read as one, its members its own
        (
            |thread| thread["properties"]["threadType"] = json!("chat"),
            unsupported.clone(),
            1,
        ),
    ];
    for (mutate, expected, reads) in cases {
        let server = MockServer::start().await;
        let mut thread = channel_thread()?;
        mutate(&mut thread);
        serve_channel(&server, &thread, &members_body()?).await?;
        let source = source(&server)?;
        assert_eq!(
            source.members(&Scope::Team { id: other_channel_id() }).await,
            expected,
            "{thread}"
        );
        assert_eq!(requests(&server).await.len(), reads, "{thread}");
    }
    // the root it names isn't a team's root
    let server = MockServer::start().await;
    let mut root = thread_body()?;
    root["properties"]["threadType"] = json!("topic");
    serve_team(&server, &team_id(), &root, &members_body()?).await;
    serve_channel_only(&server, &channel_thread()?).await;
    assert_eq!(
        source(&server)?.members(&Scope::Team { id: other_channel_id() }).await,
        unsupported
    );
    assert_eq!(requests(&server).await.len(), 2, "the two threads, no members");
    Ok(())
}

#[tokio::test]
async fn a_team_named_by_its_group_never_follows_a_channel_s_thread() -> TestResult {
    // a group learned from a channel's root is that root's; a learned thread that reads as a channel now is no team
    let server = MockServer::start().await;
    serve_channel(&server, &channel_thread()?, &members_body()?).await?;
    let source = source(&server)?;
    assert!(matches!(source.members(&team()).await, Membership::Complete(_)));
    server.reset().await;
    let mut root = channel_thread()?;
    root["id"] = json!(team_id());
    root["properties"]["spaceId"] = json!(other_channel_id());
    serve_team(&server, &team_id(), &root, &members_body()?).await;
    assert_eq!(
        source.members(&channel_of(GROUP)).await,
        Membership::Partial(PartialReason::Unsupported)
    );
    assert_eq!(requests(&server).await.len(), 1, "the thread alone");
    Ok(())
}

async fn serve_channel_only(server: &MockServer, channel: &Value) {
    Mock::given(method("GET"))
        .and(path(thread_path(&other_channel_id())))
        .respond_with(ResponseTemplate::new(200).set_body_json(channel))
        .mount(server)
        .await;
}

#[test]
fn a_team_thread_id_is_19_an_id_and_thread_tacv2_or_thread_skype() {
    for id in [
        "19:abc@thread.tacv2",
        "19:a-b_C9@thread.skype",
        "19:67gOcCWclqvxa9PhAVGaKLTDEyNeEziDsybzTQ1@thread.tacv2",
    ] {
        assert!(is_team_thread_id(id), "{id}");
    }
    for id in [
        "19:@thread.tacv2",
        "19:abc@thread.v2",
        "19:abc@thread.tacv2;messageid=1",
        "abc@thread.tacv2",
        "19:a.b@thread.tacv2",
        GROUP,
    ] {
        assert!(!is_team_thread_id(id), "{id}");
    }
    assert!(!is_team_thread_id(&format!("19:{}@thread.tacv2", "a".repeat(129))));
}

#[tokio::test]
async fn a_lab_fence_from_lab_toml_lets_a_lab_team_through() -> TestResult {
    let dir = tempfile::tempdir()?;
    let config = LabConfig::parse(&lab_toml(dir.path()), dir.path())?;
    let fence = Fence::open(&config)?;
    for who in [vec![], vec![mri(GUID_OWN)]] {
        let server = MockServer::start().await;
        serve(&server, &members_with(&who)?).await?;
        let source = source(&server)?;
        let permit = fence.check(Target::new(WriteKind::TeamChange, team()), &source).await;
        assert!(permit.is_ok(), "{who:?}: {permit:?}");
    }
    // the user's account is a contact, someone else is outside the lab
    let server = MockServer::start().await;
    serve(&server, &members_with(&[mri(GUID_OWN), mri(GUID_X)])?).await?;
    let source = source(&server)?;
    let denied = fence
        .check(Target::new(WriteKind::TeamChange, team()), &source)
        .await
        .err()
        .ok_or("allowed")?;
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });
    Ok(())
}

/// A `lab.toml` with accounts a, b, c and a contact, the user's own account (`GUID_OWN`).
fn lab_toml(data_dir: &Path) -> String {
    let mut text = format!("data_dir = \"{}\"\n", data_dir.display());
    for (alias, guid) in [("a", GUID_A), ("b", GUID_B), ("c", GUID_C)] {
        let _ = write!(
            text,
            "\n[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\nmri = \"8:orgid:{guid}\"\n"
        );
    }
    let _ = write!(text, "\n[[contacts]]\nname = \"me\"\nmri = \"8:orgid:{GUID_OWN}\"\n");
    text
}

/// A mailbox a team calendar names, and what changes the team's root before it is served.
type MailboxCase = (Option<&'static str>, fn(&mut Value));

/// The team's made-up mailbox, as its root thread names it (`teamSmtpAddress`, seen on a lab team's root 2026-11-02).
const MAILBOX: &str = "lab-team@lab.example.com";

fn team_calendar(mailbox: &str) -> Scope {
    Scope::TeamCalendar {
        channel: other_channel_id(),
        mailbox: mailbox.to_owned(),
    }
}

/// Serves a standard channel of the team as [`serve_channel`] does, the team's root naming `mailbox` (when given) and
/// `change` applied to it.
async fn serve_mailbox_team(
    server: &MockServer,
    mailbox: Option<&str>,
    change: fn(&mut Value),
    members: &Value,
) -> TestResult {
    let mut root = thread_body()?;
    if let Some(mailbox) = mailbox {
        root["properties"]["teamSmtpAddress"] = json!(mailbox);
    }
    change(&mut root);
    serve_team(server, &team_id(), &root, members).await;
    Mock::given(method("GET"))
        .and(path(thread_path(&other_channel_id())))
        .and(query_param("view", "msnp24Equivalent"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(channel_thread()?))
        .mount(server)
        .await;
    Ok(())
}

#[tokio::test]
async fn a_channel_s_meeting_on_its_team_s_calendar_is_judged_by_the_team_naming_that_mailbox() -> TestResult {
    let server = MockServer::start().await;
    serve_mailbox_team(&server, Some(MAILBOX), |_| {}, &members_with(&[mri(GUID_B)])?).await?;
    let source = source(&server)?;
    let scope = team_calendar(MAILBOX);
    assert!(source.reads_in_full(&scope));
    let permit = fence(roster())
        .check(Target::new(WriteKind::Meeting, scope.clone()), &source)
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    // the mailbox in another case is the same
    let upper = team_calendar(&MAILBOX.to_uppercase());
    let permit = fence(roster())
        .check(Target::new(WriteKind::Meeting, upper), &source)
        .await;
    assert!(permit.is_ok(), "{permit:?}");
    // someone it adds is judged too
    let reason = denied(
        &fence(roster()),
        Target::new(WriteKind::Meeting, scope).with_added([Mri::mail("someone@elsewhere.example.com")]),
        &source,
    )
    .await?;
    assert_eq!(reason, DenyReason::Outsiders { count: 1 });
    Ok(())
}

#[tokio::test]
async fn a_team_calendar_whose_team_has_someone_outside_the_lab_denies() -> TestResult {
    let server = MockServer::start().await;
    serve_mailbox_team(&server, Some(MAILBOX), |_| {}, &members_with(&[mri(GUID_X)])?).await?;
    let source = source(&server)?;
    let reason = denied(
        &fence(roster()),
        Target::new(WriteKind::Meeting, team_calendar(MAILBOX)),
        &source,
    )
    .await?;
    assert_eq!(reason, DenyReason::Outsiders { count: 1 });
    Ok(())
}

#[tokio::test]
async fn a_mailbox_the_channel_s_team_doesn_t_name_is_never_judged_by_that_team() -> TestResult {
    let unsupported = Membership::Partial(PartialReason::Unsupported);
    let cases: [MailboxCase; 3] = [
        // another team's mailbox
        (Some("other-team@lab.example.com"), |_| {}),
        // a root naming no mailbox
        (None, |_| {}),
        // a private channel's own thread, whose members are only some of the team's
        (Some(MAILBOX), |root| {
            root["properties"]["productThreadType"] = json!("TeamsPrivateChannel");
        }),
    ];
    for (named, change) in cases {
        let server = MockServer::start().await;
        serve_mailbox_team(&server, named, change, &members_body()?).await?;
        let source = source(&server)?;
        assert_eq!(source.members(&team_calendar(MAILBOX)).await, unsupported, "{named:?}");
        let members = format!("{}/members", thread_path(&team_id()));
        assert!(
            requests(&server)
                .await
                .iter()
                .all(|request| request.url.path() != members),
            "the members are never read"
        );
    }
    // a channel that isn't a team's thread isn't read at all
    let server = MockServer::start().await;
    let source = source(&server)?;
    let chat = Scope::TeamCalendar {
        channel: "19:meeting_x@thread.v2".to_owned(),
        mailbox: MAILBOX.to_owned(),
    };
    assert!(!source.reads_in_full(&chat));
    assert_eq!(source.members(&chat).await, unsupported);
    assert!(requests(&server).await.is_empty());
    Ok(())
}
