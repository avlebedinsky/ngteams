//! A message's text selected with the mouse and copied, as Teams allows: a drag across its runs and paragraphs (bold, a
//! link and inline code among them) and on into the next message, a double-click on a word, a triple-click on a
//! paragraph, a click on a link still opening it, and the selection copied as plain text. Drawn on GPUI's test
//! context; every id and link is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;

use gpui::{
    Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Point, Render, SharedString, Styled as _, TestAppContext, VisualTestContext, Window, div, point, px,
};
use teams_proto::chat::parse_html;
use teams_ui::rich_text::{self, Mentions, Rendered};
use teams_ui::selection::{MessageKey, Scope, TextSelection};
use teams_ui::theme::LIGHT_PALETTE;

fn laid_out(content: &str) -> Arc<Rendered> {
    Arc::new(rich_text::layout(&parse_html(content), &Mentions::default()))
}

/// Two messages drawn one under the other, their text selectable in one selection, as the chat view draws them.
struct Messages {
    selection: TextSelection,
    messages: Vec<(MessageKey, Arc<Rendered>)>,
}

impl Render for Messages {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self.selection.clone();
        div()
            .w(px(600.0))
            .flex()
            .flex_col()
            .gap_4()
            // as the views do: a press anywhere lets the selection go before the text takes it
            .capture_any_mouse_down(move |event, _, _| {
                if event.button == MouseButton::Left && !event.modifiers.shift {
                    let _ = selection.clear();
                }
            })
            .children(self.messages.iter().map(|(key, rendered)| {
                let id = SharedString::from(format!("message-text-{}", key.id));
                let scope = Scope::new(&self.selection, key.clone());
                let selector = format!("message-{}", key.id);
                div()
                    .debug_selector(move || selector)
                    .child(rich_text::render_selectable(
                        &id,
                        rendered,
                        LIGHT_PALETTE,
                        None,
                        Some(&scope),
                    ))
            }))
    }
}

const FIRST: &str = "<p>plain <b>bold</b> <a href=\"https://example.test/opened\">link</a> <code>x = 1</code></p>\
                     <p>second line</p>";
const SECOND: &str = "<p>next message</p>";

fn drawn() -> (
    TextSelection,
    Vec<(MessageKey, Arc<Rendered>)>,
    &'static mut VisualTestContext,
) {
    let selection = TextSelection::default();
    let messages = vec![
        (MessageKey::new((1, 0), "1"), laid_out(FIRST)),
        (MessageKey::new((2, 0), "2"), laid_out(SECOND)),
    ];
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (view_selection, view_messages) = (selection.clone(), messages.clone());
    let (_, cx) = app.add_window_view(move |_, _| Messages {
        selection: view_selection,
        messages: view_messages,
    });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    (selection, messages, cx)
}

/// Points on the paragraphs' first lines, top to bottom: the first message's two (each one line), the second's one.
fn paragraphs(cx: &mut VisualTestContext) -> Vec<Point<Pixels>> {
    let first = cx.debug_bounds("message-1").expect("the first message is drawn");
    let second = cx.debug_bounds("message-2").expect("the second message is drawn");
    vec![
        first.origin + point(px(1.0), px(4.0)),
        point(first.left() + px(1.0), first.bottom() - px(4.0)),
        second.origin + point(px(1.0), px(4.0)),
    ]
}

fn copied(selection: &TextSelection, messages: &[(MessageKey, Arc<Rendered>)]) -> Option<String> {
    selection.text(messages.iter().map(|(key, rendered)| (key.clone(), &**rendered)))
}

fn redraw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

#[test]
fn a_drag_selects_across_runs_paragraphs_and_messages() {
    let (selection, messages, cx) = drawn();
    let drawn = paragraphs(cx);
    let start = drawn[0];
    // past the end of the first message's second line
    let end = drawn[1] + point(px(500.0), px(0.0));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(
        copied(&selection, &messages).as_deref(),
        Some("plain bold link x = 1\nsecond line")
    );
    assert!(cx.opened_url().is_none(), "a drag opens no link");
    // on into the next message, from the middle of the first paragraph
    let middle = drawn[0];
    let next = drawn[2] + point(px(500.0), px(0.0));
    cx.simulate_mouse_down(middle, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(next, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(next, MouseButton::Left, Modifiers::none());
    redraw(cx);
    assert_eq!(
        copied(&selection, &messages).as_deref(),
        Some("plain bold link x = 1\nsecond line\n\nnext message")
    );
    // a click elsewhere lets it go
    cx.simulate_click(next, Modifiers::none());
    assert!(selection.is_empty());
}

#[test]
fn a_double_click_selects_a_word_and_a_triple_click_its_paragraph() {
    let (selection, messages, cx) = drawn();
    let drawn = paragraphs(cx);
    let on_second = drawn[1] + point(px(2.0), px(0.0));
    for click_count in 1..=2 {
        cx.simulate_event(MouseDownEvent {
            position: on_second,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_mouse_up(on_second, MouseButton::Left, Modifiers::none());
    }
    assert_eq!(copied(&selection, &messages).as_deref(), Some("second"));
    cx.simulate_event(MouseDownEvent {
        position: on_second,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 3,
        first_mouse: false,
    });
    cx.simulate_mouse_up(on_second, MouseButton::Left, Modifiers::none());
    assert_eq!(copied(&selection, &messages).as_deref(), Some("second line"));
}

#[test]
fn a_click_on_a_link_still_opens_it_and_selects_nothing() {
    let (selection, _, cx) = drawn();
    let first = paragraphs(cx)[0];
    // "plain bold " is before the link: find a spot on it by walking right until the click opens it
    let opened = (0..60u8).any(|step| {
        let at = first + point(px(4.0 * f32::from(step)), px(0.0));
        cx.simulate_click(at, Modifiers::none());
        cx.opened_url().is_some()
    });
    assert!(opened, "a click on the link opens it");
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.test/opened"));
    assert!(selection.is_empty(), "a click selects nothing");
}
