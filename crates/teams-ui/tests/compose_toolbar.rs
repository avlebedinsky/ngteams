//! The message box's toolbar, less crowded, on GPUI's test context over the fake engine (`common`): one AI button
//! with a menu in place of Correct, Translate and the language, and buttons the user hides, by a right click on the
//! toolbar or "Customize toolbar" in "+", or in Settings. A hidden button's action stays in "+", with its key. Send
//! and "+" cannot hide. With the AI features off, nothing of AI shows. Every id, name and text is made up.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use common::Rig;
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, px, size};
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, MenuKind, Writes};
use teams_ui::settings::{ComposeTool, Settings, SettingsModel};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// The rows of the AI button's menu.
const AI_ROWS: [&str; 3] = ["compose-correct-row", "compose-translate-row", "compose-language-row"];

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

struct Opened {
    compose: Entity<Compose>,
    settings: Entity<SettingsModel>,
    cx: &'static mut VisualTestContext,
}

/// The a–b chat in a window `width` px wide over `settings`, its box focused.
fn chat_with(rig: &Rig, settings: Settings, width: f32) -> Opened {
    rig.cx.update(teams_ui::window::init);
    rig.cx.update(teams_ui::chat_view::init);
    let parts = parts::parts(rig, settings, Arc::new(ViewTrace::off()));
    let model = parts.settings.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(width), px(640.0)));
    draw(cx);
    let compose = view.read_with(cx, |view, _| view.compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    Opened {
        compose,
        settings: model,
        cx,
    }
}

fn hiding(tools: &[ComposeTool]) -> Settings {
    Settings {
        compose_hidden: tools.to_vec(),
        ..Settings::default()
    }
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
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

fn right_click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not shown"))
        .center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    draw(cx);
}

fn hidden_tools(settings: &Entity<SettingsModel>, cx: &mut VisualTestContext) -> Vec<ComposeTool> {
    settings.read_with(cx, |model, _| model.settings().compose_hidden.clone())
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
fn one_ai_button_holds_correct_translate_and_the_language() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, Settings::default(), 900.0);
    assert!(shown(cx, "compose-ai"));
    for gone in ["compose-correct", "compose-translate", "compose-language"] {
        assert!(!shown(cx, gone), "{gone} is in the AI button's menu, not on the bar");
    }
    type_in(&compose, cx, "A made-up draft");
    click(cx, "compose-ai");
    assert!(shown(cx, "compose-ai-menu"));
    assert_eq!(
        compose.read_with(cx, |compose, _| compose.menu_open()),
        Some(MenuKind::Ai)
    );
    for row in AI_ROWS {
        assert!(shown(cx, row), "{row} is in the AI button's menu");
    }
    // the language row opens the language menu by the AI button, and the AI menu closes
    click(cx, "compose-language-row");
    assert!(!shown(cx, "compose-ai-menu"));
    assert!(shown(cx, "compose-language-menu"));
    let ai = cx.debug_bounds("compose-ai").unwrap();
    let menu = cx.debug_bounds("compose-language-menu").unwrap();
    assert!(
        menu.bottom() <= ai.top() + px(1.0),
        "the menu opens over the button: {menu:?}, {ai:?}"
    );
    cx.simulate_keystrokes("escape");
    draw(cx);
    // Escape closes the AI menu and gives the keyboard back to the text
    click(cx, "compose-ai");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-ai-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_right_click_on_the_toolbar_lists_its_buttons_and_a_click_hides_one() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, Settings::default(), 900.0);
    right_click(cx, "compose-attach");
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
    assert!(shown(cx, "compose-customize-menu"));
    for tool in ["format", "emoji", "attach", "loop", "ai", "apps", "send"] {
        let check: &'static str = Box::leak(format!("compose-customize-{tool}-check").into_boxed_str());
        assert!(shown(cx, check), "{tool} shows, so its row is checked");
    }
    click(cx, "compose-customize-emoji");
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Emoji]);
    assert!(!shown(cx, "compose-emoji"), "the button leaves the bar");
    // the list stays open, the row unchecked; a second click brings the button back
    assert!(shown(cx, "compose-customize-menu"));
    assert!(!shown(cx, "compose-customize-emoji-check"));
    click(cx, "compose-customize-emoji");
    assert!(hidden_tools(&settings, cx).is_empty());
    assert!(shown(cx, "compose-emoji"));
    // Send and "+" cannot hide
    click(cx, "compose-customize-send");
    click(cx, "compose-customize-apps");
    assert!(hidden_tools(&settings, cx).is_empty());
    assert!(shown(cx, "compose-send") && shown(cx, "compose-apps"));
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!shown(cx, "compose-customize-menu"));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn a_right_click_opens_the_list_over_the_toolbar_s_buttons() {
    let rig = Rig::start();
    let Opened { cx, .. } = chat_with(&rig, Settings::default(), 1280.0);
    right_click(cx, "compose-emoji");
    let list = cx.debug_bounds("compose-customize-menu").unwrap();
    for tool in ["compose-format", "compose-emoji", "compose-ai", "compose-send"] {
        let button = cx.debug_bounds(tool).unwrap();
        assert!(
            list.bottom() <= button.top() + px(1.0),
            "the list leaves {tool} visible: {list:?}, {button:?}"
        );
    }
}

#[test]
fn the_list_s_keys_pass_over_its_title_and_go_round() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, Settings::default(), 900.0);
    right_click(cx, "compose-attach");
    // the first Down selects Format, not the title
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    assert!(shown(cx, "compose-customize-menu"), "the list stays open");
    // the row stays selected: Enter shows Format again
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(hidden_tools(&settings, cx).is_empty());
    // Up from Format goes round to AI tools, the last row that can hide; Down from it to Format
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Ai]);
    cx.simulate_keystrokes("enter down enter");
    draw(cx);
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(!compose.read_with(cx, |compose, _| compose.customize_open()));
    assert!(cx.update(|window, cx| compose.read(cx).focused(window, cx)));
}

#[test]
fn customize_toolbar_in_plus_opens_the_same_list() {
    let rig = Rig::start();
    let Opened { settings, cx, .. } = chat_with(&rig, Settings::default(), 900.0);
    click(cx, "compose-apps");
    assert!(shown(cx, "compose-apps-menu"), "\"+\" opens its menu");
    click(cx, "compose-customize-row");
    assert!(shown(cx, "compose-customize-menu"));
    click(cx, "compose-customize-format");
    assert_eq!(hidden_tools(&settings, cx), [ComposeTool::Format]);
    assert!(!shown(cx, "compose-format"));
}

#[test]
fn a_hidden_button_s_action_stays_in_plus_with_its_key() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(
        &rig,
        hiding(&[ComposeTool::Emoji, ComposeTool::Format, ComposeTool::Ai]),
        900.0,
    );
    for gone in ["compose-emoji", "compose-format", "compose-ai"] {
        assert!(!shown(cx, gone), "{gone} is hidden");
    }
    assert!(shown(cx, "compose-attach") && shown(cx, "compose-send"));
    click(cx, "compose-apps");
    for row in [
        "compose-format-row",
        "compose-emoji-row",
        "compose-correct-row",
        "compose-translate-row",
        "compose-language-row",
        "compose-customize-row",
    ] {
        assert!(shown(cx, row), "{row} is in \"+\"");
    }
    // each row of an action with a key shows that key
    for key in [
        "compose-format-row-key",
        "compose-emoji-row-key",
        "compose-correct-row-key",
        "compose-translate-row-key",
    ] {
        assert!(shown(cx, key), "{key} shows in \"+\"");
    }
    assert!(
        !shown(cx, "compose-attach-row"),
        "a button on the bar has no row in \"+\""
    );
    // a row does what its button did: the emoji picker opens by "+"
    click(cx, "compose-emoji-row");
    assert!(compose.read_with(cx, |compose, _| compose.emoji_picker().is_some()));
    cx.simulate_keystrokes("escape");
    draw(cx);
    // the keys work as before while their buttons are hidden
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    cx.simulate_keystrokes("ctrl-shift-x");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.is_expanded()));
}

#[test]
fn the_choice_applies_to_every_box() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Format]), 900.0);
    assert!(!shown(cx, "compose-format"));
    // an edit's box
    let loaded = teams_ui::compose::Loaded {
        doc: teams_ui::editor::doc::Doc::new(vec![teams_ui::editor::doc::Block::paragraph("hello")]),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    draw(cx);
    assert!(shown(cx, "compose-mode"));
    assert!(!shown(cx, "compose-format"));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.cancel(window, cx)));
    draw(cx);
    // a channel's post and a thread's reply
    for writes in [Writes::Channel, Writes::Thread] {
        compose.update(cx, |compose, cx| {
            compose.in_channel(writes, "Message in A made-up channel", cx);
        });
        draw(cx);
        assert!(!shown(cx, "compose-format"), "{writes:?}");
        assert!(shown(cx, "compose-apps"), "{writes:?}: its \"+\" holds Format");
        click(cx, "compose-apps");
        assert!(shown(cx, "compose-format-row"), "{writes:?}");
        cx.simulate_keystrokes("escape");
        draw(cx);
    }
}

#[test]
fn with_the_ai_features_off_nothing_of_ai_shows() {
    let rig = Rig::start();
    let Opened {
        compose, settings, cx, ..
    } = chat_with(&rig, hiding(&[ComposeTool::Ai]), 900.0);
    cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(false, cx)));
    draw(cx);
    assert!(!shown(cx, "compose-ai"));
    click(cx, "compose-apps");
    for row in AI_ROWS {
        assert!(!shown(cx, row), "{row} is not in \"+\" while the AI features are off");
    }
    cx.simulate_keystrokes("escape");
    draw(cx);
    right_click(cx, "compose-attach");
    assert!(shown(cx, "compose-customize-menu"));
    assert!(!shown(cx, "compose-customize-ai"), "no AI row in the customize list");
    cx.simulate_keystrokes("escape");
    draw(cx);
    // on again: the AI button stays where the user put it, in "+"
    cx.update(|_, cx| settings.update(cx, |model, cx| model.set_ai_features(true, cx)));
    draw(cx);
    assert!(!shown(cx, "compose-ai"));
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    click(cx, "compose-apps");
    assert!(shown(cx, "compose-correct-row"));
}

#[test]
fn a_very_narrow_box_puts_the_hidden_actions_in_more_options() {
    let rig = Rig::start();
    let Opened { compose, cx, .. } = chat_with(&rig, hiding(&[ComposeTool::Emoji]), 150.0);
    assert!(!shown(cx, "compose-apps"), "\"+\" is in More options too");
    click(cx, "compose-more");
    assert!(shown(cx, "compose-emoji-row") && shown(cx, "compose-customize-row"));
    // the menu is wider than this window: the keyboard takes the last row, Customize toolbar
    cx.simulate_keystrokes("up enter");
    draw(cx);
    assert!(compose.read_with(cx, |compose, _| compose.customize_open()));
}
