//! The compose box at the width of a narrow pane (the call's Chat pane, a narrow window), on GPUI's test context over
//! the fake engine (`common`): the text field keeps a usable width, the tools that do not fit go into the box's "More
//! options" menu, and Send stays in the box. Each tool in the menu keeps its action, its key and its state. A wide box
//! keeps all its tools. Every id, name and text is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::Rig;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, Writes};
use teams_ui::settings::Settings;

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The width of the call's Chat pane, px: the box in it is the narrowest the app draws.
const PANE: f32 = 360.0;

/// The least width of the text field that a person can still type in and read, px.
const USABLE: f32 = 180.0;

/// The tools of a chat's box, in their order, Send apart.
const TOOLS: [&str; 8] = [
    "compose-format",
    "compose-emoji",
    "compose-attach",
    "compose-loop",
    "compose-apps",
    "compose-correct",
    "compose-translate",
    "compose-language",
];

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// The a–b chat in a window `width` px wide, its box focused.
fn chat(rig: &Rig, width: f32) -> (Entity<Compose>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(width), px(640.0)));
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    (compose, cx)
}

/// The selector of a tool's row in More options.
fn row(tool: &str) -> &'static str {
    Box::leak(format!("{tool}-row").into_boxed_str())
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

fn width(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
    f32::from(
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not shown"))
            .size
            .width,
    )
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not shown"))
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(cx);
}

fn type_in(compose: &Entity<Compose>, cx: &mut VisualTestContext, text: &str) {
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, |editor, cx| editor.insert(text, cx));
        });
    });
    draw(cx);
}

#[test]
fn a_narrow_box_keeps_its_text_field_usable_and_its_tools_in_more() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up message to see the text field");

    let text = width(cx, "rich-editor");
    assert!(text >= USABLE, "the text field is {text} px wide");
    assert!(
        shown(cx, "compose-more"),
        "the tools that do not fit go into More options"
    );
    assert!(shown(cx, "compose-send"), "Send stays in the box");
    assert!(compose.read_with(cx, |compose, _| compose.is_stacked()));
    let hidden: Vec<_> = TOOLS.iter().filter(|tool| cx.debug_bounds(tool).is_none()).collect();
    assert!(!hidden.is_empty(), "some tools go into the menu");
    // the box stays inside its pane
    let compose_box = cx.debug_bounds("compose-box").unwrap();
    assert!(f32::from(compose_box.right()) <= PANE, "{compose_box:?}");
}

#[test]
fn a_wide_box_keeps_all_its_tools_and_no_more_button() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 900.0);
    for tool in TOOLS {
        assert!(shown(cx, tool), "{tool} is in a wide box");
    }
    assert!(!compose.read_with(cx, |compose, _| compose.is_stacked()));
    assert!(!shown(cx, "compose-more"));
    assert!(shown(cx, "compose-send"));
}

#[test]
fn the_same_box_follows_its_own_width_both_ways() {
    let rig = Rig::start();
    let (_compose, cx) = chat(&rig, 900.0);
    cx.simulate_resize(size(px(PANE), px(640.0)));
    draw(cx);
    assert!(shown(cx, "compose-more"));
    assert!(width(cx, "rich-editor") >= USABLE);
    cx.simulate_resize(size(px(900.0), px(640.0)));
    draw(cx);
    assert!(!shown(cx, "compose-more"));
    for tool in TOOLS {
        assert!(shown(cx, tool), "{tool} comes back in a wide box");
    }
}

#[test]
fn the_menu_holds_each_tool_that_does_not_fit_and_its_rows_act() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    for tool in TOOLS {
        let in_box = shown(cx, tool);
        let in_menu = cx.debug_bounds(row(tool)).is_some();
        assert!(in_box != in_menu, "{tool}: in the box {in_box}, in the menu {in_menu}");
    }
    // the language row opens the language menu, as its button does, and the click closes More options
    click(cx, "compose-language-row");
    assert!(!shown(cx, "compose-more-menu"), "a row's click closes the menu");
    assert!(shown(cx, "compose-language-menu"));
    assert!(!compose.read_with(cx, |compose, _| compose.more_open()));
}

#[test]
fn escape_closes_the_menu_and_gives_the_keyboard_back_to_the_text() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-more-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn the_keyboard_moves_in_the_menu_and_enter_takes_a_row() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    click(cx, "compose-more");
    // Up goes to the last row that can be taken: the translation language, which opens the language menu
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert!(!shown(cx, "compose-more-menu"));
    assert!(
        shown(cx, "compose-language-menu"),
        "Enter takes the row the keyboard chose"
    );
    // Escape closes the language menu and the keyboard goes back to the text
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-language-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_channel_box_moves_only_the_tools_it_has() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    compose.update(cx, |compose, cx| {
        compose.in_channel(Writes::Channel, "Message in A made-up channel", cx);
    });
    draw(cx);
    assert!(width(cx, "rich-editor") >= USABLE);
    // its text cannot keep a usable width beside its tools: it stacks, and its own tools are in More options
    assert!(compose.read_with(cx, |compose, _| compose.is_stacked()));
    assert!(shown(cx, "compose-more"), "a narrow channel box has More options");
    click(cx, "compose-more");
    assert!(shown(cx, "compose-more-menu"));
    for tool in ["compose-correct", "compose-translate", "compose-language"] {
        assert!(shown(cx, row(tool)), "{tool} is in the menu of a channel's box");
    }
    for tool in ["compose-attach", "compose-loop", "compose-apps"] {
        assert!(!shown(cx, row(tool)), "{tool} is not in a channel's box");
        assert!(!shown(cx, tool), "{tool} is not in a channel's box");
    }
}

#[test]
fn an_open_menu_follows_the_state_of_its_tools() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, PANE);
    type_in(&compose, cx, "A made-up draft");
    click(cx, "compose-more");
    // the draft goes while the menu is open: Correct and Translate have nothing to work on now. Down selects the first
    // row, and the next Down skips the rows that are off and selects the language row
    cx.update(|_, cx| {
        compose.update(cx, |compose, cx| {
            compose.editor().update(cx, teams_ui::compose::Editor::clear);
        });
    });
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.more_open()));
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert!(
        shown(cx, "compose-language-menu"),
        "the rows show the state of their tools now"
    );
}

#[test]
fn in_a_very_narrow_box_every_tool_is_in_the_menu_and_keeps_its_action_and_key() {
    let rig = Rig::start();
    let (compose, cx) = chat(&rig, 150.0);
    for tool in TOOLS {
        assert!(!shown(cx, tool), "{tool} is in the menu");
    }
    assert!(shown(cx, "compose-more") && shown(cx, "compose-send"));

    // the keys work as before while their buttons are in the menu
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(!compose.read_with(cx, |compose, _| compose.is_expanded()));

    // each row does what its button does (by the keyboard: the menu is wider than this window)
    click(cx, "compose-more");
    assert!(shown(cx, "compose-format-row"));
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
    click(cx, "compose-more");
    cx.simulate_keystrokes("down down enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    assert!(compose.read_with(cx, |compose, _| !compose.more_open()));
}
