//! Compose and the writes from the UI (M2 Task 13) on GPUI's test context, over the fake engine (`common`), which
//! records every write and transient it is asked for: the typed text as a message's text (lines as paragraphs, links,
//! mentions by MRI only), Enter and Shift-Enter, the mention picker over the chat's members, a reply with its quote,
//! an edit (Up in an empty box edits the last), a delete confirmed first, the quick reactions, typing (its throttle,
//! the focus, the setting, never while editing), read receipts (the window active, the list at the newest, the
//! setting, one per burst), and the outbox rows shown with their states and "send again". The same writes on two
//! accounts' engines on the mock are `tests/compose_mock.rs`. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
// the shell's and a chat view's parts over the rig
#[path = "common/parts.rs"]
mod parts;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{LAB_A, LAB_B, Rig, Written, outbox_row};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{Action, Command, Event, MessageChange, Source, Transient};
use teams_fence::Mri;
use teams_proto::chat::{ComposeError, MessageText, Node, RichText};
use teams_store::{Applied, Member, Message, OutboxEntry, OutboxState, Reaction, ReadHorizon};
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, MessageAction, QUICK_REACTIONS, READ_DEBOUNCE};
use teams_ui::compose::{
    Candidate, CantEdit, Compose, DraftMention, Mode, PREVIEW_MAX, Quoted, TYPING_EVERY, TypingThrottle, load,
    matching, mention_query, message_text,
};
use teams_ui::sending::{Again, RowState};
use teams_ui::settings::Settings;

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// Someone outside the lab (made up).
const OUTSIDER: &str = "8:orgid:00000000-0000-4000-8000-0000000000ff";
/// 2026-01-01T12:00:00Z, ms.
const NOON: i64 = 1_767_268_800_000;
const MINUTE: i64 = 60_000;

// --- the text typed as a message's text ---

fn html(text: &str, mentions: &[DraftMention], quotes: &[Node]) -> String {
    message_text(text, mentions, quotes)
        .unwrap()
        .expect("a message")
        .html()
        .to_owned()
}

#[test]
fn each_line_is_a_paragraph_and_blank_lines_at_either_end_are_dropped() {
    assert_eq!(html("one\ntwo", &[], &[]), "<p>one</p><p>two</p>");
    assert_eq!(
        html("\n  \none\n\ntwo\n \n", &[], &[]),
        "<p>one</p><p></p><p>two</p>",
        "an empty line inside stays"
    );
    assert_eq!(html("a < b & \"c\"", &[], &[]), "<p>a &lt; b &amp; &quot;c&quot;</p>");
    assert!(message_text("", &[], &[]).unwrap().is_none());
    assert!(
        message_text(" \n\t\n ", &[], &[]).unwrap().is_none(),
        "whitespace sends nothing"
    );
}

#[test]
fn a_typed_url_becomes_a_link_without_the_punctuation_after_it() {
    assert_eq!(
        html("see https://example.test/a?b=1. ok", &[], &[]),
        "<p>see <a href=\"https://example.test/a?b=1\">https://example.test/a?b=1</a>. ok</p>"
    );
    assert_eq!(
        html("(http://example.test) mailto:lab@example.test", &[], &[]),
        "<p>(http://example.test) <a href=\"mailto:lab@example.test\">mailto:lab@example.test</a></p>",
        "a link starts a word"
    );
    assert_eq!(
        html("https:// and ftp://example.test and javascript:alert(1)", &[], &[]),
        "<p>https:// and ftp://example.test and javascript:alert(1)</p>",
        "only an http, https or mailto URL with something after its scheme"
    );
}

#[test]
fn a_mention_is_sent_by_the_mri_its_token_names_never_by_its_text() {
    let text = "hi Lab Bee and Lab Bee";
    let mentions = [DraftMention {
        range: 3..10,
        mri: LAB_B.to_owned(),
    }];
    let built = message_text(text, &mentions, &[]).unwrap().unwrap();
    assert!(built.html().starts_with("<p>hi <readonly"), "{}", built.html());
    assert!(
        built.html().ends_with(" and Lab Bee</p>"),
        "the name typed again is text: {}",
        built.html()
    );
    let participants: Vec<&str> = built.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B]);

    // two mentions, numbered in order, on two lines
    let two = [
        DraftMention {
            range: 0..1,
            mri: LAB_B.to_owned(),
        },
        DraftMention {
            range: 2..3,
            mri: OUTSIDER.to_owned(),
        },
    ];
    let built = message_text("b\nx", &two, &[]).unwrap().unwrap();
    assert!(built.html().contains("itemid=\"0\">b</span>") && built.html().contains("itemid=\"1\">x</span>"));
    assert_eq!(
        built.participants().len(),
        2,
        "an outsider is a participant the fence judges"
    );
    // only work accounts can be mentioned
    let bot = [DraftMention {
        range: 0..1,
        mri: "28:00000000-0000-4000-8000-000000000001".to_owned(),
    }];
    assert!(matches!(message_text("x", &bot, &[]), Err(ComposeError::NotAPerson(_))));
}

/// A stored message of the a–b chat (made up): `id` is its arrival time.
fn message(id: i64, sender: &str, content: &str) -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_string(),
        version: 1,
        client_message_id: Some(format!("{}", 9_000_000_000_000_000_000_i64 - id)),
        sender: Some(sender.to_owned()),
        sender_name: (sender == LAB_B).then(|| "Lab Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: content.to_owned(),
        properties: None,
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

#[test]
fn a_reply_quotes_the_message_its_sender_its_time_and_a_one_line_preview() {
    let quoted = message(NOON, LAB_B, "<p>first line</p><p>second   line</p>");
    let quote = Quoted::of(&quoted, "Lab Bee").unwrap();
    assert_eq!(
        quote.time,
        NOON.to_string(),
        "the quoted id, as the captured reply named it"
    );
    assert_eq!(quote.preview, "first line second line");
    let sent = html("my answer", &[], &[quote.node()]);
    assert_eq!(
        sent,
        format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{NOON}\"><strong itemprop=\"mri\" \
             itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" itemid=\"{NOON}\"></span><p \
             itemprop=\"preview\">first line second line</p></blockquote><p>my answer</p>"
        )
    );
    let long = message(NOON, LAB_B, &"word ".repeat(200));
    let preview = Quoted::of(&long, "Lab Bee").unwrap().preview;
    assert_eq!(preview.chars().count(), PREVIEW_MAX + 1, "cut, with an ellipsis");
    assert!(preview.ends_with('…'));
    let mut nobody = message(NOON, LAB_B, "<p>x</p>");
    nobody.sender = None;
    assert!(Quoted::of(&nobody, "x").is_none());
}

#[test]
fn an_edit_loads_the_text_back_its_mentions_by_mri_and_keeps_its_quote() {
    let text = RichText {
        nodes: vec![
            Node::Quote(teams_proto::chat::Quote::default()),
            Node::Paragraph(vec![
                Node::Text {
                    text: "hi ".to_owned(),
                    marks: teams_proto::chat::Marks::BOLD,
                },
                Node::Mention {
                    item_id: "0".to_owned(),
                    text: "Lab Bee".to_owned(),
                },
                Node::Mention {
                    item_id: "7".to_owned(),
                    text: "Nobody".to_owned(),
                },
            ]),
            Node::Paragraph(vec![
                Node::Link {
                    href: "https://example.test".to_owned(),
                    children: vec![Node::Text {
                        text: "a link".to_owned(),
                        marks: teams_proto::chat::Marks::NONE,
                    }],
                },
                Node::Break,
                Node::Text {
                    text: "end".to_owned(),
                    marks: teams_proto::chat::Marks::NONE,
                },
            ]),
        ],
        truncated: false,
    };
    let mentioned = HashMap::from([("0".to_owned(), LAB_B.to_owned())]);
    let loaded = load(&text, &mentioned).unwrap();
    assert_eq!(loaded.text, "hi Lab BeeNobody\na link\nend");
    assert_eq!(loaded.mentions.len(), 1, "a mention naming nobody known stays text");
    assert_eq!(
        (loaded.mentions[0].range.clone(), loaded.mentions[0].mri.as_str()),
        (3..10, LAB_B)
    );
    assert_eq!(loaded.quotes.len(), 1);

    let image = RichText {
        nodes: vec![Node::Image(teams_proto::chat::Image {
            src: "https://example.test/i.png".to_owned(),
            alt: None,
            item_type: None,
        })],
        truncated: false,
    };
    assert_eq!(load(&image, &mentioned), Err(CantEdit::Image));
    let partial = RichText {
        nodes: Vec::new(),
        truncated: true,
    };
    assert_eq!(load(&partial, &mentioned), Err(CantEdit::Truncated));
}

#[test]
fn an_at_that_starts_a_word_opens_the_picker_and_its_query_is_what_follows() {
    assert_eq!(mention_query("hi @Lab B", 9, &[]), Some((3, "Lab B")));
    assert_eq!(mention_query("@", 1, &[]), Some((0, "")));
    assert_eq!(mention_query("mail@lab", 8, &[]), None, "inside a word");
    assert_eq!(mention_query("@x\ny", 4, &[]), None, "on another line");
    assert_eq!(mention_query("@ x", 3, &[]), None, "a space right after the @");
    assert_eq!(
        mention_query("hi @Lab Bee x", 13, std::slice::from_ref(&(4..11))),
        None,
        "a mention placed since"
    );
    assert_eq!(
        mention_query(&format!("@{}", "y".repeat(50)), 51, &[]),
        None,
        "too far back"
    );
    assert_eq!(mention_query("hi @Lab", 3, &[]), None, "the cursor before it");
}

#[test]
fn the_picker_offers_the_names_holding_the_query() {
    let candidates = [
        Candidate {
            mri: LAB_B.to_owned(),
            name: "Lab Bee".into(),
        },
        Candidate {
            mri: "8:orgid:00000000-0000-4000-8000-00000000000c".to_owned(),
            name: "Lab Sea".into(),
        },
    ];
    assert_eq!(matching(&candidates, "").len(), 2);
    assert_eq!(matching(&candidates, "bE")[0].mri, LAB_B);
    assert_eq!(matching(&candidates, "lab s").len(), 1);
    assert!(matching(&candidates, "zz").is_empty());
}

#[test]
fn typing_goes_out_at_most_once_per_six_seconds_the_first_at_once() {
    let mut throttle = TypingThrottle::default();
    let start = Instant::now();
    assert!(throttle.take(start));
    assert!(!throttle.take(start + Duration::from_secs(5)));
    assert!(throttle.take(start + TYPING_EVERY));
    assert!(!throttle.take(start + TYPING_EVERY + Duration::from_secs(1)));
}

// --- the box in a chat view ---

/// The rig with the a–b chat holding `messages`, its members a and b, b named, the kit set up.
fn rig_with(messages: Vec<Message>) -> Rig {
    let rig = Rig::start();
    rig.core.messages.lock().unwrap().insert(AB.to_owned(), messages);
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.core
        .names
        .lock()
        .unwrap()
        .insert(LAB_B.to_owned(), "Lab Bee".to_owned());
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

/// The chat view of the a–b chat with `settings`, in an 800×600 window of its own, its trace written to the second
/// value; the window active, the compose box focused.
fn chat_window(rig: &Rig, settings: Settings) -> (Entity<ChatView>, &'static mut VisualTestContext, Written) {
    let written = Written::default();
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::new(written.clone())));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(600.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    focus_compose(&view, cx);
    (view, cx, written)
}

fn compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    cx.update(|_, cx| view.read(cx).compose().clone())
}

fn focus_compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    let compose = compose(view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
}

fn focus_list(view: &Entity<ChatView>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    draw(cx);
}

fn typed(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> String {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).value(cx).to_string())
}

fn mode(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Mode {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).mode().clone())
}

fn actions(rig: &Rig) -> Vec<Action> {
    std::mem::take(&mut *rig.core.actions.lock().unwrap())
}

/// The HTML of a message post or an edit.
fn text_of(action: &Action) -> &MessageText {
    match action {
        Action::Send { text, .. } | Action::Edit { text, .. } => text,
        other => panic!("not a message: {other:?}"),
    }
}

fn type_text(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    draw(cx);
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
    draw(cx);
}

fn act(view: &Entity<ChatView>, cx: &mut VisualTestContext, action: MessageAction, message: &str) {
    cx.update(|window, cx| view.update(cx, |view, cx| view.act(action, message, window, cx)));
    draw(cx);
}

fn trace_changes(written: &Written, view: &str) -> Vec<String> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view)
        .map(|line| line["change"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn enter_sends_and_shift_enter_breaks_the_line() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, written) = chat_window(&rig, Settings::default());

    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "an empty box sends nothing");
    type_text(cx, "one");
    keys(cx, "shift-enter");
    type_text(cx, "two https://example.test/a.");
    assert_eq!(typed(&view, cx), "one\ntwo https://example.test/a.");
    keys(cx, "enter");
    let sent = actions(&rig);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let Action::Send { conversation, text } = &sent[0] else {
        panic!("a post: {sent:?}")
    };
    assert_eq!(conversation, AB);
    assert_eq!(
        text.html(),
        "<p>one</p><p>two <a href=\"https://example.test/a\">https://example.test/a</a>.</p>"
    );
    assert_eq!(typed(&view, cx), "", "the box is emptied once the write is asked for");
    assert_eq!(trace_changes(&written, "compose"), ["sent"]);
    assert!(!written.text().contains("example.test"), "no text reaches the trace");
}

#[test]
fn a_mention_is_completed_after_at_from_the_chat_s_members_by_mri() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi a</p>")]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let compose = compose(&view, cx);
    let picker = |cx: &mut VisualTestContext| cx.update(|_, cx| compose.read(cx).picker().cloned());

    type_text(cx, "hi @");
    let open = picker(cx).expect("the picker opens");
    assert_eq!(
        open.matches
            .iter()
            .map(|candidate| candidate.mri.as_str())
            .collect::<Vec<_>>(),
        [LAB_B],
        "the chat's members, the account left out"
    );
    assert_eq!(open.matches[0].name.as_ref(), "Lab Bee");
    assert!(cx.debug_bounds("mention-picker").is_some());
    keys(cx, "escape");
    assert!(picker(cx).is_none(), "Escape closes it");
    assert_eq!(typed(&view, cx), "hi @");

    type_text(cx, "be");
    assert!(picker(cx).is_some(), "typing on opens it again");
    type_text(cx, "zz");
    assert!(picker(cx).is_none(), "nobody matches");
    keys(cx, "backspace backspace");
    keys(cx, "enter");
    assert!(actions(&rig).is_empty(), "Enter chose from the picker, it didn't send");
    assert_eq!(typed(&view, cx), "hi Lab Bee ");
    type_text(cx, "!");
    keys(cx, "enter");
    let sent = actions(&rig);
    let text = text_of(&sent[0]);
    let participants: Vec<&str> = text.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B]);
    assert!(text.html().contains("itemid=\"0\">Lab Bee</span>"), "{}", text.html());

    // Tab chooses too; a name typed without the picker is only text
    type_text(cx, "Lab Bee @");
    keys(cx, "tab");
    keys(cx, "enter");
    let text = text_of(&actions(&rig)[0]).clone();
    assert_eq!(text.participants().len(), 1);
    assert!(text.html().starts_with("<p>Lab Bee <readonly"), "{}", text.html());
}

#[test]
fn a_reply_carries_its_quote_and_escape_cancels_it() {
    let theirs = message(NOON, LAB_B, "<p>question?</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (view, cx, written) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::Reply, &theirs.id);
    assert!(matches!(mode(&view, cx), Mode::Reply(_)));
    assert!(cx.debug_bounds("compose-mode").is_some());
    keys(cx, "escape");
    assert_eq!(mode(&view, cx), Mode::New, "Escape cancels the reply");

    act(&view, cx, MessageAction::Reply, &theirs.id);
    type_text(cx, "answer");
    keys(cx, "enter");
    let sent = actions(&rig);
    let html = text_of(&sent[0]).html().to_owned();
    assert!(
        html.starts_with(&format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{NOON}\">"
        )),
        "{html}"
    );
    assert!(html.contains(">question?</p></blockquote><p>answer</p>"), "{html}");
    assert_eq!(mode(&view, cx), Mode::New);
    assert_eq!(
        trace_changes(&written, "compose"),
        ["replying", "cancelled", "replying", "sent"]
    );
}

#[test]
fn up_in_an_empty_box_edits_the_account_s_last_message() {
    let mut mine = message(NOON + MINUTE, LAB_A, "");
    "<p>hello <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab Bee</span></p>"
        .clone_into(&mut mine.content);
    mine.properties = Some(format!(
        "{{\"mentions\":[{{\"@type\":\"http://schema.skype.com/Mention\",\"itemid\":0,\"mri\":\"{LAB_B}\",\
         \"mentionType\":\"person\",\"displayName\":\"Lab Bee\"}}]}}"
    ));
    let rig = rig_with(vec![
        message(NOON - MINUTE, LAB_A, "<p>older</p>"),
        message(NOON, LAB_B, "<p>theirs</p>"),
        mine.clone(),
    ]);
    let (view, cx, _) = chat_window(&rig, Settings::default());

    type_text(cx, "x");
    keys(cx, "up");
    assert_eq!(mode(&view, cx), Mode::New, "only in an empty box");
    keys(cx, "backspace up");
    assert!(
        matches!(mode(&view, cx), Mode::Edit { ref message_id, .. } if *message_id == mine.id),
        "{:?}",
        mode(&view, cx)
    );
    assert_eq!(typed(&view, cx), "hello Lab Bee");
    keys(cx, "escape");
    assert_eq!(
        (mode(&view, cx), typed(&view, cx)),
        (Mode::New, String::new()),
        "Escape cancels it"
    );

    keys(cx, "up");
    type_text(cx, " again");
    keys(cx, "enter");
    let sent = actions(&rig);
    let Action::Edit {
        conversation,
        message_id,
        text,
    } = &sent[0]
    else {
        panic!("an edit: {sent:?}")
    };
    assert_eq!((conversation.as_str(), message_id.as_str()), (AB, mine.id.as_str()));
    let participants: Vec<&str> = text.participants().iter().map(Mri::as_str).collect();
    assert_eq!(participants, [LAB_B], "the mention kept by its MRI");
    assert!(text.html().ends_with(" again</p>"), "{}", text.html());
}

#[test]
fn a_delete_asks_first_and_escape_keeps_the_message() {
    let mine = message(NOON, LAB_A, "<p>oops</p>");
    let rig = rig_with(vec![mine.clone()]);
    let (view, cx, _) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::Delete, &mine.id);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).deleting().map(str::to_owned)),
        Some(mine.id.clone())
    );
    assert!(cx.debug_bounds("confirmation").is_some());
    keys(cx, "escape");
    assert!(actions(&rig).is_empty(), "cancelled, nothing written");
    assert!(cx.debug_bounds("confirmation").is_none());

    act(&view, cx, MessageAction::Delete, &mine.id);
    keys(cx, "enter");
    assert_eq!(
        actions(&rig),
        [Action::Delete {
            conversation: AB.to_owned(),
            message_id: mine.id.clone(),
        }]
    );
    // and by the button
    act(&view, cx, MessageAction::Delete, &mine.id);
    let confirm = cx.debug_bounds("confirm-delete").unwrap();
    cx.simulate_click(confirm.center(), Modifiers::none());
    draw(cx);
    assert_eq!(actions(&rig).len(), 1);
}

#[test]
fn the_quick_reactions_set_the_account_s_own_and_a_chip_takes_it_back() {
    let theirs = message(NOON, LAB_B, "<p>nice</p>");
    let rig = rig_with(vec![theirs.clone()]);
    rig.core.reactions.lock().unwrap().insert(
        theirs.id.clone(),
        vec![Reaction {
            key: "like".to_owned(),
            user: LAB_A.to_owned(),
            time: NOON,
        }],
    );
    let (view, cx, _) = chat_window(&rig, Settings::default());

    act(&view, cx, MessageAction::React, &theirs.id);
    assert_eq!(
        QUICK_REACTIONS,
        ["like", "heart", "laugh", "surprised"],
        "M2 Task 10's capture"
    );
    for pick in ["pick-like", "pick-heart", "pick-laugh", "pick-surprised"] {
        assert!(cx.debug_bounds(pick).is_some(), "{pick} offered");
    }
    keys(cx, "2");
    assert_eq!(
        actions(&rig),
        [Action::React {
            conversation: AB.to_owned(),
            message_id: theirs.id.clone(),
            key: "heart".to_owned(),
        }]
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).reacting().map(str::to_owned)), None);
    // the account's own like, chosen again, is taken back
    act(&view, cx, MessageAction::React, &theirs.id);
    keys(cx, "1");
    let chip = cx.debug_bounds("reaction-like").unwrap();
    cx.simulate_click(chip.center(), Modifiers::none());
    draw(cx);
    let unreact = Action::Unreact {
        conversation: AB.to_owned(),
        message_id: theirs.id.clone(),
        key: "like".to_owned(),
    };
    assert_eq!(actions(&rig), [unreact.clone(), unreact]);
}

fn typing_sent(rig: &Rig) -> usize {
    std::mem::take(&mut *rig.core.transients.lock().unwrap())
        .into_iter()
        .inspect(|transient| {
            assert_eq!(
                transient,
                &Transient::Typing {
                    conversation: AB.to_owned()
                }
            );
        })
        .count()
}

#[test]
fn typing_goes_out_while_the_box_has_text_and_focus_no_more_often_than_the_capture_shows() {
    let rig = rig_with(vec![
        message(NOON, LAB_B, "<p>hi</p>"),
        message(NOON + 1, LAB_A, "<p>mine</p>"),
    ]);
    // read receipts off: any write here would be typing's
    let settings = Settings {
        send_read_receipts: false,
        ..Settings::default()
    };
    let (view, cx, _) = chat_window(&rig, settings);

    type_text(cx, " ");
    assert_eq!(typing_sent(&rig), 0, "whitespace isn't text");
    type_text(cx, "h");
    type_text(cx, "e");
    assert_eq!(typing_sent(&rig), 1, "the first at once, the next held back");
    cx.executor()
        .advance_clock(TYPING_EVERY.saturating_sub(Duration::from_secs(1)));
    type_text(cx, "l");
    assert_eq!(typing_sent(&rig), 0);
    cx.executor().advance_clock(Duration::from_secs(1));
    type_text(cx, "l");
    assert_eq!(typing_sent(&rig), 1, "{TYPING_EVERY:?} after the last");

    // without the focus, nothing
    cx.executor().advance_clock(TYPING_EVERY);
    let compose = compose(&view, cx);
    focus_list(&view, cx);
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| {
            compose.input().update(cx, |input, cx| input.insert("o", window, cx));
        });
    });
    draw(cx);
    let compose_focused = cx.update(|window, cx| compose.read(cx).focused(window, cx));
    assert!(!compose_focused);
    assert_eq!(typing_sent(&rig), 0, "not while another view has the focus");

    // never while editing
    focus_compose(&view, cx);
    keys(cx, "ctrl-a backspace up");
    assert!(matches!(mode(&view, cx), Mode::Edit { .. }));
    cx.executor().advance_clock(TYPING_EVERY);
    type_text(cx, "x");
    assert_eq!(typing_sent(&rig), 0, "not while editing");
    keys(cx, "escape");
    assert!(
        rig.core.asked().iter().all(|asked| *asked != "write"),
        "typing is never a write"
    );
}

#[test]
fn typing_stays_off_with_its_setting_off() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (_view, cx, _) = chat_window(
        &rig,
        Settings {
            send_typing: false,
            ..Settings::default()
        },
    );
    type_text(cx, "hello");
    assert_eq!(typing_sent(&rig), 0);
}

fn receipts(rig: &Rig) -> Vec<String> {
    actions(rig)
        .into_iter()
        .filter_map(|action| match action {
            Action::Read {
                conversation,
                message_id,
            } if conversation == AB => Some(message_id),
            _ => None,
        })
        .collect()
}

fn deliver(rig: &Rig, message: &Message) -> Event {
    rig.core
        .messages
        .lock()
        .unwrap()
        .get_mut(AB)
        .unwrap()
        .push(message.clone());
    Event::Message {
        conversation: AB.to_owned(),
        id: message.id.clone(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message.clone())),
        source: Source::Delivery,
    }
}

#[test]
fn a_read_receipt_follows_the_newest_message_shown_in_the_active_window_once_per_burst() {
    let read = message(NOON, LAB_B, "<p>read already</p>");
    let unread = message(NOON + MINUTE, LAB_B, "<p>new</p>");
    let rig = rig_with(vec![read.clone(), unread.clone()]);
    rig.core.horizons.lock().unwrap().insert(
        AB.to_owned(),
        ReadHorizon {
            conversation_id: AB.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: read.arrival_time,
            updated: NOON,
            raw: None,
        },
    );
    let (view, cx, _) = chat_window(&rig, Settings::default());
    assert!(receipts(&rig).is_empty(), "not before its wait is over");
    debounced(cx);
    assert_eq!(
        receipts(&rig),
        std::slice::from_ref(&unread.id),
        "up to the newest message"
    );
    assert_eq!(cx.update(|_, cx| view.read(cx).receipts()), 1);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "once");

    // the account's own message moves nothing; two from b within the wait give one receipt, for the newer
    let mine = message(NOON + 2 * MINUTE, LAB_A, "<p>mine</p>");
    rig.send(deliver(&rig, &mine));
    debounced(cx);
    assert!(receipts(&rig).is_empty());
    rig.send(deliver(&rig, &message(NOON + 3 * MINUTE, LAB_B, "<p>one</p>")));
    let second = message(NOON + 4 * MINUTE, LAB_B, "<p>two</p>");
    rig.send(deliver(&rig, &second));
    debounced(cx);
    assert_eq!(receipts(&rig), std::slice::from_ref(&second.id));

    // in a window that isn't active, none; once it is, the receipt goes
    cx.deactivate_window();
    let third = message(NOON + 5 * MINUTE, LAB_B, "<p>three</p>");
    rig.send(deliver(&rig, &third));
    debounced(cx);
    debounced(cx);
    assert!(receipts(&rig).is_empty(), "not while the window isn't the user's");
    cx.update(|window, _| window.activate_window());
    draw(cx);
    debounced(cx);
    assert_eq!(receipts(&rig), std::slice::from_ref(&third.id));
}

/// Lets a read receipt's wait run out.
fn debounced(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(READ_DEBOUNCE);
    draw(cx);
}

#[test]
fn read_receipts_stay_off_with_their_setting_off_or_scrolled_up() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>new</p>")]);
    let (_view, cx, _) = chat_window(
        &rig,
        Settings {
            send_read_receipts: false,
            ..Settings::default()
        },
    );
    debounced(cx);
    assert!(receipts(&rig).is_empty());

    let many: Vec<Message> = (0..120).map(|n| message(NOON + n, LAB_B, "<p>line</p>")).collect();
    let rig = rig_with(many);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    debounced(cx);
    assert_eq!(receipts(&rig).len(), 1);
    // scrolled up from the newest: a new message isn't read
    cx.update(|_, cx| view.read(cx).list_state().pause_following_tail());
    cx.update(|_, cx| view.read(cx).list_state().scroll_to_reveal_item(0));
    draw(cx);
    rig.send(deliver(&rig, &message(NOON + 500, LAB_B, "<p>later</p>")));
    debounced(cx);
    assert!(receipts(&rig).is_empty());
    // back at the newest, it is
    cx.update(|_, cx| view.update(cx, ChatView::jump_to_newest));
    draw(cx);
    debounced(cx);
    assert_eq!(receipts(&rig), [(NOON + 500).to_string()]);
}

/// A row of the a–b chat whose payload is the command it carries.
fn row(id: i64, command: &Command, state: OutboxState) -> OutboxEntry {
    let mut row = outbox_row(id, command.kind(), state);
    row.conversation_id = Some(AB.to_owned());
    row.payload = serde_json::to_string(command).unwrap();
    row
}

fn post(text: &str) -> Command {
    Command::Send {
        conversation: AB.to_owned(),
        text: MessageText::plain(text).unwrap(),
    }
}

fn pending(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Vec<(i64, RowState, Again)> {
    cx.update(|_, cx| {
        view.read(cx)
            .pending_items()
            .map(|pending| (pending.note.row, pending.note.state, pending.note.again))
            .collect()
    })
}

/// Clicks what `selector` names, and lets the view draw.
fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

/// The a–b chat's rows that aren't sent, one of each state (rows 1–6 posts, row 7 an edit of `theirs`), and the one
/// an earlier session left.
fn rows_of_every_state(theirs: &Message) -> (Vec<OutboxEntry>, OutboxEntry) {
    let mut waiting = row(2, &post("waiting"), OutboxState::Pending);
    waiting.not_before = Some(i64::MAX);
    let mut maybe = row(5, &post("maybe"), OutboxState::Failed);
    maybe.unreadable_seen_at = Some(NOON);
    let earlier = row(6, &post("left"), OutboxState::Sending);
    let edit = Command::Edit {
        conversation: AB.to_owned(),
        message_id: theirs.id.clone(),
        text: MessageText::plain("x").unwrap(),
        original: teams_core::Sent {
            client_message_id: "1".to_owned(),
            compose_time_ms: NOON,
            arrival_time_ms: NOON,
        },
    };
    let rows = vec![
        row(1, &post("sending"), OutboxState::Pending),
        waiting,
        row(3, &post("denied"), OutboxState::Denied),
        row(4, &post("failed"), OutboxState::Failed),
        maybe,
        earlier.clone(),
        row(7, &edit, OutboxState::Failed),
    ];
    (rows, earlier)
}

#[test]
fn each_post_on_its_way_shows_its_state_and_a_failed_or_earlier_one_offers_send_again() {
    let theirs = message(NOON, LAB_B, "<p>hi</p>");
    let rig = rig_with(vec![theirs.clone()]);
    let (rows, earlier) = rows_of_every_state(&theirs);
    *rig.core.unsettled.lock().unwrap() = rows;
    *rig.core.earlier.lock().unwrap() = vec![earlier];
    let (view, cx, written) = chat_window(&rig, Settings::default());

    assert_eq!(
        pending(&view, cx),
        [
            (1, RowState::Sending, Again::No),
            (2, RowState::Waiting, Again::No),
            (3, RowState::Denied, Again::No),
            (4, RowState::Failed, Again::Yes),
            (5, RowState::MayHaveArrived, Again::AfterWarning),
            (6, RowState::EarlierSession, Again::Yes),
        ]
    );
    let note = cx.update(|_, cx| {
        view.read(cx)
            .message_items()
            .find(|item| item.id == theirs.id)
            .and_then(|item| item.outbox.clone())
    });
    assert_eq!(
        note.map(|note| (note.row, note.state)),
        Some((7, RowState::Failed)),
        "an edit on its message"
    );
    assert!(cx.debug_bounds("pending-3").is_some() && cx.debug_bounds("send-again-3").is_none());
    let line = written
        .lines()
        .into_iter()
        .rfind(|line| line["view"] == "outbox")
        .unwrap();
    assert_eq!(
        line["kinds"],
        serde_json::json!([
            "failed",
            "sending",
            "waiting",
            "denied",
            "failed",
            "may-have-arrived",
            "earlier-session"
        ]),
        "the message's note, then the posts on their way: {line}"
    );
    assert_eq!(line["messages"], serde_json::json!(["7", "1", "2", "3", "4", "5", "6"]));
    assert_eq!(rig.shutdowns(), 0, "a view never stops the engine");

    // send again: at once on a failed row, after the warning on one that may have arrived
    click(cx, "send-again-4");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4]);
    click(cx, "send-again-5");
    assert_eq!(cx.update(|_, cx| view.read(cx).resending()), Some(5));
    assert_eq!(*rig.core.resent.lock().unwrap(), [4], "warned first");
    click(cx, "resend-anyway");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4, 5]);
    click(cx, "send-again-6");
    assert_eq!(*rig.core.resent.lock().unwrap(), [4, 5, 6]);
    assert!(
        actions(&rig).is_empty(),
        "send again is the explicit resend, never a new write"
    );
}

#[test]
fn a_post_shows_at_once_as_it_is_queued_then_sent_until_its_message_is_held() {
    let rig = rig_with(vec![message(NOON, LAB_B, "<p>hi</p>")]);
    let (view, cx, _) = chat_window(&rig, Settings::default());
    let command = post("on its way");
    let queued = row(9, &command, OutboxState::Pending);
    rig.send(Event::Queued(queued.clone()));
    draw(cx);
    assert_eq!(pending(&view, cx), [(9, RowState::Sending, Again::No)]);
    assert!(cx.debug_bounds("pending-9").is_some());

    let mut sent = queued.clone();
    sent.state = OutboxState::Sent;
    rig.send(Event::Outbox(sent.clone()));
    draw(cx);
    assert_eq!(
        pending(&view, cx),
        [(9, RowState::Sent, Again::No)],
        "sent, its message not held yet"
    );

    let mut mine = message(NOON + MINUTE, LAB_A, "<p>on its way</p>");
    mine.client_message_id = Some(sent.client_message_id.clone());
    rig.send(deliver(&rig, &mine));
    draw(cx);
    assert!(pending(&view, cx).is_empty(), "the message itself shows now");
    let shown = cx.update(|_, cx| view.read(cx).message_items().any(|item| item.id == mine.id));
    assert!(shown);
}
