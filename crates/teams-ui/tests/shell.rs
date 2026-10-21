//! The shell, the chat list, the chat switcher and the settings view on GPUI's test context, over the fake engine
//! (`common`): the three panes and the list pane's resizing; the chats by last activity with their names, previews,
//! times and unread counts from the store, channels and own streams left out, kept up to date by the engine's events
//! (a catch-up's too); the connection state; Ctrl+K's switcher, fuzzy, Enter opening, Escape closing; the selected chat
//! kept in the settings and opened again; the settings view changing the settings. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px};
use teams_core::sync::CatchUpReport;
use teams_core::{Cause, Event, MessageChange, Source};
use teams_store::{Applied, Conversation, ConversationKind, Member, Message};
use teams_trouter::{Down, Stop};
use teams_ui::chat_list::{ChatList, DELETED, RowKind, format_time, listed, one_to_one_partner, preview};
use teams_ui::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use teams_ui::shell::{LIST_MAX, LIST_MIN, LIST_WIDTH, Main, RailItem, Shell, ShellParts};
use teams_ui::switcher::{fuzzy_score, matches};
use teams_ui::view::{Shown, status_text};
use teams_ui::{Root, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const UNNAMED: &str = "19:00000000000000000000000000unnamed@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
const NOTIFICATIONS: &str = "48:notifications";

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, content: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

fn members(mris: &[&str]) -> Vec<Member> {
    mris.iter().map(|mri| Member::new(mri, None)).collect()
}

/// a's store: the a–b chat (b named, two unread, its newest text after a thread event), the a–c chat (nothing of c
/// stored but its id, a's own message last), the trio by its topic, an unnamed group (only b named), a team's channel
/// and a's notifications stream.
fn seed(core: &FakeCore) {
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, 3_000),
        chat(AC, ConversationKind::OneToOne, None, 2_000),
        chat(TRIO, ConversationKind::Group, Some("Lab trio"), 4_000),
        chat(UNNAMED, ConversationKind::Group, Some("  "), 1_000),
        chat(CHANNEL, ConversationKind::Channel, Some("General"), 5_000),
        chat(
            NOTIFICATIONS,
            ConversationKind::Other("notifications".to_owned()),
            None,
            6_000,
        ),
    ]);
    let mut event = message(AB, "3001", LAB_B, "<addmember/>", 3_001);
    event.message_type = "ThreadActivity/AddMember".to_owned();
    let mut gone = message(UNNAMED, "999", LAB_B, "", 999);
    gone.delete_time = Some(1_000);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(
        AB.to_owned(),
        vec![
            event,
            message(AB, "3000", LAB_B, "<p>hello <b>there</b></p><p>second line</p>", 3_000),
        ],
    );
    messages.insert(AC.to_owned(), vec![message(AC, "2000", LAB_A, "<p>mine</p>", 2_000)]);
    messages.insert(UNNAMED.to_owned(), vec![gone]);
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    stored.insert(TRIO.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    stored.insert(UNNAMED.to_owned(), members(&[LAB_A, LAB_B, LAB_C]));
    drop(stored);
    core.unread.lock().unwrap().insert(AB.to_owned(), 2);
    core.names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// The rig with a's store seeded, and the kit set up.
fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig.core);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn parts(rig: &Rig, settings: Settings, trace: Arc<ViewTrace>) -> ShellParts {
    // the views' request side, as the app hands it: the bridge's
    ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace,
    }
}

/// The shell in a window of its own.
fn shell_window(
    rig: &Rig,
    settings: Settings,
    trace: Arc<ViewTrace>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts(rig, settings, trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    (shell, cx)
}

fn list_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatList> {
    cx.update(|_, cx| shell.read(cx).list().clone())
}

fn ids(list: &Entity<ChatList>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| list.read(cx).rows().iter().map(|row| row.id.clone()).collect())
}

/// Draws the window again and gives the bounds of a selector.
fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn the_list_shows_the_chats_by_last_activity_with_names_previews_and_unread_counts() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    let list = list_of(&shell, cx);

    let rows = cx.update(|_, cx| list.read(cx).rows().to_vec());
    let shown: Vec<(&str, RowKind, &str, bool, &str, u64)> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.kind,
                row.name.as_ref(),
                row.named,
                row.preview.as_ref(),
                row.unread,
            )
        })
        .collect();
    let unnamed = format!("Lab Bee, {LAB_C}");
    assert_eq!(
        shown,
        [
            (TRIO, RowKind::Group, "Lab trio", true, "", 0),
            (AB, RowKind::OneToOne, "Lab Bee", true, "hello there", 2),
            (AC, RowKind::OneToOne, LAB_C, false, "You: mine", 0),
            (UNNAMED, RowKind::Group, unnamed.as_str(), false, DELETED, 0),
        ]
    );
    assert!(rows.iter().all(|row| row.last_activity.is_some()));

    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(line["conversations"], serde_json::json!([TRIO, AB, AC, UNNAMED]));
    assert_eq!(line["unread"], serde_json::json!([0, 2, 0, 0]));
    assert_eq!(
        line["kinds"],
        serde_json::json!(["group", "one-to-one", "one-to-one", "group"])
    );
    let all = written.text();
    for text in ["Lab Bee", "Lab trio", "hello", "mine"] {
        assert!(!all.contains(text), "{text} reached the trace");
    }
}

#[test]
fn channels_and_the_account_s_own_streams_aren_t_chats() {
    assert!(!listed(&chat(CHANNEL, ConversationKind::Channel, None, 1)));
    assert!(!listed(&chat(
        NOTIFICATIONS,
        ConversationKind::Other("x".to_owned()),
        None,
        1
    )));
    assert!(!listed(&chat("48:notes", ConversationKind::Group, None, 1)));
    assert!(listed(&chat(AB, ConversationKind::OneToOne, None, 1)));
    assert!(listed(&chat(TRIO, ConversationKind::Meeting, None, 1)));
    assert_eq!(one_to_one_partner(AB, LAB_A).as_deref(), Some(LAB_B));
    assert_eq!(one_to_one_partner(AB, LAB_B).as_deref(), Some(LAB_A));
    assert_eq!(one_to_one_partner(AB, LAB_C), None, "not a's own chat");
    assert_eq!(one_to_one_partner(TRIO, LAB_A), None);
}

#[test]
fn a_row_s_time_is_the_time_of_day_today_then_the_weekday_then_the_day() {
    use chrono::TimeZone as _;
    let zone = chrono::FixedOffset::east_opt(3 * 3600).expect("offset");
    // Wednesday 21 October 2026, 15:00 local
    let now = zone.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).single().expect("now");
    let at = |y, mo, d, h, mi| {
        zone.with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .expect("time")
            .timestamp_millis()
    };
    assert_eq!(format_time(at(2026, 10, 21, 9, 5), &now), "09:05");
    assert_eq!(format_time(at(2026, 10, 19, 9, 5), &now), "Mon");
    assert_eq!(format_time(at(2026, 10, 1, 9, 5), &now), "1 Oct");
    assert_eq!(format_time(at(2025, 12, 31, 9, 5), &now), "31 Dec 2025");
}

#[test]
fn the_list_follows_the_engine_s_events_and_builds_again_only_what_changed() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let members_asked = |rig: &Rig| rig.core.asked().iter().filter(|name| **name == "members").count();
    let before = members_asked(&rig);
    assert_eq!(before, 4, "one row built per chat");

    // a new message in the a–c chat: it moves to the top with its unread count; nothing else is built again
    {
        let mut stored = rig.core.conversations.lock().unwrap();
        stored.iter_mut().find(|chat| chat.id == AC).expect("ac").last_activity = Some(9_000);
    }
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(AC)
        .expect("ac")
        .insert(0, message(AC, "9000", LAB_C, "<p>new from c</p>", 9_000));
    rig.core.unread.lock().unwrap().insert(AC.to_owned(), 1);
    rig.send(Event::Message {
        conversation: AC.to_owned(),
        id: "9000".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: None,
        source: Source::Delivery,
    });
    cx.run_until_parked();
    assert_eq!(ids(&list, cx), [AC, TRIO, AB, UNNAMED]);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.preview.as_ref(), ac.unread), ("new from c", 1));
    assert_eq!(members_asked(&rig), before + 1);

    // a catch-up may bring names from any chat: every row is built again
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    rig.send(Event::CaughtUp {
        cause: Cause::Connected,
        report: CatchUpReport::default(),
    });
    cx.run_until_parked();
    assert_eq!(members_asked(&rig), before + 1 + 4);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), ("Lab Sea", true));
}

#[test]
fn a_name_delivered_in_one_chat_renames_every_row_named_by_that_person() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let members_asked = |rig: &Rig| rig.core.asked().iter().filter(|name| **name == "members").count();
    let before = members_asked(&rig);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), (LAB_C, false));

    // c's first named message comes live in the trio: the store now names c
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    let mut named = message(TRIO, "9100", LAB_C, "<p>hi all</p>", 9_100);
    named.sender_name = Some("Lab Sea".to_owned());
    rig.send(Event::Message {
        conversation: TRIO.to_owned(),
        id: "9100".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(named)),
        source: Source::Delivery,
    });
    cx.run_until_parked();

    // the trio (its own change), and the two rows named by c; the a–b row, named by b alone, is kept
    assert_eq!(members_asked(&rig), before + 3);
    let ac = cx.update(|_, cx| list.read(cx).row(AC).cloned()).expect("row");
    assert_eq!((ac.name.as_ref(), ac.named), ("Lab Sea", true));
    let unnamed = cx.update(|_, cx| list.read(cx).row(UNNAMED).cloned()).expect("row");
    assert_eq!((unnamed.name.as_ref(), unnamed.named), ("Lab Bee, Lab Sea", true));
}

#[test]
fn a_text_message_s_preview_is_its_text_as_it_came() {
    let mut plain = message(AB, "1", LAB_B, "Use <name> here &amp; there\nsecond line", 1);
    plain.message_type = "Text".to_owned();
    assert_eq!(preview(&[plain], LAB_A).as_ref(), "Use <name> here &amp; there");
    let rich = message(AB, "2", LAB_B, "<p>a &amp; <b>b</b></p>", 2);
    assert_eq!(preview(&[rich], LAB_A).as_ref(), "a & b");
}

#[test]
fn a_chat_whose_id_the_trace_refuses_leaves_the_trace_whole_rows_in_step() {
    const ODD: &str = "19:odd.thread@thread.v2";
    let rig = seeded();
    rig.core
        .conversations
        .lock()
        .unwrap()
        .push(chat(ODD, ConversationKind::Meeting, Some("Odd"), 3_500));
    rig.core.unread.lock().unwrap().insert(ODD.to_owned(), 5);
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    let list = list_of(&shell, cx);
    assert_eq!(ids(&list, cx), [TRIO, ODD, AB, AC, UNNAMED], "the list shows it");

    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(line["conversations"], serde_json::json!([TRIO, AB, AC, UNNAMED]));
    assert_eq!(
        line["kinds"],
        serde_json::json!(["group", "one-to-one", "one-to-one", "group"])
    );
    assert_eq!(line["unread"], serde_json::json!([0, 2, 0, 0]));
    assert_eq!(line["refused"], 1);
}

#[test]
fn a_stopped_engine_leaves_the_rows_shown() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    let loads = cx.update(|_, cx| list.read(cx).loads());
    *rig.core.stopped.lock().unwrap() = true;
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| list.read(cx).loads()), loads + 1, "it asked again");
    assert_eq!(ids(&list, cx), [TRIO, AB, AC, UNNAMED]);
}

#[test]
fn the_engine_shut_down_leaves_the_rows_shown_and_the_list_offline() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let list = list_of(&shell, cx);
    rig.cx
        .update(|cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown).detach());
    rig.cx.run_until_parked();
    assert_eq!(rig.shutdowns(), 1);
    assert_eq!(ids(&list, cx), [TRIO, AB, AC, UNNAMED]);
    assert_eq!(
        rig.cx.read(|cx| status_text(rig.models.status.read(cx))).as_ref(),
        "Offline"
    );
}

#[test]
fn the_connection_state_follows_the_push_channel() {
    let rig = Rig::start();
    let text = |rig: &Rig| rig.cx.read(|cx| status_text(rig.models.status.read(cx)).to_string());
    assert_eq!(text(&rig), "Connecting…");
    rig.send(Event::Connected {
        endpoint: "00000000-0000-4000-8000-0000000000e1".to_owned(),
    });
    assert_eq!(text(&rig), "Connected");
    rig.send(Event::Disconnected {
        down: Down::Closed,
        heard: None,
    });
    assert_eq!(text(&rig), "Reconnecting…");
    rig.send(Event::PushStopped(Stop::Requested));
    assert_eq!(text(&rig), "Offline");
    assert_eq!(rig.cx.read(|cx| Shown::of(rig.models.status.read(cx))), Shown::Offline);
}

#[test]
fn the_shell_has_three_panes_and_the_list_pane_resizes_within_its_range() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let rail = bounds(cx, "rail").expect("the rail");
    let list = bounds(cx, "list-pane").expect("the list pane");
    let main = bounds(cx, "conversation-pane").expect("the conversation pane");
    assert!(
        rail.right() <= list.left() && list.right() <= main.left(),
        "{rail:?} {list:?} {main:?}"
    );
    assert_eq!(list.size.width, px(LIST_WIDTH));
    assert!(main.size.width > px(0.0));

    let panes = cx.update(|_, cx| shell.read(cx).panes().clone());
    for (asked, got) in [(400.0, 400.0), (100.0, LIST_MIN), (2_000.0, LIST_MAX)] {
        cx.update(|window, cx| panes.update(cx, |panes, cx| panes.resize_panel(0, px(asked), window, cx)));
        let list = bounds(cx, "list-pane").expect("the list pane");
        assert_eq!(list.size.width, px(got), "asked {asked}");
        let main = bounds(cx, "conversation-pane").expect("the conversation pane");
        assert!(list.right() <= main.left());
    }
}

#[test]
fn the_rail_switches_the_list_pane_and_opens_the_settings() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    assert!(bounds(cx, "chat-row-0").is_some());
    click(cx, "rail-teams");
    assert_eq!(cx.update(|_, cx| shell.read(cx).rail()), RailItem::Teams);
    assert!(bounds(cx, "teams-list").is_some());
    assert!(bounds(cx, "chat-row-0").is_none());
    click(cx, "rail-settings");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Settings);
    assert!(bounds(cx, "settings").is_some());
    click(cx, "rail-chats");
    assert_eq!(cx.update(|_, cx| shell.read(cx).rail()), RailItem::Chats);
    assert!(bounds(cx, "chat-row-0").is_some());
}

#[test]
fn a_chat_clicked_opens_and_is_kept_as_the_selected_chat() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
    click(cx, "chat-row-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(AB.to_owned())
    );
    assert_eq!(settings_of(&shell, cx).selected_chat.as_deref(), Some(AB));
    let opened = written
        .lines()
        .into_iter()
        .find(|line| line["view"] == "chat" && line["change"] == "opened")
        .expect("traced");
    assert_eq!(opened["conversation"], AB);
    assert!(bounds(cx, "chat-header").is_some());
}

#[test]
fn the_selected_chat_is_opened_again_at_the_start() {
    let rig = seeded();
    let settings = Settings {
        selected_chat: Some(AC.to_owned()),
        ..Settings::default()
    };
    let (shell, cx) = shell_window(&rig, settings, Arc::new(ViewTrace::off()));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AC.to_owned())
    );
    let list = list_of(&shell, cx);
    assert_eq!(
        cx.update(|_, cx| list.read(cx).selected().map(str::to_owned)),
        Some(AC.to_owned())
    );
}

#[test]
fn ctrl_k_s_switcher_matches_fuzzily_and_enter_opens_the_best_match() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Settings::default(), trace);
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert_eq!(
        cx.update(|_, cx| switcher.read(cx).matches().len()),
        4,
        "every chat at first"
    );

    cx.simulate_input("lbe");
    cx.run_until_parked();
    let names: Vec<String> = cx.update(|_, cx| {
        switcher
            .read(cx)
            .matches()
            .iter()
            .map(|(_, name)| name.to_string())
            .collect()
    });
    assert_eq!(names.first().map(String::as_str), Some("Lab Bee"), "{names:?}");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(AB.to_owned())
    );
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_none()),
        "closed once a chat opened"
    );
    let changes: Vec<String> = written
        .lines()
        .iter()
        .filter(|line| line["view"] == "switcher" || line["view"] == "chat")
        .map(|line| format!("{} {}", line["view"], line["change"]))
        .collect();
    assert_eq!(
        changes,
        [r#""switcher" "opened""#, r#""chat" "opened""#, r#""switcher" "closed""#]
    );
}

#[test]
fn escape_closes_the_switcher_whatever_was_typed_and_ctrl_k_toggles_it() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("trio");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_some()));
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);

    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_some()));
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
}

#[test]
fn a_fuzzy_match_takes_the_query_s_characters_in_order_and_ranks_word_starts_and_runs() {
    assert_eq!(fuzzy_score("", "anything"), Some(0));
    assert!(fuzzy_score("lt", "Lab trio").is_some());
    assert!(
        fuzzy_score("LAB TRIO", "lab trio").is_some(),
        "any case, spaces left out"
    );
    assert_eq!(fuzzy_score("oil", "Lab trio"), None, "out of order");
    assert_eq!(fuzzy_score("x", "Lab trio"), None);
    assert!(
        fuzzy_score("tri", "Lab trio") > fuzzy_score("tri", "a t r i"),
        "a run counts more"
    );
    assert!(
        fuzzy_score("b", "Lab Bee") > fuzzy_score("b", "Lab"),
        "a word's start counts more"
    );

    let rows = vec![
        ("1".to_owned(), "Abacus".into()),
        ("2".to_owned(), "Bob".into()),
        ("3".to_owned(), "Rob B".into()),
        ("4".to_owned(), "Zed".into()),
    ];
    let found: Vec<String> = matches("b", &rows).into_iter().map(|(id, _)| id).collect();
    // word starts first (Bob, then Rob B in the list's order), then the rest in the list's order
    assert_eq!(found, ["2", "3", "1"]);
}

#[test]
fn the_settings_view_changes_the_settings() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    cx.update(|_, cx| shell.update(cx, Shell::show_settings));
    for selector in [
        "send-typing",
        "send-read-receipts",
        "notifications-mentions",
        "idle-15",
        "status-do-not-disturb",
    ] {
        click(cx, selector);
    }
    let settings = settings_of(&shell, cx);
    assert_eq!(
        settings,
        Settings {
            send_typing: false,
            send_read_receipts: false,
            notifications: Notifications::Mentions,
            idle_minutes: 15,
            manual_status: ManualStatus::DoNotDisturb,
            ..Settings::default()
        }
    );
}

/// The settings the shell keeps.
fn settings_of(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Settings {
    cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().clone())
}

#[test]
fn the_root_shows_the_shell_until_the_account_is_signed_out() {
    let rig = seeded();
    let parts = parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let models = rig.models.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (_, cx) = app.add_window_view(|window, cx| {
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        Root::new(models, Arc::new(ViewTrace::off()), cx).with_shell(shell)
    });
    assert!(bounds(cx, "rail").is_some());
    rig.cx
        .update(|cx| rig.models.signed_out("run `proto-lab auth a`".into(), cx));
    assert!(bounds(cx, "rail").is_none(), "only the sign-in hint is left");
}
