//! The chat view on GPUI's test context, over the fake engine (`common`): the messages from the store, the oldest at
//! the top, with their edited and deleted markers, reactions (counts, the account's own), thread events and a
//! placeholder for a type this build doesn't show; `Control/…` never shown; a sender's messages grouped within
//! 5 minutes, day separators, the "New messages" divider at the read horizon; rendered rich text cached per message id
//! and version; new messages followed at the bottom, older pages (the store's, then the service's) loaded as the top
//! comes near, a 1 000-message chat read a page at a time; the "Jump to newest" pill; typing shown and cleared (by its
//! end, a timeout or the message); hover and keyboard actions, edit and delete on the account's own only; the shell
//! opening the chat's view. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
// the shell's and a chat view's parts over the rig
#[path = "common/parts.rs"]
mod parts;
// names and pictures in the chat list and the chat view (M2 Task 12)
#[path = "chat_view/people.rs"]
mod people;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{FixedOffset, NaiveDate};
use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{
    AppContext as _, Entity, ListOffset, Modifiers, ScrollDelta, ScrollWheelEvent, TestAppContext, TouchPhase,
    VisualTestContext, point, px, size,
};
use teams_core::{Event, MessageChange, Source};
use teams_proto::chat::web_chat_link;
use teams_store::{Applied, Message, Reaction, ReadHorizon};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{
    Body, ChatView, ChatViewEvent, GROUP_MS, History, Item, MAX_FRUITLESS, MessageAction, MessageItem, PAGE,
    ReactionCount, RenderedText, UnshownParts, arrange, count_reactions, day_label, reaction_label,
};
use teams_ui::models::TYPING_TIMEOUT;
use teams_ui::rich_text::Rendered;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

/// A message of the a–b chat: rich text from `sender`.
fn message(id: &str, sender: &str, content: &str, at: i64) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: (sender == LAB_B).then(|| "Lab Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// `count` plain messages from b, a minute apart, ending at `NOON`; ids from 1.
fn many(count: usize) -> Vec<Message> {
    (1..=count)
        .map(|n| {
            let back = i64::try_from(count - n).unwrap();
            message(
                &n.to_string(),
                LAB_B,
                &format!("<p>number {n}</p>"),
                NOON - back * MINUTE,
            )
        })
        .collect()
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// The rig with the a–b chat holding `messages`, b named in the store, the kit set up.
fn rig_with(messages: Vec<Message>) -> Rig {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    rig.cx.update(teams_ui::window::init);
    rig
}

/// Draws the window and lets what it started run.
fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// The chat view of the a–b chat in an 800×600 window of its own.
fn chat_window(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    (view, cx)
}

fn items(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<MessageItem> {
    cx.update(|_, cx| view.read(cx).message_items().cloned().collect())
}

fn ids(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<String> {
    items(view, cx).into_iter().map(|item| item.id).collect()
}

/// A delivery of `message` (already in the fake store) as the engine tells it.
fn delivered(message: &Message) -> Event {
    Event::Message {
        conversation: AB.to_owned(),
        id: message.id.clone(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message.clone())),
        source: Source::Delivery,
    }
}

/// Adds `message` to the fake store and delivers it.
fn arrive(rig: &Rig, cx: &mut VisualTestContext, message: &Message) {
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(AB)
        .unwrap()
        .push(message.clone());
    rig.send(delivered(message));
    draw(cx);
}

fn reaction(key: &str, user: &str, time: i64) -> Reaction {
    Reaction {
        key: key.to_owned(),
        user: user.to_owned(),
        time,
    }
}

/// The a–b chat with a thread event, a message from b with reactions, a's edited one, a deleted one, a typing signal
/// and a card (a type this build doesn't show).
fn varied_chat() -> Rig {
    let mut event = message("100", AB, "", NOON - 10 * MINUTE);
    event.message_type = "ThreadActivity/AddMember".to_owned();
    event.sender_name = None;
    event.content = format!(
        "<addmember><eventtime>1</eventtime><initiator>{LAB_A}</initiator><target>{LAB_B}</target></addmember>"
    );
    let hello = message("101", LAB_B, "<p>hello <b>there</b></p>", NOON);
    let mut edited = message("102", LAB_A, "<p>mine, edited</p>", NOON + MINUTE);
    edited.edit_time = Some(NOON + 2 * MINUTE);
    let mut gone = message("103", LAB_B, "", NOON + 2 * MINUTE);
    gone.delete_time = Some(NOON + 3 * MINUTE);
    let mut typing = message("104", LAB_B, "", NOON + 3 * MINUTE);
    typing.message_type = "Control/Typing".to_owned();
    let mut card = message("105", LAB_B, "<div>a card's markup</div>", NOON + 4 * MINUTE);
    card.message_type = "RichText/Media_Card".to_owned();
    let rig = rig_with(vec![event, hello, edited, gone, typing, card]);
    rig.core.reactions.lock().unwrap().insert(
        "101".to_owned(),
        vec![
            reaction("heart", LAB_B, NOON + 30_000),
            reaction("like", LAB_B, NOON + 10_000),
            reaction("like", LAB_A, NOON + 20_000),
        ],
    );
    rig
}

/// The trace's last chat-view line names the messages and their kinds, never their text or a name.
fn traced_without_text(written: &Written) {
    let text = written.text();
    let line = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "chat-view")
        .expect("a chat-view line");
    assert_eq!(line["change"], "shown");
    assert_eq!(line["conversation"], AB);
    assert_eq!(line["messages"], serde_json::json!(["100", "101", "102", "103", "105"]));
    assert_eq!(
        line["kinds"],
        serde_json::json!(["event", "text", "edited", "deleted", "unsupported"])
    );
    for secret in ["hello", "there", "mine", "Lab Bee", "card"] {
        assert!(!text.contains(secret), "the trace holds {secret}");
    }
}

#[test]
fn the_messages_show_oldest_first_with_their_markers_reactions_and_events() {
    let rig = varied_chat();
    let (trace, written) = trace();
    let (view, cx) = chat_window(&rig, trace);

    let shown = items(&view, cx);
    let kinds: Vec<(&str, &'static str, bool, bool)> = shown
        .iter()
        .map(|item| (item.id.as_str(), item.body.kind(), item.edited, item.own))
        .collect();
    assert_eq!(
        kinds,
        [
            ("100", "event", false, false),
            ("101", "text", false, false),
            ("102", "text", true, true),
            ("103", "deleted", false, false),
            ("105", "unsupported", false, false),
        ],
        "the control message is never shown"
    );
    assert_eq!(shown[0].body, Body::Event("You added Lab Bee".into()));
    assert_eq!(shown[4].body, Body::Unsupported("RichText/Media_Card".to_owned()));
    assert_eq!(
        shown[1].reactions,
        [
            ReactionCount {
                key: "like".to_owned(),
                count: 2,
                mine: true,
            },
            ReactionCount {
                key: "heart".to_owned(),
                count: 1,
                mine: false,
            },
        ]
    );
    assert_eq!(shown[1].sender_name.as_ref(), "Lab Bee");
    for selector in [
        "message-101",
        "edited",
        "reaction-like",
        "reaction-heart",
        "history-top",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    traced_without_text(&written);
}

/// A made-up row for `arrange`: text from `sender` at `at`.
fn row(id: &str, sender: &str, at: i64) -> MessageItem {
    MessageItem {
        id: id.to_owned(),
        version: 1,
        sender: Some(sender.to_owned()),
        sender_name: "Someone".into(),
        face: teams_ui::people::Face::default(),
        own: sender == LAB_A,
        header: true,
        arrival_time: at,
        body: Body::Text(RenderedText(Arc::new(Rendered::default()))),
        message_type: "RichText/Html".to_owned(),
        edited: false,
        reactions: Vec::new(),
        outbox: Vec::new(),
        unshown: UnshownParts::default(),
    }
}

/// The shape of arranged rows: `top`, `day`, `new`, or a message's id with `+` when it heads its group.
fn shape(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .map(|item| match item {
            Item::Top(_) => "top".to_owned(),
            Item::Day(_) => "day".to_owned(),
            Item::NewMessages => "new".to_owned(),
            Item::Message(message) if message.header => format!("+{}", message.id),
            Item::Message(message) => message.id.clone(),
            Item::Pending(pending) => format!("pending {}", pending.note.row),
        })
        .collect()
}

#[test]
fn a_sender_s_messages_group_within_five_minutes_and_days_are_separated() {
    let utc = FixedOffset::east_opt(0).unwrap();
    let mut event = row("e", LAB_B, NOON + 30 * MINUTE);
    event.body = Body::Event("added".into());
    event.message_type = "ThreadActivity/AddMember".to_owned();
    let rows = vec![
        row("1", LAB_B, NOON),
        row("2", LAB_B, NOON + MINUTE),
        row("3", LAB_B, NOON + MINUTE + GROUP_MS),
        row("4", LAB_B, NOON + 2 * MINUTE + 2 * GROUP_MS),
        row("5", LAB_A, NOON + 20 * MINUTE),
        row("6", LAB_A, NOON + 21 * MINUTE),
        event,
        row("7", LAB_A, NOON + 31 * MINUTE),
        row("8", LAB_A, NOON + 24 * 60 * MINUTE),
    ];
    let items = arrange(rows.clone(), History::Complete, None, &utc);
    assert_eq!(
        shape(&items),
        ["top", "day", "+1", "2", "3", "+4", "+5", "6", "+e", "+7", "day", "+8"],
        "within 5 minutes grouped (5 minutes to the dot too), past them not; another sender, an event or a new day \
         starts a group"
    );
    assert_eq!(items[0], Item::Top(History::Complete));
    assert_eq!(items[1], Item::Day(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()));

    // the divider before the first message from someone else after the horizon; it starts a group
    let items = arrange(rows.clone(), History::More, Some(NOON + MINUTE), &utc);
    assert_eq!(
        shape(&items),
        [
            "top", "day", "+1", "2", "new", "+3", "+4", "+5", "6", "+e", "+7", "day", "+8"
        ]
    );
    // the account's own messages after it, or an event, never place it
    let items = arrange(rows, History::More, Some(NOON + 3 * MINUTE + 2 * GROUP_MS), &utc);
    assert!(!items.contains(&Item::NewMessages));

    let today = NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();
    assert_eq!(day_label(today, today), "Today");
    assert_eq!(day_label(today.pred_opt().unwrap(), today), "Yesterday");
    assert_eq!(
        day_label(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), today),
        "Thursday, 1 January"
    );
    assert_eq!(
        day_label(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(), today),
        "Wednesday, 31 December 2025"
    );
}

#[test]
fn the_divider_counts_what_the_store_counts_unread_a_card_too_never_a_deleted_one() {
    let utc = FixedOffset::east_opt(0).unwrap();
    let mut card = row("2", LAB_B, NOON + MINUTE);
    card.message_type = "RichText/Media_Card".to_owned();
    card.body = Body::Unsupported("RichText/Media_Card".to_owned());
    let mut gone = row("3", LAB_B, NOON + 2 * MINUTE);
    gone.body = Body::Deleted;
    let rows = vec![row("1", LAB_B, NOON), card, gone, row("4", LAB_B, NOON + 3 * MINUTE)];
    let items = arrange(rows.clone(), History::More, Some(NOON), &utc);
    assert_eq!(
        shape(&items),
        ["top", "day", "+1", "new", "+2", "3", "4"],
        "a card the view doesn't show counts unread"
    );
    let items = arrange(rows, History::More, Some(NOON + MINUTE), &utc);
    assert_eq!(
        shape(&items),
        ["top", "day", "+1", "2", "3", "new", "+4"],
        "a deleted message doesn't"
    );
}

#[test]
fn reactions_count_by_key_in_the_order_first_given() {
    let counted = count_reactions(
        &[
            reaction("surprised", LAB_B, 30),
            reaction("like", LAB_B, 10),
            reaction("like", "8:orgid:00000000-0000-4000-8000-00000000000c", 40),
            reaction("custom-key", LAB_A, 20),
        ],
        LAB_A,
    );
    let shown: Vec<(&str, usize, bool)> = counted
        .iter()
        .map(|reaction| (reaction.key.as_str(), reaction.count, reaction.mine))
        .collect();
    assert_eq!(
        shown,
        [("like", 2, false), ("custom-key", 1, true), ("surprised", 1, false)]
    );
    assert_eq!(reaction_label("like"), "👍");
    assert_eq!(
        reaction_label("custom-key"),
        "custom-key",
        "a key Teams doesn't name shows as itself"
    );
}

#[test]
fn the_new_messages_divider_sits_at_the_read_horizon_as_the_chat_opened() {
    let rig = rig_with(vec![
        message("1", LAB_B, "<p>read</p>", NOON),
        message("2", LAB_A, "<p>mine</p>", NOON + MINUTE),
        message("3", LAB_B, "<p>unread</p>", NOON + 2 * MINUTE),
    ]);
    rig.core.horizons.lock().unwrap().insert(
        AB.to_owned(),
        ReadHorizon {
            conversation_id: AB.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: NOON,
            updated: NOON,
            raw: None,
        },
    );
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let divider_before = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let items = view.read(cx).items();
            let at = items.iter().position(|item| *item == Item::NewMessages)?;
            match items.get(at + 1) {
                Some(Item::Message(message)) => Some(message.id.clone()),
                _ => None,
            }
        })
    };
    assert_eq!(divider_before(cx).as_deref(), Some("3"));
    assert!(cx.debug_bounds("new-messages").is_some());
    // the horizon moves on (the account read the chat); the divider stays where it was while the chat is open
    rig.core.horizons.lock().unwrap().get_mut(AB).unwrap().last_read_arrival = NOON + 2 * MINUTE;
    arrive(&rig, cx, &message("4", LAB_B, "<p>newer</p>", NOON + 3 * MINUTE));
    assert_eq!(divider_before(cx).as_deref(), Some("3"));
}

#[test]
fn rendered_rich_text_is_cached_per_message_id_and_version() {
    let rig = rig_with(many(3));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let layouts = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).layouts());
    assert_eq!(layouts(cx), 3);
    // a change that changed none of them lays none out again
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    assert_eq!(layouts(cx), 3);
    // an edit: a new version
    let edited = {
        let mut messages = rig.core.messages.lock().unwrap();
        let stored = messages
            .get_mut(AB)
            .unwrap()
            .iter_mut()
            .find(|message| message.id == "2")
            .unwrap();
        stored.version = 2;
        stored.content = "<p>number two, edited</p>".to_owned();
        stored.edit_time = Some(NOON);
        stored.clone()
    };
    rig.send(Event::Message {
        conversation: AB.to_owned(),
        id: "2".to_owned(),
        change: MessageChange::Edit,
        applied: Applied::Updated,
        message: Some(Box::new(edited)),
        source: Source::Delivery,
    });
    draw(cx);
    assert_eq!(layouts(cx), 4);
    assert!(items(&view, cx)[1].edited);
    arrive(&rig, cx, &message("4", LAB_B, "<p>four</p>", NOON + MINUTE));
    assert_eq!(layouts(cx), 5);
}

#[test]
fn a_new_message_shows_at_the_bottom_and_the_list_follows_it() {
    let rig = rig_with(many(40));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    assert!(!cx.update(|_, cx| view.read(cx).scrolled_up()));
    arrive(&rig, cx, &message("41", LAB_B, "<p>the newest</p>", NOON + MINUTE));
    assert_eq!(ids(&view, cx).last().map(String::as_str), Some("41"));
    assert!(!cx.update(|_, cx| view.read(cx).scrolled_up()), "still at the bottom");
    assert!(cx.debug_bounds("message-41").is_some(), "the newest is in view");
    assert!(cx.debug_bounds("jump-to-newest").is_none());
}

/// Scrolls the message list up by `by` px.
fn scroll_up(cx: &mut VisualTestContext, by: f32) {
    let list = cx.debug_bounds("chat-messages").expect("the message list is drawn");
    cx.simulate_event(ScrollWheelEvent {
        position: list.center(),
        delta: ScrollDelta::Pixels(point(px(0.0), px(by))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
    });
    draw(cx);
}

#[test]
fn scrolled_up_the_pill_shows_and_takes_the_list_back_to_the_newest() {
    let rig = rig_with(many(100));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    scroll_up(cx, 1_000.0);
    assert!(cx.update(|_, cx| view.read(cx).scrolled_up()));
    assert!(cx.debug_bounds("jump-to-newest").is_some());
    // a new message doesn't pull the list down while it is scrolled up
    arrive(&rig, cx, &message("101", LAB_B, "<p>meanwhile</p>", NOON + MINUTE));
    assert!(cx.update(|_, cx| view.read(cx).scrolled_up()));
    assert!(cx.debug_bounds("message-101").is_none());
    let pill = cx.debug_bounds("jump-to-newest").unwrap();
    cx.simulate_click(pill.center(), Modifiers::none());
    draw(cx);
    assert!(!cx.update(|_, cx| view.read(cx).scrolled_up()));
    assert!(cx.debug_bounds("message-101").is_some());
    assert!(cx.debug_bounds("jump-to-newest").is_none());
}

#[test]
fn older_pages_come_from_the_store_then_the_service_until_the_beginning() {
    // 30 stored, 40 more at the service in two pages
    let all = many(70);
    let rig = rig_with(all[40..].to_vec());
    let mut first: Vec<Message> = all[20..40].to_vec();
    first.reverse();
    let mut second: Vec<Message> = all[..20].to_vec();
    second.reverse();
    rig.core
        .older
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![first, second]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));

    let shown = ids(&view, cx);
    let expected: Vec<String> = (1..=70).map(|n| n.to_string()).collect();
    assert_eq!(shown, expected, "the whole history, the oldest first");
    assert_eq!(cx.update(|_, cx| view.read(cx).history()), History::Complete);
    let asked = rig.core.asked();
    assert_eq!(
        asked.iter().filter(|name| **name == "load_older").count(),
        3,
        "two pages, then nothing older: {asked:?}"
    );
    let top = cx.update(|_, cx| view.read(cx).items()[0].clone());
    assert_eq!(top, Item::Top(History::Complete));
}

#[test]
fn a_long_chat_scrolls_without_loading_everything_at_once() {
    const ROWS_IN_A_FRAME: usize = 80;
    let rig = rig_with(many(1_000));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let held = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).held());
    let drawn = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).drawn().len());

    let opened = held(cx);
    assert!(
        (PAGE..=3 * PAGE).contains(&opened),
        "opening reads the newest pages only: {opened}"
    );
    assert!(drawn(cx) <= ROWS_IN_A_FRAME, "rows laid out in a frame: {}", drawn(cx));
    assert_eq!(ids(&view, cx).last().map(String::as_str), Some("1000"));

    let mut before = opened;
    let mut steps = 0;
    while held(cx) < 1_000 {
        steps += 1;
        assert!(steps < 200, "the top was never reached: {}", held(cx));
        scroll_up(cx, 2_000.0);
        let now = held(cx);
        assert!(now - before <= 3 * PAGE, "a scroll read {} messages", now - before);
        assert!(drawn(cx) <= ROWS_IN_A_FRAME, "rows laid out in a frame: {}", drawn(cx));
        before = now;
    }
    assert!(steps > 3, "the history came in steps: {steps}");
    // the store read a page at a time, never the whole chat at once
    let limits = rig.core.limits.lock().unwrap().clone();
    assert!(limits.len() >= 1_000 / PAGE, "{} reads", limits.len());
    assert!(limits.iter().all(|limit| *limit == PAGE), "{limits:?}");
    on_to_the_top_each_message_parsed_once(&rig, &view, cx);
}

/// On to the top of the 1 000-message chat: rows above are measured as they come, so a wheel step moves the list a
/// screen or so. Every load rebuilt the rows, yet each message was parsed and laid out once, and a new one adds one.
fn on_to_the_top_each_message_parsed_once(rig: &Rig, view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let mut more = 0;
    while cx.debug_bounds("message-1").is_none() {
        more += 1;
        assert!(more < 100, "the oldest never came into view");
        scroll_up(cx, 2_000.0);
    }
    assert_eq!(cx.update(|_, cx| view.read(cx).history()), History::Complete);
    assert!(cx.debug_bounds("message-1").is_some(), "the oldest is shown at the top");
    let counted = |cx: &mut VisualTestContext| cx.update(|_, cx| (view.read(cx).parses(), view.read(cx).layouts()));
    assert_eq!(counted(cx), (1_000, 1_000));
    arrive(rig, cx, &message("1001", LAB_B, "<p>one more</p>", NOON + MINUTE));
    assert_eq!(cx.update(|_, cx| view.read(cx).held()), 1_001);
    assert_eq!(counted(cx), (1_001, 1_001));
}

/// The test selector of the message `id`'s row (selectors live as long as the test).
fn row_of(id: &str) -> &'static str {
    Box::leak(format!("message-{id}").into_boxed_str())
}

/// The first message row at or below the list's top, by id.
fn read_from(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        let view = view.read(cx);
        let top = view.list_state().logical_scroll_top();
        view.items()[top.item_ix..].iter().find_map(|item| match item {
            Item::Message(message) => Some(message.id.clone()),
            _ => None,
        })
    })
    .expect("a message at the list's top")
}

#[test]
fn an_edit_above_the_view_and_a_new_message_below_it_leave_the_view_where_it_is() {
    let rig = rig_with(many(100));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    scroll_up(cx, 1_000.0);
    let oldest = ids(&view, cx)[0].clone();
    let read = read_from(&view, cx);
    assert_ne!(read, oldest);
    let before = cx.debug_bounds(row_of(&read)).expect("the message read is drawn");
    // one change edits the oldest message held and brings a new one
    {
        let mut messages = rig.core.messages.lock().unwrap();
        let stored = messages.get_mut(AB).unwrap();
        let edited = stored.iter_mut().find(|message| message.id == oldest).unwrap();
        edited.version = 2;
        edited.content = "<p>edited, and longer than it was before</p>".to_owned();
        edited.edit_time = Some(NOON);
        stored.push(message("101", LAB_B, "<p>meanwhile</p>", NOON + MINUTE));
    }
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    assert!(items(&view, cx).iter().any(|item| item.id == oldest && item.edited));
    assert_eq!(ids(&view, cx).last().map(String::as_str), Some("101"));
    let after = cx.debug_bounds(row_of(&read)).expect("the message read is still drawn");
    assert_eq!(after.origin, before.origin, "the view didn't move");
    assert!(cx.update(|_, cx| view.read(cx).scrolled_up()));
}

#[test]
fn an_older_page_landing_while_the_list_sits_at_its_top_keeps_what_was_read_in_view() {
    let rig = rig_with(many(1_000));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let held = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).held());
    let oldest = ids(&view, cx)[0].clone();
    let opened = held(cx);
    // the list sits at its very top (a fling got there before the next older page landed): the page lands now
    cx.update(|_, cx| {
        view.read(cx).list_state().scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        });
    });
    draw(cx);
    let settled = held(cx);
    assert!(settled > opened, "an older page landed");
    assert!(settled < 1_000, "the whole chat loaded");
    for _ in 0..5 {
        draw(cx);
    }
    assert_eq!(held(cx), settled, "no older page loads without scrolling on");
    assert!(
        cx.debug_bounds(row_of(&oldest)).is_some(),
        "the message at the top before the page landed is still in view"
    );
    assert_eq!(read_from(&view, cx), oldest);
}

#[test]
fn a_chat_never_synced_isn_t_its_beginning_and_loads_older_once_it_is_synced() {
    let all = many(70);
    let rig = rig_with(all[60..].to_vec());
    rig.core.unsynced.lock().unwrap().insert(AB.to_owned());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let history = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).history());
    assert_eq!(history(cx), History::Unsynced);
    assert!(cx.debug_bounds("load-older").is_some(), "the top offers to load again");
    assert_eq!(ids(&view, cx).len(), 10);

    // the catch-up syncs it, keeping where the service's older history starts
    rig.core.unsynced.lock().unwrap().clear();
    let mut first: Vec<Message> = all[20..60].to_vec();
    first.reverse();
    let mut second: Vec<Message> = all[..20].to_vec();
    second.reverse();
    rig.core
        .older
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![first, second]);
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    for _ in 0..3 {
        draw(cx);
    }
    let expected: Vec<String> = (1..=70).map(|n| n.to_string()).collect();
    assert_eq!(ids(&view, cx), expected);
    assert_eq!(history(cx), History::Complete);
}

#[test]
fn a_catch_up_between_the_older_load_and_the_history_read_leaves_the_stored_pages_reachable() {
    let all = many(PAGE * 2 + 20);
    let rig = rig_with(Vec::new());
    rig.core.unsynced.lock().unwrap().insert(AB.to_owned());
    // the view holds nothing, so its top asks for older: the service has nothing yet, and before the history is read
    // a catch-up stores the whole chat and marks its history complete
    rig.core
        .catch_up_before_history_complete
        .lock()
        .unwrap()
        .insert(AB.to_owned(), all.clone());
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let history = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).history());
    let held = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).held());
    assert_eq!(held(cx), 0);
    assert!(rig.core.catch_up_before_history_complete.lock().unwrap().is_empty());

    // the catch-up's change brings the window: the newest page, with the store's older pages behind it
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    assert!(held(cx) >= PAGE);
    assert_eq!(history(cx), History::More, "the store still has older pages");

    for _ in 0..10 {
        if history(cx) == History::Complete {
            break;
        }
        scroll_up(cx, 100_000.0);
        draw(cx);
    }
    let expected: Vec<String> = (1..=all.len()).map(|n| n.to_string()).collect();
    assert_eq!(ids(&view, cx), expected);
    assert_eq!(history(cx), History::Complete);
}

#[test]
fn older_loads_that_bring_nothing_stall_until_asked_again() {
    let fruitless = usize::try_from(MAX_FRUITLESS).unwrap();
    let rig = rig_with(many(10));
    // pages the store keeps nothing of, two more than the view asks for by itself
    rig.core
        .older
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Vec::new(); fruitless + 2]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    draw(cx);
    let history = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).history());
    let older_loads = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).loads().1);
    assert_eq!(history(cx), History::Stalled);
    assert_eq!(older_loads(cx), u64::from(MAX_FRUITLESS));
    draw(cx);
    assert_eq!(
        older_loads(cx),
        u64::from(MAX_FRUITLESS),
        "nothing asks again by itself"
    );
    // the top's button asks again: the two pages left bring nothing, then the service has none
    let again = cx.debug_bounds("load-older").expect("the top offers to load again");
    cx.simulate_click(again.center(), Modifiers::none());
    for _ in 0..3 {
        draw(cx);
    }
    assert_eq!(history(cx), History::Complete);
    assert_eq!(older_loads(cx), u64::from(MAX_FRUITLESS) + 3);
}

/// A message from b whose `properties` hold `properties` (as the store keeps them: JSON text).
fn with_properties(id: &str, at: i64, properties: &str) -> Message {
    let mut message = message(id, LAB_B, "<p>a made-up caption</p>", at);
    message.properties = Some(properties.to_owned());
    message
}

#[test]
fn a_file_or_card_no_capture_showed_is_said_never_guessed_and_opens_the_chat_on_the_web() {
    let file = r#"{"files":"[{\"fileName\":\"made-up.txt\"}]","cards":"[]","mentions":"[]"}"#;
    let rig = rig_with(vec![
        with_properties(
            "201",
            NOON,
            r#"{"files":"[]","cards":"[]","links":"[]","mentions":"[]"}"#,
        ),
        with_properties("202", NOON + MINUTE, file),
        with_properties("203", NOON + 2 * MINUTE, r#"{"cards":[{"made":"up"}]}"#),
        with_properties("204", NOON + 3 * MINUTE, "not an object"),
    ]);
    let (trace, written) = trace();
    let (view, cx) = chat_window(&rig, trace);
    let shown: Vec<(String, UnshownParts)> = items(&view, cx)
        .into_iter()
        .map(|item| (item.id, item.unshown))
        .collect();
    assert_eq!(
        shown,
        [
            ("201".to_owned(), UnshownParts::default()),
            (
                "202".to_owned(),
                UnshownParts {
                    files: true,
                    ..UnshownParts::default()
                }
            ),
            (
                "203".to_owned(),
                UnshownParts {
                    cards: true,
                    ..UnshownParts::default()
                }
            ),
            (
                "204".to_owned(),
                UnshownParts {
                    unreadable: true,
                    ..UnshownParts::default()
                }
            ),
        ]
    );
    // the text shows as ever, the part only as the line saying it can't be shown: never its name or link
    assert!(cx.debug_bounds("unshown-201").is_none());
    for selector in ["unshown-202", "unshown-203", "unshown-204"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    let text = written.text();
    assert!(!text.contains("made-up"), "the trace holds the part");
    let line = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "chat-view")
        .expect("a chat-view line");
    assert_eq!(
        line["kinds"],
        serde_json::json!(["text", "unshown", "unshown", "unshown"])
    );
    // the button opens the chat in the web client, through GPUI's opener (here the test platform's record)
    let open = cx.debug_bounds("open-on-the-web-202").expect("the button is drawn");
    cx.simulate_click(open.center(), Modifiers::none());
    assert_eq!(cx.opened_url(), Some(web_chat_link(AB)));
    assert!(web_chat_link(AB).starts_with("https://teams.microsoft.com/l/chat/19%3A"));
}

#[test]
fn a_message_of_a_type_not_shown_opens_the_chat_on_the_web_and_a_deleted_one_doesnt() {
    let mut card = with_properties("301", NOON, r#"{"cards":[{"made":"up"}]}"#);
    card.message_type = "RichText/Media_Card".to_owned();
    let mut plain = message("302", LAB_B, "", NOON + MINUTE);
    plain.message_type = "RichText/Media_Made_Up".to_owned();
    let mut gone = with_properties(
        "303",
        NOON + 2 * MINUTE,
        r#"{"files":"[{\"fileName\":\"made-up.txt\"}]"}"#,
    );
    gone.delete_time = Some(NOON + 3 * MINUTE);
    let rig = rig_with(vec![card, plain, gone]);
    let (trace, written) = trace();
    let (view, cx) = chat_window(&rig, trace);
    let unshown: Vec<bool> = items(&view, cx).into_iter().map(|item| item.unshown.any()).collect();
    assert_eq!(unshown, [false, false, false]);
    // the type's placeholder with the button; a deleted message says so, with neither line nor button
    for selector in [
        "unsupported-301",
        "open-on-the-web-301",
        "unsupported-302",
        "open-on-the-web-302",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    for selector in ["unsupported-303", "unshown-303", "open-on-the-web-303"] {
        assert!(cx.debug_bounds(selector).is_none(), "{selector}");
    }
    let line = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "chat-view")
        .expect("a chat-view line");
    assert_eq!(
        line["kinds"],
        serde_json::json!(["unsupported", "unsupported", "deleted"])
    );
    let open = cx.debug_bounds("open-on-the-web-301").expect("the button is drawn");
    cx.simulate_click(open.center(), Modifiers::none());
    assert_eq!(cx.opened_url(), Some(web_chat_link(AB)));
}

#[test]
fn a_deleted_message_shows_no_reactions() {
    let rig = rig_with(many(2));
    rig.core
        .reactions
        .lock()
        .unwrap()
        .insert("2".to_owned(), vec![reaction("like", LAB_A, NOON)]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    assert_eq!(items(&view, cx)[1].reactions.len(), 1);
    assert!(cx.debug_bounds("reaction-like").is_some());
    {
        let mut messages = rig.core.messages.lock().unwrap();
        let stored = messages
            .get_mut(AB)
            .unwrap()
            .iter_mut()
            .find(|message| message.id == "2")
            .unwrap();
        stored.version = 2;
        stored.content.clear();
        stored.delete_time = Some(NOON + MINUTE);
    }
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    draw(cx);
    let shown = items(&view, cx);
    assert_eq!(shown[1].body, Body::Deleted);
    assert!(shown[1].reactions.is_empty(), "{:?}", shown[1].reactions);
    assert!(cx.debug_bounds("reaction-like").is_none());
}

#[test]
fn typing_shows_who_is_typing_and_clears_by_its_end_a_timeout_or_their_message() {
    let rig = rig_with(many(2));
    let (trace, written) = trace();
    let (view, cx) = chat_window(&rig, trace);
    let typing = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).typing_text(cx));
    let signal = |rig: &Rig, from: &str, typing: bool| {
        rig.send(Event::Typing {
            conversation: AB.to_owned(),
            from: Some(from.to_owned()),
            typing,
        });
    };

    signal(&rig, LAB_B, true);
    draw(cx);
    assert_eq!(typing(cx).as_deref(), Some("Lab Bee is typing…"));
    assert!(cx.debug_bounds("typing").is_some());
    signal(&rig, LAB_B, false);
    draw(cx);
    assert_eq!(typing(cx), None);
    assert!(cx.debug_bounds("typing").is_none());

    // no further signal: it clears after the timeout, not before
    signal(&rig, LAB_B, true);
    cx.executor().advance_clock(TYPING_TIMEOUT / 2);
    draw(cx);
    assert!(typing(cx).is_some());
    // a newer signal starts the timeout again
    signal(&rig, LAB_B, true);
    cx.executor().advance_clock(TYPING_TIMEOUT * 3 / 4);
    draw(cx);
    assert!(
        typing(cx).is_some(),
        "the first signal's timeout doesn't clear the newer one"
    );
    cx.executor().advance_clock(TYPING_TIMEOUT / 2);
    draw(cx);
    assert_eq!(typing(cx), None);

    // a reaction on one of their messages is no message of theirs: it stays
    signal(&rig, LAB_B, true);
    rig.send(Event::Message {
        conversation: AB.to_owned(),
        id: "1".to_owned(),
        change: MessageChange::Reaction,
        applied: Applied::Updated,
        message: Some(Box::new(message("1", LAB_B, "<p>number 1</p>", NOON - MINUTE))),
        source: Source::Delivery,
    });
    draw(cx);
    assert!(typing(cx).is_some(), "a reaction doesn't end their typing");

    // their message ends it
    signal(&rig, LAB_B, true);
    arrive(&rig, cx, &message("3", LAB_B, "<p>done typing</p>", NOON + MINUTE));
    assert_eq!(typing(cx), None);

    // the account's own typing (another of its devices) isn't shown
    signal(&rig, LAB_A, true);
    draw(cx);
    assert_eq!(typing(cx), None);

    let changes: Vec<String> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "typing")
        .map(|line| line["change"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(changes, ["shown", "cleared", "shown", "cleared", "shown", "cleared"]);
}

#[test]
fn hover_and_keyboard_actions_offer_edit_and_delete_on_the_account_s_own_only() {
    let mut gone = message("3", LAB_A, "", NOON + 2 * MINUTE);
    gone.delete_time = Some(NOON + 3 * MINUTE);
    let rig = rig_with(vec![
        message("1", LAB_B, "<p>theirs</p>", NOON),
        message("2", LAB_A, "<p>mine</p>", NOON + MINUTE),
        gone,
    ]);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let events: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let seen = Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let took = |events: &Rc<RefCell<Vec<ChatViewEvent>>>| std::mem::take(&mut *events.borrow_mut());
    let action = |action: MessageAction, message: &str| ChatViewEvent::Action {
        action,
        message: message.to_owned(),
    };

    // under the pointer: someone else's message offers reply and react
    let theirs = cx.debug_bounds("message-1").unwrap();
    cx.simulate_mouse_move(theirs.center(), None, Modifiers::none());
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).hovered().map(str::to_owned)).as_deref(),
        Some("1")
    );
    assert!(cx.debug_bounds("action-reply").is_some() && cx.debug_bounds("action-react").is_some());
    assert!(cx.debug_bounds("action-edit").is_none() && cx.debug_bounds("action-delete").is_none());
    // the account's own: edit and delete too, and a click tells it
    let mine = cx.debug_bounds("message-2").unwrap();
    cx.simulate_mouse_move(mine.center(), None, Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("action-edit").is_some() && cx.debug_bounds("action-delete").is_some());
    let edit = cx.debug_bounds("action-edit").unwrap();
    cx.simulate_click(edit.center(), Modifiers::none());
    draw(cx);
    assert_eq!(took(&events), [action(MessageAction::Edit, "2")]);

    // the keyboard, once the list has the focus: Up from the newest (a deleted one offers nothing)
    cx.update(|window, cx| {
        view.update(cx, |view, cx| view.select(None, cx));
        view.read(cx).focus_handle().clone().focus(window, cx);
    });
    cx.simulate_keystrokes("up");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected().map(str::to_owned))
            .as_deref(),
        Some("3")
    );
    cx.simulate_keystrokes("e delete r a");
    assert!(took(&events).is_empty(), "a deleted message offers no action");
    // an edit and a reply go on in the compose box, which takes the focus: the list gets it back for the next key
    let focus_list = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    };
    cx.simulate_keystrokes("up e");
    focus_list(cx);
    cx.simulate_keystrokes("r");
    focus_list(cx);
    assert_eq!(
        took(&events),
        [action(MessageAction::Edit, "2"), action(MessageAction::Reply, "2")]
    );
    cx.simulate_keystrokes("up e delete r");
    focus_list(cx);
    cx.simulate_keystrokes("a");
    assert_eq!(
        took(&events),
        [action(MessageAction::Reply, "1"), action(MessageAction::React, "1")],
        "edit and delete only on the account's own"
    );
    cx.simulate_keystrokes("up");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected().map(str::to_owned))
            .as_deref(),
        Some("1")
    );
    // Escape closes the quick reactions react opened first, then the selection
    cx.simulate_keystrokes("escape");
    assert_eq!(cx.update(|_, cx| view.read(cx).reacting().map(str::to_owned)), None);
    cx.simulate_keystrokes("escape");
    assert_eq!(cx.update(|_, cx| view.read(cx).selected().map(str::to_owned)), None);
}

#[test]
fn a_selection_moved_up_with_the_keyboard_stays_in_view() {
    let rig = rig_with(many(100));
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    for _ in 0..30 {
        cx.simulate_keystrokes("up");
        draw(cx);
    }
    let selected = cx.update(|_, cx| view.read(cx).selected().map(str::to_owned));
    assert_eq!(selected.as_deref(), Some("71"));
    assert!(
        cx.update(|_, cx| view.read(cx).scrolled_up()),
        "the list no longer follows the newest"
    );
    let list = cx.debug_bounds("chat-messages").unwrap();
    let row = cx.debug_bounds("message-71").expect("the selected message is drawn");
    assert!(
        row.top() >= list.top() - px(1.0) && row.bottom() <= list.bottom() + px(1.0),
        "the selected message is in view: {row:?} in {list:?}"
    );
}

#[test]
fn the_shell_shows_the_chat_opened_in_its_view() {
    let rig = rig_with(many(3));
    rig.core.messages.lock().unwrap().insert(AC.to_owned(), Vec::new());
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| {
            cx.new(|_| {
                SettingsModel::in_memory(Settings {
                    selected_chat: Some(AB.to_owned()),
                    ..Settings::default()
                })
            })
        }),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    let opened = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            shell
                .read(cx)
                .chat()
                .map(|chat| chat.read(cx).conversation().to_owned())
        })
    };
    assert_eq!(
        opened(cx).as_deref(),
        Some(AB),
        "the chat kept in the settings opens in its view"
    );
    assert!(cx.debug_bounds("chat-view").is_some());
    assert!(cx.debug_bounds("message-3").is_some());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AC.to_owned(), window, cx)));
    draw(cx);
    assert_eq!(opened(cx).as_deref(), Some(AC));
    assert!(cx.debug_bounds("message-3").is_none());
    assert_eq!(rig.shutdowns(), 0, "a view never stops the engine");
}
