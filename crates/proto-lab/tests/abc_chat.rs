//! The `abc-chat` scenario's offline parts: its deep links, how it finds the group chat a first message went to, its
//! forced-deny labels and how it judges denies. Every id here is made up.

use proto_lab::fence_http::{Denial, DenyCause};
use proto_lab::scenario::ab_chat::{EARLIER_RUN_PROBE, PROBE_MARKER, THIS_RUN_PROBE};
use proto_lab::scenario::abc_chat::{
    EXPECTED_FORCED, FIRST_MESSAGE, chat_link_by_id, first_message_text, group_chat_of_message_post, messaging_blocked,
    new_chat_link, policy,
};

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

#[test]
fn the_first_message_is_a_probe_to_every_scenario_that_denies_an_earlier_run_s_probe() {
    let text = first_message_text("nonce-1");
    assert!(text.starts_with(PROBE_MARKER), "{text}");
    assert!(text.ends_with("nonce-1"));
    // this run's probe first, then the first message, then any earlier probe: a deny names the first rule matched
    assert_eq!(EXPECTED_FORCED, [THIS_RUN_PROBE, FIRST_MESSAGE, EARLIER_RUN_PROBE]);
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
fn the_messaging_run_doesn_t_start_while_the_fence_can_t_read_a_group_chat_in_full() {
    // a message the fence denies stays unsent in a's client and is sent again on every later load
    let blocked = messaging_blocked(GROUP).unwrap_or_default();
    assert!(blocked.contains("Partial(Unsupported)"), "{blocked}");
    assert!(blocked.contains("nothing was opened"), "{blocked}");
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
