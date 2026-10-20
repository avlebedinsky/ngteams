//! The fence decision: a table of every `WriteKind` against participant sets, the special targets (new chats,
//! invites, org-wide teams, incoming calls), fresh membership on every check, and the `fence.log` entries.
//!
//! MRIs here are placeholders, never real ones (the fixture guard would flag a real `8:orgid:<guid>`).

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use fixture_guard::Guard;
use teams_fence::lab::LabConfig;
use teams_fence::{
    ArmingCause, ArmingRecord, Channel, CloseOutcome, Containment, DenyReason, FailClosed, Fence, FenceLog,
    FetchFailure, Membership, MembershipFuture, MembershipSource, Mri, OpenError, PartialReason, RequestOutcome,
    RequestRecord, Roster, Scope, Target, WriteKind,
};

const LAB_A: &str = "8:orgid:lab-a";
const LAB_B: &str = "8:orgid:lab-b";
const LAB_C: &str = "8:orgid:lab-c";
const ECHO: &str = "28:echo-service";
const COWORKER: &str = "8:orgid:coworker";
const OTHER_BOT: &str = "28:other-bot";
/// A credential the membership source holds; it must never reach the log or an error.
const PLANTED_TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJwbGFudGVkLXN1YmplY3QifQ.c2lnbmF0dXJlLXZhbHVlLXBsYW50ZWQ";

fn mri(text: &str) -> Mri {
    Mri::new(text)
}

fn mris(texts: &[&str]) -> Vec<Mri> {
    texts.iter().copied().map(mri).collect()
}

fn roster() -> Roster {
    Roster::new(mris(&[LAB_A, LAB_B, LAB_C]), mris(&[ECHO]))
}

fn chat() -> Scope {
    Scope::Conversation {
        id: "19:lab-chat@thread.v2".to_owned(),
    }
}

/// A source that answers every fetch with the same membership, counts fetches and holds a credential.
struct Fixed {
    membership: Mutex<Membership>,
    fetches: AtomicUsize,
    credential: String,
}

impl Fixed {
    fn new(membership: Membership) -> Self {
        Self {
            membership: Mutex::new(membership),
            fetches: AtomicUsize::new(0),
            credential: format!("Bearer {PLANTED_TOKEN}"),
        }
    }

    fn complete(members: &[&str]) -> Self {
        Self::new(Membership::Complete(mris(members)))
    }

    fn set(&self, membership: Membership) {
        *self.membership.lock().unwrap_or_else(PoisonError::into_inner) = membership;
    }

    fn fetches(&self) -> usize {
        self.fetches.load(Ordering::SeqCst)
    }
}

impl MembershipSource for Fixed {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        assert!(!self.credential.is_empty());
        let membership = self.membership.lock().unwrap_or_else(PoisonError::into_inner).clone();
        Box::pin(async move { membership })
    }
}

struct Lab {
    _dir: tempfile::TempDir,
    log_path: std::path::PathBuf,
    fence: Fence,
}

fn try_lab() -> std::io::Result<Lab> {
    let dir = tempfile::tempdir()?;
    let log_path = dir.path().join("fence.log");
    let fence = Fence::new(roster(), FenceLog::open(&log_path)?);
    Ok(Lab {
        _dir: dir,
        log_path,
        fence,
    })
}

fn log_entries(path: &Path) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    let entries = text.lines().map(serde_json::from_str).collect::<Result<_, _>>()?;
    Ok(entries)
}

/// The verdict for a participant set; `None` means allow.
type Expected = Option<DenyReason>;

fn membership_cases() -> Vec<(&'static str, Membership, Expected)> {
    vec![
        ("lab only", Membership::Complete(mris(&[LAB_A, LAB_B])), None),
        (
            "all three lab accounts",
            Membership::Complete(mris(&[LAB_A, LAB_B, LAB_C])),
            None,
        ),
        (
            "lab + approved service",
            Membership::Complete(mris(&[LAB_A, ECHO])),
            None,
        ),
        (
            "lab + coworker",
            Membership::Complete(mris(&[LAB_A, LAB_B, COWORKER])),
            Some(DenyReason::Outsiders { count: 1 }),
        ),
        (
            "lab + unapproved service",
            Membership::Complete(mris(&[LAB_A, OTHER_BOT])),
            Some(DenyReason::Outsiders { count: 1 }),
        ),
        (
            "only outsiders",
            Membership::Complete(mris(&[COWORKER, OTHER_BOT])),
            Some(DenyReason::Outsiders { count: 2 }),
        ),
        ("empty", Membership::Complete(Vec::new()), Some(DenyReason::NoMembers)),
        (
            "fetch failed (network)",
            Membership::Partial(PartialReason::FetchFailed(FetchFailure::Network)),
            Some(DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Network))),
        ),
        (
            "fetch failed (status)",
            Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(503))),
            Some(DenyReason::Partial(PartialReason::FetchFailed(FetchFailure::Status(
                503,
            )))),
        ),
        (
            "paginated",
            Membership::Partial(PartialReason::Paginated),
            Some(DenyReason::Partial(PartialReason::Paginated)),
        ),
        (
            "truncated",
            Membership::Partial(PartialReason::Truncated),
            Some(DenyReason::Partial(PartialReason::Truncated)),
        ),
        ("org-wide", Membership::OrgWide, Some(DenyReason::OrgWide)),
    ]
}

#[tokio::test]
async fn every_write_kind_is_fenced_by_its_participants() {
    let lab = try_lab().unwrap();
    for kind in WriteKind::ALL {
        for (case, membership, expected) in membership_cases() {
            let source = Fixed::new(membership);
            let verdict = lab.fence.check(Target::new(kind, chat()), &source).await;
            match (&verdict, &expected) {
                (Ok(permit), None) => assert_eq!(permit.target(), &Target::new(kind, chat()), "{kind:?} / {case}"),
                (Err(denied), Some(reason)) => {
                    assert_eq!(denied.reason(), reason, "{kind:?} / {case}");
                    assert_eq!(denied.target(), &Target::new(kind, chat()), "{kind:?} / {case}");
                }
                _ => panic!("{kind:?} / {case}: got {verdict:?}, expected {expected:?}"),
            }
            assert_eq!(
                source.fetches(),
                1,
                "{kind:?} / {case}: membership is fetched once per check"
            );
        }
    }
}

#[tokio::test]
async fn write_kinds_are_distinct_and_named() {
    let names: BTreeSet<&str> = WriteKind::ALL.iter().map(|kind| kind.as_str()).collect();
    assert_eq!(names.len(), WriteKind::ALL.len());
    for name in [
        "message",
        "edit",
        "delete",
        "reaction",
        "typing",
        "read_receipt",
        "call",
        "call_answer",
        "invite",
        "meeting",
        "member_change",
        "chat_create",
        "channel_post",
        "bot_reply",
    ] {
        assert!(names.contains(name), "missing write kind {name}");
    }
    for kind in WriteKind::ALL {
        assert_eq!(
            serde_json::to_value(kind).unwrap(),
            kind.as_str(),
            "fence.log names {kind:?} as as_str does"
        );
        assert_eq!(kind.to_string(), kind.as_str());
    }
}

#[tokio::test]
async fn org_wide_team_and_channel_are_denied_even_with_lab_only_members_listed() {
    let lab = try_lab().unwrap();
    let team = Scope::Team {
        id: "19:org-wide-team@thread.tacv2".to_owned(),
    };
    let channel = Scope::Channel {
        team: "19:org-wide-team@thread.tacv2".to_owned(),
        channel: "19:general@thread.tacv2".to_owned(),
    };
    for (kind, scope) in [
        (WriteKind::ChannelPost, channel.clone()),
        (WriteKind::Reaction, channel),
        (WriteKind::MemberChange, team),
    ] {
        let denied = lab
            .fence
            .check(Target::new(kind, scope), &Fixed::new(Membership::OrgWide))
            .await
            .unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::OrgWide);
    }
}

#[tokio::test]
async fn a_lab_only_channel_allows_a_post() {
    let lab = try_lab().unwrap();
    let channel = Scope::Channel {
        team: "19:lab-team@thread.tacv2".to_owned(),
        channel: "19:lab-general@thread.tacv2".to_owned(),
    };
    let target = Target::new(WriteKind::ChannelPost, channel);
    let permit = lab
        .fence
        .check(target.clone(), &Fixed::complete(&[LAB_A, LAB_B, LAB_C]))
        .await
        .unwrap();
    assert_eq!(permit.target(), &target);
}

#[tokio::test]
async fn answering_an_incoming_call_needs_a_lab_caller() {
    let lab = try_lab().unwrap();
    let call = Scope::Call {
        id: "call-from-coworker".to_owned(),
    };
    let denied = lab
        .fence
        .check(
            Target::new(WriteKind::CallAnswer, call.clone()),
            &Fixed::complete(&[COWORKER, LAB_A]),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });

    let lab_call = lab
        .fence
        .check(
            Target::new(WriteKind::CallAnswer, call),
            &Fixed::complete(&[LAB_B, LAB_A]),
        )
        .await;
    assert!(
        lab_call.is_ok(),
        "a call from another lab account may be answered: {lab_call:?}"
    );
}

#[tokio::test]
async fn a_new_chat_is_judged_by_its_proposed_members_without_a_fetch() {
    let lab = try_lab().unwrap();
    let unused = Fixed::new(Membership::Partial(PartialReason::Truncated));
    let create = |members: &[&str]| Target::new(WriteKind::ChatCreate, Scope::New).with_added(mris(members));

    assert!(lab.fence.check(create(&[LAB_A, LAB_B]), &unused).await.is_ok());
    assert!(lab.fence.check(create(&[LAB_A, ECHO]), &unused).await.is_ok());
    let outsider = lab.fence.check(create(&[LAB_A, COWORKER]), &unused).await.unwrap_err();
    assert_eq!(outsider.reason(), &DenyReason::Outsiders { count: 1 });
    let empty = lab.fence.check(create(&[]), &unused).await.unwrap_err();
    assert_eq!(empty.reason(), &DenyReason::NoMembers);
    assert_eq!(unused.fetches(), 0, "a new target has nothing to fetch");
}

#[tokio::test]
async fn only_a_kind_that_creates_a_target_may_name_a_new_one() {
    let lab = try_lab().unwrap();
    let creating = [WriteKind::ChatCreate, WriteKind::Call, WriteKind::Meeting];
    for kind in WriteKind::ALL {
        let unused = Fixed::complete(&[LAB_A, LAB_B]);
        let target = Target::new(kind, Scope::New).with_added(mris(&[LAB_A, LAB_B]));
        let verdict = lab.fence.check(target, &unused).await;
        assert_eq!(kind.creates_target(), creating.contains(&kind), "{kind:?}");
        if creating.contains(&kind) {
            assert!(verdict.is_ok(), "{kind:?} may create a lab-only target: {verdict:?}");
        } else {
            let denied = verdict.unwrap_err();
            assert_eq!(denied.reason(), &DenyReason::NotCreatable, "{kind:?}");
        }
        assert_eq!(unused.fetches(), 0, "{kind:?}: a new target is never fetched");
    }
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), WriteKind::ALL.len());
    let not_creatable = entries
        .iter()
        .filter(|entry| entry["reason"] == "not_creatable")
        .count();
    assert_eq!(not_creatable, WriteKind::ALL.len() - creating.len());
}

#[tokio::test]
async fn a_denied_write_shows_added_participants_only_as_a_count() {
    let lab = try_lab().unwrap();
    let target = Target::new(WriteKind::Invite, chat()).with_added(mris(&[COWORKER, OTHER_BOT]));
    let denied = lab
        .fence
        .check(target, &Fixed::complete(&[LAB_A, LAB_B]))
        .await
        .unwrap_err();
    for shown in [
        format!("{denied}"),
        format!("{denied:?}"),
        format!("{:?}", denied.target()),
    ] {
        assert!(!shown.contains(COWORKER), "{shown}");
        assert!(!shown.contains(OTHER_BOT), "{shown}");
    }
    assert!(format!("{denied:?}").contains("added: 2"), "{denied:?}");
    assert_eq!(denied.target().added().len(), 2, "the accessor still gives the MRIs");
}

#[tokio::test]
async fn an_invite_counts_the_invitees_and_the_current_members() {
    let lab = try_lab().unwrap();
    let invite = |members: &[&str]| Target::new(WriteKind::Invite, chat()).with_added(mris(members));

    let lab_only = Fixed::complete(&[LAB_A, LAB_B]);
    assert!(lab.fence.check(invite(&[LAB_C]), &lab_only).await.is_ok());
    let outsider_invited = lab.fence.check(invite(&[COWORKER]), &lab_only).await.unwrap_err();
    assert_eq!(outsider_invited.reason(), &DenyReason::Outsiders { count: 1 });

    let outsider_present = Fixed::complete(&[LAB_A, COWORKER]);
    let denied = lab.fence.check(invite(&[LAB_C]), &outsider_present).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });
}

#[tokio::test]
async fn an_empty_fetch_is_denied_even_when_lab_members_are_added() {
    let lab = try_lab().unwrap();
    let target = Target::new(WriteKind::MemberChange, chat()).with_added(mris(&[LAB_B]));
    let denied = lab.fence.check(target, &Fixed::complete(&[])).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::NoMembers);
}

#[tokio::test]
async fn a_lab_account_or_service_without_an_mri_matches_nobody() {
    let dir = tempfile::tempdir().unwrap();
    // Account c and the echo service have no MRI yet, so nothing in the roster stands for them.
    let fence = Fence::new(
        Roster::new(mris(&[LAB_A, LAB_B]), Vec::new()),
        FenceLog::open(&dir.path().join("fence.log")).unwrap(),
    );
    let with_c = fence
        .check(
            Target::new(WriteKind::Message, chat()),
            &Fixed::complete(&[LAB_A, LAB_C]),
        )
        .await
        .unwrap_err();
    assert_eq!(with_c.reason(), &DenyReason::Outsiders { count: 1 });
    let with_echo = fence
        .check(Target::new(WriteKind::Call, chat()), &Fixed::complete(&[LAB_A, ECHO]))
        .await
        .unwrap_err();
    assert_eq!(with_echo.reason(), &DenyReason::Outsiders { count: 1 });
}

#[tokio::test]
async fn mri_matching_is_exact() {
    let lab = try_lab().unwrap();
    for lookalike in ["8:orgid:LAB-A", "8:orgid:lab-a ", "8:orgid:lab-a2", "orgid:lab-a"] {
        let denied = lab
            .fence
            .check(
                Target::new(WriteKind::Message, chat()),
                &Fixed::complete(&[LAB_B, lookalike]),
            )
            .await
            .unwrap_err();
        assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 }, "{lookalike:?}");
    }
}

#[tokio::test]
async fn membership_is_fetched_fresh_on_every_check() {
    let lab = try_lab().unwrap();
    let source = Fixed::complete(&[LAB_A, LAB_B]);
    let target = Target::new(WriteKind::Message, chat());
    assert!(lab.fence.check(target.clone(), &source).await.is_ok());

    source.set(Membership::Complete(mris(&[LAB_A, LAB_B, COWORKER])));
    let denied = lab.fence.check(target.clone(), &source).await.unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::Outsiders { count: 1 });

    source.set(Membership::Partial(PartialReason::Paginated));
    assert!(lab.fence.check(target, &source).await.is_err());
    assert_eq!(source.fetches(), 3);
}

#[tokio::test]
async fn every_deny_is_logged_with_its_reason_and_no_tokens() {
    let lab = try_lab().unwrap();
    let mut denies = 0;
    let mut allows = 0;
    for (case, membership, expected) in membership_cases() {
        let source = Fixed::new(membership);
        let verdict = lab.fence.check(Target::new(WriteKind::Message, chat()), &source).await;
        assert_eq!(verdict.is_err(), expected.is_some(), "{case}");
        if let Err(denied) = verdict {
            denies += 1;
            let shown = format!("{denied} {denied:?}");
            assert!(!shown.contains(PLANTED_TOKEN), "{case}: the error shows a token");
        } else {
            allows += 1;
        }
    }

    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), denies + allows, "one entry per decision");
    let deny_entries: Vec<_> = entries.iter().filter(|entry| entry["verdict"] == "deny").collect();
    assert_eq!(deny_entries.len(), denies);
    for entry in &deny_entries {
        assert_eq!(entry["kind"], "message");
        assert!(!entry["reason"].is_null(), "a deny without a reason: {entry}");
        assert!(
            entry["message"].as_str().is_some_and(|text| !text.is_empty()),
            "{entry}"
        );
        assert!(entry["ts_ms"].as_u64().is_some_and(|ts| ts > 0), "{entry}");
        assert_eq!(entry["scope"]["conversation"]["id"], "19:lab-chat@thread.v2");
    }
    for entry in entries.iter().filter(|entry| entry["verdict"] == "allow") {
        assert!(entry.get("reason").is_none(), "an allow carries no reason: {entry}");
    }

    let text = fs::read_to_string(&lab.log_path).unwrap();
    assert!(!text.contains(PLANTED_TOKEN));
    assert!(!text.contains("Bearer"));
    assert_eq!(
        Guard::new().unwrap().scan_text(&text),
        Vec::new(),
        "the guard finds secrets in fence.log"
    );
}

#[tokio::test]
async fn deny_reasons_are_logged_by_name() {
    let lab = try_lab().unwrap();
    let cases = [
        (Membership::Complete(mris(&[COWORKER])), r#"{"outsiders":{"count":1}}"#),
        (Membership::Complete(Vec::new()), r#""no_members""#),
        (Membership::OrgWide, r#""org_wide""#),
        (
            Membership::Partial(PartialReason::Paginated),
            r#"{"partial":"paginated"}"#,
        ),
        (
            Membership::Partial(PartialReason::FetchFailed(FetchFailure::Status(429))),
            r#"{"partial":{"fetch_failed":{"status":429}}}"#,
        ),
    ];
    for (membership, _) in &cases {
        let _ = lab
            .fence
            .check(Target::new(WriteKind::Typing, chat()), &Fixed::new(membership.clone()))
            .await;
    }
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), cases.len());
    for (entry, (_, reason)) in entries.iter().zip(cases) {
        let expected: serde_json::Value = serde_json::from_str(reason).unwrap();
        assert_eq!(entry["reason"], expected, "{entry}");
        assert_eq!(entry["kind"], "typing");
    }
}

#[tokio::test]
async fn the_fail_closed_source_denies_every_write_that_needs_a_fetch() {
    let lab = try_lab().unwrap();
    let call = Scope::Call {
        id: "call-from-lab-b".to_owned(),
    };
    for kind in WriteKind::ALL {
        for scope in [chat(), call.clone()] {
            let denied = lab
                .fence
                .check(Target::new(kind, scope), &FailClosed)
                .await
                .unwrap_err();
            assert_eq!(
                denied.reason(),
                &DenyReason::Partial(PartialReason::NoSource),
                "{kind:?}"
            );
        }
    }
    let entries = log_entries(&lab.log_path).unwrap();
    assert_eq!(entries.len(), WriteKind::ALL.len() * 2);
    for entry in &entries {
        assert_eq!(entry["reason"], serde_json::json!({"partial": "no_source"}), "{entry}");
    }

    // a new chat isn't fetched: it is still judged by its proposed members alone
    let create = Target::new(WriteKind::ChatCreate, Scope::New).with_added(mris(&[LAB_A, LAB_B]));
    assert!(lab.fence.check(create, &FailClosed).await.is_ok());
}

#[tokio::test]
async fn the_log_appends_across_fences_and_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fence.log");
    for _ in 0..2 {
        let fence = Fence::new(roster(), FenceLog::open(&path).unwrap());
        let _ = fence
            .check(Target::new(WriteKind::Reaction, chat()), &Fixed::complete(&[COWORKER]))
            .await;
    }
    assert_eq!(log_entries(&path).unwrap().len(), 2);
    let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn an_allow_that_cannot_be_logged_is_denied() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("disk full"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fence = Fence::new(roster(), FenceLog::from_writer(Broken));
    let denied = fence
        .check(
            Target::new(WriteKind::Message, chat()),
            &Fixed::complete(&[LAB_A, LAB_B]),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.reason(), &DenyReason::LogUnavailable);

    let outsider = fence
        .check(Target::new(WriteKind::Message, chat()), &Fixed::complete(&[COWORKER]))
        .await
        .unwrap_err();
    assert_eq!(
        outsider.reason(),
        &DenyReason::Outsiders { count: 1 },
        "the original deny reason is kept"
    );
}

/// A writer that takes `budget` bytes, fails once the budget runs out (a full disk), and takes everything after that
/// failure (the disk has room again).
struct FillsUp {
    written: Arc<Mutex<Vec<u8>>>,
    budget: usize,
    failed: bool,
}

impl std::io::Write for FillsUp {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        if self.failed {
            written.extend_from_slice(buf);
            return Ok(buf.len());
        }
        if self.budget == 0 {
            self.failed = true;
            return Err(std::io::Error::other("disk full"));
        }
        let taken = buf.len().min(self.budget);
        self.budget -= taken;
        written.extend_from_slice(&buf[..taken]);
        Ok(taken)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn after_a_half_written_entry_the_log_refuses_every_later_one() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = FillsUp {
        written: Arc::clone(&written),
        budget: 20,
        failed: false,
    };
    let fence = Fence::new(roster(), FenceLog::from_writer(sink));
    let allow = || Target::new(WriteKind::Message, chat());
    let lab_only = Fixed::complete(&[LAB_A, LAB_B]);

    let first = fence.check(allow(), &lab_only).await.unwrap_err();
    assert_eq!(first.reason(), &DenyReason::LogUnavailable);
    let second = fence.check(allow(), &lab_only).await.unwrap_err();
    assert_eq!(
        second.reason(),
        &DenyReason::LogUnavailable,
        "the writer would take it now, but it would run into the half-written entry"
    );
    let bytes = written.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(bytes.len(), 20, "nothing is written after the failed append");
}

#[tokio::test]
async fn reopening_terminates_a_half_written_last_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fence.log");
    let fence = Fence::new(roster(), FenceLog::open(&path).unwrap());
    let _ = fence
        .check(Target::new(WriteKind::Reaction, chat()), &Fixed::complete(&[COWORKER]))
        .await;
    drop(fence);
    let fragment = r#"{"ts_ms":1760000000000,"verdict":"al"#;
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    std::io::Write::write_all(&mut file, fragment.as_bytes()).unwrap();
    drop(file);

    let fence = Fence::new(roster(), FenceLog::open(&path).unwrap());
    let _ = fence
        .check(Target::new(WriteKind::Typing, chat()), &Fixed::complete(&[COWORKER]))
        .await;
    let text = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!(lines[1], fragment, "the fragment keeps a line of its own");
    let last: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(last["kind"], "typing");
    assert!(text.ends_with('\n'));

    drop(fence);
    let before = fs::read_to_string(&path).unwrap();
    drop(FenceLog::open(&path).unwrap());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        before,
        "a well-formed log is left as it is"
    );
}

// --- browser requests decided without a membership check ---

#[test]
fn a_request_entry_names_the_endpoint_but_not_its_query_fragment_or_user_info() {
    let lab = try_lab().unwrap();
    let log = lab.fence.log();
    let url = format!(
        "https://user:{PLANTED_TOKEN}@chat.example.com:8443/v1/users/ME/conversations?token={PLANTED_TOKEN}#{PLANTED_TOKEN}"
    );
    log.record_request(&RequestRecord {
        channel: Channel::Http,
        method: "POST",
        url: &url,
        outcome: RequestOutcome::Unclassified,
    })
    .unwrap();
    log.record_request(&RequestRecord {
        channel: Channel::Http,
        method: "POST",
        url: "https://login.example.com/common/oauth2/v2.0/token?x=1",
        outcome: RequestOutcome::Own("entra_token"),
    })
    .unwrap();
    log.record_request(&RequestRecord {
        channel: Channel::WebSocket,
        method: "SEND",
        url: &format!("wss://trouter.example.com/v4/c?tc={PLANTED_TOKEN}"),
        outcome: RequestOutcome::SentWithoutAllow,
    })
    .unwrap();

    let entries = log_entries(&lab.log_path).unwrap();
    let expected: Vec<serde_json::Value> = vec![
        serde_json::json!({
            "verdict": "deny",
            "channel": "http",
            "method": "POST",
            "url": "https://chat.example.com:8443/v1/users/ME/conversations",
            "reason": "unclassified",
            "message": "no rule classifies this request",
        }),
        serde_json::json!({
            "verdict": "allow",
            "channel": "http",
            "method": "POST",
            "url": "https://login.example.com/common/oauth2/v2.0/token",
            "own": "entra_token",
        }),
        serde_json::json!({
            "verdict": "breach",
            "channel": "web_socket",
            "method": "SEND",
            "url": "wss://trouter.example.com/v4/c",
            "reason": "sent_without_allow",
            "message": "this left the browser without the fence's allow",
        }),
    ];
    for (entry, expected) in entries.iter().zip(&expected) {
        let mut entry = entry.clone();
        let ts = entry.as_object_mut().unwrap().remove("ts_ms").unwrap();
        assert!(ts.as_u64().is_some_and(|ts| ts > 0));
        assert_eq!(&entry, expected);
    }
    assert_eq!(entries.len(), expected.len());
    let text = fs::read_to_string(&lab.log_path).unwrap();
    assert!(!text.contains(PLANTED_TOKEN), "{text}");
}

#[test]
fn request_entries_share_the_log_with_write_decisions_and_its_failure_mode() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = FillsUp {
        written: Arc::clone(&written),
        budget: 30,
        failed: false,
    };
    let log = FenceLog::from_writer(sink);
    let record = RequestRecord {
        channel: Channel::WebSocket,
        method: "SEND",
        url: "wss://trouter.example.com/v4/c",
        outcome: RequestOutcome::Unclassified,
    };
    assert!(log.record_request(&record).is_err(), "a half-written entry fails");
    assert!(log.record_request(&record).is_err(), "and so does every later one");
    let text = written.lock().unwrap().clone();
    assert!(!text.contains(&b'\n'), "nothing after the half-written entry");
}

// --- targets the browser fence couldn't arm ---

fn held_worker(close: CloseOutcome, containment: Containment) -> ArmingRecord<'static> {
    ArmingRecord {
        target_type: "worker",
        step: Some("Network.enable"),
        cause: ArmingCause::NoAnswer,
        resume_sent: false,
        held: true,
        close,
        containment,
    }
}

#[test]
fn an_arming_failure_is_a_breach_only_when_the_target_may_have_run_or_was_not_contained() {
    let closed = held_worker(CloseOutcome::Closed, Containment::Closed);
    assert!(!closed.may_have_run() && !closed.breach());
    let killed = held_worker(CloseOutcome::Unanswered, Containment::BrowserKilled);
    assert!(
        !killed.may_have_run() && !killed.breach(),
        "held, never resumed, killed with the browser"
    );
    let refused = held_worker(CloseOutcome::Refused, Containment::BrowserKilled);
    assert!(!refused.breach());

    let loose = held_worker(CloseOutcome::Unanswered, Containment::Failed);
    assert!(!loose.may_have_run() && loose.breach(), "not contained");
    let resumed = ArmingRecord {
        step: Some("Runtime.runIfWaitingForDebugger"),
        resume_sent: true,
        ..closed
    };
    assert!(resumed.may_have_run() && resumed.breach(), "its resume was sent");
    let running = ArmingRecord { held: false, ..killed };
    assert!(
        running.may_have_run() && running.breach(),
        "it was running before its arming began"
    );
}

#[test]
fn an_arming_failure_entry_names_the_step_and_its_containment_and_no_ids() {
    let lab = try_lab().unwrap();
    let log = lab.fence.log();
    log.record_arming(&held_worker(CloseOutcome::Refused, Containment::BrowserKilled))
        .unwrap();
    log.record_arming(&ArmingRecord {
        target_type: "iframe",
        step: None,
        cause: ArmingCause::Malformed,
        resume_sent: false,
        held: true,
        close: CloseOutcome::NoTarget,
        containment: Containment::Failed,
    })
    .unwrap();

    let entries = log_entries(&lab.log_path).unwrap();
    let expected = [
        serde_json::json!({
            "verdict": "contained",
            "event": "arming_failure",
            "target_type": "worker",
            "step": "Network.enable",
            "cause": "no_answer",
            "resume_sent": false,
            "held": true,
            "close": "refused",
            "containment": "browser_killed",
            "message": "a target not armed in time was contained before it ran",
        }),
        serde_json::json!({
            "verdict": "breach",
            "event": "arming_failure",
            "target_type": "iframe",
            "step": null,
            "cause": "malformed",
            "resume_sent": false,
            "held": true,
            "close": "no_target",
            "containment": "failed",
            "message": "a target not armed may have run unguarded",
        }),
    ];
    assert_eq!(entries.len(), expected.len());
    for (entry, expected) in entries.iter().zip(&expected) {
        let mut entry = entry.clone();
        let ts = entry.as_object_mut().unwrap().remove("ts_ms").unwrap();
        assert!(ts.as_u64().is_some_and(|ts| ts > 0));
        assert_eq!(&entry, expected);
    }
}

#[test]
fn arming_entries_share_the_log_s_failure_mode() {
    let written = Arc::new(Mutex::new(Vec::new()));
    let sink = FillsUp {
        written: Arc::clone(&written),
        budget: 30,
        failed: false,
    };
    let log = FenceLog::from_writer(sink);
    let record = held_worker(CloseOutcome::Closed, Containment::Closed);
    assert!(log.record_arming(&record).is_err(), "a half-written entry fails");
    assert!(log.record_arming(&record).is_err(), "and so does every later one");
    let request = RequestRecord {
        channel: Channel::Http,
        method: "GET",
        url: "https://example.com/",
        outcome: RequestOutcome::Unclassified,
    };
    assert!(log.record_request(&request).is_err(), "and every other kind of entry");
}

/// A `lab.toml` with MRIs for accounts a and b but not c, and for one of two service participants.
fn lab_toml(data_dir: &Path) -> String {
    let account = |alias: &str, mri: Option<&str>| {
        let mri = mri.map(|mri| format!("mri = \"{mri}\"\n")).unwrap_or_default();
        format!("[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\n{mri}\n")
    };
    format!(
        "data_dir = \"{}\"\n\n{}{}{}[[service_participants]]\nname = \"echo\"\nmri = \"{ECHO}\"\n\n\
         [[service_participants]]\nname = \"other\"\n",
        data_dir.display(),
        account("a", Some(LAB_A)),
        account("b", Some(LAB_B)),
        account("c", None),
    )
}

#[tokio::test]
async fn the_lab_fence_takes_its_roster_and_log_from_lab_toml() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    fs::create_dir(&data_dir).unwrap();
    let config = LabConfig::parse(&lab_toml(&data_dir), dir.path()).unwrap();
    let fence = Fence::open(&config).unwrap();

    let message = || Target::new(WriteKind::Message, chat());
    let allowed = fence.check(message(), &Fixed::complete(&[LAB_A, LAB_B, ECHO])).await;
    assert!(allowed.is_ok(), "the accounts and the service with an MRI are admitted");
    // account c has no MRI in lab.toml yet, so its MRI matches nobody
    let denied = fence.check(message(), &Fixed::complete(&[LAB_A, LAB_C])).await;
    assert_eq!(denied.unwrap_err().reason(), &DenyReason::Outsiders { count: 1 });

    let log = data_dir.join("fence.log");
    let verdicts: Vec<_> = log_entries(&log)
        .unwrap()
        .into_iter()
        .map(|entry| entry["verdict"].clone())
        .collect();
    assert_eq!(verdicts, ["allow", "deny"]);
    assert_eq!(fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);

    // a log that can't be opened: no fence, and the error names the file
    let missing = LabConfig::parse(&lab_toml(&dir.path().join("missing")), dir.path()).unwrap();
    let error = Fence::open(&missing).unwrap_err();
    assert!(matches!(error, OpenError::Log { .. }), "{error:?}");
    assert!(error.to_string().contains("missing/fence.log"), "{error}");
}
