//! A meeting's chat as Teams shows it, on GPUI's test context over the fake engine (`common`): the meeting's start and
//! end as cards titled by the chat's name, a recording's state as lines after their time and a card once it is ready,
//! the transcript's card, who was invited or left as lines; none of them with an author row, and a thread never named
//! as a sender, not even for a kind this build doesn't show. Every id, name and value here is made up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::common::{LAB_A, LAB_B, Rig};
use super::parts;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{Body, ChatView, ChatViewEvent, EventCard, MessageItem};
use teams_ui::settings::Settings;
use teams_ui::system_message::CardKind;

const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

fn message(id: &str, sender: &str, kind: &str, content: &str, minute: i64) -> Message {
    Message {
        conversation_id: MEETING.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: kind.to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: NOON + minute * MINUTE,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn recording(status: &str, code: &str, contents: &str) -> String {
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Weekly sync</Title><a href=\"\">Play</a><OriginalName v=\"Weekly sync-Meeting Recording.mp4\" />\
         <RecordingInitiatorId value=\"{LAB_B}\" isAutoRecording=\"False\" />\
         <RecordingContent contentTypes=\"{contents}\" duration=\"0:16:50.3\"></RecordingContent></URIObject>"
    )
}

/// The meeting chat's messages: the thread's own events, the meeting's start, a recording, a message, the end, the
/// recording and the transcript ready, and thread events of a kind or shape this build doesn't show.
fn meeting_messages() -> Vec<Message> {
    let mut messages = until_the_meeting();
    messages.extend(from_the_meeting());
    messages
}

/// Up to the meeting's first message: invited, named, started, recorded, a message.
fn until_the_meeting() -> Vec<Message> {
    vec![
        message(
            "1",
            MEETING,
            "ThreadActivity/MemberJoined",
            &format!(
                r#"{{"eventtime":1,"initiator":"{LAB_B}","members":[{{"id":"{LAB_A}","friendlyname":""}},{{"id":"{LAB_B}","friendlyname":""}}]}}"#
            ),
            0,
        ),
        message(
            "2",
            MEETING,
            "ThreadActivity/TopicUpdate",
            &format!(
                "<topicupdate><eventtime>2</eventtime><initiator>{LAB_B}</initiator><value>Weekly sync</value></topicupdate>"
            ),
            1,
        ),
        message(
            "3",
            LAB_B,
            "Event/Call",
            "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType>",
            60,
        ),
        message(
            "4",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Recording"),
            61,
        ),
        message(
            "5",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Initial", "0", "Transcript"),
            61,
        ),
        message("6", LAB_B, "RichText/Html", "<p>agenda first</p>", 62),
    ]
}

/// From a visitor joining: the end, the recording and transcript ready, and what this build doesn't show.
fn from_the_meeting() -> Vec<Message> {
    let part = |mri: &str| {
        format!(
            "<part identity=\"{mri}\"><name>{mri}</name><displayName>x</displayName><duration>1930</duration></part>"
        )
    };
    vec![
        message(
            "7",
            MEETING,
            "ThreadActivity/MemberJoined",
            r#"{"eventtime":3,"initiator":"x","members":[{"id":"8:teamsvisitor:abc","friendlyname":"Note Taker","meetingMemberType":"temp"}]}"#,
            63,
        ),
        message(
            "8",
            LAB_B,
            "Event/Call",
            &format!(
                "<ended/><partlist alt=\"\" count=\"2\">{}{}</partlist><callEventType>callEnded</callEventType>",
                part(LAB_B),
                part(LAB_A)
            ),
            92,
        ),
        message(
            "9",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Success", "200", "Recording+Transcript"),
            93,
        ),
        message(
            "10",
            LAB_B,
            "RichText/Media_CallTranscript",
            r#"{\"callId\":\"c\",\"isDeleted\":false}"#,
            94,
        ),
        message("11", MEETING, "ThreadActivity/SomethingNew", "<somethingnew/>", 95),
        // an addition naming no initiator: a shape no capture showed, kept as the placeholder
        message(
            "12",
            MEETING,
            "ThreadActivity/AddMember",
            &format!("<addmember><eventtime>4</eventtime><target>{LAB_A}</target></addmember>"),
            96,
        ),
    ]
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

fn meeting_window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, MEETING.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(1600.0)));
    draw(cx);
    (view, cx)
}

fn items(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<MessageItem> {
    cx.update(|_, cx| view.read(cx).message_items().cloned().collect())
}

fn card(kind: CardKind, title: &str, detail: &str) -> Body {
    Body::Card(Box::new(EventCard {
        kind,
        title: title.to_owned().into(),
        detail: detail.to_owned().into(),
    }))
}

#[test]
fn a_meeting_chat_shows_its_system_messages_as_teams_does() {
    let rig = Rig::start();
    rig.core
        .messages
        .lock()
        .unwrap()
        .insert(MEETING.to_owned(), meeting_messages());
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    rig.cx.update(teams_ui::window::init);
    let (view, cx) = meeting_window(&rig);
    let shown: Vec<(String, Body)> = items(&view, cx).into_iter().map(|item| (item.id, item.body)).collect();
    let event = |text: &str| Body::Event(text.to_owned().into());
    let timed = |text: &str| Body::TimedEvent(text.to_owned().into());
    assert_eq!(
        shown,
        vec![
            ("1".to_owned(), event("You and Lab Bee were invited to the meeting.")),
            ("2".to_owned(), event("Lab Bee named the meeting Weekly sync.")),
            (
                "3".to_owned(),
                card(CardKind::MeetingStarted, "Meeting", "Meeting started")
            ),
            ("4".to_owned(), timed("Lab Bee started recording.")),
            // "5", the transcript's half of the recording, shows nothing
            ("6".to_owned(), shown_text(&items(&view, cx), "6"),),
            ("7".to_owned(), event("Note Taker has temporarily joined the chat.")),
            (
                "8".to_owned(),
                card(CardKind::MeetingEnded, "Meeting", "Meeting ended: 32m 10s")
            ),
            (
                "9".to_owned(),
                card(
                    CardKind::Recording,
                    "Weekly sync-Meeting Recording",
                    "Recording and transcript \u{b7} 16m 50s"
                )
            ),
            (
                "10".to_owned(),
                card(CardKind::Transcript, "Transcript", "Transcript is available")
            ),
            (
                "11".to_owned(),
                Body::UnsupportedEvent("ThreadActivity/SomethingNew".to_owned())
            ),
            (
                "12".to_owned(),
                Body::UnsupportedEvent("ThreadActivity/AddMember".to_owned())
            ),
        ]
    );
    drawn_without_authors(cx);
    view_recap_tells_the_shell(&view, cx);

    // the chat's name titles the meeting's cards
    cx.update(|_, cx| view.update(cx, |view, cx| view.set_title(Some("Weekly sync".into()), cx)));
    draw(cx);
    let titled = items(&view, cx);
    let body_of = |id: &str| titled.iter().find(|item| item.id == id).map(|item| item.body.clone());
    assert_eq!(
        body_of("3"),
        Some(card(CardKind::MeetingStarted, "Weekly sync", "Meeting started"))
    );
    assert_eq!(
        body_of("8"),
        Some(card(CardKind::MeetingEnded, "Weekly sync", "Meeting ended: 32m 10s"))
    );
}

/// Every system row is drawn centred without an author: no sender's name or avatar, never the thread's id; the cards
/// as their kinds, a recording's and a transcript's with "View recap" (the meeting's Recap tab, in the app).
fn drawn_without_authors(cx: &mut VisualTestContext) {
    for id in ["1", "2", "4", "7", "11"] {
        assert!(
            cx.debug_bounds(sel(&format!("event-{id}"))).is_some(),
            "event {id} drawn"
        );
        assert!(
            cx.debug_bounds(sel(&format!("avatar-{id}"))).is_none(),
            "event {id} has no avatar"
        );
    }
    for (id, kind) in [
        ("3", "meeting-started"),
        ("8", "meeting-ended"),
        ("9", "recording"),
        ("10", "transcript"),
    ] {
        assert!(
            cx.debug_bounds(sel(&format!("event-card-{id} {kind}"))).is_some(),
            "card {id} drawn"
        );
    }
    // a recording and a transcript open the meeting's recap in the app, not Teams on the web; a meeting's card neither
    assert!(cx.debug_bounds("view-recap-9").is_some());
    assert!(cx.debug_bounds("view-recap-10").is_some());
    assert!(cx.debug_bounds("open-on-the-web-9").is_none());
    assert!(cx.debug_bounds("view-recap-8").is_none());
    assert!(cx.debug_bounds("open-on-the-web-8").is_none());
    assert!(cx.debug_bounds("unsupported-11").is_some());
    // the message among them keeps its author
    assert!(cx.debug_bounds("avatar-6").is_some());
}

/// "View recap" on the recording's card tells the shell to show the Recap tab.
fn view_recap_tells_the_shell(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let button = cx.debug_bounds("view-recap-9").expect("View recap is drawn");
    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();
    assert!(
        matches!(told.borrow().as_slice(), [ChatViewEvent::OpenRecap(_)]),
        "the recap asked for once: {:?}",
        told.borrow()
    );
}

/// The body of the message `id` as shown (a text message's, compared as itself).
fn shown_text(items: &[MessageItem], id: &str) -> Body {
    let body = items.iter().find(|item| item.id == id).map(|item| item.body.clone());
    assert!(matches!(body, Some(Body::Text(_))), "{id} is a text: {body:?}");
    body.unwrap()
}

/// A test selector (selectors live as long as the test).
fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_owned().into_boxed_str())
}
