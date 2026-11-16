//! The avatar of who a call is with in its window on GPUI's test context, over the fake engine (`common`): a group
//! chat's call shows the chat's avatar as its list row and its header show it, its two members split; a 1:1 chat's
//! call shows the other member's initials. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;

use std::rc::Rc;
use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::{CallId, CallStatus, Event, RegistrationReport};
use teams_proto::Mri;
use teams_store::{Conversation, ConversationKind, Member};
use teams_ui::ViewTrace;
use teams_ui::call_window::{AccountNamer, CallWindows};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};

const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(1_000),
    }
}

/// a's store: the a–b chat, and the trio by its topic, b and c named.
fn rig() -> Rig {
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(Some(Mri::new(ECHO)), cx));
    rig.core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None),
        chat(TRIO, ConversationKind::Group, Some("Lab trio")),
    ]);
    let mut members = rig.core.members.lock().unwrap();
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
    let mut names = rig.core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Cee".to_owned());
    drop(names);
    rig.cx.update(teams_ui::window::init);
    rig
}

/// The shell in a window of its own on `chat`, with the call windows, whose namer reads the shell's chat list as the
/// app's does.
fn app(rig: &Rig, chat: &str) -> (&'static mut VisualTestContext, Entity<CallWindows>) {
    let settings = Settings {
        selected_chat: Some(chat.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace: Arc::new(ViewTrace::off()),
    };
    let windows = rig.cx.update(|cx| {
        let windows = cx.new(|cx| CallWindows::new("ngteams-test", cx));
        windows.update(cx, |windows, cx| windows.add(parts.clone(), cx));
        windows
    });
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    let namer: AccountNamer =
        Rc::new(move |_: &str, chat: &str, cx: &gpui::App| shell.read(cx).list().read(cx).row(chat).cloned());
    rig.cx
        .update(|cx| windows.update(cx, |windows, _| windows.set_namer(namer)));
    draw(cx);
    rig.send(Event::CallingRegistration(RegistrationReport::Registered));
    (cx, windows)
}

fn stage(rig: &Rig, windows: &Entity<CallWindows>) -> &'static mut VisualTestContext {
    rig.cx.run_until_parked();
    let window = rig
        .cx
        .read(|cx| windows.read(cx).window())
        .expect("the call window is open");
    let cx = Box::leak(Box::new(VisualTestContext::from_window(window.into(), &rig.cx)));
    draw(cx);
    cx
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let found = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn a_group_chat_s_call_shows_the_chat_s_avatar_as_its_row_does() {
    let rig = rig();
    let (cx, windows) = app(&rig, TRIO);
    assert!(drawn(cx, "chat-header-avatar-split"), "the header splits b and c");
    click(cx, "chat-call");
    rig.send(Event::Call {
        call: CallId::for_tests(1),
        status: CallStatus::Ringing,
    });
    let stage = stage(&rig, &windows);
    assert!(
        drawn(stage, "call-calling-split"),
        "the call splits them as the chat does"
    );
    assert!(!drawn(stage, "call-calling-initials"));
}

#[test]
fn a_one_to_one_chat_s_call_shows_the_other_member() {
    let rig = rig();
    let (cx, windows) = app(&rig, AB);
    click(cx, "chat-call");
    rig.send(Event::Call {
        call: CallId::for_tests(1),
        status: CallStatus::Ringing,
    });
    let stage = stage(&rig, &windows);
    assert!(drawn(stage, "call-calling-initials"), "Lab Bee's initials");
    assert!(!drawn(stage, "call-calling-split"));
}
