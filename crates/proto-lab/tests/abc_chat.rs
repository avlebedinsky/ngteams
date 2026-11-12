//! The `abc-chat` scenario's offline parts: its deep links, how it finds the group chat a first message went to, its
//! forced-deny labels and how it judges denies. Every id here is made up.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use proto_lab::capture_hold::{HeldRequest, PausedRequest};
use proto_lab::config::LabChats;
use proto_lab::config::LabPaths;
use proto_lab::scenario::ab_chat::HeldPost;
use proto_lab::scenario::abc_chat::{
    ANY_GROUP_CHAT, THIS_RUN_FIRST_MESSAGE, chat_link_by_id, first_message_text, group_chat_of_message_post,
    new_chat_link, record_creation,
};
use proto_lab::scenario::{ScenarioError, ScenarioName, ScenarioReport};
use regex::Regex;

const GROUP: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";
const GROUP_ENCODED: &str = "19%3A0123456789abcdef0123456789abcdef%40thread.v2";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1";

const MRI_B: &str = "8:orgid:0000000b-0000-4000-8000-00000000000b";
const MRI_C: &str = "8:orgid:0000000c-0000-4000-8000-00000000000c";

#[test]
fn the_deep_links_name_the_accounts_by_mri_or_the_chat_by_its_id() {
    assert_eq!(
        new_chat_link(&[MRI_B, MRI_C]),
        "https://teams.microsoft.com/l/chat/0/0?users=8%3Aorgid%3A0000000b-0000-4000-8000-00000000000b,\
         8%3Aorgid%3A0000000c-0000-4000-8000-00000000000c"
    );
    assert_eq!(
        chat_link_by_id(GROUP),
        format!("https://teams.microsoft.com/l/chat/{GROUP_ENCODED}/0")
    );
}

/// A message post of `text` to the group chat, as `Fetch.requestPaused` would show it.
fn message_post_of(text: &str) -> PausedRequest {
    PausedRequest {
        request_id: "interception-1".to_owned(),
        method: "POST".to_owned(),
        url: format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/messages"),
        body: Some(format!(r#"{{"content":"<p>{text}</p>","messagetype":"RichText/Html"}}"#).into_bytes()),
        network_id: Some("network-1".to_owned()),
    }
}

#[test]
fn the_creation_run_waits_for_this_run_s_first_message_only() -> Result<(), regex::Error> {
    let any_group = Regex::new(ANY_GROUP_CHAT)?;
    let this_run = HeldPost::new(THIS_RUN_FIRST_MESSAGE, &any_group)?;
    let message_post = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/messages");
    let forced = |label: &str| HeldRequest {
        label: Some(label.to_owned()),
        ..HeldRequest::new("POST", &message_post)
    };
    assert!(this_run.matches(&forced(THIS_RUN_FIRST_MESSAGE)));
    // an earlier chat's first message, sent again when a's page loaded, names that chat: never this run's
    assert!(!this_run.matches(&forced("another capture")));
    assert!(!this_run.matches(&forced("another capture")));
    // not forced, or not a message post
    assert!(!this_run.matches(&HeldRequest::new("POST", &message_post)));
    let properties = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/properties");
    assert!(!this_run.matches(&HeldRequest {
        label: Some(THIS_RUN_FIRST_MESSAGE.to_owned()),
        ..HeldRequest::new("PUT", &properties)
    }));
    Ok(())
}

#[test]
fn a_creation_run_that_failed_after_the_chat_existed_keeps_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    // the first message's deny named the chat, then the post didn't fail in the page as blocked
    let failed = Err(ScenarioError::Setup(
        "a's first message post failed with net::ERR_FAILED".to_owned(),
    ));
    let chat_id = record_creation(&paths, Some(GROUP.to_owned()), failed, &mut report);
    assert_eq!(chat_id.as_deref(), Some(GROUP));
    assert!(report.failed());
    assert!(report.failures[0].contains("ERR_FAILED"), "{:?}", report.failures);
    // so the next run messages this chat instead of creating another
    assert_eq!(LabChats::load(&paths).unwrap().abc.as_deref(), Some(GROUP));
}

#[test]
fn a_creation_run_that_never_saw_the_deny_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let failed = Err(ScenarioError::Setup("no message post denied".to_owned()));
    assert_eq!(record_creation(&paths, None, failed, &mut report), None);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(LabChats::load(&paths).unwrap().abc, None);
}

#[test]
fn a_creation_run_that_can_t_record_the_chat_fails_and_says_where_its_id_is() {
    let dir = tempfile::tempdir().unwrap();
    let paths = LabPaths::new(dir.path().to_path_buf());
    let other = "19:fedcba9876543210fedcba9876543210@thread.v2";
    std::fs::write(paths.chats_file(), format!("abc = \"{other}\"\n")).unwrap();
    let mut report = ScenarioReport::new(ScenarioName::AbcChat);
    let chat_id = record_creation(&paths, Some(GROUP.to_owned()), Ok(()), &mut report);
    assert_eq!(chat_id.as_deref(), Some(GROUP));
    assert_eq!(report.failures.len(), 1, "{:?}", report.failures);
    assert!(report.failures[0].contains("captures"), "{}", report.failures[0]);
    // the chat's id stays out of the report: it is lab data
    assert!(!report.failures[0].contains(GROUP), "{}", report.failures[0]);
    assert_eq!(LabChats::load(&paths).unwrap().abc.as_deref(), Some(other));
}

#[test]
fn the_group_chat_comes_only_from_a_message_post_to_a_group_chat() {
    let post = |chat: &str| format!("{BASE}/users/ME/conversations/{chat}/messages");
    assert_eq!(group_chat_of_message_post(&post(GROUP_ENCODED)).as_deref(), Some(GROUP));
    assert_eq!(group_chat_of_message_post(&post(GROUP)).as_deref(), Some(GROUP));
    for url in [
        // a 1:1 chat, a meeting's chat, a channel
        post("19:0000000a-0000-4000-8000-00000000000a_0000000b-0000-4000-8000-00000000000b@unq.gbl.spaces"),
        post("19:meeting_0123456789abcdef@thread.v2"),
        post("19:0123456789abcdef@thread.tacv2"),
        // not a message post
        format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/properties"),
        format!("{BASE}/threads"),
        // not the chat service
        format!("https://example.com/api/chatsvc/amer/v1/users/ME/conversations/{GROUP_ENCODED}/messages"),
        "not a url".to_owned(),
    ] {
        assert_eq!(group_chat_of_message_post(&url), None, "{url}");
    }
}

#[test]
fn creation_holds_only_this_runs_first_message() {
    let message = first_message_text("synthetic-1");
    let rules = proto_lab::scenario::abc_chat::first_message_holds("synthetic-1");
    assert!(rules.iter().any(|rule| rule.matches(&message_post_of(&message))));
    assert!(
        !rules
            .iter()
            .any(|rule| rule.matches(&message_post_of(&first_message_text("synthetic-2"))))
    );
    assert!(
        !rules
            .iter()
            .any(|rule| rule.matches(&message_post_of("ordinary message")))
    );
}
