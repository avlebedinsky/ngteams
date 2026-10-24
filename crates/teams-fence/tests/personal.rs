//! The personal fence (`docs/personal-build.md`, approved on 2026-10-23): its files, its keep-apart checks, and its
//! decision over the personal membership source's read of a chat's thread (a local mock, `with_mock_origin`).
//!
//! The thread answers are the synthetic fixtures `tests/fixtures/chatsvc/thread-group.json` and
//! `thread-one-to-one.json`, with made-up GUIDs filled in. No real MRI or token appears here.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalError, PersonalMembershipSource, PersonalPaths};
use teams_fence::{
    ChatCredential, ChatPeople, DenyReason, Fence, FenceLog, Membership, MembershipSource, Mri, PartialReason,
    PostReadDenyReason, Roster, Scope, Target, WriteKind, parse_listing,
};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TENANT: &str = "00000000-0000-4000-8000-000000000001";
const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_C: &str = "00000000-0000-4000-8000-00000000000c";
/// Someone outside the lab: the personal fence writes to a chat with them, which the lab fence never does.
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GROUP: &str = "19:0000000000000000000000000000abcd@thread.v2";
const TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJza3lwZWlkIjoicGxhbnRlZCJ9.cGxhbnRlZC1zaWduYXR1cmU";
const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const GROUP_FIXTURE: &str = include_str!("../../../tests/fixtures/chatsvc/thread-group.json");

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn one_to_one(first: &str, second: &str) -> String {
    format!("19:{first}_{second}@unq.gbl.spaces")
}

/// The group fixture's thread answer for chat `id` listing `members` (MRIs).
fn thread(id: &str, members: &[&str]) -> TestResult<Value> {
    let text = GROUP_FIXTURE
        .replace("<guid:a>", GUID_A)
        .replace("<guid:b>", GUID_B)
        .replace("<guid:c>", GUID_C)
        .replace("<guid:tenant>", TENANT)
        .replace("<region>", "amer")
        .replace("19:<group>@thread.v2", id);
    let fixture: Value = serde_json::from_str(&text)?;
    let mut body = fixture["response"]["body"].clone();
    let template = body["members"][0].clone();
    body["members"] = members
        .iter()
        .map(|member| {
            let mut entry = template.clone();
            entry["id"] = json!(member);
            entry
        })
        .collect();
    Ok(body)
}

/// A fence log whose lines can be read back.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Lines {
    fn entries(&self) -> Vec<Value> {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect()
    }
}

/// The personal fence of account `guid`, and its log.
fn personal(guid: &str) -> (Fence, Lines) {
    let lines = Lines::default();
    (Fence::personal(mri(guid), FenceLog::from_writer(lines.clone())), lines)
}

/// A personal source reading from `server`.
fn source(server: &MockServer) -> TestResult<PersonalMembershipSource> {
    let source = PersonalMembershipSource::with_mock_origin(&server.uri())?;
    source.set_credential(ChatCredential::bearer(CHAT_BASE, TOKEN).ok_or("no credential")?);
    Ok(source)
}

/// Serves `body` (with `status`) for chat `id`'s thread read.
async fn serve(server: &MockServer, id: &str, status: u16, body: &Value) {
    Mock::given(method("GET"))
        .and(path(format!("/api/chatsvc/amer/v1/threads/{id}")))
        .and(query_param("view", "msnp24Equivalent"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

fn message(id: &str) -> Target {
    Target::new(WriteKind::Message, Scope::Conversation { id: id.to_owned() })
}

#[tokio::test]
async fn a_chat_with_an_outsider_that_lists_the_account_is_writable_where_the_lab_fence_denies() -> TestResult {
    let server = MockServer::start().await;
    let chat = one_to_one(GUID_A, GUID_X);
    let x = format!("8:orgid:{GUID_X}");
    let a = format!("8:orgid:{GUID_A}");
    serve(&server, &chat, 200, &thread(&chat, &[&a, &x])?).await;
    let (fence, log) = personal(GUID_A);
    let permit = fence.check(message(&chat), &source(&server)?).await?;
    assert_eq!(permit.target(), &message(&chat));
    assert_eq!(
        server.received_requests().await.unwrap_or_default().len(),
        1,
        "read fresh"
    );
    let entries = log.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["verdict"], "allow");
    // the lab fence, which needs every member to be a lab account, denies the same write
    let lab = Fence::new(
        Roster::new([mri(GUID_A), mri(GUID_B)], []),
        FenceLog::from_writer(std::io::sink()),
    );
    assert!(lab.check(message(&chat), &source(&server)?).await.is_err());
    Ok(())
}

#[tokio::test]
async fn every_kind_the_personal_build_sends_goes_to_a_group_chat_listing_the_account() -> TestResult {
    let server = MockServer::start().await;
    let (a, x) = (format!("8:orgid:{GUID_A}"), format!("8:orgid:{GUID_X}"));
    serve(
        &server,
        GROUP,
        200,
        &thread(GROUP, &[&a, &x, "28:00000000-0000-4000-8000-0000000000b0"])?,
    )
    .await;
    let (fence, _) = personal(GUID_A);
    let source = source(&server)?;
    for kind in teams_fence::personal::PERSONAL_KINDS {
        let target = Target::new(kind, Scope::Conversation { id: GROUP.to_owned() });
        assert!(fence.check(target, &source).await.is_ok(), "{kind}");
    }
    // typing and read receipts are read like every write: the personal fence has no exception for them
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 6);
    Ok(())
}

#[tokio::test]
async fn a_conversation_whose_fresh_read_doesn_t_list_the_account_is_denied() -> TestResult {
    let server = MockServer::start().await;
    let (a, b) = (format!("8:orgid:{GUID_A}"), format!("8:orgid:{GUID_B}"));
    serve(&server, GROUP, 200, &thread(GROUP, &[&a, &b])?).await;
    let (fence, log) = personal(GUID_C);
    let denied = fence.check(message(GROUP), &source(&server)?).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::NotAMember);
    assert_eq!(log.entries()[0]["verdict"], "deny");
    // a 1:1 chat whose id doesn't name the account is denied before any read
    let ab = one_to_one(GUID_A, GUID_B);
    let denied = fence.check(message(&ab), &source(&server)?).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::NotAMember);
    assert_eq!(server.received_requests().await.unwrap_or_default().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_partial_read_is_denied() -> TestResult {
    let (a, b) = (format!("8:orgid:{GUID_A}"), format!("8:orgid:{GUID_B}"));
    let listed = thread(GROUP, &[&a, &b])?;
    let mut paged = listed.clone();
    paged["_metadata"] = json!({"syncState": "more"});
    let mut counted = listed.clone();
    counted["properties"]["memberCount"] = json!(3);
    let mut guest = thread(GROUP, &[&a, "8:guest:someone"])?;
    guest["properties"]["threadType"] = json!("chat");
    let mut meeting = listed.clone();
    meeting["properties"]["threadType"] = json!("meeting");
    let mut untyped = listed.clone();
    untyped["properties"]
        .as_object_mut()
        .ok_or("properties")?
        .remove("threadType");
    for (body, status, reason) in [
        (&paged, 200, PartialReason::Paginated),
        (&counted, 200, PartialReason::Truncated),
        (&guest, 200, PartialReason::UnknownForm),
        (&meeting, 200, PartialReason::UnknownForm),
        (&untyped, 200, PartialReason::UnknownForm),
        (
            &listed,
            503,
            PartialReason::FetchFailed(teams_fence::FetchFailure::Status(503)),
        ),
    ] {
        let server = MockServer::start().await;
        serve(&server, GROUP, status, body).await;
        let (fence, _) = personal(GUID_A);
        let denied = fence.check(message(GROUP), &source(&server)?).await.unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::Partial(reason), "{reason:?}");
    }
    // no credential: nothing is read
    let server = MockServer::start().await;
    let (fence, _) = personal(GUID_A);
    let unread = PersonalMembershipSource::with_mock_origin(&server.uri())?;
    let denied = fence.check(message(GROUP), &unread).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Partial(PartialReason::NoCredential));
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[tokio::test]
async fn kinds_and_targets_the_personal_build_doesn_t_write_are_denied_unread() -> TestResult {
    let server = MockServer::start().await;
    let (fence, _) = personal(GUID_A);
    let source = source(&server)?;
    let chat = Scope::Conversation { id: GROUP.to_owned() };
    for kind in [
        WriteKind::ChatCreate,
        WriteKind::MemberChange,
        WriteKind::Invite,
        WriteKind::ChannelPost,
        WriteKind::Call,
        WriteKind::CallAnswer,
        WriteKind::Meeting,
        WriteKind::BotReply,
    ] {
        let denied = fence.check(Target::new(kind, chat.clone()), &source).await.unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::NotPersonalKind, "{kind}");
    }
    for (scope, reason) in [
        (Scope::New, DenyReason::NotCreatable),
        (
            Scope::Channel {
                team: "19:t@thread.tacv2".into(),
                channel: "19:c@thread.tacv2".into(),
            },
            DenyReason::NotPersonalScope,
        ),
        (
            Scope::Conversation {
                id: "19:meeting_abc@thread.v2".into(),
            },
            DenyReason::NotPersonalScope,
        ),
        (
            Scope::Conversation {
                id: "48:notifications".into(),
            },
            DenyReason::NotPersonalScope,
        ),
        (Scope::Call { id: "call".into() }, DenyReason::NotPersonalScope),
    ] {
        let denied = fence
            .check(Target::new(WriteKind::Message, scope.clone()), &source)
            .await
            .unwrap_err();
        assert_eq!(denied.reason(), &reason, "{scope}");
    }
    let adding = message(GROUP).with_added([mri(GUID_B)]);
    assert_eq!(
        fence.check(adding, &source).await.unwrap_err().reason(),
        &DenyReason::AddsParticipants
    );
    assert!(server.received_requests().await.unwrap_or_default().is_empty());
    Ok(())
}

#[tokio::test]
async fn the_account_s_own_availability_is_its_only_write_without_a_conversation() -> TestResult {
    let (fence, _) = personal(GUID_A);
    let source = teams_fence::FailClosed;
    let own = |guid: &str| {
        Target::new(
            WriteKind::OwnAvailability,
            Scope::Account {
                mri: format!("8:orgid:{guid}"),
            },
        )
    };
    assert!(fence.check(own(GUID_A), &source).await.is_ok());
    assert_eq!(
        fence.check(own(GUID_B), &source).await.unwrap_err().reason(),
        &DenyReason::NotOwnAccount
    );
    let message_on_account = Target::new(
        WriteKind::Message,
        Scope::Account {
            mri: format!("8:orgid:{GUID_A}"),
        },
    );
    assert_eq!(
        fence.check(message_on_account, &source).await.unwrap_err().reason(),
        &DenyReason::AccountScope
    );
    assert_eq!(fence.own_account(), Some(&mri(GUID_A)));
    assert!(fence.is_personal());
    Ok(())
}

#[tokio::test]
async fn a_lab_fence_denies_a_listing_and_a_personal_fence_a_complete_list() -> TestResult {
    struct Fixed(Membership);
    impl MembershipSource for Fixed {
        fn members<'a>(&'a self, _: &'a Scope) -> teams_fence::MembershipFuture<'a> {
            let answer = self.0.clone();
            Box::pin(async move { answer })
        }
    }
    let lab = Fence::new(Roster::new([mri(GUID_A)], []), FenceLog::from_writer(std::io::sink()));
    let listed = Fixed(Membership::Listed(vec![mri(GUID_A)]));
    assert_eq!(
        lab.check(message(GROUP), &listed).await.unwrap_err().reason(),
        &DenyReason::Partial(PartialReason::Unsupported)
    );
    assert!(!lab.is_personal());
    let (fence, _) = personal(GUID_A);
    let complete = Fixed(Membership::Complete(vec![mri(GUID_A)]));
    assert_eq!(
        fence.check(message(GROUP), &complete).await.unwrap_err().reason(),
        &DenyReason::Partial(PartialReason::Unsupported)
    );
    Ok(())
}

#[test]
fn a_read_by_post_names_the_account_or_people_of_its_chats_only() {
    let (fence, log) = personal(GUID_A);
    let people = ChatPeople::new();
    people.replace([mri(GUID_X), mri(GUID_B)]);
    assert!(fence.check_post_read_among("profile", &[mri(GUID_X)], &people).is_ok());
    assert!(
        fence
            .check_post_read_among("profile", &[mri(GUID_A), mri(GUID_B)], &people)
            .is_ok()
    );
    for (names, reason) in [
        (vec![mri(GUID_C)], PostReadDenyReason::NotInChats { count: 1 }),
        (
            vec![Mri::new("28:00000000-0000-4000-8000-0000000000b0")],
            PostReadDenyReason::NotInChats { count: 1 },
        ),
        (vec![], PostReadDenyReason::NoNames),
        (vec![mri(GUID_B), mri(GUID_B)], PostReadDenyReason::Duplicate),
    ] {
        let denied = fence.check_post_read_among("profile", &names, &people).unwrap_err();
        assert_eq!(denied.reason(), reason, "{names:?}");
    }
    // the bot is in a chat, but a service's id is never named
    people.replace([Mri::new("28:00000000-0000-4000-8000-0000000000b0")]);
    assert!(
        fence
            .check_post_read_among(
                "profile",
                &[Mri::new("28:00000000-0000-4000-8000-0000000000b0")],
                &people
            )
            .is_err()
    );
    // without the people, only the account itself
    assert!(fence.check_post_read("profile", &[mri(GUID_A)]).is_ok());
    assert!(fence.check_post_read("profile", &[mri(GUID_X)]).is_err());
    assert_eq!(log.entries().len(), 9);
    // a lab fence keeps its own rule whatever the people
    let lab = Fence::new(Roster::new([mri(GUID_A)], []), FenceLog::from_writer(std::io::sink()));
    people.replace([mri(GUID_X)]);
    assert!(lab.check_post_read_among("profile", &[mri(GUID_X)], &people).is_err());
}

#[test]
fn a_listing_of_a_one_to_one_chat_holds_exactly_its_pair() -> TestResult {
    let chat = one_to_one(GUID_A, GUID_X);
    let (a, x, b) = (
        format!("8:orgid:{GUID_A}"),
        format!("8:orgid:{GUID_X}"),
        format!("8:orgid:{GUID_B}"),
    );
    assert_eq!(
        parse_listing(&chat, &thread(&chat, &[&a, &x])?),
        Membership::Listed(vec![mri(GUID_A), mri(GUID_X)])
    );
    assert!(matches!(
        parse_listing(&chat, &thread(&chat, &[&a, &b])?),
        Membership::Partial(_)
    ));
    assert!(matches!(
        parse_listing(&chat, &thread(&chat, &[&a, &x, &x])?),
        Membership::Partial(_)
    ));
    // another chat's answer
    assert!(matches!(
        parse_listing(&chat, &thread(GROUP, &[&a, &x])?),
        Membership::Partial(_)
    ));
    Ok(())
}

/// The paths of a personal build under `dir`.
fn paths_in(dir: &tempfile::TempDir) -> PersonalPaths {
    PersonalPaths::new(dir.path().join("config/teams"), dir.path().join("share/teams"))
}

/// Lab locations under `dir`, with a `lab.toml` naming lab accounts a, b and c when `text` is given.
fn lab_in(dir: &tempfile::TempDir, text: Option<&str>) -> TestResult<LabLocations> {
    let config_file = dir.path().join("config/teams-lab/lab.toml");
    if let Some(text) = text {
        fs::create_dir_all(config_file.parent().ok_or("dir")?)?;
        fs::write(&config_file, text)?;
    }
    Ok(LabLocations {
        config_file,
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/teams"),
    })
}

fn lab_toml() -> String {
    let mut text = String::new();
    for (alias, guid) in ["a", "b", "c"].iter().zip([GUID_A, GUID_B, GUID_C]) {
        text.push_str(
            &[
                "[[accounts]]\nalias = \"",
                alias,
                "\"\nupn = \"bot-",
                alias,
                "@lab.example.test\"\n",
            ]
            .concat(),
        );
        text.push_str(&["mri = \"8:orgid:", guid, "\"\n"].concat());
    }
    text
}

#[test]
fn the_personal_fence_opens_only_with_an_account_apart_from_the_lab() -> TestResult {
    let dir = tempfile::tempdir()?;
    let paths = paths_in(&dir);
    let lab = lab_in(&dir, Some(&lab_toml()))?;
    // before the first sign-in there is no account, or one without its object id
    assert!(matches!(
        Fence::open_personal_with(&paths, &lab),
        Err(PersonalError::NoAccount)
    ));
    PersonalAccount::pending("someone@example.test")?.save(&paths)?;
    assert!(matches!(
        Fence::open_personal_with(&paths, &lab),
        Err(PersonalError::NoAccount)
    ));
    // a lab account, by UPN or by MRI, is refused
    for (upn, oid) in [("BOT-A@lab.example.test", GUID_X), ("someone@example.test", GUID_B)] {
        PersonalAccount::new(upn, oid, TENANT)?.save(&paths)?;
        assert!(matches!(
            Fence::open_personal_with(&paths, &lab),
            Err(PersonalError::LabAccount)
        ));
    }
    let account = PersonalAccount::new("someone@example.test", GUID_X, TENANT)?;
    account.save(&paths)?;
    let fence = Fence::open_personal_with(&paths, &lab)?;
    assert_eq!(fence.own_account(), Some(&mri(GUID_X)));
    assert!(fence.is_personal());
    assert!(paths.fence_log().is_file());
    assert_eq!(fs::metadata(paths.account_file())?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(paths.data_dir())?.permissions().mode() & 0o777, 0o700);
    assert_eq!(PersonalAccount::load(&paths)?, Some(account));
    Ok(())
}

#[test]
fn a_personal_data_dir_overlapping_the_lab_s_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let lab = lab_in(&dir, None)?;
    for data in [
        dir.path().join("share/teams-lab"),
        dir.path().join("share/teams-lab/teams"),
        dir.path().join("share"),
    ] {
        let paths = PersonalPaths::new(dir.path().join("config/teams"), data.clone());
        assert!(
            matches!(paths.check_apart(&lab), Err(PersonalError::LabDataDir { .. })),
            "{data:?}"
        );
    }
    // lab.toml's data_dir too
    let elsewhere = dir.path().join("share/teams");
    let lab = lab_in(&dir, Some(&format!("data_dir = {elsewhere:?}\n{}", lab_toml())))?;
    assert!(matches!(
        paths_in(&dir).check_apart(&lab),
        Err(PersonalError::LabDataDir { .. })
    ));
    Ok(())
}

#[test]
fn account_toml_is_strict() {
    let good = format!("upn = \"someone@example.test\"\noid = \"{GUID_X}\"\ntid = \"{TENANT}\"\n");
    assert!(PersonalAccount::parse(&good).is_ok());
    let upper = good.replace(GUID_X, &GUID_X.to_ascii_uppercase());
    assert_eq!(
        PersonalAccount::parse(&upper).map(|account| account.oid).ok(),
        Some(Some(GUID_X.to_owned()))
    );
    // before the first sign-in: the UPN alone
    let pending = PersonalAccount::parse("upn = \"someone@example.test\"\n").unwrap();
    assert!(!pending.is_signed_in() && pending.mri().is_none());
    for bad in [
        good.replace("someone@example.test", "someone"),
        good.replace(GUID_X, "not-a-guid"),
        good.replace(&format!("tid = \"{TENANT}\"\n"), ""),
        format!("{good}token = \"x\"\n"),
        format!("{good}idp_hosts = [\"Bad Host\"]\n"),
        format!("{good}idp_hosts = [\"localhost\"]\n"),
    ] {
        assert!(PersonalAccount::parse(&bad).is_err(), "{bad}");
    }
    let hosts = format!("{good}idp_hosts = [\"sso.example.test\"]\n");
    assert_eq!(
        PersonalAccount::parse(&hosts).map(|account| account.idp_hosts).ok(),
        Some(vec!["sso.example.test".to_owned()])
    );
}
