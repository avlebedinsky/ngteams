//! A click on a reply's quote goes to the quoted message, as Teams' does (`ChatView::go_to_quoted`), on GPUI's test
//! context over the fake engine: one the view holds is scrolled to the middle of the view and ringed for a moment; one
//! older than the pages held is read back to (the store's, then the service's older pages) and shown so; one found
//! nowhere leaves the view where it is, as Teams does. Every id, name and text is made up.

use std::sync::Arc;

use gpui::{Bounds, Entity, Modifiers, Pixels, VisualTestContext, px};
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, FLASH};

use super::common::{LAB_A, LAB_B};
use super::{MINUTE, NOON, chat_window, draw, ids, many, message, rig_with};

/// A reply from a, `id`, sent a minute after `NOON`, quoting b's message `quoted` sent at `time`.
fn reply(id: &str, quoted: &str, time: i64) -> Message {
    message(
        id,
        LAB_A,
        &format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{quoted}\">\
             <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" \
             itemid=\"{time}\"></span><p itemprop=\"preview\">what was said</p></blockquote><p>my answer</p>"
        ),
        NOON + MINUTE,
    )
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

/// Clicks the quote of message `id`'s text.
fn click_quote(cx: &mut VisualTestContext, id: &str) {
    let quote = bounds(cx, &format!("message-text-{id}-quote-[0]")).expect("the reply's quote is drawn");
    cx.simulate_click(quote.center(), Modifiers::none());
    draw(cx);
}

/// Whether message `id`'s row is drawn inside the message list.
fn in_view(cx: &mut VisualTestContext, id: &str) -> bool {
    let list = bounds(cx, "chat-messages").expect("the message list");
    bounds(cx, &format!("message-{id}")).is_some_and(|row| row.top() >= list.top() && row.bottom() <= list.bottom())
}

/// Asserts message `id`'s row is in the middle of the message list, as Teams scrolls the message a quote goes to.
fn centred(cx: &mut VisualTestContext, id: &str) {
    let list = bounds(cx, "chat-messages").expect("the message list");
    let row = bounds(cx, &format!("message-{id}")).expect("the row is drawn");
    let off = (row.center().y - list.center().y).abs();
    assert!(off < px(40.0), "in the middle of the view: {row:?} in {list:?}");
}

fn flashed(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| view.read(cx).flashed().map(str::to_owned))
}

#[test]
fn a_click_on_a_quote_scrolls_to_the_message_it_quotes_and_highlights_it_for_a_moment() {
    let mut messages = many(100);
    let quoted = messages[19].clone();
    messages.push(reply("101", &quoted.id, quoted.arrival_time));
    let rig = rig_with(messages);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    assert!(!in_view(cx, "20"), "the quoted message is out of view at first");

    click_quote(cx, "101");
    assert!(in_view(cx, "20"), "scrolled to the quoted message");
    centred(cx, "20");
    assert_eq!(flashed(&view, cx).as_deref(), Some("20"));
    let ringed = bounds(cx, "quoted-flash-20").expect("the ring is drawn");
    let bubble = bounds(cx, "bubble-20").unwrap();
    assert_eq!(ringed, bubble, "over the bubble");
    assert!(
        cx.update(|_, cx| view.read(cx).scrolled_up()),
        "the list stops following the newest"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).selected().map(str::to_owned)),
        None,
        "nothing selected"
    );
    assert!(
        !rig.core.asked().contains(&"message"),
        "nothing asked of the store for a message held"
    );

    cx.executor().advance_clock(FLASH);
    draw(cx);
    assert_eq!(flashed(&view, cx), None, "the highlight goes after a moment");
    assert!(bounds(cx, "quoted-flash-20").is_none());
    assert!(in_view(cx, "20"), "the list stays at the message");
}

#[test]
fn a_quote_of_a_message_older_than_the_pages_held_reads_back_to_it_then_scrolls_to_it() {
    // 200 stored, 100 more at the service in two pages; the newest a reply quoting the 10th
    let all = many(300);
    let target = all[9].clone();
    let mut stored = all[100..].to_vec();
    stored.push(reply("301", &target.id, target.arrival_time));
    let rig = rig_with(stored);
    let pages: Vec<Vec<Message>> = [&all[50..100], &all[..50]]
        .into_iter()
        .map(|page| page.iter().rev().cloned().collect())
        .collect();
    rig.core.older.lock().unwrap().insert(super::AB.to_owned(), pages);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    assert!(!ids(&view, cx).contains(&"10".to_owned()), "not held at first");

    click_quote(cx, "301");
    assert!(
        ids(&view, cx).contains(&"10".to_owned()),
        "read back to the quoted message"
    );
    let asked = rig.core.asked();
    assert!(asked.contains(&"message"), "the store asked for it: {asked:?}");
    assert!(
        asked.contains(&"load_older"),
        "the service's older pages read: {asked:?}"
    );
    assert_eq!(flashed(&view, cx).as_deref(), Some("10"));
    centred(cx, "10");
}

#[test]
fn a_quote_whose_message_is_nowhere_leaves_the_view_where_it_is() {
    // the whole chat is held: the quoted message is gone from it
    let mut messages = many(30);
    messages.push(reply("31", "5000", NOON - 10 * MINUTE));
    let rig = rig_with(messages);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let before = ids(&view, cx);

    click_quote(cx, "31");
    assert_eq!(flashed(&view, cx), None);
    assert!(in_view(cx, "31"), "the view stays at the reply");
    assert!(
        !cx.update(|_, cx| view.read(cx).scrolled_up()),
        "still following the newest"
    );
    assert_eq!(ids(&view, cx), before, "nothing new read");
    assert!(rig.core.asked().contains(&"message"), "the store was asked for it");
}

#[test]
fn a_quote_of_a_deleted_message_goes_to_its_placeholder() {
    let mut messages = many(60);
    messages[9].delete_time = Some(NOON);
    let quoted = messages[9].clone();
    messages.push(reply("61", &quoted.id, quoted.arrival_time));
    let rig = rig_with(messages);
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));

    click_quote(cx, "61");
    assert_eq!(flashed(&view, cx).as_deref(), Some("10"));
    centred(cx, "10");
}
