//! A meeting's chat as Teams shows it, on GPUI's test context over the fake engine (`common`): the thread's events and
//! the meeting's and its recording's states as lines after their icons (the times first for a call's or a recording's,
//! the meeting's length in bold, an anonymous member marked unverified), the recording saved with its expiry, the
//! transcript told by the recap card under the meeting's end (View recap, Transcript, the thumbnail with its length and
//! expiry, Share), a meeting's start left out when its end follows at once; none of them with an author row, and a
//! thread never named as a sender, not even for a kind this build doesn't show. Every id, name and value here is made
//! up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use super::common::{LAB_A, LAB_B, Rig};
use super::parts;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{Body, ChatView, ChatViewEvent, MessageItem, RecapOpen};
use teams_ui::settings::Settings;
use teams_ui::system_message::{EventIcon, Span};

const MEETING: &str = "19:meeting_bWFkZS11cA@thread.v2";
const CALL: &str = "00000000-0000-4000-8000-0000000000c1";
const LINK: &str = "https://madeup-my.sharepoint.com/:v:/g/personal/lab_b/MadeUpRecording";
/// 2026-01-01 12:00 UTC.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;

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

/// A recording's message of the call in `status` (with its code) holding `contents`, started by b at 13:01; once ready
/// its link and its file in b's OneDrive.
fn recording(status: &str, code: &str, contents: &str) -> String {
    let ready = status == "Success";
    let link = if ready { LINK } else { "" };
    let file = if ready {
        format!("<item type=\"onedriveForBusinessVideo\" uri=\"{LINK}\" driveId=\"b!drive1\" driveItemId=\"ITEM1\" />")
    } else {
        String::new()
    };
    format!(
        "<URIObject type=\"Video.2/CallRecording.1\"><RecordingStatus status=\"{status}\" code=\"{code}\" />\
         <Title>Weekly sync</Title><a href=\"{link}\">Play</a><OriginalName v=\"Weekly sync-Meeting Recording.mp4\" />\
         <MeetingOrganizerId value=\"{LAB_B}\" /><RecordingInitiatorId value=\"{LAB_B}\" isAutoRecording=\"False\" />\
         <Identifiers><Id type=\"callId\" value=\"{CALL}\" /></Identifiers>\
         <RecordingContent contentTypes=\"{contents}\" timestamp=\"2026-01-01T13:01:00.0000000Z\" duration=\"0:28:38.3\">\
         {file}</RecordingContent></URIObject>"
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
            &format!(
                "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType><callId>{CALL}</callId>"
            ),
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
            "<part identity=\"{mri}\"><name>{mri}</name><displayName>x</displayName><duration>3697</duration></part>"
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
                "<ended/><partlist alt=\"\" count=\"2\">{}{}</partlist><callEventType>callEnded</callEventType>\
                 <callId>{CALL}</callId>",
                part(LAB_B),
                part(LAB_A)
            ),
            122,
        ),
        message(
            "9",
            LAB_B,
            "RichText/Media_CallRecording",
            &recording("Success", "200", "Recording+Transcript"),
            123,
        ),
        message(
            "10",
            LAB_B,
            "RichText/Media_CallTranscript",
            &format!(r#"{{\"callId\":\"{CALL}\",\"isDeleted\":false}}"#),
            124,
        ),
        message("11", MEETING, "ThreadActivity/SomethingNew", "<somethingnew/>", 125),
        // an addition naming no initiator: a shape no capture showed, kept as the placeholder
        message(
            "12",
            MEETING,
            "ThreadActivity/AddMember",
            &format!("<addmember><eventtime>4</eventtime><target>{LAB_A}</target></addmember>"),
            126,
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
    cx.simulate_resize(size(px(900.0), px(1600.0)));
    draw(cx);
    (view, cx)
}

fn items(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<MessageItem> {
    cx.update(|_, cx| view.read(cx).message_items().cloned().collect())
}

/// What a row says: its kind and, for a line, its icon and words.
type Said = (&'static str, Option<(EventIcon, String)>);

/// A row's kind and, for a line, its icon and words.
fn said(body: &Body) -> Said {
    match body {
        Body::Event(line) | Body::TimedEvent(line) => (body.kind(), Some((line.icon, line.text()))),
        Body::MeetingEnded(end) => (body.kind(), Some((end.line.icon, end.line.text()))),
        _ => (body.kind(), None),
    }
}

fn rig_with(messages: Vec<Message>) -> Rig {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(MEETING.to_owned(), messages);
    let mut names = rig.core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Lab Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    drop(names);
    rig.core.expirations.lock().unwrap().insert(
        "ITEM1".to_owned(),
        chrono::Utc::now().timestamp_millis() + 102 * DAY + DAY / 2,
    );
    rig.cx.update(teams_ui::window::init);
    rig
}

#[test]
fn a_meeting_chat_shows_its_system_messages_as_teams_does() {
    let rig = rig_with(meeting_messages());
    let (view, cx) = meeting_window(&rig);
    let shown = items(&view, cx);
    let rows: Vec<(String, Said)> = shown.iter().map(|item| (item.id.clone(), said(&item.body))).collect();
    let line = |kind, icon, text: &str| (kind, Some((icon, text.to_owned())));
    assert_eq!(
        rows,
        vec![
            // the account by its name, as Teams' meeting chat names it
            (
                "1".to_owned(),
                line(
                    "event",
                    EventIcon::Invited,
                    "Lab Ay and Lab Bee were invited to the meeting."
                )
            ),
            (
                "2".to_owned(),
                line("event", EventIcon::Named, "Lab Bee named the meeting Weekly sync.")
            ),
            (
                "3".to_owned(),
                line("timed-event", EventIcon::Meeting, "Meeting started")
            ),
            (
                "4".to_owned(),
                line("timed-event", EventIcon::RecordingStarted, "Lab Bee started recording.")
            ),
            // "5", the transcript's half of the recording, shows nothing
            ("6".to_owned(), ("text", None)),
            (
                "7".to_owned(),
                line(
                    "event",
                    EventIcon::Joined,
                    "Note Taker has temporarily joined the chat."
                )
            ),
            (
                "8".to_owned(),
                line("meeting-ended", EventIcon::Meeting, "Meeting ended: 1h 1m 37s")
            ),
            (
                "9".to_owned(),
                line(
                    "timed-event",
                    EventIcon::RecordingSaved,
                    "Recording has been saved to Lab Bee's OneDrive."
                )
            ),
            // "10", the transcript: the recap card tells it
            ("11".to_owned(), ("unsupported-event", None)),
            ("12".to_owned(), ("unsupported-event", None)),
        ]
    );
    // the recording's expiry, once its item says it, counted in days up
    let Some(Body::TimedEvent(saved)) = shown.iter().find(|item| item.id == "9").map(|item| &item.body) else {
        panic!("no recording saved");
    };
    assert_eq!(
        cx.update(|_, cx| view.read(cx).drawn_text(saved)),
        "Recording has been saved to Lab Bee's OneDrive and will expire in 103 days."
    );
    let visitor = shown.iter().find(|item| item.id == "7").map(|item| item.body.clone());
    assert!(
        matches!(&visitor, Some(Body::Event(line)) if line.spans.contains(&Span::Unverified)),
        "{visitor:?}"
    );
    the_recap_card(&shown);
    drawn_without_authors(cx);
    the_card_opens_the_recap(&view, cx);
}

/// The meeting's end carries its recap card: its recording, its transcript and its span.
fn the_recap_card(shown: &[MessageItem]) {
    let Some(Body::MeetingEnded(end)) = shown.iter().find(|item| item.id == "8").map(|item| &item.body) else {
        panic!("no meeting end");
    };
    let card = end.recap.as_ref().expect("a recap card");
    assert_eq!(card.recordings.len(), 1);
    assert_eq!(card.recordings[0].link, LINK);
    assert!(card.transcript);
    assert!(!card.saving);
    assert_eq!(card.badge().as_deref(), Some("28m 38s"));
    // the recording's own span: 13:01 and its 28 minutes
    let start = NOON + 61 * MINUTE;
    assert_eq!(card.span, Some((start, start + (28 * 60 + 38) * 1_000)));
    let link = card.share_link(MEETING).expect("a recap link");
    assert!(
        link.starts_with("https://teams.microsoft.com/l/meetingrecap?"),
        "{link}"
    );
    assert!(link.contains("driveItemId=ITEM1"), "{link}");
}

/// Every system row is drawn without an author: no sender's name or avatar, never the thread's id; the recap card with
/// its controls, its thumbnail's expiry and the unverified mark.
fn drawn_without_authors(cx: &mut VisualTestContext) {
    for id in ["1", "2", "3", "4", "7", "8", "9", "11"] {
        assert!(
            cx.debug_bounds(sel(&format!("event-{id}"))).is_some(),
            "event {id} drawn"
        );
        assert!(
            cx.debug_bounds(sel(&format!("avatar-{id}"))).is_none(),
            "event {id} has no avatar"
        );
    }
    for selector in [
        "recap-card-8",
        "view-recap-8",
        "recap-card-share-8",
        "recap-card-transcript-8",
        "recap-card-thumbnail-8",
        "recap-card-expiry-8",
        "unverified-7-0",
        "event-icon recording-started",
        "event-icon recording-saved",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} drawn");
    }
    assert!(cx.debug_bounds("unsupported-11").is_some());
    // the message among them keeps its author
    assert!(cx.debug_bounds("avatar-6").is_some());
}

/// View recap, the Transcript chip and the thumbnail each tell the shell to show the Recap tab at the recording.
fn the_card_opens_the_recap(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    for selector in ["view-recap-8", "recap-card-transcript-8", "recap-card-thumbnail-8"] {
        let button = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        cx.simulate_click(button.center(), Modifiers::none());
        cx.run_until_parked();
    }
    let opened = |open| ChatViewEvent::OpenRecap {
        recording: Some(LINK.to_owned()),
        open,
    };
    assert_eq!(
        *told.borrow(),
        vec![
            opened(RecapOpen::Recap),
            opened(RecapOpen::Transcript),
            opened(RecapOpen::Play)
        ]
    );
}

#[test]
fn a_meeting_s_start_followed_at_once_by_its_end_is_left_out() {
    let mut messages = from_the_meeting();
    messages.retain(|message| ["8", "10"].contains(&message.id.as_str()));
    messages.insert(
        0,
        message(
            "3",
            LAB_B,
            "Event/Call",
            &format!(
                "<partlist alt =\"\"></partlist><callEventType>callStarted</callEventType><callId>{CALL}</callId>"
            ),
            60,
        ),
    );
    let rig = rig_with(messages);
    let (view, cx) = meeting_window(&rig);
    let shown = items(&view, cx);
    let ids: Vec<&str> = shown.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(ids, ["8"], "the start is left out, the transcript told by the card");
    let Some(Body::MeetingEnded(end)) = shown.first().map(|item| &item.body) else {
        panic!("no meeting end");
    };
    let card = end.recap.as_ref().expect("a transcript's recap card");
    assert!(card.transcript);
    assert!(card.recordings.is_empty());
    assert!(cx.debug_bounds("recap-card-transcript-8").is_some());
    assert!(cx.debug_bounds("recap-card-thumbnail-8").is_none());
}

/// A test selector (selectors live as long as the test).
fn sel(selector: &str) -> &'static str {
    Box::leak(selector.to_owned().into_boxed_str())
}
