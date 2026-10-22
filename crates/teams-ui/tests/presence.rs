//! Others' presence in the views (M2 Task 17) on GPUI's test context, over the fake engine (`common`): the engine is
//! asked about the people the views show (a 1:1 chat's other member in the chat list, the chat's members in the
//! mention picker), and each `Event::Presence` the bridge applies is drawn as a dot on the chat list's avatar, in the
//! chat's header and beside each person the picker offers; "unknown" (a hollow ring) before the first and after a
//! failed read; a group chat's row shows none. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use teams_core::{Availability, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::chat_view::ChatView;
use teams_ui::presence::{fill, label};
use teams_ui::settings::Settings;
use teams_ui::shell::Shell;
use teams_ui::{Presences, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, sender: &str, arrival: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: arrival.to_string(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: "<p>hi</p>".to_owned(),
        properties: None,
        arrival_time: arrival,
        compose_time: Some(arrival),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the a–b chat and the trio, b and c named.
fn seed(core: &FakeCore) {
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, 3_000),
        chat(TRIO, ConversationKind::Group, Some("Lab trio"), 2_000),
    ]);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), vec![message(AB, LAB_B, 3_000)]);
    messages.insert(TRIO.to_owned(), vec![message(TRIO, LAB_C, 2_000)]);
    drop(messages);
    let mut members = core.members.lock().unwrap();
    members.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    members.insert(
        TRIO.to_owned(),
        vec![
            Member::new(LAB_A, None),
            Member::new(LAB_B, None),
            Member::new(LAB_C, None),
        ],
    );
    drop(members);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
}

fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig.core);
    rig.cx.update(teams_ui::window::init);
    rig
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

fn told(rig: &Rig, mri: &str, availability: Availability) {
    rig.send(Event::Presence {
        mri: mri.to_owned(),
        availability,
    });
}

/// The shell with the a–b chat open, in a window of its own.
fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_traced(rig, Arc::new(ViewTrace::off()))
}

/// [`shell_window`], its view trace `trace`.
fn shell_traced(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let settings = Settings {
        selected_chat: Some(AB.to_owned()),
        ..Settings::default()
    };
    let parts = parts::parts(rig, settings, trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    draw(cx);
    (shell, cx)
}

/// The presence each listed row shows, by chat id.
fn rows_presence(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Vec<(String, Option<Availability>)> {
    cx.update(|_, cx| {
        let list = shell.read(cx).list().read(cx);
        list.rows()
            .iter()
            .map(|row| (row.id.clone(), list.presence_of(row, cx)))
            .collect()
    })
}

fn header_presence(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<Availability> {
    cx.update(|_, cx| shell.read(cx).header_presence(cx))
}

#[test]
fn a_one_to_one_chat_shows_its_other_members_presence_in_the_list_and_the_header() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);

    // the engine was asked about b, the a–b chat's other member, and nobody else (a group's row shows none)
    assert_eq!(*rig.core.presence_asked.lock().unwrap(), [LAB_B]);
    assert_eq!(
        rows_presence(&shell, cx),
        [(AB.to_owned(), Some(Availability::Unknown)), (TRIO.to_owned(), None)]
    );
    assert_eq!(header_presence(&shell, cx), Some(Availability::Unknown));
    assert!(
        cx.debug_bounds("presence-unknown").is_some(),
        "a hollow ring until it is read"
    );
    assert!(cx.debug_bounds("chat-header-presence").is_some());

    told(&rig, LAB_B, Availability::Available);
    draw(cx);
    assert_eq!(rows_presence(&shell, cx)[0].1, Some(Availability::Available));
    assert_eq!(header_presence(&shell, cx), Some(Availability::Available));
    assert!(cx.debug_bounds("presence-available").is_some());
    assert!(cx.debug_bounds("presence-unknown").is_none());

    // a read that failed: unknown again
    told(&rig, LAB_B, Availability::Unknown);
    draw(cx);
    assert_eq!(header_presence(&shell, cx), Some(Availability::Unknown));
    assert!(cx.debug_bounds("presence-available").is_none());

    // someone else's presence changes nothing shown
    told(&rig, LAB_C, Availability::Busy);
    draw(cx);
    assert_eq!(rows_presence(&shell, cx)[0].1, Some(Availability::Unknown));
    assert!(cx.debug_bounds("presence-busy").is_none());
}

#[test]
fn the_trace_carries_no_presence_and_a_stopped_engine_leaves_it_unknown() {
    let rig = seeded();
    let written = Written::default();
    let (shell, cx) = shell_traced(&rig, Arc::new(ViewTrace::new(written.clone())));
    told(&rig, LAB_B, Availability::Busy);
    draw(cx);
    assert!(rig.core.asked().contains(&"presence"));
    assert!(!written.text().contains("Busy"), "the trace names no one's presence");
    assert!(written.lines().iter().any(|line| line["view"] == "chat-list"));

    let shutdown = cx.update(|_, cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown));
    shutdown.detach();
    draw(cx);
    assert_eq!(rig.shutdowns(), 1);
    // what was told stays drawn; nothing new comes once the engine stopped
    assert_eq!(header_presence(&shell, cx), Some(Availability::Busy));
}

#[test]
fn the_group_chats_header_shows_no_presence() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);

    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(TRIO.to_owned(), window, cx)));
    draw(cx);

    assert_eq!(header_presence(&shell, cx), None);
    assert!(cx.debug_bounds("chat-header-presence").is_none());
}

#[test]
fn the_mention_picker_shows_each_members_presence() {
    let rig = seeded();
    let parts = parts::parts(&rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, TRIO.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    draw(cx);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    let shown = |cx: &mut VisualTestContext| cx.update(|_, cx| compose.read(cx).picker_presence(cx));
    assert!(shown(cx).is_empty(), "no picker, no dots");

    told(&rig, LAB_C, Availability::DoNotDisturb);
    cx.simulate_input("@");
    draw(cx);

    // the members but the account, by name, each asked about as the picker read them
    assert_eq!(
        shown(cx),
        [
            (LAB_B.to_owned(), Availability::Unknown),
            (LAB_C.to_owned(), Availability::DoNotDisturb)
        ]
    );
    let asked = rig.core.presence_asked.lock().unwrap().clone();
    assert!(
        asked.contains(&LAB_B.to_owned()) && asked.contains(&LAB_C.to_owned()),
        "{asked:?}"
    );
    assert!(
        !asked.contains(&LAB_A.to_owned()),
        "never the account itself: {asked:?}"
    );
    assert!(cx.debug_bounds("presence-do-not-disturb").is_some());

    // a change while it is open is drawn
    told(&rig, LAB_B, Availability::Away);
    draw(cx);
    assert_eq!(shown(cx)[0], (LAB_B.to_owned(), Availability::Away));
    assert!(cx.debug_bounds("presence-away").is_some());
}

#[test]
fn the_model_keeps_what_the_engine_told_and_says_unknown_for_anyone_else() {
    let rig = Rig::start();
    let presence = |mri: &str| rig.cx.read(|cx| rig.models.presence.read(cx).of(mri));
    assert_eq!(presence(LAB_B), Availability::Unknown);

    told(&rig, LAB_B, Availability::Offline);
    told(&rig, LAB_C, Availability::Other("InAMeeting".to_owned()));

    assert_eq!(presence(LAB_B), Availability::Offline);
    assert_eq!(presence(LAB_C), Availability::Other("InAMeeting".to_owned()));
    assert_eq!(presence(LAB_A), Availability::Unknown);
    assert_eq!(Presences::default().of(LAB_B), Availability::Unknown);
}

#[test]
fn every_availability_has_a_label_and_only_named_ones_a_fill() {
    let cases = [
        (Availability::Available, "Available", true),
        (Availability::AvailableIdle, "Available, idle", true),
        (Availability::Away, "Away", true),
        (Availability::BeRightBack, "Be right back", true),
        (Availability::Busy, "Busy", true),
        (Availability::BusyIdle, "Busy, idle", true),
        (Availability::DoNotDisturb, "Do not disturb", true),
        (Availability::Offline, "Offline", true),
        (Availability::Other("InAMeeting".to_owned()), "InAMeeting", false),
        (Availability::Unknown, "Unknown", false),
    ];
    for (availability, text, filled) in cases {
        assert_eq!(label(&availability).as_ref(), text);
        assert_eq!(fill(&availability).is_some(), filled, "{availability:?}");
    }
    assert_ne!(fill(&Availability::Available), fill(&Availability::Busy));
    assert_ne!(fill(&Availability::Busy), fill(&Availability::Away));
    assert_ne!(fill(&Availability::Away), fill(&Availability::Offline));
}
