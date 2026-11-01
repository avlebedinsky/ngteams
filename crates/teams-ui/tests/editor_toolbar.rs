//! The editor's format bar acts on the selected text, preserves focus, and emits owner actions on GPUI's test platform.

#![cfg(test)]

use gpui::{
    AppContext as _, Context, Entity, IntoElement, Modifiers, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div, px, size,
};
use teams_proto::chat::{FontSize, Marks, Rgb};
use teams_ui::editor::Editor;
use teams_ui::editor::doc::{Block, Doc, Kind, Pos};
use teams_ui::editor::toolbar::{FONT_COLOURS, FormatBar, FormatBarEvent, HIGHLIGHTS};

struct Host {
    editor: Entity<Editor>,
    bar: Entity<FormatBar>,
    discarded: usize,
    _events: gpui::Subscription,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(self.bar.clone())
            .child(self.editor.clone())
    }
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn window() -> (Entity<Host>, &'static mut VisualTestContext) {
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    app.update(teams_ui::window::init);
    let (host, cx) = app.add_window_view(|window, cx| {
        let editor = cx.new(|cx| Editor::new(window, cx));
        let bar = cx.new(|cx| FormatBar::new(editor.clone(), cx));
        let events = cx.subscribe(&bar, |host: &mut Host, _, event, _| {
            if *event == FormatBarEvent::Discard {
                host.discarded += 1;
            }
        });
        editor.update(cx, |editor, cx| {
            editor.insert("Synthetic text", cx);
            editor.focus(window, cx);
        });
        Host {
            editor,
            bar,
            discarded: 0,
            _events: events,
        }
    });
    cx.simulate_resize(size(px(850.), px(700.)));
    draw(cx);
    cx.simulate_keystrokes("ctrl-a");
    draw(cx);
    (host, cx)
}

fn editor(host: &Entity<Host>, cx: &mut VisualTestContext) -> Entity<Editor> {
    cx.update(|_, cx| host.read(cx).editor.clone())
}

#[test]
fn toolbar_marks_colours_and_sizes_change_the_selection_and_restore_editor_focus() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    for (selector, mark) in [
        ("format-bold", Marks::BOLD),
        ("format-italic", Marks::ITALIC),
        ("format-underline", Marks::UNDERLINE),
        ("format-strikethrough", Marks::STRIKE),
    ] {
        click(cx, selector);
        assert!(cx.update(|window, cx| editor.read(cx).has_mark(mark) && editor.read(cx).is_focused(window)));
    }
    click(cx, "format-colour");
    click(cx, "format-colour-0");
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).current_attrs().span.color),
        Some(Rgb(FONT_COLOURS[0].1))
    );
    click(cx, "format-highlight");
    click(cx, "format-highlight-0");
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).current_attrs().span.highlight),
        Some(Rgb(HIGHLIGHTS[0].1))
    );
    for (selector, expected) in [
        ("format-size-small", Some(FontSize::Small)),
        ("format-size-large", Some(FontSize::Large)),
        ("format-size-medium", None),
    ] {
        click(cx, "format-size");
        click(cx, selector);
        assert_eq!(cx.update(|_, cx| editor.read(cx).current_attrs().span.size), expected);
    }
    click(cx, "format-colour");
    click(cx, "format-colour-none");
    click(cx, "format-highlight");
    click(cx, "format-highlight-none");
    assert!(cx.update(|_, cx| {
        let attrs = editor.read(cx).current_attrs();
        attrs.span.color.is_none() && attrs.span.highlight.is_none()
    }));
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "Synthetic text");
}

#[test]
fn block_and_history_controls_change_the_document_and_discard_notifies_the_owner() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    for (selector, kind) in [
        ("format-bullets", Kind::Bullet),
        ("format-numbers", Kind::Numbered),
        ("format-quote", Kind::Quote),
    ] {
        click(cx, selector);
        assert_eq!(cx.update(|_, cx| editor.read(cx).block_kind()), kind);
    }
    click(cx, "format-more");
    click(cx, "format-heading-2");
    assert_eq!(cx.update(|_, cx| editor.read(cx).block_kind()), Kind::Heading(2));
    click(cx, "format-more");
    click(cx, "format-paragraph");
    click(cx, "format-more");
    click(cx, "format-indent");
    assert_eq!(cx.update(|_, cx| editor.read(cx).doc().blocks[0].indent), 1);
    click(cx, "format-more");
    click(cx, "format-outdent");
    assert_eq!(cx.update(|_, cx| editor.read(cx).doc().blocks[0].indent), 0);
    click(cx, "format-more");
    click(cx, "format-paragraph");
    click(cx, "format-bold");
    click(cx, "format-more");
    click(cx, "format-clear");
    assert!(!cx.update(|_, cx| editor.read(cx).has_mark(Marks::BOLD)));
    click(cx, "format-more");
    click(cx, "format-undo");
    assert!(cx.update(|_, cx| editor.read(cx).has_mark(Marks::BOLD)));
    click(cx, "format-more");
    click(cx, "format-redo");
    assert!(!cx.update(|_, cx| editor.read(cx).has_mark(Marks::BOLD)));
    click(cx, "format-more");
    click(cx, "format-rule");
    assert!(cx.update(|_, cx| {
        editor
            .read(cx)
            .doc()
            .blocks
            .iter()
            .any(|block| block.kind == Kind::Rule)
    }));
    click(cx, "format-discard");
    assert_eq!(cx.update(|_, cx| host.read(cx).discarded), 1);
}

#[test]
fn the_link_dialog_keeps_the_selected_text_and_normalizes_its_address() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    click(cx, "format-link");
    cx.simulate_input("docs.example.test/guide");
    draw(cx);
    click(cx, "format-link-insert");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "Synthetic text");
    assert!(cx.update(|_, cx| {
        editor.read(cx).doc().blocks[0]
            .runs
            .iter()
            .any(|run| run.attrs.link.as_deref() == Some("https://docs.example.test/guide"))
    }));
    cx.simulate_keystrokes("right");
    click(cx, "format-link");
    click(cx, "format-link-cancel");
    assert!(cx.debug_bounds("format-menu").is_none());
    click(cx, "format-size");
    cx.update(|window, cx| {
        host.read(cx).bar.clone().update(cx, |bar, cx| {
            bar.toggle(teams_ui::editor::toolbar::Menu::FontSize, window, cx);
        });
    });
    draw(cx);
    assert!(
        cx.debug_bounds("format-menu").is_none(),
        "toggling the same menu closes it"
    );
}

fn document(editor: &Entity<Editor>, cx: &mut VisualTestContext, lines: &[&str]) {
    let doc = Doc::new(lines.iter().map(|line| Block::paragraph(line)).collect());
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_doc(doc, cx)));
    draw(cx);
}

#[test]
fn keyboard_navigation_selects_characters_words_lines_and_the_whole_document() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    document(&editor, cx, &["alpha beta", "гамма delta", "omega"]);
    cx.simulate_keystrokes("ctrl-home right shift-right");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "l");
    cx.simulate_keystrokes("left ctrl-left ctrl-shift-right");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "alpha");
    cx.simulate_keystrokes("ctrl-home ctrl-right");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selection().head), Pos::new(0, 5));
    cx.simulate_keystrokes("ctrl-home down shift-end");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "гамма delta");
    cx.simulate_keystrokes("right shift-home");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "гамма delta");
    cx.simulate_keystrokes("ctrl-home shift-down");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "alpha beta\n");
    cx.simulate_keystrokes("ctrl-end ctrl-shift-left");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), "omega");
    cx.simulate_keystrokes("ctrl-home ctrl-shift-end");
    let all = "alpha beta\nгамма delta\nomega";
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), all);
    cx.simulate_keystrokes("ctrl-end ctrl-shift-home");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_text()), all);
    cx.simulate_keystrokes("ctrl-end up shift-up shift-down home end");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), all);
}

#[test]
fn keyboard_word_deletion_and_undo_redo_keep_paragraph_boundaries_consistent() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    document(&editor, cx, &["one two three", "next"]);
    cx.simulate_keystrokes("ctrl-home ctrl-delete");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), " two three\nnext");
    cx.simulate_keystrokes("delete");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "two three\nnext");
    cx.simulate_keystrokes("ctrl-end ctrl-backspace backspace");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "two three");
    cx.simulate_keystrokes("shift-backspace");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "two thre");
    cx.simulate_keystrokes("ctrl-z");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "two three");
    cx.simulate_keystrokes("ctrl-y");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "two thre");
    cx.simulate_keystrokes("ctrl-home shift-right delete");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "wo thre");
}

#[test]
fn cut_and_plain_paste_preserve_text_and_read_only_edits_leave_the_document_intact() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    cx.simulate_keystrokes("ctrl-b ctrl-c ctrl-x");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("Synthetic text")
    );
    assert!(cx.update(|_, cx| editor.read(cx).is_empty()));
    cx.simulate_keystrokes("ctrl-v");
    assert!(cx.update(|_, cx| {
        editor.read(cx).doc().blocks[0].runs[0]
            .attrs
            .marks
            .contains(Marks::BOLD)
    }));
    document(&editor, cx, &[""]);
    cx.simulate_keystrokes("ctrl-shift-v");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "Synthetic text");
    assert!(!cx.update(|_, cx| {
        editor.read(cx).doc().blocks[0].runs[0]
            .attrs
            .marks
            .contains(Marks::BOLD)
    }));
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_read_only(true, cx)));
    cx.simulate_keystrokes("ctrl-a ctrl-x delete backspace ctrl-v ctrl-shift-v ctrl-b");
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "Synthetic text");
    assert!(!cx.update(|_, cx| {
        editor.read(cx).doc().blocks[0].runs[0]
            .attrs
            .marks
            .contains(Marks::BOLD)
    }));
}

#[test]
fn keyboard_block_formatting_and_list_indentation_follow_the_selected_text() {
    let (host, cx) = window();
    let editor = editor(&host, cx);
    for (key, kind) in [
        ("ctrl-alt-1", Kind::Heading(1)),
        ("ctrl-alt-2", Kind::Heading(2)),
        ("ctrl-alt-3", Kind::Heading(3)),
        ("ctrl-alt-4", Kind::Quote),
        ("ctrl-alt-p", Kind::Paragraph),
        ("ctrl-shift-8", Kind::Bullet),
        ("ctrl-shift-7", Kind::Numbered),
    ] {
        cx.simulate_keystrokes(key);
        assert_eq!(cx.update(|_, cx| editor.read(cx).block_kind()), kind);
    }
    cx.simulate_keystrokes("tab");
    assert_eq!(cx.update(|_, cx| editor.read(cx).doc().blocks[0].indent), 1);
    cx.simulate_keystrokes("shift-tab");
    assert_eq!(cx.update(|_, cx| editor.read(cx).doc().blocks[0].indent), 0);
    cx.simulate_keystrokes("ctrl-shift-b");
    assert_eq!(cx.update(|_, cx| editor.read(cx).block_kind()), Kind::Code);
    cx.simulate_keystrokes("ctrl-shift-b");
    assert_eq!(cx.update(|_, cx| editor.read(cx).block_kind()), Kind::Paragraph);
    cx.simulate_keystrokes("ctrl-a ctrl-alt-x");
    assert!(cx.update(|_, cx| editor.read(cx).has_mark(Marks::STRIKE)));
    cx.dispatch_action(teams_ui::editor::InlineCode);
    assert!(cx.update(|_, cx| editor.read(cx).has_mark(Marks::CODE)));
    assert_eq!(cx.update(|_, cx| editor.read(cx).text()), "Synthetic text");
}
