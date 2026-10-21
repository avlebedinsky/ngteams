//! The chat reads and the message model: the Task 7/8 synthetic fixtures read into the model (versions, edits, deletes,
//! reactions, mentions, quotes, threads, members; unknown fields and kinds kept), the message HTML read into rich text,
//! and the reads sent through the native transport to the mock chat service (`teams-mock`, through the `test-support`
//! mock origin): pages, the service's own sync and backward links followed only on the same chat base, a region hop
//! not followed.
//!
//! Every id and token here is made up (the mock's, or a placeholder), never a real one.

use std::future::{self, Future};
use std::io;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_mock::{AB_CHAT, ABC_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, NOTIFICATIONS, TENANT};
use teams_proto::chat::{
    ChatReadError, ChatReads, Conversation, ConversationKind, ConversationList, Field, MAX_CONTENT, MAX_DEPTH, Marks,
    MemberList, MentionKind, Message, MessagePage, MessageType, Node, Quote, RichText, Thread, link_endpoint,
    parse_html,
};
use teams_proto::{Client, Credentials, HttpError, HttpTransport};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/chatsvc");

fn oid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// A fixture with its placeholders filled in with the mock's made-up ids.
fn fixture(name: &str) -> TestResult<Value> {
    let text = std::fs::read_to_string(Path::new(FIXTURES).join(name))?;
    let group = ABC_CHAT.trim_start_matches("19:").trim_end_matches("@thread.v2");
    let text = text
        .replace("<region>", "amer")
        .replace("<guid:a>", oid(LAB_A))
        .replace("<guid:b>", oid(LAB_B))
        .replace("<guid:c>", oid(LAB_C))
        .replace("<guid:tenant>", TENANT)
        .replace("<guid:group>", "00000000-0000-4000-8000-000000000002")
        .replace("<guid:context>", "00000000-0000-4000-8000-000000000003")
        .replace("<group>", group)
        .replace("<token:1>", "fixture-token-1")
        .replace("<token:2>", "fixture-token-2");
    Ok(serde_json::from_str(&text)?)
}

fn body(value: &Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

fn message(value: &Value) -> TestResult<Message> {
    Ok(serde_json::from_value(value.clone())?)
}

fn text(text: &str) -> Node {
    Node::Text {
        text: text.to_owned(),
        marks: Marks::NONE,
    }
}

// ---- the fixtures read into the model ----

fn conversation_list(value: &Value) -> TestResult<ConversationList> {
    Ok(ConversationList::from_body(&body(value))?)
}

fn page_of(value: &Value) -> TestResult<MessagePage> {
    Ok(MessagePage::from_body(&body(value))?)
}

#[test]
fn the_conversation_list_reads_each_kind_and_marks_channels_read_only() -> TestResult {
    let fixture = fixture("conversations.json")?;
    let list = conversation_list(&fixture["response"]["body"])?;
    assert!(list.unreadable.is_empty());
    let kinds: Vec<ConversationKind> = list.conversations.iter().map(Conversation::kind).collect();
    assert_eq!(
        kinds,
        [
            ConversationKind::OneToOne,
            ConversationKind::OneToOne,
            ConversationKind::Channel,
            ConversationKind::Notifications
        ]
    );
    let read_only: Vec<bool> = list.conversations.iter().map(Conversation::is_read_only).collect();
    assert_eq!(read_only, [false, false, true, true]);
    assert_eq!(list.total_count(), Field::Known(4));
    assert!(
        list.sync_state()
            .is_some_and(|link| link.contains("syncState=fixture-token-1"))
    );
    assert_eq!(list.forward_link(), None, "an empty forwardLink is no link");
    assert_eq!(
        list.conversations[2].thread_property("spaceThreadTopic"),
        Some("A made-up team")
    );
    Ok(())
}

#[test]
fn a_conversation_entry_reads_its_horizon_and_last_message() -> TestResult {
    let list = conversation_list(&fixture("conversations.json")?["response"]["body"])?;
    let ab = &list.conversations[0];
    assert_eq!(ab.version, Field::Known(1_760_000_300_100));
    let horizon = ab.consumption_horizon().ok_or("no horizon")?;
    assert_eq!(horizon.last_read_arrival, Some(1_760_000_300_000));
    assert_eq!(horizon.updated, Some(1_760_000_300_100));
    assert_eq!(horizon.client_message_id.as_deref(), Some("3333333333333333333"));
    let newest = ab.last_message().ok_or("no last message")?;
    assert_eq!(newest.id, "1760000300000");
    assert_eq!(newest.sender(), Some(LAB_A));
    assert!(ab.extra.contains_key("targetLink"), "an unnamed field is kept");
    assert_eq!(ab.member_properties["role"], "Admin");

    let empty = &list.conversations[1];
    assert_eq!(empty.last_message(), None, "an empty chat's lastMessage is {{}}");
    assert_eq!(empty.consumption_horizon(), None);
    Ok(())
}

#[test]
fn a_conversations_roster_version_is_absent_known_or_kept_as_it_came() -> TestResult {
    let roster = |properties: Value| -> TestResult<Field<u64>> {
        let listed: Conversation = serde_json::from_value(json!({"id": ABC_CHAT, "threadProperties": properties}))?;
        Ok(listed.roster_version())
    };
    assert_eq!(roster(json!({}))?, Field::Absent);
    assert_eq!(
        roster(json!({"rosterVersion": "1760000000000"}))?,
        Field::Known(1_760_000_000_000)
    );
    assert_eq!(roster(json!({"rosterVersion": 7}))?, Field::Known(7));
    assert_eq!(roster(json!({"rosterVersion": "r1"}))?, Field::Unknown(json!("r1")));
    assert_eq!(roster(json!({"rosterVersion": null}))?, Field::Unknown(Value::Null));
    Ok(())
}

#[test]
fn a_conversation_kind_this_build_does_not_name_is_kept_and_read_only() {
    let kind = ConversationKind::of("19:meeting_abc@thread.v2", Some("chat"));
    assert_eq!(kind, ConversationKind::Unknown("chat".to_owned()));
    assert!(kind.is_read_only());
    assert_eq!(
        ConversationKind::of(ABC_CHAT, Some("meeting")),
        ConversationKind::Unknown("meeting".to_owned())
    );
    assert_eq!(
        ConversationKind::of(AB_CHAT, None),
        ConversationKind::Unknown(String::new())
    );
    assert_eq!(ConversationKind::of(ABC_CHAT, Some("chat")), ConversationKind::Group);
    assert!(!ConversationKind::Group.is_read_only());
}

#[test]
fn a_sync_page_reads_its_links_and_a_deleted_message() -> TestResult {
    let fixture = fixture("messages.json")?;
    let page: MessagePage = page_of(&fixture["sync"]["response"]["body"])?;
    assert!(page.unreadable.is_empty());
    assert_eq!(page.messages.len(), 3);
    assert!(
        page.sync_state()
            .is_some_and(|link| link.contains("syncState=fixture-token-2"))
    );
    assert_eq!(page.last_complete_segment_end(), Field::Known(1_760_000_068_000));
    assert_eq!(page.backward_link(), None);
    assert_eq!(page.extra["tenantId"], TENANT);

    let deleted = &page.messages[0];
    assert!(deleted.is_deleted());
    assert_eq!(deleted.delete_time(), Field::Known(1_760_000_068_000));
    assert_eq!(deleted.skype_edited_id.as_ref(), deleted.client_message_id.as_known());
    assert_eq!(deleted.version, Field::Known(1_760_000_068_000));
    assert_eq!(deleted.rich_text(), Some(RichText::default()));
    Ok(())
}

fn sync_page() -> TestResult<MessagePage> {
    page_of(&fixture("messages.json")?["sync"]["response"]["body"])
}

#[test]
fn a_quoted_reply_reads_its_quote_from_content_and_properties() -> TestResult {
    let page = sync_page()?;
    let reply = &page.messages[1];
    let quotes = reply.quotes().known().ok_or("qtdMsgs as read back")?;
    assert_eq!(quotes.len(), 1);
    assert_eq!(quotes[0].message_id, "1760000010000", "a number as read back");
    assert_eq!(quotes[0].sender.as_deref(), Some(LAB_A));
    assert_eq!(quotes[0].validation_result.as_deref(), Some("Valid"));
    let rich = reply.rich_text().ok_or("no rich text")?;
    let quote = Quote {
        message_id: Some("1760000010000".to_owned()),
        sender: Some(LAB_A.to_owned()),
        sender_name: Some("Lab A".to_owned()),
        time: Some("1760000010000".to_owned()),
        body: vec![Node::Paragraph(vec![text("a made-up message, edited")])],
    };
    assert_eq!(
        rich.nodes,
        [
            Node::Quote(quote),
            text("\n"),
            Node::Paragraph(vec![text("a made-up reply")])
        ]
    );
    assert_eq!(reply.mentions(), Field::Known(Vec::new()));
    assert_eq!(reply.files(), Field::Known(Vec::new()));
    assert_eq!(reply.ams_references, Some(Vec::new()));
    Ok(())
}

#[test]
fn an_edited_message_reads_its_edit_time_and_taken_back_reaction() -> TestResult {
    let page = sync_page()?;
    let edited = &page.messages[2];
    assert_eq!(edited.edit_time(), Field::Known(1_760_000_020_000));
    assert!(!edited.is_deleted());
    let reactions = edited.reactions().known().ok_or("emotions")?;
    assert_eq!(reactions.len(), 1);
    assert_eq!((reactions[0].key.as_str(), reactions[0].users.len()), ("like", 0));
    assert_eq!(edited.annotations_summary, Some(json!({})));
    assert_eq!(edited.message_type, MessageType::RichTextHtml);
    assert_eq!(edited.sequence_id, Field::Known(3));
    assert_eq!(edited.compose_time_ms(), Some(1_767_225_610_000));
    assert_eq!(edited.arrival_time_ms(), Some(1_767_225_610_000));
    assert!(edited.extra.contains_key("conversationLink"));
    assert_eq!(
        edited.properties["languageStamp"],
        "languages=en:100;length:30;&detector=Bling"
    );
    Ok(())
}

#[test]
fn the_page_around_a_time_reads_the_chats_first_event() -> TestResult {
    let fixture = fixture("messages.json")?;
    let page = page_of(&fixture["around"]["response"]["body"])?;
    assert_eq!(page.last_complete_segment_start(), Field::Known(0));
    assert_eq!(page.sync_state(), None);
    let event = &page.messages[1];
    assert_eq!(event.message_type, MessageType::AddMember);
    assert_eq!(event.client_message_id, Field::Absent);
    assert_eq!(event.sender(), Some(AB_CHAT), "a thread's event comes from the thread");
    assert_eq!(event.rich_text(), None);
    let activity = event.thread_activity().ok_or("no activity")?;
    assert_eq!(activity.kind, "addmember");
    assert_eq!(activity.initiator.as_deref(), Some(LAB_A));
    assert_eq!(activity.targets, [LAB_A, LAB_B]);
    assert_eq!(activity.roster_version, Field::Known(1_760_000_000_000));
    assert_eq!(activity.event_time, Field::Known(1_760_000_000_399));
    assert!(event.extra.contains_key("origincontextid"));
    Ok(())
}

#[test]
fn edits_deletes_and_reactions_read_back_as_their_fixtures_recorded() -> TestResult {
    let edited = message(&fixture("edit.json")?["read_back"])?;
    assert_eq!(edited.edit_time(), Field::Known(1_760_000_020_900));
    assert_eq!(edited.version, Field::Known(1_760_000_020_900));
    assert_eq!(edited.skype_edited_id, None);
    assert_eq!(
        edited.rich_text().map(|rich| rich.plain_text()),
        Some("a made-up message, edited\n".to_owned())
    );

    let deleted = message(&fixture("delete.json")?["read_back"])?;
    assert!(deleted.is_deleted());
    assert_eq!(deleted.content, "");
    assert_eq!(deleted.ams_references, None);

    let reaction = fixture("reaction.json")?;
    let kept = message(&reaction["read_back_kept"])?;
    let reactions = kept.reactions().known().ok_or("emotions")?;
    let user = &reactions[0].users[0];
    assert_eq!(
        (user.mri.as_str(), &user.time, user.value.as_known().map(String::as_str)),
        (LAB_A, &Field::Known(1_760_000_055_100), Some("1760000055000"))
    );
    assert_eq!(kept.annotations_summary, Some(json!({"emotions": {"like": 1}})));
    let taken_back = message(&reaction["read_back_taken_back"])?;
    assert_eq!(taken_back.reactions().known().map(|list| list[0].users.len()), Some(0));
    Ok(())
}

#[test]
fn a_mention_reads_in_the_bundles_form_from_content_and_properties() -> TestResult {
    let sent = message(&fixture("mention.json")?["request"]["body"])?;
    let mentions = sent.mentions().known().ok_or("mentions as a JSON string")?;
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0].item_id, "0");
    assert_eq!(mentions[0].mri.as_deref(), Some(LAB_B));
    assert_eq!(mentions[0].kind, MentionKind::Person);
    assert_eq!(mentions[0].display_name.as_deref(), Some("Lab B"));
    assert_eq!(mentions[0].extra["@type"], "http://schema.skype.com/Mention");
    let rich = sent.rich_text().ok_or("no rich text")?;
    assert_eq!(
        rich.nodes,
        [Node::Paragraph(vec![
            Node::Mention {
                item_id: "0".to_owned(),
                text: "Lab B".to_owned()
            },
            text(" a made-up mention"),
        ])]
    );
    Ok(())
}

#[test]
fn a_quote_as_sent_names_its_message_as_a_string() -> TestResult {
    let sent = message(&fixture("reply.json")?["request"]["body"])?;
    let quotes = sent.quotes().known().ok_or("qtdMsgs as a JSON string")?;
    assert_eq!(
        (quotes[0].message_id.as_str(), &quotes[0].time),
        ("1760000010000", &Field::Known(1_760_000_010_000))
    );
    Ok(())
}

#[test]
fn the_group_thread_read_and_member_list_read_their_members() -> TestResult {
    let thread: Thread = serde_json::from_value(fixture("thread-group.json")?["response"]["body"].clone())?;
    assert_eq!(thread.kind(), ConversationKind::Group);
    let members: Vec<&str> = thread.members.iter().map(|member| member.id.as_str()).collect();
    assert_eq!(members, [LAB_A, LAB_B, LAB_C]);
    assert_eq!(thread.members[0].role.as_deref(), Some("Admin"));
    assert_eq!(thread.roster_version, Field::Known(1_760_000_000_000));
    assert!(thread.extra.contains_key("eTag"));
    let list: MemberList = serde_json::from_value(fixture("thread-group-members.json")?["response"]["body"].clone())?;
    assert_eq!(
        (list.members.len(), &list.total_member_count, &list.roster_version),
        (3, &Field::Known(3), &Field::Known(0))
    );
    Ok(())
}

#[test]
fn what_this_build_does_not_know_is_kept_never_a_parse_failure() -> TestResult {
    let odd = message(&json!({
        "id": 17,
        "version": "x1",
        "messagetype": "Event/Call",
        "content": "<partlist/>",
        "newField": {"nested": true},
        "properties": {
            "mentions": "{\"not\": \"a list\"}",
            "emotions": "like",
            "edittime": null,
            "qtdMsgs": [{"sender": "no id"}],
            "files": 3
        }
    }))?;
    assert_eq!(odd.id, "17");
    assert_eq!(odd.version, Field::Unknown(json!("x1")), "kept, not dropped");
    assert_eq!(odd.message_type, MessageType::Unknown("Event/Call".to_owned()));
    assert_eq!(odd.message_type.as_str(), "Event/Call");
    assert_eq!(odd.rich_text(), None);
    assert_eq!(odd.thread_activity(), None);
    assert_eq!(odd.extra["newField"], json!({"nested": true}));
    assert_eq!(odd.mentions(), Field::Unknown(json!("{\"not\": \"a list\"}")));
    assert_eq!(odd.reactions(), Field::Unknown(json!("like")));
    assert_eq!(odd.edit_time(), Field::Unknown(Value::Null));
    assert_eq!(odd.quotes(), Field::Unknown(json!([{"sender": "no id"}])));
    assert_eq!(odd.files(), Field::Unknown(json!(3)));
    Ok(())
}

#[test]
fn a_messages_named_numbers_and_ids_are_absent_known_or_kept_as_they_came() -> TestResult {
    let absent = message(&json!({"id": "1"}))?;
    assert_eq!(
        (&absent.sequence_id, &absent.version, &absent.client_message_id),
        (&Field::Absent, &Field::Absent, &Field::Absent)
    );
    let known = message(&json!({"id": "1", "sequenceId": 4, "version": "1760000000000", "clientmessageid": 42}))?;
    assert_eq!(
        (&known.sequence_id, &known.version, &known.client_message_id),
        (
            &Field::Known(4),
            &Field::Known(1_760_000_000_000),
            &Field::Known("42".to_owned())
        )
    );
    let odd = json!({"id": "1", "sequenceId": -1, "version": 1.5, "clientmessageid": {}});
    let unknown = message(&odd)?;
    assert_eq!(
        (&unknown.sequence_id, &unknown.version, &unknown.client_message_id),
        (
            &Field::Unknown(json!(-1)),
            &Field::Unknown(json!(1.5)),
            &Field::Unknown(json!({}))
        )
    );
    let nulls = message(&json!({"id": "1", "version": null, "clientmessageid": null}))?;
    assert_eq!(
        (&nulls.version, &nulls.client_message_id),
        (&Field::Unknown(Value::Null), &Field::Unknown(Value::Null)),
        "null is no form a capture showed"
    );

    let page = page_of(&json!({"messages": [odd]}))?;
    assert_eq!(
        (page.messages.len(), page.unreadable.len()),
        (1, 0),
        "read, not unreadable"
    );
    assert_eq!(page.messages[0].version, Field::Unknown(json!(1.5)));

    let back = serde_json::to_value(&unknown)?;
    assert_eq!(
        (&back["sequenceId"], &back["version"], &back["clientmessageid"]),
        (&json!(-1), &json!(1.5), &json!({})),
        "serialized as it came"
    );
    let back = serde_json::to_value(&absent)?;
    assert!(back.get("version").is_none() && back.get("clientmessageid").is_none());
    Ok(())
}

#[test]
fn a_conversations_named_numbers_are_absent_known_or_kept_as_they_came() -> TestResult {
    let list = conversation_list(&json!({"conversations": [
        {"id": AB_CHAT},
        {"id": AB_CHAT, "version": 7, "lastUpdatedMessageId": "1760000000000", "lastUpdatedMessageVersion": 0},
        {"id": AB_CHAT, "version": null, "lastUpdatedMessageId": "17e3", "lastUpdatedMessageVersion": [1]}
    ]}))?;
    assert!(list.unreadable.is_empty());
    let fields = |conversation: &Conversation| {
        (
            conversation.version.clone(),
            conversation.last_updated_message_id.clone(),
            conversation.last_updated_message_version.clone(),
        )
    };
    assert_eq!(
        fields(&list.conversations[0]),
        (Field::Absent, Field::Absent, Field::Absent)
    );
    assert_eq!(
        fields(&list.conversations[1]),
        (Field::Known(7), Field::Known(1_760_000_000_000), Field::Known(0))
    );
    assert_eq!(
        fields(&list.conversations[2]),
        (
            Field::Unknown(Value::Null),
            Field::Unknown(json!("17e3")),
            Field::Unknown(json!([1]))
        )
    );
    Ok(())
}

#[test]
fn reaction_users_and_quotes_keep_a_time_or_value_of_another_form() -> TestResult {
    let read = message(&json!({"id": "1", "properties": {
        "emotions": [{"key": "like", "users": [
            {"mri": "8:orgid:x"},
            {"mri": "8:orgid:y", "time": "5", "value": 6},
            {"mri": "8:orgid:z", "time": "soon", "value": true}
        ]}],
        "qtdMsgs": [{"messageId": "1"}, {"messageId": "2", "time": 3}, {"messageId": "3", "time": -3}]
    }}))?;
    let reactions = read.reactions().known().ok_or("emotions")?;
    let users: Vec<_> = reactions[0]
        .users
        .iter()
        .map(|user| (user.time.clone(), user.value.clone()))
        .collect();
    assert_eq!(
        users,
        [
            (Field::Absent, Field::Absent),
            (Field::Known(5), Field::Known("6".to_owned())),
            (Field::Unknown(json!("soon")), Field::Unknown(json!(true)))
        ]
    );
    let quotes = read.quotes().known().ok_or("qtdMsgs")?;
    let times: Vec<_> = quotes.iter().map(|quote| quote.time.clone()).collect();
    assert_eq!(times, [Field::Absent, Field::Known(3), Field::Unknown(json!(-3))]);
    Ok(())
}

#[test]
fn a_threads_and_member_lists_named_numbers_are_absent_known_or_kept_as_they_came() -> TestResult {
    let absent: Thread = serde_json::from_value(json!({"id": ABC_CHAT}))?;
    assert_eq!(
        (&absent.roster_version, &absent.version),
        (&Field::Absent, &Field::Absent)
    );
    let read: Thread = serde_json::from_value(json!({"id": ABC_CHAT, "rosterVersion": "r1", "version": "7"}))?;
    assert_eq!(
        (&read.roster_version, &read.version),
        (&Field::Unknown(json!("r1")), &Field::Known(7))
    );
    let list: MemberList = serde_json::from_value(json!({"totalMemberCount": [3], "rosterVersion": 2}))?;
    assert_eq!(
        (&list.total_member_count, &list.roster_version),
        (&Field::Unknown(json!([3])), &Field::Known(2))
    );
    let empty: MemberList = serde_json::from_value(json!({}))?;
    assert_eq!(
        (&empty.total_member_count, &empty.roster_version),
        (&Field::Absent, &Field::Absent)
    );
    Ok(())
}

#[test]
fn a_thread_events_numbers_and_a_messages_edit_and_delete_times_keep_another_form() -> TestResult {
    let event = message(&json!({
        "id": "1",
        "messagetype": "ThreadActivity/AddMember",
        "content": "<addmember><eventtime>soon</eventtime><rosterVersion>+5</rosterVersion></addmember>"
    }))?;
    let activity = event.thread_activity().ok_or("no activity")?;
    assert_eq!(
        (&activity.event_time, &activity.roster_version),
        (&Field::Unknown(json!("soon")), &Field::Unknown(json!("+5")))
    );
    let bare = message(&json!({"id": "1", "messagetype": "ThreadActivity/AddMember", "content": "<addmember/>"}))?;
    let activity = bare.thread_activity().ok_or("no activity")?;
    assert_eq!(
        (&activity.event_time, &activity.roster_version),
        (&Field::Absent, &Field::Absent)
    );

    let odd = message(&json!({"id": "1", "properties": {"deletetime": "yesterday", "edittime": "5"}}))?;
    assert_eq!(odd.delete_time(), Field::Unknown(json!("yesterday")));
    assert!(
        !odd.is_deleted(),
        "a deletetime no capture showed isn't read as a delete"
    );
    assert_eq!(odd.edit_time(), Field::Known(5));
    let plain = message(&json!({"id": "1"}))?;
    assert_eq!((plain.edit_time(), plain.delete_time()), (Field::Absent, Field::Absent));
    Ok(())
}

#[test]
fn a_mention_kind_or_message_type_this_build_does_not_name_is_kept() -> TestResult {
    let unknown_kind = message(&json!({
        "id": "1",
        "properties": {"mentions": [{"itemid": 0, "mri": "8:orgid:x", "mentionType": "nearby"}]}
    }))?;
    assert_eq!(unknown_kind.message_type, MessageType::Unknown(String::new()));
    let mentions = unknown_kind.mentions().known().ok_or("a list of mentions")?;
    assert_eq!(mentions[0].kind, MentionKind::Unknown("nearby".to_owned()));
    assert_eq!(mentions[0].item_id, "0");
    Ok(())
}

#[test]
fn a_segment_time_in_a_form_no_capture_showed_is_unknown() -> TestResult {
    let page = page_of(&json!({"messages": [], "_metadata": {"lastCompleteSegmentEndTime": null}}))?;
    assert_eq!(page.last_complete_segment_end(), Field::Unknown(Value::Null));
    assert_eq!(page.last_complete_segment_start(), Field::Absent);
    Ok(())
}

// ---- rich text ----

#[test]
fn no_script_style_or_event_attribute_survives() {
    let rich = parse_html(concat!(
        "<p onclick=\"alert(1)\" style=\"color:red\">hi<script>alert(2)</script><style>p{}</style></p>",
        "<a href=\"javascript:alert(3)\" onmouseover=\"alert(4)\">click</a>",
        "<img src=\"data:image/png;base64,AAAA\" alt=\"inline\" onerror=\"alert(5)\">",
        "<img src=\"javascript:alert(6)\">",
        "<iframe src=\"https://example.test/\">framed</iframe><template><b>inert</b></template>",
    ));
    assert_eq!(rich.nodes, [Node::Paragraph(vec![text("hi")]), text("clickinline")]);
    let debug = format!("{rich:?}");
    for active in ["alert", "onclick", "color", "javascript", "framed", "inert", "data:"] {
        assert!(!debug.contains(active), "{active} survived: {debug}");
    }
}

#[test]
fn unknown_markup_is_kept_as_text_and_known_markup_typed() {
    let rich = parse_html(concat!(
        "<div><marquee>moving <b>bold <i>both</i></b></marquee><br><code>x</code></div>",
        "<a href=\"https://example.test/a\"><u>link</u></a>",
        "<img src=\"https://example.test/i.png\" alt=\"pic\" itemtype=\"http://schema.skype.com/AMSImage\">",
        "<blockquote><p>plain quote</p></blockquote>",
        "<!-- a comment -->",
    ));
    let bold_italic = Marks::BOLD.with(Marks::ITALIC);
    assert!(bold_italic.contains(Marks::ITALIC) && !bold_italic.contains(Marks::CODE));
    assert_eq!(
        rich.nodes,
        [
            Node::Paragraph(vec![
                text("moving "),
                Node::Text {
                    text: "bold ".to_owned(),
                    marks: Marks::BOLD
                },
                Node::Text {
                    text: "both".to_owned(),
                    marks: bold_italic
                },
                Node::Break,
                Node::Text {
                    text: "x".to_owned(),
                    marks: Marks::CODE
                },
            ]),
            Node::Link {
                href: "https://example.test/a".to_owned(),
                children: vec![Node::Text {
                    text: "link".to_owned(),
                    marks: Marks::UNDERLINE
                }],
            },
            Node::Image(teams_proto::chat::Image {
                src: "https://example.test/i.png".to_owned(),
                alt: Some("pic".to_owned()),
                item_type: Some("http://schema.skype.com/AMSImage".to_owned()),
            }),
            Node::Quote(Quote {
                body: vec![Node::Paragraph(vec![text("plain quote")])],
                ..Quote::default()
            }),
        ]
    );
    assert_eq!(rich.plain_text(), "moving bold both\nx\nlinkpic\nplain quote\n");
}

#[test]
fn deep_nesting_is_bounded_and_keeps_its_text() {
    // `div`s and `blockquote`s nest as written (a nested `a` or `p` would close the one before it)
    let depth = 1_000;
    let content = format!(
        "{}deep{}",
        "<div><blockquote><span>".repeat(depth),
        "</span></blockquote></div>".repeat(depth)
    );
    let rich = parse_html(&content);
    assert!(rich.plain_text().contains("deep"));
    let mut deepest = 0;
    let mut level: Vec<(&Node, usize)> = rich.nodes.iter().map(|node| (node, 1)).collect();
    while let Some((node, at)) = level.pop() {
        deepest = deepest.max(at);
        let children = match node {
            Node::Paragraph(children) | Node::Link { children, .. } => children.as_slice(),
            Node::Quote(quote) => quote.body.as_slice(),
            _ => &[],
        };
        level.extend(children.iter().map(|child| (child, at + 1)));
    }
    assert!(deepest <= MAX_DEPTH + 1, "nested {deepest} deep");
}

#[test]
fn content_over_the_cap_is_read_up_to_it() {
    // two bytes a character, so the cap falls inside one unless the cut moves to a boundary
    let long = format!("<p>a{}</p>", "é".repeat(MAX_CONTENT / 2));
    let rich = parse_html(&long);
    assert!(rich.truncated);
    let shown = rich.plain_text();
    assert!(shown.starts_with("aé") && shown.len() < MAX_CONTENT, "{}", shown.len());
    assert!(!parse_html("<p>short</p>").truncated);
}

#[test]
fn a_mention_elsewhere_than_a_span_is_text() {
    let rich = parse_html("<b itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab B</b>");
    assert_eq!(
        rich.nodes,
        [Node::Text {
            text: "Lab B".to_owned(),
            marks: Marks::BOLD
        }]
    );
}

// ---- reads through the native transport, against the mock ----

/// The account's token as the transport asks for it.
struct MockToken(String);

impl Credentials for MockToken {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

fn client_for(origin: &str, token: &str) -> TestResult<Client<HttpTransport<MockToken>>> {
    let fence = Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(io::sink()),
    );
    let transport = HttpTransport::with_mock_origin(
        Arc::new(MockToken(token.to_owned())),
        Arc::new(fence),
        CHAT_BASE,
        origin,
    )?;
    Ok(Client::new(transport))
}

fn lab_client(mock: &ChatServiceMock, who: &str) -> TestResult<Client<HttpTransport<MockToken>>> {
    client_for(&mock.origin(), &mock.token(who).ok_or("no such account")?)
}

#[tokio::test]
async fn the_conversation_list_and_its_sync_link_through_the_transport() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let list = reads.conversations().await?;
    let kind = |id: &str| {
        list.conversations
            .iter()
            .find(|conversation| conversation.id == id)
            .map(Conversation::kind)
    };
    assert_eq!(list.conversations.len(), 6);
    assert_eq!(kind(AB_CHAT), Some(ConversationKind::OneToOne));
    assert_eq!(kind(ABC_CHAT), Some(ConversationKind::Group));
    assert_eq!(kind(CHANNEL), Some(ConversationKind::Channel));
    assert_eq!(kind(NOTIFICATIONS), Some(ConversationKind::Notifications));
    let sync = list.sync_state().ok_or("no sync link")?;
    let since = reads.conversations_from(sync).await?;
    assert!(since.conversations.is_empty(), "nothing changed since");
    assert!(since.sync_state().is_some());
    Ok(())
}

#[tokio::test]
async fn message_pages_follow_the_services_backward_and_sync_links() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_B)?;
    let reads = ChatReads::new(&client);

    let first = reads.messages(AB_CHAT, 2).await?;
    let mut ids: Vec<String> = first.messages.iter().map(|message| message.id.clone()).collect();
    let mut backward = first.backward_link().map(str::to_owned);
    while let Some(link) = backward {
        let page = reads.messages_from(AB_CHAT, &link).await?;
        ids.extend(page.messages.iter().map(|message| message.id.clone()));
        backward = page.backward_link().map(str::to_owned);
    }
    assert_eq!(
        ids,
        [
            "1760000060000",
            "1760000050000",
            "1760000010000",
            "1760000005000",
            "1760000000400"
        ],
        "newest first, every message once"
    );

    let whole = reads.messages(AB_CHAT, 200).await?;
    assert_eq!(whole.messages.len(), 5);
    assert!(whole.unreadable.is_empty());
    assert!(whole.messages[0].is_deleted());
    assert_eq!(whole.messages[2].edit_time(), Field::Known(1_760_000_020_000));
    assert_eq!(whole.messages[4].message_type, MessageType::AddMember);
    let sync = whole.sync_state().ok_or("no sync link")?;
    let since = reads.messages_from(AB_CHAT, sync).await?;
    assert!(since.messages.is_empty(), "nothing changed since");

    let around = reads.messages_around(AB_CHAT, 1_760_000_000_000, 40).await?;
    assert_eq!(around.messages.len(), 5);
    assert_eq!(around.sync_state(), None);
    Ok(())
}

#[tokio::test]
async fn threads_and_member_lists_through_the_transport() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let one_to_one = reads.thread(AB_CHAT).await?;
    let members: Vec<&str> = one_to_one.members.iter().map(|member| member.id.as_str()).collect();
    assert_eq!(members, [LAB_A, LAB_B]);
    let group = reads.thread(ABC_CHAT).await?;
    assert_eq!(group.kind(), ConversationKind::Group);
    assert_eq!(group.members.len(), 3);
    let list = reads.members(ABC_CHAT).await?;
    assert_eq!((list.members.len(), &list.total_member_count), (3, &Field::Known(3)));
    // a chat the caller isn't in is the service's refusal, as the transport reports it
    let outside = lab_client(&mock, LAB_C)?;
    let refused = ChatReads::new(&outside).thread(AB_CHAT).await;
    assert!(
        matches!(&refused, Err(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 403),
        "{refused:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_link_off_the_chat_base_or_of_another_read_is_never_followed() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let client = lab_client(&mock, LAB_A)?;
    let reads = ChatReads::new(&client);
    let messages = format!("v1/users/ME/conversations/{AB_CHAT}/messages?syncState=mock-since-0");
    let foreign = [
        format!("https://elsewhere.example.test/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com/api/chatsvc/emea/{messages}"),
        format!("https://amer.ng.msg.teams.microsoft.com/{messages}"),
        format!("http://teams.microsoft.com/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com:8443/api/chatsvc/amer/{messages}"),
        format!("https://user@teams.microsoft.com/api/chatsvc/amer/{messages}"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/{messages}#fragment"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/{ABC_CHAT}/messages"),
        format!("https://teams.microsoft.com/api/chatsvc/amer/v1/threads/{AB_CHAT}"),
        format!("{}/api/chatsvc/amer/{messages}", mock.origin()),
        "not a url".to_owned(),
    ];
    for link in &foreign {
        let refused = reads.messages_from(AB_CHAT, link).await;
        assert!(
            matches!(refused, Err(ChatReadError::Link { .. })),
            "{link}: {refused:?}"
        );
    }
    let list_link = format!("https://teams.microsoft.com/api/chatsvc/amer/{messages}");
    assert!(matches!(
        reads.conversations_from(&list_link).await,
        Err(ChatReadError::Link { .. })
    ));
    assert!(mock.received_requests().await.is_empty(), "nothing was sent");
    // the same link on the chat base is followed
    assert!(reads.messages_from(AB_CHAT, &list_link).await.is_ok());
    assert_eq!(mock.received_requests().await.len(), 1);
    Ok(())
}

#[test]
fn a_link_on_the_chat_base_becomes_its_endpoint_with_its_query() -> TestResult {
    let endpoint = link_endpoint(
        CHAT_BASE,
        "https://teams.microsoft.com/api/chatsvc/amer/v1/users/ME/conversations/19%3Ax%40thread.v2/messages?view=a|b&syncState=t%2Bu&pageSize=200",
    )
    .ok_or("refused")?;
    assert_eq!(
        endpoint.segments(),
        ["v1", "users", "ME", "conversations", "19:x@thread.v2", "messages"]
    );
    let query: Vec<(&str, &str)> = endpoint
        .query()
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    assert_eq!(query, [("view", "a|b"), ("syncState", "t+u"), ("pageSize", "200")]);
    let regional = "https://amer.ng.msg.teams.microsoft.com/";
    assert!(link_endpoint(regional, "https://amer.ng.msg.teams.microsoft.com/v1/threads/x").is_some());
    assert!(link_endpoint(regional, "https://emea.ng.msg.teams.microsoft.com/v1/threads/x").is_none());
    assert!(link_endpoint(CHAT_BASE, "https://teams.microsoft.com/api/chatsvc/amer/v1x/threads").is_none());
    Ok(())
}

#[tokio::test]
async fn a_404_with_a_location_is_a_region_hop_not_followed() -> TestResult {
    let server = MockServer::start().await;
    let hop = format!("{}/api/chatsvc/emea/v1/users/ME/conversations", server.uri());
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404).insert_header("Location", hop.as_str()))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let reads = ChatReads::new(&client);
    let moved = reads.conversations().await;
    assert!(
        matches!(moved, Err(ChatReadError::RegionMoved { status: 404 })),
        "{moved:?}"
    );
    let error = moved.err().ok_or("no error")?;
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("emea") && !shown.contains("127.0.0.1"), "{shown}");
    }
    assert_eq!(server.received_requests().await.map(|requests| requests.len()), Some(1));
    Ok(())
}

#[tokio::test]
async fn a_404_without_a_location_and_a_malformed_answer_are_errors() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let reads = ChatReads::new(&client);
    let missing = reads.thread(AB_CHAT).await;
    assert!(
        matches!(&missing, Err(ChatReadError::Http(HttpError::Status(answer))) if answer.status() == 404),
        "{missing:?}"
    );
    assert!(matches!(reads.conversations().await, Err(ChatReadError::Malformed(_))));
    assert!(matches!(
        reads.messages(AB_CHAT, 10).await,
        Err(ChatReadError::Malformed(_))
    ));
    Ok(())
}

#[tokio::test]
async fn an_entry_that_is_not_a_message_is_kept_apart_and_the_page_read() -> TestResult {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "messages": [{"id": "1", "messagetype": "Text", "content": "kept"}, {"no": "id"}, {"id": "2", "content": 5}],
            "_metadata": {"syncState": ""}
        })))
        .mount(&server)
        .await;
    let client = client_for(&server.uri(), "placeholder-token")?;
    let page = ChatReads::new(&client).messages(AB_CHAT, 10).await?;
    assert_eq!(page.messages.len(), 1);
    assert_eq!(
        page.messages[0].rich_text().map(|rich| rich.plain_text()),
        Some("kept".to_owned())
    );
    assert_eq!(page.unreadable, [json!({"no": "id"}), json!({"id": "2", "content": 5})]);
    assert_eq!(page.sync_state(), None, "an empty link is no link");
    Ok(())
}
