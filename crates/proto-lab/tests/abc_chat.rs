//! The `abc-chat` scenario's offline parts: its deep links, how it finds the group chat a first message went to, its
//! forced-deny labels and how it judges denies. Every id here is made up.

use std::fmt::Write as _;

use clap::ValueEnum as _;
use proto_lab::config::LabPaths;
use proto_lab::fence_http::{Denial, DenyCause, ForcedDeny, PausedRequest};
use proto_lab::scenario::ab_chat::{
    self, EARLIER_RUN_PROBE, ForcedPost, PROBE_MARKER, THIS_RUN_PROBE, one_to_one_id, probe_rules, probe_text,
};
use proto_lab::scenario::abc_chat::{
    ANY_GROUP_CHAT, EXPECTED_FORCED, FIRST_MESSAGE, THIS_RUN_FIRST_MESSAGE, chat_link_by_id, first_message_text,
    forced_denies, group_chat_of_message_post, messaging_blocked, new_chat_link, policy, record_creation,
};
use proto_lab::scenario::{DenyPolicy, ScenarioError, ScenarioName, ScenarioReport};
use proto_lab::scenario::{ab_image, ab_trouter, chat_shapes, echo_call};
use regex::Regex;
use teams_fence::LiveMembershipSource;
use teams_fence::lab::{LabChats, LabConfig, record_abc_chat};

const GROUP: &str = "19:0123456789abcdef0123456789abcdef@thread.v2";
const GROUP_ENCODED: &str = "19%3A0123456789abcdef0123456789abcdef%40thread.v2";
const BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer/v1";
const MRI_A: &str = "8:orgid:0000000a-0000-4000-8000-00000000000a";
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

#[test]
fn the_first_message_is_a_probe_to_every_scenario_that_denies_an_earlier_run_s_probe() {
    let text = first_message_text("nonce-1");
    assert!(text.starts_with(PROBE_MARKER), "{text}");
    assert!(text.ends_with("nonce-1."));
    // ended, so a nonce that begins with another doesn't hold the other's text
    assert!(!first_message_text("nonce-12").contains(&text));
    // this run's probe and first message first, then any first message, then any earlier probe: a deny names the first
    // rule matched
    assert_eq!(
        EXPECTED_FORCED,
        [THIS_RUN_PROBE, THIS_RUN_FIRST_MESSAGE, FIRST_MESSAGE, EARLIER_RUN_PROBE]
    );
}

/// A message post of `text` to the group chat, as `Fetch.requestPaused` would show it.
fn message_post_of(text: &str) -> PausedRequest {
    PausedRequest {
        request_id: "interception-1".to_owned(),
        method: "POST".to_owned(),
        url: format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/messages"),
        headers: Vec::new(),
        body: Some(format!(r#"{{"content":"<p>{text}</p>","messagetype":"RichText/Html"}}"#).into_bytes()),
        body_unread: false,
        network_id: Some("network-1".to_owned()),
    }
}

#[test]
fn only_this_run_s_texts_are_denied_under_this_run_s_labels() {
    let rules = forced_denies("nonce-2");
    // the label of the first rule that matches, as the fence names a forced deny
    let label = |request: &PausedRequest| {
        rules
            .iter()
            .find(|rule| rule.matches(request))
            .and_then(|rule| rule.label().map(str::to_owned))
    };
    let cases = [
        (probe_text("nonce-2"), THIS_RUN_PROBE),
        (first_message_text("nonce-2"), THIS_RUN_FIRST_MESSAGE),
        // an earlier creation run's first message, sent again on load: not this run's, even when its nonce begins with
        // this one
        (first_message_text("nonce-1"), FIRST_MESSAGE),
        (first_message_text("nonce-23"), FIRST_MESSAGE),
        (probe_text("nonce-1"), EARLIER_RUN_PROBE),
    ];
    for (text, expected) in cases {
        assert_eq!(label(&message_post_of(&text)).as_deref(), Some(expected), "{text}");
        let put = PausedRequest {
            method: "PUT".to_owned(),
            ..message_post_of(&text)
        };
        assert_eq!(label(&put).as_deref(), Some(expected), "PUT {text}");
    }
    assert_eq!(label(&message_post_of("lab message nonce-2")), None);
}

#[test]
fn the_creation_run_waits_for_this_run_s_first_message_only() -> Result<(), regex::Error> {
    let any_group = Regex::new(ANY_GROUP_CHAT)?;
    let this_run = ForcedPost::new(THIS_RUN_FIRST_MESSAGE, &any_group)?;
    let message_post = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/messages");
    let forced = |label: &str| Denial {
        label: Some(label.to_owned()),
        ..Denial::new("POST", &message_post, DenyCause::Forced)
    };
    assert!(this_run.matches(&forced(THIS_RUN_FIRST_MESSAGE)));
    // an earlier chat's first message, sent again when a's page loaded, names that chat: never this run's
    assert!(!this_run.matches(&forced(FIRST_MESSAGE)));
    assert!(!this_run.matches(&forced(EARLIER_RUN_PROBE)));
    // not forced, or not a message post
    assert!(!this_run.matches(&Denial::new("POST", &message_post, DenyCause::Unclassified)));
    let properties = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/properties");
    assert!(!this_run.matches(&Denial {
        label: Some(THIS_RUN_FIRST_MESSAGE.to_owned()),
        ..Denial::new("PUT", &properties, DenyCause::Forced)
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
    assert!(report.failures[0].contains("fence.log"), "{}", report.failures[0]);
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

/// A `lab.toml` in `dir` whose a, b and c have made-up MRIs (`c` none when `with_c` is false).
fn lab_config(dir: &std::path::Path, with_c: bool) -> Result<LabConfig, teams_fence::lab::ConfigError> {
    let accounts = [("a", Some(MRI_A)), ("b", Some(MRI_B)), ("c", with_c.then_some(MRI_C))]
        .into_iter()
        .fold(String::new(), |mut out, (alias, mri)| {
            let mri = mri.map(|mri| format!("mri = \"{mri}\"\n")).unwrap_or_default();
            let _ = write!(
                out,
                "\n[[accounts]]\nalias = \"{alias}\"\nupn = \"bot-{alias}@lab.example.com\"\n{mri}"
            );
            out
        });
    let text = format!("data_dir = {:?}\n{accounts}", dir.display().to_string());
    LabConfig::parse(&text, dir)
}

#[test]
fn the_messaging_run_starts_only_when_the_lab_s_source_reads_the_recorded_chat_in_full() {
    // a message the fence denies stays unsent in a's client and is sent again on every later load
    let dir = tempfile::tempdir().unwrap();
    let config = lab_config(dir.path(), true).unwrap();
    let source = || {
        LiveMembershipSource::with_mock_origin("http://127.0.0.1:9")
            .unwrap()
            .with_lab(&config)
    };
    let blocked = messaging_blocked(&source(), GROUP).unwrap_or_default();
    assert!(blocked.contains("chats.toml records"), "{blocked}");
    assert!(blocked.contains("nothing was opened"), "{blocked}");

    record_abc_chat(config.paths(), GROUP).unwrap();
    assert_eq!(messaging_blocked(&source(), GROUP), None);
    // not with a source that doesn't know the lab, nor for another chat, nor in a lab without c's MRI
    let bare = LiveMembershipSource::with_mock_origin("http://127.0.0.1:9").unwrap();
    assert!(messaging_blocked(&bare, GROUP).is_some());
    assert!(messaging_blocked(&source(), "19:fedcba9876543210fedcba9876543210@thread.v2").is_some());
    let without_c = lab_config(dir.path(), false).unwrap();
    let source = LiveMembershipSource::with_mock_origin("http://127.0.0.1:9")
        .unwrap()
        .with_lab(&without_c);
    assert!(messaging_blocked(&source, GROUP).is_some());
}

#[test]
fn only_the_scenario_s_forced_denies_and_the_list_are_expected_and_nothing_on_the_group_chat()
-> Result<(), regex::Error> {
    let policy = policy(Some(GROUP))?;
    let message_post = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/messages");
    let forced = |label: &str| Denial {
        label: Some(label.to_owned()),
        ..Denial::new("POST", &message_post, DenyCause::Forced)
    };
    for label in EXPECTED_FORCED {
        assert!(policy.expects(&forced(label)), "{label}");
    }
    assert!(!policy.expects(&forced("some other rule")));
    // the message itself, denied by a partial membership read: never expected
    assert!(!policy.expects(&Denial::new("POST", &message_post, DenyCause::Unclassified)));
    let receipt = format!("{BASE}/users/ME/conversations/{GROUP_ENCODED}/properties?name=consumptionhorizon");
    assert!(!policy.expects(&Denial::new("PUT", &receipt, DenyCause::Unclassified)));
    // a listed class elsewhere is
    let telemetry = "https://eu-mobile.events.data.microsoft.com/OneCollector/1.0/";
    assert!(policy.expects(&Denial::new("POST", telemetry, DenyCause::Unclassified)));
    // without the chat's id (a creation run that didn't learn it) the forced denies are still the only ones expected
    let creation = proto_lab::scenario::abc_chat::policy(None)?;
    assert!(creation.expects(&forced(FIRST_MESSAGE)));
    assert!(!creation.expects(&forced("some other rule")));
    Ok(())
}

/// The rules `name` forces on a's fence before its page loads in a run with `nonce`, and the policy it judges a's denies
/// by; `None` for `load`, which reads no members (`FailClosed`: every write to a chat is denied, and every deny is
/// expected there).
fn forced_and_policy(name: ScenarioName, nonce: &str) -> Result<Option<(Vec<ForcedDeny>, DenyPolicy)>, regex::Error> {
    let probe = probe_text(nonce);
    let ab = one_to_one_id(MRI_A, MRI_B);
    let ab = ab.as_deref();
    Ok(Some(match name {
        ScenarioName::Load => return Ok(None),
        ScenarioName::AbChat | ScenarioName::AbChatFull => (probe_rules(Some(&probe))?, ab_chat::policy(ab)?),
        ScenarioName::AbTrouter | ScenarioName::AbTrouterQuiet => {
            (probe_rules(Some(&probe))?, ab_trouter::trouter_policy(ab)?)
        }
        ScenarioName::AbImage => (
            [probe_rules(Some(&probe))?, ab_image::media_rules()?].concat(),
            ab_image::policy(ab)?,
        ),
        ScenarioName::AbcChat => (forced_denies(nonce), policy(Some(GROUP))?),
        ScenarioName::ChatShapes => (forced_denies(nonce), chat_shapes::policy(ab, Some(GROUP))?),
        ScenarioName::EchoCall => (echo_call::forced_rules()?, echo_call::policy()),
        ScenarioName::OwnPresenceActivity
        | ScenarioName::OwnPresenceEndpoint
        | ScenarioName::OwnPresenceBoth
        | ScenarioName::OwnPresenceManualHeld
        | ScenarioName::OwnPresenceManual => {
            let report = proto_lab::scenario::own_presence::Report::of(name)
                .unwrap_or(proto_lab::scenario::own_presence::Report::Both);
            (
                probe_rules(Some(&probe))?,
                proto_lab::scenario::own_presence::policy(report, ab)?,
            )
        }
    }))
}

#[test]
fn every_scenario_that_reads_members_live_force_denies_the_first_message_sent_again() -> Result<(), regex::Error> {
    // a creation run's first message stays unsent in a's client, which sends it again on every later load of a's
    // profile; once the chat is recorded, the live read would let it out to b and c
    let resent = message_post_of(&first_message_text("nonce-1"));
    for &name in ScenarioName::value_variants() {
        let Some((rules, policy)) = forced_and_policy(name, "nonce-2")? else {
            continue;
        };
        // the first rule that matches names the deny
        let label = rules
            .iter()
            .find(|rule| rule.matches(&resent))
            .and_then(ForcedDeny::label);
        let Some(label) = label else {
            panic!("{name} doesn't force-deny the first message sent again");
        };
        assert!(
            ![THIS_RUN_PROBE, THIS_RUN_FIRST_MESSAGE].contains(&label),
            "{name}: {label}"
        );
        let denial = Denial {
            label: Some(label.to_owned()),
            ..Denial::new("POST", &resent.url, DenyCause::Forced)
        };
        assert!(policy.expects(&denial), "{name} doesn't expect {label:?}");
    }
    Ok(())
}
