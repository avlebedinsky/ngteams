//! Local search on GPUI's test context, over the fake engine (`common`, whose search plays the store's on the messages
//! it holds): Ctrl+Shift+F's search lists the messages of the chats the list shows (a channel's left out), the newest
//! first, with the chat, the sender ("You" for the account) and the matched text; a query too short asks nothing; a hit
//! chosen opens its chat at the message, read back from beyond the newest page, selected and scrolled to; the switcher
//! lists the same hits under its chats; Escape closes, and the switcher and the search take turns. The store's own
//! quoting of FTS syntax is `teams-store`'s test. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
// the shell's parts over the rig
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use teams_core::Event;
use teams_store::search::{MATCH_END, MATCH_START};
use teams_store::{Conversation, ConversationKind, Member, Message};
use teams_ui::chat_view::{ChatView, PAGE};
use teams_ui::search::{MIN_QUERY, YOU, searchable, split_snippet};
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};
use teams_ui::{HitTarget, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;
/// How many messages the a–b chat holds: more than two of the view's pages.
const MANY: usize = 2 * PAGE + 20;
/// The a–b chat's old message the tests look for: far beyond the newest page.
const OLD: &str = "7";

fn chat(id: &str, kind: ConversationKind, topic: Option<&str>, last: i64) -> Conversation {
    Conversation {
        id: id.to_owned(),
        kind,
        topic: topic.map(str::to_owned),
        version: 1,
        last_activity: Some(last),
    }
}

fn message(conversation: &str, id: &str, sender: &str, content: &str, at: i64) -> Message {
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
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    }
}

/// a's store: the a–b chat of [`MANY`] messages from b a minute apart up to `NOON`, the old one ([`OLD`]) about the
/// zeppelin plan, and a's own about the zeppelin newer than all; the trio, c's message about the zeppelin newest; a
/// team's channel with one too, which the list doesn't show.
fn seeded() -> Rig {
    let rig = Rig::start();
    let core = &rig.core;
    core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None, NOON + MINUTE),
        chat(TRIO, ConversationKind::Group, Some("Lab trio"), NOON + 2 * MINUTE),
        chat(CHANNEL, ConversationKind::Channel, Some("General"), NOON + 3 * MINUTE),
    ]);
    let mut ab: Vec<Message> = (1..=MANY)
        .map(|n| {
            let back = i64::try_from(MANY - n).unwrap();
            let text = if n.to_string() == OLD {
                "<p>the zeppelin plan</p>".to_owned()
            } else {
                format!("<p>number {n}</p>")
            };
            message(AB, &n.to_string(), LAB_B, &text, NOON - back * MINUTE)
        })
        .collect();
    ab.push(message(AB, "9001", LAB_A, "<p>zeppelin again</p>", NOON + MINUTE));
    let mut messages = core.messages.lock().unwrap();
    messages.insert(AB.to_owned(), ab);
    messages.insert(
        TRIO.to_owned(),
        vec![message(
            TRIO,
            "9002",
            LAB_C,
            "<p>a zeppelin in the trio</p>",
            NOON + 2 * MINUTE,
        )],
    );
    messages.insert(
        CHANNEL.to_owned(),
        vec![message(
            CHANNEL,
            "9003",
            LAB_B,
            "<p>zeppelin channel post</p>",
            NOON + 3 * MINUTE,
        )],
    );
    drop(messages);
    let mut stored = core.members.lock().unwrap();
    stored.insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    stored.insert(
        TRIO.to_owned(),
        [LAB_A, LAB_B, LAB_C].iter().map(|mri| Member::new(mri, None)).collect(),
    );
    drop(stored);
    let mut names = core.names.lock().unwrap();
    names.insert(LAB_B.to_owned(), "Lab Bee".to_owned());
    names.insert(LAB_C.to_owned(), "Lab Sea".to_owned());
    drop(names);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
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

/// The shell in a 1000×700 window of its own.
fn shell_window(rig: &Rig, trace: Arc<ViewTrace>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = parts::parts(rig, Settings::default(), trace);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    draw(cx);
    (shell, cx)
}

/// The hits the search shows: (conversation, message, chat, sender, snippet).
fn search_hits(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Vec<(String, String, String, String, String)> {
    cx.update(|_, cx| {
        let search = shell.read(cx).search().cloned().expect("the search is open");
        search
            .read(cx)
            .hits()
            .iter()
            .map(|hit| {
                (
                    hit.target.conversation.clone(),
                    hit.target.message.clone(),
                    hit.chat.to_string(),
                    hit.sender.to_string(),
                    hit.snippet.to_string(),
                )
            })
            .collect()
    })
}

fn chat_view(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChatView> {
    cx.update(|_, cx| shell.read(cx).chat().cloned())
        .expect("a chat is open")
}

/// The chat is open at `message`: it is held, selected and the list no longer follows the newest.
fn opened_at(shell: &Entity<Shell>, cx: &mut VisualTestContext, conversation: &str, message: &str) {
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(conversation.to_owned())
    );
    let view = chat_view(shell, cx);
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.conversation(), conversation);
        assert!(view.message_items().any(|item| item.id == message), "{message} is held");
        assert_eq!(view.selected(), Some(message), "and selected");
        assert!(!view.list_state().is_following_tail(), "the list stays at it");
    });
}

fn changes(written: &Written, view: &str) -> Vec<serde_json::Value> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view)
        .collect()
}

#[test]
fn ctrl_shift_f_finds_the_listed_chats_messages_the_newest_first_and_enter_opens_the_chat_at_the_message() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.simulate_keystrokes("ctrl-shift-f");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| shell.read(cx).search().is_some()), "open");

    cx.simulate_input("zeppelin");
    draw(cx);
    let hits = search_hits(&shell, cx);
    let shown: Vec<(&str, &str, &str, &str)> = hits
        .iter()
        .map(|(conversation, message, chat, sender, _)| {
            (conversation.as_str(), message.as_str(), chat.as_str(), sender.as_str())
        })
        .collect();
    assert_eq!(
        shown,
        [
            (TRIO, "9002", "Lab trio", "Lab Sea"),
            (AB, "9001", "Lab Bee", YOU),
            (AB, OLD, "Lab Bee", "Lab Bee"),
        ],
        "the newest first; the channel's post left out"
    );
    // the matched term is marked, the marks taken out of the text
    let (_, _, _, _, snippet) = &hits[2];
    assert!(snippet.contains("zeppelin"), "{snippet}");
    assert!(!snippet.contains(MATCH_START) && !snippet.contains(MATCH_END));
    let matched = cx.update(|_, cx| shell.read(cx).search().unwrap().read(cx).hits()[2].matched.clone());
    assert_eq!(matched.len(), 1);
    assert_eq!(&snippet[matched[0].clone()], "zeppelin");

    // the old message is the third: down twice, then Enter
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert!(
        cx.update(|_, cx| shell.read(cx).search().is_none()),
        "closed once a hit opened"
    );
    opened_at(&shell, cx, AB, OLD);
    let view = chat_view(&shell, cx);
    assert!(
        cx.update(|_, cx| view.read(cx).held()) >= MANY - 6,
        "read back to the message, beyond the newest page"
    );

    let search = changes(&written, "search");
    let named: Vec<&str> = search.iter().map(|line| line["change"].as_str().unwrap()).collect();
    assert_eq!(named, ["opened", "shown", "closed"]);
    assert_eq!(search[1]["messages"], serde_json::json!(["9002", "9001", OLD]));
    assert!(!written.text().contains("zeppelin"), "no query or text in the trace");
    let revealed: Vec<serde_json::Value> = changes(&written, "chat-view")
        .into_iter()
        .filter(|line| line["change"] == "revealed")
        .collect();
    assert_eq!(revealed.len(), 1);
    assert_eq!(revealed[0]["conversation"], AB);
    assert_eq!(revealed[0]["messages"], serde_json::json!([OLD]));
}

#[test]
fn a_query_too_short_asks_nothing_and_a_longer_one_is_asked_as_typed() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.simulate_keystrokes("ctrl-shift-f");
    cx.simulate_input("z");
    draw(cx);
    assert!(
        rig.core.searches.lock().unwrap().is_empty(),
        "one character asks nothing"
    );
    assert!(search_hits(&shell, cx).is_empty());
    cx.simulate_input("e");
    draw(cx);
    assert_eq!(*rig.core.searches.lock().unwrap(), ["ze"]);
    // FTS syntax goes to the engine as typed: the store quotes it
    cx.simulate_input(" (NEAR \"-*:");
    draw(cx);
    assert_eq!(
        rig.core.searches.lock().unwrap().last().map(String::as_str),
        Some("ze (NEAR \"-*:")
    );
    assert!(searchable("ab") && !searchable(" a ") && !searchable(""));
    assert_eq!(MIN_QUERY, 2);
}

#[test]
fn the_switcher_lists_the_messages_found_under_the_chats_and_one_chosen_opens_its_chat_at_it() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("zeppelin plan");
    draw(cx);
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    let (chats, hits) = cx.update(|_, cx| {
        let switcher = switcher.read(cx);
        (
            switcher.matches().len(),
            switcher
                .hits()
                .iter()
                .map(|hit| hit.target.clone())
                .collect::<Vec<HitTarget>>(),
        )
    });
    assert_eq!(chats, 0, "no chat's name matches");
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].conversation.as_str(), hits[0].message.as_str()), (AB, OLD));

    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()));
    opened_at(&shell, cx, AB, OLD);
    assert!(
        changes(&written, "chat-view")
            .iter()
            .any(|line| line["change"] == "revealed" && line["messages"] == serde_json::json!([OLD]))
    );

    // a chat's name still comes first: Enter opens the chat, not a message
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("trio");
    draw(cx);
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    let hits = cx.update(|_, cx| switcher.read(cx).hits().len());
    assert_eq!(hits, 1, "the trio's message is found too");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.to_owned())
    );
    let view = chat_view(&shell, cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).selected().map(str::to_owned)), None);
}

#[test]
fn a_hit_in_the_chat_already_open_scrolls_its_view_to_the_message() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let before = chat_view(&shell, cx);
    assert!(cx.update(|_, cx| before.read(cx).list_state().is_following_tail()));
    assert_eq!(cx.update(|_, cx| before.read(cx).selected().map(str::to_owned)), None);
    cx.simulate_keystrokes("ctrl-shift-f");
    cx.simulate_input("zeppelin plan");
    draw(cx);
    cx.simulate_keystrokes("enter");
    draw(cx);
    opened_at(&shell, cx, AB, OLD);
    assert_eq!(chat_view(&shell, cx), before, "the same view");
}

#[test]
fn escape_closes_the_search_and_the_switcher_and_the_search_take_turns() {
    let rig = seeded();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, trace);
    let open = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| {
            let shell = shell.read(cx);
            (shell.search().is_some(), shell.switcher().is_some())
        })
    };
    cx.simulate_keystrokes("ctrl-shift-f");
    cx.simulate_input("zeppelin");
    draw(cx);
    assert_eq!(open(cx), (true, false));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert_eq!(open(cx), (false, false), "Escape closes it whatever was typed");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);

    cx.simulate_keystrokes("ctrl-shift-f");
    draw(cx);
    cx.simulate_keystrokes("ctrl-shift-f");
    draw(cx);
    assert_eq!(open(cx), (false, false), "Ctrl+Shift+F toggles it");

    cx.simulate_keystrokes("ctrl-shift-f");
    draw(cx);
    cx.simulate_keystrokes("ctrl-k");
    draw(cx);
    assert_eq!(open(cx), (false, true), "the switcher takes the search's place");
    cx.simulate_keystrokes("ctrl-shift-f");
    draw(cx);
    assert_eq!(open(cx), (true, false), "and the other way round");
    let named: Vec<String> = written
        .lines()
        .iter()
        .filter(|line| line["view"] == "search" || line["view"] == "switcher")
        .map(|line| {
            format!(
                "{} {}",
                line["view"].as_str().unwrap(),
                line["change"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            "search opened",
            "search shown",
            "search closed",
            "search opened",
            "search closed",
            "search opened",
            "search closed",
            "switcher opened",
            "switcher closed",
            "search opened",
        ]
    );
}

#[test]
fn once_the_engine_stopped_a_query_shows_no_hit() {
    let rig = seeded();
    let (shell, cx) = shell_window(&rig, Arc::new(ViewTrace::off()));
    // the backfill's end changes nothing shown
    rig.send(Event::Indexed { messages: 3 });
    let shutdown = cx.update(|_, cx| rig.bridge.update(cx, teams_ui::Bridge::shutdown));
    shutdown.detach();
    draw(cx);
    assert_eq!(rig.shutdowns(), 1);
    // the stopped engine answers every request `Stopped`
    *rig.core.stopped.lock().unwrap() = true;
    cx.simulate_keystrokes("ctrl-shift-f");
    cx.simulate_input("zeppelin");
    draw(cx);
    assert!(rig.core.asked().contains(&"search"), "asked, and answered stopped");
    assert!(search_hits(&shell, cx).is_empty());
    let searching = cx.update(|_, cx| shell.read(cx).search().unwrap().read(cx).searching());
    assert!(!searching);
}

#[test]
fn a_snippet_s_marks_become_the_ranges_of_its_matches() {
    let snippet = format!("…the {MATCH_START}zep{MATCH_END}pelin and {MATCH_START}plan{MATCH_END}…");
    let (text, matched) = split_snippet(&snippet);
    assert_eq!(text, "…the zeppelin and plan…");
    let parts: Vec<&str> = matched.iter().map(|range| &text[range.clone()]).collect();
    assert_eq!(parts, ["zep", "plan"]);
    // an end without a start, or an empty match, marks nothing
    let (text, matched) = split_snippet(&format!("a{MATCH_END}b{MATCH_START}{MATCH_END}c"));
    assert_eq!(text, "abc");
    assert!(matched.is_empty());
}
