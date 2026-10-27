//! The chat list's sections and rows as Teams shows them, on GPUI's test context over the fake engine (`common`): the
//! favourites above the other chats in their order (the account's own notes as "<name> (You)" with its presence), not
//! again under "Chats", the "Favorites" section folding like "Chats" and shown at the top when the favourites come
//! after the list was first drawn (the first run); a chat the service calls empty left out while
//! nothing of it is stored; a meeting's chat by its stored thread type; a bot never named or watched by its MRI; a
//! group without a topic by its members' first names. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{FakeCore, LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::{Availability, ChatListing, Event};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::ViewTrace;
use teams_ui::chat_list::{BOT, ChatList, ChatRow, ECHO, NOTES, RowKind, UNKNOWN_USER, arrange, joined, unnamed};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Shell, ShellParts};
use teams_ui::sidebar::Section;

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const LAB_D: &str = "8:orgid:00000000-0000-4000-8000-00000000000d";
const BOT_MRI: &str = "28:00000000-0000-4000-8000-0000000000bb";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const AC: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000c@unq.gbl.spaces";
const A_BOT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-0000000000bb@unq.gbl.spaces";
const GROUP: &str = "19:00000000000000000000000000000group@thread.v2";
const MEETING: &str = "19:meeting_00000000000000000000000000000000@thread.v2";
const QUIET: &str = "19:meeting_0000000000000000000000000000quiet@thread.v2";

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

fn meeting() -> ConversationKind {
    ConversationKind::Other("thread_type:meeting".to_owned())
}

/// a's store: the a–b and a–c chats, a 1:1 with a bot, a group without a topic, a meeting's chat and a meeting nobody
/// wrote in; a's notes and the a–c chat its favourites (in that order), the quiet meeting and the bot's chat empty.
fn seed(core: &FakeCore) {
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, 6_000),
        chat(AC, ConversationKind::OneToOne, None, 5_000),
        chat(A_BOT, ConversationKind::OneToOne, None, 4_000),
        chat(GROUP, ConversationKind::Group, None, 3_000),
        chat(MEETING, meeting(), Some("Weekly sync"), 2_000),
        chat(QUIET, meeting(), Some("Test"), 1_000),
    ]);
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), vec![message(AB, "6000", LAB_B, "<p>hi</p>", 6_000)]);
    messages.insert(AC.to_owned(), vec![message(AC, "5000", LAB_C, "<p>yo</p>", 5_000)]);
    messages.insert(
        GROUP.to_owned(),
        vec![message(GROUP, "3000", LAB_D, "<p>all</p>", 3_000)],
    );
    messages.insert(
        MEETING.to_owned(),
        vec![message(MEETING, "2000", LAB_B, "<p>notes</p>", 2_000)],
    );
    let mut call = message(A_BOT, "4000", LAB_A, "<partlist/>", 4_000);
    call.message_type = "Event/Call".to_owned();
    messages.insert(A_BOT.to_owned(), vec![call]);
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), members(&[LAB_A, LAB_B]));
    stored.insert(AC.to_owned(), members(&[LAB_A, LAB_C]));
    stored.insert(A_BOT.to_owned(), members(&[LAB_A, BOT_MRI]));
    stored.insert(GROUP.to_owned(), members(&[LAB_A, LAB_B, LAB_D]));
    stored.insert(MEETING.to_owned(), members(&[LAB_A, LAB_B]));
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_A.to_owned(), "Ann Able".to_owned());
    names.insert(LAB_B.to_owned(), "Sean Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Cleo Sea".to_owned());
    names.insert(LAB_D.to_owned(), "Evgenia Dee".to_owned());
    drop(names);
    *core.listing.lock().unwrap() = ChatListing {
        favorites: Some(vec![
            NOTES.to_owned(),
            AC.to_owned(),
            "19:not-stored@thread.v2".to_owned(),
        ]),
        empty: vec![QUIET.to_owned(), A_BOT.to_owned()],
    };
}

fn seeded() -> Rig {
    let rig = Rig::start();
    seed(&rig.core);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    shell_window_traced(rig, Arc::new(ViewTrace::off()))
}

fn shell_window_traced(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace,
    };
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

fn shown(rows: &[ChatRow]) -> Vec<(&str, RowKind, &str, bool)> {
    rows.iter()
        .map(|row| (row.id.as_str(), row.kind, row.name.as_ref(), row.named))
        .collect()
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(selector).is_some()
}

#[test]
fn the_favourites_come_first_in_their_order_and_not_again_under_chats() {
    let rig = seeded();
    let written = Written::default();
    let (shell, cx) = shell_window_traced(&rig, Arc::new(ViewTrace::new(written.clone())));
    let list = list_of(&shell, cx);
    let (favorites, rows) = cx.update(|_, cx| (list.read(cx).favorites().to_vec(), list.read(cx).rows().to_vec()));

    // a's notes by a's own name, then the a–c chat; a favourite the store doesn't hold left out
    assert_eq!(
        shown(&favorites),
        [
            (NOTES, RowKind::Notes, "Ann Able (You)", true),
            (AC, RowKind::OneToOne, "Cleo Sea", true),
        ]
    );
    assert_eq!(
        favorites[0].partner.as_deref(),
        Some(LAB_A),
        "a's own presence on its notes"
    );
    // the quiet meeting and the bot's chat (only a call event stored) left out as the service calls them empty
    assert_eq!(
        shown(&rows),
        [
            (AB, RowKind::OneToOne, "Sean Bee", true),
            (GROUP, RowKind::Group, "Evgenia and Sean", true),
            (MEETING, RowKind::Meeting, "Weekly sync", true),
        ]
    );
    assert_eq!(rows[1].faces.len(), 2, "a group's avatar is split between two members");
    assert!(
        rows[2].faces.is_empty() && rows[2].partner.is_none(),
        "a meeting shows its calendar"
    );
    // the switcher and the header find a favourite too
    assert!(cx.update(|_, cx| list.read(cx).row(NOTES).is_some()));
    rig.send(Event::Presence {
        mri: LAB_A.to_owned(),
        availability: Availability::Available,
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| list.read(cx).presence_of(&favorites[0], cx)),
        Some(Availability::Available)
    );
    assert!(rig.core.asked().contains(&"chat_listing"));
    // traced by kind, the favourites first, never a name
    let lines = written.lines();
    let line = lines
        .iter()
        .rev()
        .find(|line| line["view"] == "chat-list")
        .expect("traced");
    assert_eq!(
        line["kinds"],
        serde_json::json!(["notes", "one-to-one", "one-to-one", "group", "meeting"])
    );
    let all = written.text();
    for name in ["Ann Able", "Cleo", "Evgenia", "Weekly"] {
        assert!(!all.contains(name), "{name} reached the trace");
    }
}

#[test]
fn the_favorites_section_folds_like_chats_and_is_shown_only_with_favourites() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig);
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    for selector in [
        "section-favorites",
        "favorite-row-0",
        "favorite-row-1",
        "section-chats",
        "chat-row-0",
    ] {
        assert!(drawn(cx, selector), "{selector}");
    }
    let found = cx.debug_bounds("section-favorites").unwrap();
    cx.simulate_click(found.center(), Modifiers::none());
    assert!(!cx.update(|_, cx| sidebar.read(cx).is_open(Section::Favorites)));
    assert!(!drawn(cx, "favorite-row-0"));
    assert!(drawn(cx, "chat-row-0"), "only the favourites folded");

    // no favourites: no section
    let rig = seeded();
    rig.core.listing.lock().unwrap().favorites = Some(Vec::new());
    let (_, cx) = shell_window(&rig);
    assert!(!drawn(cx, "section-favorites"));
    assert!(drawn(cx, "section-chats"));
}

#[test]
fn favourites_read_after_the_list_was_drawn_show_above_its_chats() {
    // the first run: the window opens before anything is stored, so the list's first layout doesn't fill the pane
    let rig = seeded();
    let stored = std::mem::take(&mut *rig.core.conversations.lock().unwrap());
    rig.core.listing.lock().unwrap().favorites = None;
    let (shell, cx) = shell_window(&rig);
    assert!(drawn(cx, "section-chats"));
    assert!(!drawn(cx, "section-favorites"));
    // the catch-up stores more chats than the pane holds
    let mut many = stored;
    many.extend((0..40).map(|at| {
        let id = format!("19:{at:032}@thread.v2");
        chat(&id, ConversationKind::Group, Some("Topic"), 500 - at)
    }));
    let ids: Vec<String> = many.iter().map(|conversation| conversation.id.clone()).collect();
    *rig.core.conversations.lock().unwrap() = many;
    for conversation in ids {
        rig.send(Event::ConversationChanged { conversation });
    }
    assert!(drawn(cx, "section-chats"));
    // then the teams' read gives the favourites, only the notes (so every row under "Chats" stays where it was):
    // shown at the top, not scrolled past
    rig.core.listing.lock().unwrap().favorites = Some(vec![NOTES.to_owned()]);
    rig.send(Event::Teams);
    let list = list_of(&shell, cx);
    assert_eq!(cx.update(|_, cx| list.read(cx).favorites().len()), 1);
    for selector in ["section-favorites", "favorite-row-0", "section-chats"] {
        assert!(drawn(cx, selector), "{selector}");
    }
    // in view at the list's top, right under the header
    let favorites = cx.debug_bounds("section-favorites").unwrap();
    let sidebar = cx.debug_bounds("sidebar").unwrap();
    let header = gpui::px(48.0);
    assert!(
        favorites.top() >= sidebar.top() + header && favorites.top() < sidebar.top() + header + gpui::px(4.0),
        "{favorites:?} at the list's top in {sidebar:?}"
    );
}

#[test]
fn a_bot_is_never_titled_or_watched_by_its_mri() {
    let rig = seeded();
    // the bot's chat with text in it: listed, as the service no longer calls it empty
    rig.core.listing.lock().unwrap().empty.retain(|id| id != A_BOT);
    rig.core.messages.lock().unwrap().insert(
        A_BOT.to_owned(),
        vec![message(A_BOT, "4001", BOT_MRI, "<p>beep</p>", 4_001)],
    );
    let (shell, cx) = shell_window(&rig);
    let list = list_of(&shell, cx);
    let row = cx.update(|_, cx| list.read(cx).row(A_BOT).cloned()).expect("listed");
    assert_eq!(
        (row.name.as_ref(), row.named, row.partner.as_deref()),
        (BOT, false, None)
    );
    assert!(!rig.core.presence_asked.lock().unwrap().iter().any(|mri| mri == BOT_MRI));
    // the meeting nobody wrote in stays out
    assert!(cx.update(|_, cx| list.read(cx).row(QUIET).is_none()));
    assert_eq!(rig.shutdowns(), 0, "the list never stops the engine");
}

#[test]
fn someone_without_a_name_is_named_by_what_they_are() {
    assert_eq!(unnamed(LAB_B), UNKNOWN_USER);
    assert_eq!(unnamed(BOT_MRI), BOT);
    assert_eq!(unnamed(teams_proto::TEAMS_ECHO_BOT_MRI), ECHO);
}

#[test]
fn names_are_joined_as_teams_joins_them() {
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
    assert_eq!(joined(&names(&["Ann"]), 0), "Ann");
    assert_eq!(joined(&names(&["Evgenia", "Sean"]), 0), "Evgenia and Sean");
    assert_eq!(joined(&names(&["Ann", "Bob", "Cleo"]), 0), "Ann, Bob, and Cleo");
    assert_eq!(
        joined(&names(&["Ann", "Bob", "Cleo", "Dee"]), 2),
        "Ann, Bob, Cleo, Dee, +2"
    );
}

#[test]
fn arranging_keeps_a_chat_called_empty_once_it_has_text_or_unread() {
    let row = |id: &str, preview: &str, unread: u64| ChatRow {
        id: id.to_owned(),
        kind: RowKind::Group,
        name: "x".into(),
        named: true,
        picture: None,
        partner: None,
        faces: Vec::new(),
        preview: preview.to_owned().into(),
        last_activity: None,
        unread,
    };
    let listing = ChatListing {
        favorites: None,
        empty: vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
    };
    let (favorites, rows) = arrange(
        vec![row("a", "", 0), row("b", "hi", 0), row("c", "", 1)],
        &listing,
        None,
    );
    assert!(favorites.is_empty());
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["b", "c"]);
}
