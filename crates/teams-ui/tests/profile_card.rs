//! A person's profile card on the fake core, in the shell's own window: opened by a rest or a click on a message's face
//! or name, the 1:1 chat's header and the chat's people; Chat, the quick message, the calls, the tabs, the Organization
//! tab's people and Back, the read-only mode and the account's own card. Synthetic accounts and chats only.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

pub mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;
use std::time::Duration;

use common::{LAB_A, LAB_B, Rig, outbox_row};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point, px, size};
use teams_core::{Action, CallAction, CallId, CallStatus, CardPerson, CardPresence, Event, ProfileCard as CardData};
use teams_proto::{ClockTime, FreeBusyStatus, Mri, Schedule, Weekday};
use teams_store::{Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState};
use teams_ui::ViewTrace;
use teams_ui::profile_card::{CardOf, CardTab, Form, HOVER_CLOSE, HOVER_OPEN, Loaded, ProfileCard, QuickNote};
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
/// Someone the account never met, whose card can't be read.
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const BOT: &str = "28:00000000-0000-4000-8000-0000000000bb";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
/// The a–b–c chat.
const TALK: &str = "19:0000000000000000000000000000talk@thread.v2";
/// The 1:1 chat a quick message to c makes.
const MADE: &str = "19:made-synthetic@unq.gbl.spaces";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

fn chat(id: &str, kind: ConversationKind, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: None,
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, at: i64) -> Message {
    Message {
        conversation_id: conversation.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        // wide enough that the hover bar over its top right stays clear of the name
        content: "<p>A synthetic message, long enough to draw its bubble wider than its hover bar</p>".to_owned(),
        properties: None,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// A made-up card: Monday to Friday, 8 to 5 at UTC+3, working with `with`.
fn card(mri: &str, name: &str, with: &[(&str, &str)]) -> CardData {
    CardData {
        mri: mri.to_owned(),
        name: Some(name.to_owned()),
        emails: vec![format!("{}@lab.example.test", name.to_lowercase().replace(' ', "."))],
        im_addresses: vec![format!("{}@lab.example.test", name.to_lowercase().replace(' ', "."))],
        presence: Some(CardPresence {
            availability: teams_core::Availability::Offline,
            last_active_ms: Some(NOON),
            out_of_office: Some(false),
        }),
        out_of_office: Some(false),
        schedule: Some(Schedule {
            current: Some(FreeBusyStatus::Free),
            next: Some(FreeBusyStatus::Free),
            until_ms: None,
            work_days: Weekday::WORK_WEEK.to_vec(),
            work_start: Some(ClockTime { hour: 8, minute: 0 }),
            work_end: Some(ClockTime { hour: 17, minute: 0 }),
            utc_offset_minutes: Some(180),
            zone_name: Some("GMT+03:00".to_owned()),
        }),
        organization: Some(teams_core::CardOrganization::default()),
        working_with: Some(
            with.iter()
                .map(|(mri, name)| CardPerson {
                    mri: (*mri).to_owned(),
                    name: Some((*name).to_owned()),
                    email: None,
                })
                .collect(),
        ),
        job_title: Some("Synthetic tester".to_owned()),
        ..CardData::default()
    }
}

/// a's store: the a–b 1:1 chat and the a–b–c chat where b, c and a bot wrote; b's, c's and a's own cards.
fn seed(rig: &Rig) {
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(TALK, ConversationKind::Group, NOON + 5 * MINUTE),
        chat(AB, ConversationKind::OneToOne, NOON + 2 * MINUTE),
    ]);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(
        TALK.to_owned(),
        vec![
            message(TALK, "1", LAB_C, NOON),
            message(TALK, "2", LAB_B, NOON + MINUTE),
            message(TALK, "3", BOT, NOON + 2 * MINUTE),
        ],
    );
    messages.insert(AB.to_owned(), vec![message(AB, "9", LAB_B, NOON)]);
    drop(messages);
    let mut members = core.members.lock().unwrap();
    members.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    members.insert(
        TALK.to_owned(),
        [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    drop(members);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Lab Ay".to_owned());
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    let mut cards = core.profile_cards.lock().unwrap();
    cards.insert(LAB_B.to_owned(), card(LAB_B, "Lab Bee", &[(LAB_C, "Lab Sea")]));
    cards.insert(LAB_C.to_owned(), card(LAB_C, "Lab Sea", &[(LAB_B, "Lab Bee")]));
    cards.insert(LAB_A.to_owned(), card(LAB_A, "Lab Ay", &[]));
    drop(cards);
    rig.cx.update(teams_ui::window::init);
}

fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig);
    rig
}

/// A build that shows calls with video.
fn calling() -> Rig {
    let feed = teams_ui::video::VideoFeed::new();
    let rig = Rig::start_with(move |cx| teams_ui::Models::with_video_calls(None, feed, cx));
    seed(&rig);
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

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1200.0), px(800.0)));
    draw(cx);
    (shell, cx)
}

fn open(shell: &Entity<Shell>, cx: &mut VisualTestContext, id: &str) {
    shell.update_in(cx, |shell, window, cx| shell.open_chat(id.to_owned(), window, cx));
    draw(cx);
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let drawn = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(drawn.center(), Modifiers::none());
    draw(cx);
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// Rests the pointer on `selector` long enough for its card to open.
fn rest_on(cx: &mut VisualTestContext, selector: &'static str) {
    let drawn = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_mouse_move(drawn.center(), None, Modifiers::none());
    draw(cx);
    cx.executor().advance_clock(HOVER_OPEN + Duration::from_millis(50));
    draw(cx);
}

fn card_view(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Option<Entity<ProfileCard>> {
    cx.update(|_, cx| shell.read(cx).profile_card().cloned())
}

fn shown(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> (String, Form, CardTab) {
    let view = card_view(shell, cx).expect("a card is open");
    cx.update(|_, cx| {
        let card = view.read(cx);
        (card.of().mri.clone(), card.form(), card.tab())
    })
}

fn main(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Main {
    cx.update(|_, cx| shell.read(cx).main().clone())
}

fn sent_row(id: i64, kind: OutboxKind, made: Option<&str>) -> teams_store::OutboxEntry {
    let mut row = outbox_row(id, kind, OutboxState::Sent);
    row.message_id = made.map(str::to_owned);
    row
}

#[test]
fn a_rest_on_a_message_s_face_opens_the_compact_card_and_leaving_closes_it() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    assert!(!drawn(cx, "profile-card-from-avatar-3"), "an app's face opens no card");
    let face = cx.debug_bounds("profile-card-from-avatar-1").expect("c's face");
    cx.simulate_mouse_move(face.center(), None, Modifiers::none());
    draw(cx);
    assert!(card_view(&shell, cx).is_none(), "not before the pointer rests");
    cx.executor().advance_clock(HOVER_OPEN + Duration::from_millis(50));
    draw(cx);
    assert_eq!(shown(&shell, cx), (LAB_C.to_owned(), Form::Compact, CardTab::Overview));
    assert!(rig.core.profile_cards_asked.lock().unwrap().contains(&LAB_C.to_owned()));
    for selector in [
        "profile-card-chat",
        "profile-card-org",
        "profile-card-quick",
        "profile-card-availability",
        "profile-card-clock",
        "profile-card-profile",
        "profile-card-email-0",
        "profile-card-show-more",
    ] {
        assert!(drawn(cx, selector), "{selector}");
    }
    assert!(!drawn(cx, "profile-card-video"), "no calls in a build that shows none");
    // the pointer onto the card keeps it; off both, it closes
    let card = cx.debug_bounds("profile-card").unwrap();
    cx.simulate_mouse_move(card.center(), None, Modifiers::none());
    draw(cx);
    cx.executor().advance_clock(HOVER_CLOSE * 2);
    draw(cx);
    assert!(card_view(&shell, cx).is_some(), "kept while the pointer is on it");
    cx.simulate_mouse_move(point(px(1190.0), px(790.0)), None, Modifiers::none());
    draw(cx);
    cx.executor().advance_clock(HOVER_CLOSE * 2);
    draw(cx);
    assert!(card_view(&shell, cx).is_none(), "closed once the pointer left");
}

#[test]
fn a_click_on_a_face_opens_the_full_card_whose_tabs_take_the_keys_and_escape_closes_it() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-avatar-2");
    assert_eq!(shown(&shell, cx), (LAB_B.to_owned(), Form::Full, CardTab::Overview));
    let chat = cx.update(|_, cx| shell.read(cx).chat().cloned()).unwrap();
    assert_eq!(
        cx.update(|_, cx| chat.read(cx).selected().map(str::to_owned)),
        None,
        "the message isn't selected"
    );
    for selector in [
        "profile-card-close",
        "profile-card-contact",
        "profile-card-more-contact",
        "profile-card-im",
    ] {
        assert!(drawn(cx, selector), "{selector}");
    }
    click(cx, "profile-card-tab-profile");
    assert_eq!(shown(&shell, cx).2, CardTab::Profile);
    cx.simulate_keystrokes("right");
    draw(cx);
    assert_eq!(shown(&shell, cx).2, CardTab::Organization);
    cx.simulate_keystrokes("home");
    draw(cx);
    assert_eq!(shown(&shell, cx).2, CardTab::Overview);
    cx.simulate_keystrokes("end");
    draw(cx);
    assert_eq!(shown(&shell, cx).2, CardTab::Organization);
    cx.simulate_keystrokes("up");
    draw(cx);
    assert_eq!(shown(&shell, cx).2, CardTab::Profile);
    click(cx, "profile-card-tab-overview");
    click(cx, "profile-card-more-contact");
    assert_eq!(shown(&shell, cx).2, CardTab::Profile, "Show more contact information");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(card_view(&shell, cx).is_none());
    click(cx, "profile-card-from-avatar-2");
    click(cx, "profile-card-close");
    assert!(card_view(&shell, cx).is_none(), "Close");
}

#[test]
fn a_click_on_a_name_opens_the_compact_card_whose_links_open_the_full_one() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-1");
    assert_eq!(shown(&shell, cx), (LAB_C.to_owned(), Form::Compact, CardTab::Overview));
    click(cx, "profile-card-show-more");
    assert_eq!(shown(&shell, cx), (LAB_C.to_owned(), Form::Full, CardTab::Profile));
    cx.simulate_keystrokes("escape");
    draw(cx);
    click(cx, "profile-card-from-name-1");
    click(cx, "profile-card-profile");
    assert_eq!(shown(&shell, cx).1, Form::Full);
    cx.simulate_keystrokes("escape");
    draw(cx);
    // a press outside closes the compact card
    click(cx, "profile-card-from-name-1");
    cx.simulate_click(point(px(1190.0), px(790.0)), Modifiers::none());
    draw(cx);
    assert!(card_view(&shell, cx).is_none());
}

#[test]
fn chat_opens_the_one_to_one_chat_or_a_new_one_and_writes_nothing() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-2");
    click(cx, "profile-card-chat");
    assert_eq!(main(&shell, cx), Main::Chat(AB.to_owned()));
    assert!(card_view(&shell, cx).is_none(), "the card closed");
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-1");
    click(cx, "profile-card-chat");
    assert_eq!(main(&shell, cx), Main::NewChat, "no a–c chat yet: New message to c");
    let new_chat = cx.update(|_, cx| shell.read(cx).new_chat().cloned()).unwrap();
    let people: Vec<String> = cx.update(|_, cx| new_chat.read(cx).people().into_iter().map(str::to_owned).collect());
    assert_eq!(people, [LAB_C]);
    assert!(rig.core.actions.lock().unwrap().is_empty(), "nothing written");
}

#[test]
fn a_quick_message_goes_through_the_outbox_and_the_card_stays() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-2");
    let view = card_view(&shell, cx).unwrap();
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(sent_row(1, OutboxKind::Send, None));
    view.update_in(cx, |view, window, cx| view.set_quick("Lunch?", window, cx));
    click(cx, "profile-card-send");
    let actions = rig.core.actions.lock().unwrap().clone();
    assert!(matches!(actions.as_slice(), [Action::Send { conversation, .. }] if conversation == AB));
    assert_eq!(cx.update(|_, cx| view.read(cx).note().cloned()), Some(QuickNote::Sent));
    assert!(drawn(cx, "profile-card-quick-note"));
    assert!(card_view(&shell, cx).is_some(), "the card stays open");
    // the note goes after a while
    cx.executor().advance_clock(Duration::from_secs(5));
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).note().cloned()), None);
    // nothing typed: nothing sent
    view.update_in(cx, ProfileCard::send_quick);
    draw(cx);
    assert_eq!(rig.core.actions.lock().unwrap().len(), 1);
    // a failed one says so and keeps the text
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(outbox_row(2, OutboxKind::Send, OutboxState::Failed));
    view.update_in(cx, |view, window, cx| {
        view.set_quick("Again?", window, cx);
        view.send_quick(window, cx);
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).note().cloned()),
        Some(QuickNote::Failed)
    );
}

#[test]
fn a_quick_message_to_someone_without_a_chat_makes_the_chat_first() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-1");
    let view = card_view(&shell, cx).unwrap();
    let mut answers = rig.core.write_answers.lock().unwrap();
    answers.push_back(sent_row(1, OutboxKind::CreateChat, Some(MADE)));
    answers.push_back(sent_row(2, OutboxKind::Send, None));
    drop(answers);
    view.update_in(cx, |view, window, cx| view.set_quick("Hello", window, cx));
    // Enter in the box sends it
    click(cx, "profile-card-quick");
    cx.simulate_keystrokes("enter");
    draw(cx);
    let actions = rig.core.actions.lock().unwrap().clone();
    match actions.as_slice() {
        [
            Action::CreateChat { others, topic: None },
            Action::Send { conversation, .. },
        ] => {
            assert_eq!(others, &[Mri::new(LAB_C)]);
            assert_eq!(conversation, MADE);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(cx.update(|_, cx| view.read(cx).note().cloned()), Some(QuickNote::Sent));
    // a chat that couldn't be made sends nothing more
    rig.core
        .write_answers
        .lock()
        .unwrap()
        .push_back(outbox_row(3, OutboxKind::CreateChat, OutboxState::Failed));
    view.update_in(cx, |view, window, cx| {
        view.set_quick("Hello again", window, cx);
        view.send_quick(window, cx);
    });
    draw(cx);
    assert_eq!(rig.core.actions.lock().unwrap().len(), 3);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).note().cloned()),
        Some(QuickNote::Failed)
    );
}

#[test]
fn the_call_buttons_call_the_one_to_one_chat_only_when_clicked_a_video_call_with_the_camera() {
    let rig = calling();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-avatar-1");
    assert!(drawn(cx, "profile-card-video") && drawn(cx, "profile-card-audio"));
    assert!(
        rig.core.call_actions.lock().unwrap().is_empty(),
        "nothing placed by opening it"
    );
    click(cx, "profile-card-video");
    assert!(card_view(&shell, cx).is_none(), "the card closed");
    assert_eq!(
        rig.core.call_actions.lock().unwrap().clone(),
        [CallAction::Call { chat: AC.to_owned() }]
    );
    // the camera goes on once the call is up, not before
    let status = |status: CallStatus| Event::Call {
        call: CallId::for_tests(1),
        status,
    };
    rig.send(status(CallStatus::Placing));
    draw(cx);
    assert_eq!(rig.core.call_actions.lock().unwrap().len(), 1);
    rig.send(status(CallStatus::Connected));
    draw(cx);
    assert_eq!(
        rig.core.call_actions.lock().unwrap().last(),
        Some(&CallAction::Camera {
            call: CallId::for_tests(1),
            on: true
        })
    );
}

#[test]
fn an_audio_call_from_the_card_is_refused_while_another_call_is_up() {
    let rig = calling();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-2");
    click(cx, "profile-card-audio");
    assert_eq!(
        rig.core.call_actions.lock().unwrap().clone(),
        [CallAction::Call { chat: AB.to_owned() }]
    );
    rig.send(Event::Call {
        call: CallId::for_tests(1),
        status: CallStatus::Connected,
    });
    draw(cx);
    click(cx, "profile-card-from-name-1");
    click(cx, "profile-card-audio");
    assert_eq!(rig.core.call_actions.lock().unwrap().len(), 1, "busy: refused");
}

#[test]
fn the_read_only_mode_offers_no_quick_message_nor_calls_but_chat_opens_a_chat_there_is() {
    let rig = calling();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-1");
    assert!(!drawn(cx, "profile-card-quick"));
    assert!(!drawn(cx, "profile-card-video") && !drawn(cx, "profile-card-audio"));
    let view = card_view(&shell, cx).unwrap();
    assert!(!cx.update(|_, cx| view.read(cx).can_chat(cx)), "no chat with c to open");
    view.update_in(cx, |view, window, cx| {
        view.call(false, cx);
        view.set_quick("no", window, cx);
        view.send_quick(window, cx);
        view.chat(cx);
    });
    draw(cx);
    assert!(rig.core.actions.lock().unwrap().is_empty());
    assert!(rig.core.call_actions.lock().unwrap().is_empty());
    cx.simulate_keystrokes("escape");
    draw(cx);
    click(cx, "profile-card-from-name-2");
    click(cx, "profile-card-chat");
    assert_eq!(main(&shell, cx), Main::Chat(AB.to_owned()));
}

#[test]
fn the_own_card_has_its_calls_off_no_quick_message_and_the_local_time() {
    let rig = calling();
    let (shell, cx) = shell_window(&rig);
    shell.update_in(cx, |shell, window, cx| {
        let of = CardOf::new(LAB_A, Some("Lab Ay".into()));
        shell.open_card(
            of,
            (Form::Compact, CardTab::Overview),
            gpui::Bounds::default(),
            window,
            cx,
        );
    });
    draw(cx);
    let view = card_view(&shell, cx).unwrap();
    assert!(cx.update(|_, cx| view.read(cx).own()));
    assert!(drawn(cx, "profile-card-video"), "drawn, off");
    click(cx, "profile-card-video");
    assert!(rig.core.call_actions.lock().unwrap().is_empty());
    assert!(!drawn(cx, "profile-card-quick"));
    assert!(drawn(cx, "profile-card-clock"));
}

#[test]
fn the_organization_tab_opens_the_people_they_work_with_and_back_returns() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "profile-card-from-name-1");
    click(cx, "profile-card-org");
    assert_eq!(shown(&shell, cx), (LAB_C.to_owned(), Form::Full, CardTab::Organization));
    assert!(drawn(cx, "profile-card-no-org"), "the directory names no manager");
    assert!(drawn(cx, "profile-card-works-with"));
    click(cx, "profile-card-person-0");
    assert_eq!(shown(&shell, cx), (LAB_B.to_owned(), Form::Full, CardTab::Organization));
    let view = card_view(&shell, cx).unwrap();
    assert_eq!(cx.update(|_, cx| view.read(cx).back().len()), 1);
    click(cx, "profile-card-back");
    assert_eq!(shown(&shell, cx), (LAB_C.to_owned(), Form::Full, CardTab::Organization));
    assert!(!drawn(cx, "profile-card-back"));
}

#[test]
fn the_organization_tab_draws_managers_and_reports() {
    let rig = seeded();
    let mut boss = card(LAB_C, "Lab Sea", &[]);
    boss.organization = Some(teams_core::CardOrganization {
        managers: vec![CardPerson {
            mri: LAB_B.to_owned(),
            name: Some("Lab Bee".to_owned()),
            email: Some("bee@lab.example.test".to_owned()),
        }],
        directs: vec![CardPerson {
            mri: LAB_A.to_owned(),
            name: None,
            email: None,
        }],
    });
    rig.core.profile_cards.lock().unwrap().insert(LAB_C.to_owned(), boss);
    let (shell, cx) = shell_window(&rig);
    shell.update_in(cx, |shell, window, cx| {
        let of = CardOf::new(LAB_C, None);
        shell.open_card(
            of,
            (Form::Full, CardTab::Organization),
            gpui::Bounds::default(),
            window,
            cx,
        );
    });
    draw(cx);
    for selector in ["profile-card-chart", "profile-card-manager-0", "profile-card-direct-0"] {
        assert!(drawn(cx, selector), "{selector}");
    }
    assert!(!drawn(cx, "profile-card-no-org"));
    let view = card_view(&shell, cx).unwrap();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).name().to_string()),
        "Lab Sea",
        "the engine's name"
    );
    click(cx, "profile-card-manager-0");
    assert_eq!(shown(&shell, cx).0, LAB_B);
}

#[test]
fn a_card_that_cannot_be_read_says_so_quietly() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    shell.update_in(cx, |shell, window, cx| {
        shell.open_card(
            CardOf::new(LAB_D, None),
            (Form::Compact, CardTab::Overview),
            gpui::Bounds::default(),
            window,
            cx,
        );
    });
    draw(cx);
    let view = card_view(&shell, cx).unwrap();
    assert_eq!(cx.update(|_, cx| view.read(cx).loaded().clone()), Loaded::Failed);
    assert!(drawn(cx, "profile-card-failed"));
    // nobody without a card gets one
    cx.simulate_keystrokes("escape");
    draw(cx);
    shell.update_in(cx, |shell, window, cx| {
        let of = CardOf::new(BOT, None);
        shell.open_card(of, (Form::Full, CardTab::Overview), gpui::Bounds::default(), window, cx);
    });
    draw(cx);
    assert!(card_view(&shell, cx).is_none());
}

#[test]
fn the_one_to_one_header_opens_the_partner_s_card() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, AB);
    click(cx, "profile-card-from-header-avatar");
    assert_eq!(shown(&shell, cx), (LAB_B.to_owned(), Form::Full, CardTab::Overview));
    cx.simulate_keystrokes("escape");
    draw(cx);
    click(cx, "profile-card-from-header-name");
    assert_eq!(shown(&shell, cx), (LAB_B.to_owned(), Form::Compact, CardTab::Overview));
    open(&shell, cx, TALK);
    assert!(
        !drawn(cx, "profile-card-from-header-avatar"),
        "a group chat's header opens none"
    );
}

#[test]
fn the_people_flyout_opens_a_card_on_a_face_and_the_chat_on_a_row() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    rest_on(cx, "profile-card-from-people-1");
    assert_eq!(shown(&shell, cx), (LAB_B.to_owned(), Form::Compact, CardTab::Overview));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(card_view(&shell, cx).is_none());
    if cx.update(|_, cx| shell.read(cx).people().is_none()) {
        click(cx, "chat-header-people");
    }
    // the account first, then b and c as the store lists them
    click(cx, "chat-people-member-1");
    assert_eq!(
        main(&shell, cx),
        Main::Chat(AB.to_owned()),
        "b's row opens the a–b chat"
    );
    open(&shell, cx, TALK);
    click(cx, "chat-header-people");
    click(cx, "chat-people-member-2");
    assert_eq!(main(&shell, cx), Main::NewChat, "c's row: New message to c");
    assert!(rig.core.actions.lock().unwrap().is_empty());
}
