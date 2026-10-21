//! The UI kit (`gpui-component`) on GPUI's test context: its `Textarea` as the compose box will use it (typing,
//! selection, copy, cut, paste of plain text only, undo, soft wrap, IME composition through GPUI's input handler,
//! Enter and Shift-Enter told apart), and our theme on it (Teams-like and compact, light and dark, switching live).
//! The clipboard is the test platform's own. The same text area on the real platform, in the UI sandbox, is
//! `tests/kit_window.rs`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use gpui::{
    AppContext as _, ClipboardEntry, ClipboardItem, ClipboardString, Context, Entity, EntityInputHandler as _, Image,
    ImageFormat, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    TestAppContext, VisualTestContext, Window, WindowAppearance, div, px, rgb,
};
use gpui_component::input::{Enter, InputEvent, Textarea, TextareaState};
use gpui_component::theme::{Theme, ThemeMode};
use teams_ui::theme::{self, DARK, DARK_PALETTE, FONT_SIZE, LIGHT, LIGHT_PALETTE};

/// The compose box's width: narrow, so a long line wraps.
const WIDTH: f32 = 240.0;

/// A window with one text area, configured as the compose box will be (Enter submits, Shift-Enter breaks the line,
/// soft wrap, growing to a few rows), recording the Enters it emits. The text area hands a plain Enter on to its
/// parent, which takes it as the send (as the compose box will): left to propagate, the platform would type it.
struct Compose {
    state: Entity<TextareaState>,
    /// Each `PressEnter`'s `shift`.
    enters: Vec<bool>,
    /// The Enters that reached the compose box as a send.
    sends: usize,
    renders: usize,
    _events: Subscription,
}

impl Compose {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .soft_wrap(true)
                .auto_grow(1, 6)
        });
        let events = cx.subscribe(&state, |this: &mut Self, _, event: &InputEvent, _| {
            if let InputEvent::PressEnter { shift, .. } = event {
                this.enters.push(*shift);
            }
        });
        state.update(cx, |state, cx| state.focus(window, cx));
        Self {
            state,
            enters: Vec::new(),
            sends: 0,
            renders: 0,
            _events: events,
        }
    }
}

impl Render for Compose {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders += 1;
        let colors = theme::current(cx);
        div()
            .size_full()
            .bg(rgb(colors.background))
            .on_action(cx.listener(|this: &mut Self, _: &Enter, _, _| this.sends += 1))
            .child(div().w(px(WIDTH)).child(Textarea::new(&self.state)))
    }
}

/// The kit and our theme set up, a compose window open and focused.
fn compose(cx: &mut TestAppContext) -> (Entity<Compose>, VisualTestContext) {
    cx.update(theme::init);
    let window = cx.add_window(Compose::new);
    let view = window.root(cx).expect("the window's view");
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    (view, cx)
}

fn value(view: &Entity<Compose>, cx: &mut VisualTestContext) -> String {
    cx.read(|cx| view.read(cx).state.read(cx).value().to_string())
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
}

fn type_text(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
}

#[gpui::test]
fn typed_text_goes_in_through_gpuis_input_handler(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "hello, lab");
    assert_eq!(value(&view, &mut cx), "hello, lab");
    keys(&mut cx, "backspace backspace backspace");
    assert_eq!(value(&view, &mut cx), "hello, ");
}

#[gpui::test]
fn enter_and_shift_enter_are_told_apart(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "one");
    keys(&mut cx, "shift-enter");
    type_text(&mut cx, "two");
    assert_eq!(value(&view, &mut cx), "one\ntwo", "Shift-Enter breaks the line");
    keys(&mut cx, "enter");
    assert_eq!(value(&view, &mut cx), "one\ntwo", "Enter adds no line break");
    assert_eq!(cx.read(|cx| view.read(cx).enters.clone()), vec![true, false]);
    assert_eq!(cx.read(|cx| view.read(cx).sends), 1, "only the plain Enter is a send");
}

#[gpui::test]
fn a_selection_is_copied_cut_and_pasted_on_the_test_clipboard(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "alpha beta");
    keys(&mut cx, "shift-left shift-left shift-left shift-left");
    let selected = cx.read(|cx| view.read(cx).state.read(cx).selected_value().to_string());
    assert_eq!(selected, "beta");
    keys(&mut cx, "ctrl-c");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("beta")
    );
    keys(&mut cx, "ctrl-a ctrl-x");
    assert_eq!(value(&view, &mut cx), "");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("alpha beta")
    );
    keys(&mut cx, "ctrl-v ctrl-v");
    assert_eq!(value(&view, &mut cx), "alpha betaalpha beta");
}

#[gpui::test]
fn a_paste_takes_plain_text_only(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    let picture = Image::from_bytes(ImageFormat::Png, vec![0x89, b'P', b'N', b'G']);
    cx.write_to_clipboard(ClipboardItem::new_image(&picture));
    keys(&mut cx, "ctrl-v");
    assert_eq!(value(&view, &mut cx), "", "an image on the clipboard pastes nothing");

    // text with an image beside it: the text only, with no markup or metadata of its own
    cx.write_to_clipboard(ClipboardItem {
        entries: vec![
            ClipboardEntry::Image(picture),
            ClipboardEntry::String(ClipboardString::new("<b>plain</b> text".to_owned()).with_json_metadata(1)),
        ],
    });
    keys(&mut cx, "ctrl-v");
    assert_eq!(value(&view, &mut cx), "<b>plain</b> text");
}

#[gpui::test]
fn undo_and_redo_step_through_the_edits(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "first");
    keys(&mut cx, "shift-enter");
    type_text(&mut cx, "second");
    keys(&mut cx, "ctrl-a ctrl-x");
    assert_eq!(value(&view, &mut cx), "");
    keys(&mut cx, "ctrl-z");
    assert_eq!(value(&view, &mut cx), "first\nsecond", "the cut undone");
    keys(&mut cx, "ctrl-z");
    assert_eq!(
        value(&view, &mut cx),
        "first\n",
        "the typing after the line break undone"
    );
    keys(&mut cx, "ctrl-y");
    assert_eq!(value(&view, &mut cx), "first\nsecond", "and done again");
}

#[gpui::test]
fn a_long_line_wraps_in_a_narrow_box(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "short");
    let rows = |view: &Entity<Compose>, cx: &mut VisualTestContext| {
        cx.read(|cx| view.read(cx).state.read(cx).display_map().wrap_row_count())
    };
    assert_eq!(rows(&view, &mut cx), 1);
    type_text(
        &mut cx,
        " and then a much longer line of words that cannot fit in two hundred and forty pixels",
    );
    cx.run_until_parked();
    assert!(
        rows(&view, &mut cx) > 1,
        "the long line wraps: {} rows",
        rows(&view, &mut cx)
    );
    assert!(!value(&view, &mut cx).contains('\n'), "a wrap is not a line break");
}

#[gpui::test]
fn an_ime_composition_is_marked_then_committed_through_the_input_handler(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    type_text(&mut cx, "hi ");
    let state = cx.read(|cx| view.read(cx).state.clone());
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.replace_and_mark_text_in_range(None, "n", Some(1..1), window, cx);
            state.replace_and_mark_text_in_range(None, "ni", Some(2..2), window, cx);
            assert_eq!(
                state.marked_text_range(window, cx),
                Some(3..5),
                "the composition is marked"
            );
            assert_eq!(state.value().as_ref(), "hi ni");
            state.replace_text_in_range(None, "你", window, cx);
            assert_eq!(state.marked_text_range(window, cx), None, "the commit ends it");
        });
    });
    assert_eq!(value(&view, &mut cx), "hi 你");
    keys(&mut cx, "ctrl-z");
    assert_eq!(value(&view, &mut cx), "hi ", "the composition undoes as one edit");
    // a composition given up (the IME clears its marked text) leaves nothing behind
    cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.replace_and_mark_text_in_range(None, "x", Some(1..1), window, cx);
            assert_eq!(state.value().as_ref(), "hi x");
            state.replace_and_mark_text_in_range(None, "", None, window, cx);
            assert_eq!(state.marked_text_range(window, cx), None);
        });
    });
    assert_eq!(value(&view, &mut cx), "hi ");
}

#[gpui::test]
fn the_theme_is_ours_compact_and_light_at_first(cx: &mut TestAppContext) {
    cx.update(theme::init);
    cx.read(|cx| {
        let kit = Theme::global(cx);
        assert_eq!(kit.mode, ThemeMode::Light);
        assert_eq!(kit.light_theme.name.as_ref(), LIGHT);
        assert_eq!(kit.dark_theme.name.as_ref(), DARK);
        assert_eq!(kit.font_size, px(FONT_SIZE));
        assert_eq!(kit.radius, px(4.0));
        assert!(!kit.shadow);
        assert_eq!(kit.background, rgb(LIGHT_PALETTE.background).into());
        assert_eq!(kit.primary, rgb(LIGHT_PALETTE.brand).into());
        assert_eq!(theme::current(cx), LIGHT_PALETTE);
    });
    // a second set-up changes nothing
    cx.update(|cx| {
        theme::apply(WindowAppearance::Dark, cx);
        theme::init(cx);
    });
    assert_eq!(cx.read(theme::mode), ThemeMode::Dark);
}

#[gpui::test]
fn the_theme_switches_live_and_every_window_redraws(cx: &mut TestAppContext) {
    let (view, mut cx) = compose(cx);
    let renders = |view: &Entity<Compose>, cx: &mut VisualTestContext| cx.read(|cx| view.read(cx).renders);
    let before = renders(&view, &mut cx);

    cx.update(|_, cx| theme::apply(WindowAppearance::Dark, cx));
    cx.run_until_parked();
    cx.read(|cx| {
        let kit = Theme::global(cx);
        assert_eq!(kit.mode, ThemeMode::Dark);
        assert_eq!(kit.background, rgb(DARK_PALETTE.background).into());
        assert_eq!(kit.foreground, rgb(DARK_PALETTE.foreground).into());
        assert_eq!(kit.font_size, px(FONT_SIZE), "compact in either mode");
    });
    let dark = renders(&view, &mut cx);
    assert!(dark > before, "the window redrew in the dark theme without being asked");

    cx.update(|_, cx| theme::apply(WindowAppearance::VibrantLight, cx));
    cx.run_until_parked();
    assert_eq!(cx.read(theme::current), LIGHT_PALETTE);
    assert!(renders(&view, &mut cx) > dark);
    assert_eq!(value(&view, &mut cx), "", "switching keeps the window's state");
}

#[gpui::test]
fn a_followed_window_sets_the_mode_from_its_appearance(cx: &mut TestAppContext) {
    let (_view, mut cx) = compose(cx);
    cx.update(|_, cx| theme::apply(WindowAppearance::Dark, cx));
    assert_eq!(cx.read(theme::mode), ThemeMode::Dark);
    // the test platform's windows are light: following one switches back at once
    let following = cx.update(theme::follow);
    assert_eq!(cx.read(theme::mode), ThemeMode::Light);
    drop(following);
}

#[gpui::test]
fn the_app_s_window_sets_the_kit_up_and_follows_its_appearance(cx: &mut TestAppContext) {
    let models = cx.update(teams_ui::Models::new);
    let window = cx
        .update(|cx| {
            teams_ui::window::open(
                cx,
                teams_ui::window::LAB_APP_ID,
                "lab a".into(),
                models,
                std::sync::Arc::new(teams_ui::ViewTrace::off()),
            )
        })
        .expect("the window opens");
    assert!(window.read_with(cx, |root, _| root.follows_appearance()).expect("open"));
    cx.read(|cx| {
        assert_eq!(Theme::global(cx).light_theme.name.as_ref(), LIGHT);
        assert_eq!(theme::mode(cx), ThemeMode::Light);
    });
    assert_eq!(teams_ui::view::BACKGROUND, LIGHT_PALETTE.background);
}
