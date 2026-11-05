//! Synthetic participant and thread identifier cases moved with their shared helpers.

use teams_proto::{
    Mri, is_chat_id, is_group_chat_id, is_meeting_chat_id, is_team_thread_id, one_to_one_id, one_to_one_pair,
    reply_chain, reply_chain_id, stored_conversation,
};

const GUID_A: &str = "00000000-0000-4000-8000-00000000000a";
const GUID_B: &str = "00000000-0000-4000-8000-00000000000b";
const GUID_X: &str = "00000000-0000-4000-8000-0000000000ff";
const GROUP: &str = "0000000000000000000000000000abcd";

fn mri(guid: &str) -> Mri {
    Mri::new(format!("8:orgid:{guid}"))
}

fn chat_id() -> String {
    format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces")
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
fn a_meeting_chat_id_is_19_meeting_an_id_and_thread_v2_and_never_a_group_chat_s() {
    for id in [
        "19:meeting_0123@thread.v2",
        "19:MEETING_0123@thread.v2",
        "19:Meeting_MjA4ZmVlNjYtYWJjZC00@thread.v2",
        "19:meeting_a_b-C9@thread.v2",
    ] {
        assert!(is_meeting_chat_id(id), "{id}");
        assert!(!is_group_chat_id(id), "{id}");
    }
    for id in [
        "19:meeting_@thread.v2",
        "19:meeting@thread.v2",
        "19:meeting_01 23@thread.v2",
        "19:meeting_0123@thread.tacv2",
        "19:meeting_0123@thread.v2;messageid=1",
        "19:meeting_0123%40thread.v2",
        "meeting_0123@thread.v2",
        &format!("19:{GROUP}@thread.v2"),
        &format!("19:{GUID_A}_{GUID_B}@unq.gbl.spaces"),
        &format!("19:meeting_{}@thread.v2", "a".repeat(121)),
    ] {
        assert!(!is_meeting_chat_id(id), "{id}");
    }
    assert!(is_meeting_chat_id(&format!("19:meeting_{}@thread.v2", "a".repeat(120))));
}

#[test]
fn a_group_chat_id_is_19_an_id_and_thread_v2_and_never_a_meeting_s() {
    assert!(is_group_chat_id(&format!("19:{GROUP}@thread.v2")));
    assert!(is_group_chat_id("19:a_b-C9@thread.v2"));
    for id in [
        "19:meeting_0123@thread.v2",
        "19:MEETING_0123@thread.v2",
        "19:meeting_@thread.v2",
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

#[test]
fn chat_like_ids_outside_the_recognized_forms_are_not_chat_ids() {
    for id in [
        format!("19:{GUID_A}_{GUID_A}@unq.gbl.spaces"),
        "19:alice_bob@unq.gbl.spaces".to_owned(),
        "19:group.chat@thread.v2".to_owned(),
        "19:groupchat=@thread.v2".to_owned(),
        format!("19:{}@thread.v2", "a".repeat(129)),
        "19:meeting_@thread.v2".to_owned(),
    ] {
        assert!(!is_chat_id(&id), "{id}");
    }
    for id in [
        chat_id(),
        "19:group@thread.v2".to_owned(),
        "19:meeting_abc@thread.v2".to_owned(),
    ] {
        assert!(is_chat_id(&id), "{id}");
    }
}

#[test]
fn mris_keep_exact_identity_with_mail_normalization_explicit() {
    let value = mri(GUID_A);
    assert_eq!(value, Mri::new(format!("8:orgid:{GUID_A}")));
    assert_ne!(value, Mri::new(format!("8:ORGID:{GUID_A}")));
    assert_ne!(value, Mri::new(format!("8:orgid:{GUID_A} ")));
    assert_eq!(value.to_string(), value.as_str());
    assert_eq!(Mri::mail(" A@Example.Test ").as_str(), "smtp:a@example.test");
    assert_eq!(Mri::mail(" A@Example.Test ").mail_address(), Some("a@example.test"));
    assert_eq!(value.mail_address(), None);
    assert!(Mri::new("28:bot").is_bot());
    assert!(!value.is_bot());
}

#[test]
fn a_reply_chain_joins_a_channel_and_its_post_and_reads_back() {
    let chain = reply_chain_id("19:abc@thread.tacv2", "1760000000000");
    assert_eq!(chain.as_deref(), Some("19:abc@thread.tacv2;messageid=1760000000000"));
    let chain = chain.unwrap_or_default();
    assert_eq!(reply_chain(&chain), Some(("19:abc@thread.tacv2", "1760000000000")));
    assert_eq!(stored_conversation(&chain), "19:abc@thread.tacv2");
    assert_eq!(stored_conversation("19:abc@thread.tacv2"), "19:abc@thread.tacv2");
    assert_eq!(stored_conversation(GROUP), GROUP);
    for (channel, post) in [
        ("19:abc@thread.tacv2", ""),
        ("19:abc@thread.tacv2", "12a"),
        (GROUP, "1"),
        ("19:abc@thread.tacv2;messageid=1", "2"),
    ] {
        assert_eq!(reply_chain_id(channel, post), None, "{channel} {post}");
    }
    for id in [
        "19:abc@thread.tacv2",
        "19:abc@thread.tacv2;messageid=",
        "19:abc@thread.v2;messageid=1",
    ] {
        assert_eq!(reply_chain(id), None, "{id}");
    }
}
