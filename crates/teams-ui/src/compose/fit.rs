//! How the box's tools fit its own width, as Teams' box does it. The rule depends only on the width the box was last
//! drawn at, so a narrow window, a thread's pane and a call's Chat pane give the same box.
//!
//! - A box at least [`NARROW_BOX`] wide, or one whose text keeps [`MIN_TEXT`] beside all its tools, stays on one line.
//! - A narrower box is stacked, as Teams' narrow box is: the text has the full width on top, and the tools and Send
//!   go on a row under it. The row holds Teams' own tools (Format, Emoji, Attach, Loop, Actions and apps) while they
//!   fit. This app's own tools (Correct, Translate and its language) and each Teams tool that does not fit go into
//!   "More options", as the user asked for this width. Send always stays on the row.
//!
//! A tool in the menu keeps its action, its key (shown in its row, bound in the box's key context as before) and its
//! state (checked while its button would be lit). The language row opens the same language menu, by More options.

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, SharedString, Styled as _, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::{Selectable as _, Sizable as _, Size};

use super::{AttachFiles, Compose, CorrectDraft, NARROW_BOX, TOOL, ToggleEmoji, TranslateDraft};
use crate::draft_translate::menu;
use crate::editor;
use crate::icons::{Glyph, icon_button, selected_by};

/// The least width of the text beside the tools on one line, px; a box that cannot give it is stacked.
pub(super) const MIN_TEXT: f32 = 160.0;
/// The border and the padding left and right of a box on one line, px.
const LINE_CHROME: f32 = 2.0 + 12.0 + 8.0;
/// The border and the padding left and right of a stacked box, px.
const STACKED_CHROME: f32 = 2.0 + 12.0 + 12.0;
/// A tool button and the gap after it, px.
const SLOT: f32 = TOOL + 4.0;
/// The compact language caret and the gap after it, px.
const CARET_SLOT: f32 = 24.0;
/// The divider and Send at the end of the row, with their gaps, px.
const END: f32 = 4.0 + 1.0 + 8.0 + 4.0 + TOOL;

/// A tool of the box, Send apart, in the order the box shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    Format,
    Emoji,
    Attach,
    Loop,
    Apps,
    Correct,
    Translate,
    Language,
}

impl Tool {
    /// A tool of Teams' own box, which a stacked row keeps while it fits.
    fn of_teams(self) -> bool {
        matches!(
            self,
            Self::Format | Self::Emoji | Self::Attach | Self::Loop | Self::Apps
        )
    }

    fn slot(self) -> f32 {
        if self == Self::Language { CARET_SLOT } else { SLOT }
    }

    /// The tool's selector: its button's, and with `-row` its row's in the menu.
    #[must_use]
    pub(crate) fn selector(self) -> &'static str {
        match self {
            Self::Format => "compose-format",
            Self::Emoji => "compose-emoji",
            Self::Attach => "compose-attach",
            Self::Loop => "compose-loop",
            Self::Apps => "compose-apps",
            Self::Correct => "compose-correct",
            Self::Translate => "compose-translate",
            Self::Language => "compose-language",
        }
    }

    fn glyph(self) -> Glyph {
        match self {
            Self::Format => Glyph::TEXT_FORMAT,
            Self::Emoji => Glyph::EMOJI,
            Self::Attach => Glyph::PAPERCLIP,
            Self::Loop => Glyph::REPEAT,
            Self::Apps => Glyph::PLUS,
            Self::Correct => Glyph::SPELL_CHECK,
            Self::Translate | Self::Language => Glyph::LANGUAGES,
        }
    }

    fn action(self) -> Option<&'static dyn gpui::Action> {
        match self {
            Self::Format => Some(&editor::ToggleExpanded),
            Self::Emoji => Some(&ToggleEmoji),
            Self::Attach => Some(&AttachFiles),
            Self::Correct => Some(&CorrectDraft),
            Self::Translate => Some(&TranslateDraft),
            Self::Loop | Self::Apps | Self::Language => None,
        }
    }
}

/// Where each tool of a box goes at its width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fit {
    /// The text on top and the tools on a row under it.
    pub(crate) stacked: bool,
    /// The tools on the box's row, in order.
    pub(crate) row: Vec<Tool>,
    /// The tools in "More options", in order.
    pub(crate) more: Vec<Tool>,
}

/// Where `tools` go in a box last drawn `width` wide (`None`: not drawn yet, so as a wide box).
#[must_use]
pub(crate) fn fit(width: Option<Pixels>, tools: &[Tool]) -> Fit {
    let all = || Fit {
        stacked: false,
        row: tools.to_vec(),
        more: Vec::new(),
    };
    let Some(width) = width.map(f32::from).filter(|width| *width < NARROW_BOX) else {
        return all();
    };
    let tools_width: f32 = tools.iter().map(|tool| tool.slot()).sum();
    if width - LINE_CHROME - tools_width - END - 4.0 >= MIN_TEXT {
        return all();
    }
    let room = width - STACKED_CHROME - END - SLOT;
    let mut used = 0.0;
    let row: Vec<Tool> = tools
        .iter()
        .copied()
        .take_while(|tool| {
            used += tool.slot();
            tool.of_teams() && used <= room
        })
        .collect();
    let more = tools.iter().copied().filter(|tool| !row.contains(tool)).collect();
    Fit {
        stacked: true,
        row,
        more,
    }
}

/// What a menu row shows of its tool at the time the menu opens.
#[derive(Clone)]
struct Row {
    tool: Tool,
    label: SharedString,
    disabled: bool,
    checked: bool,
}

impl Compose {
    /// Where this box's tools go at the width it was last drawn at.
    pub(super) fn fit(&self) -> Fit {
        let mut tools = vec![Tool::Format, Tool::Emoji];
        if self.writes_a_chat() {
            tools.extend([Tool::Attach, Tool::Loop, Tool::Apps]);
        }
        tools.extend([Tool::Correct, Tool::Translate, Tool::Language]);
        fit(self.box_at.get().map(|bounds| bounds.size.width), &tools)
    }

    /// Whether the box was last drawn stacked: the text on top, its tools on a row under it.
    #[must_use]
    pub fn is_stacked(&self) -> bool {
        self.fit().stacked
    }

    /// What a tool's row in the menu says, and whether it is off or lit, as its button would be.
    fn row(&self, tool: Tool, cx: &App) -> Row {
        let (label, disabled, checked): (SharedString, bool, bool) = match tool {
            Tool::Format => {
                let label = if self.expanded {
                    "Hide formatting options"
                } else {
                    "Show formatting options"
                };
                (label.into(), false, self.expanded)
            }
            Tool::Emoji => ("Emoji".into(), false, self.emoji.is_some()),
            Tool::Attach => ("Attach files".into(), !self.accepts_files(), false),
            Tool::Loop => ("Loop components".into(), true, false),
            Tool::Apps => ("Actions and apps".into(), true, false),
            Tool::Correct => {
                let (label, lit, can) = self.correct_state(cx);
                (label.into(), !lit && !can, lit)
            }
            Tool::Translate => {
                let (label, busy, can) = self.translate_state(cx);
                (label, !busy && !can, busy)
            }
            Tool::Language => {
                let language = self.settings.read(cx).settings().message_translate_to.clone();
                (format!("Translation language: {language}").into(), false, false)
            }
        };
        Row {
            tool,
            label,
            disabled,
            checked,
        }
    }

    /// Does what a tool's button does.
    fn take(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        match tool {
            Tool::Format => self.toggle_expanded(window, cx),
            Tool::Emoji => self.toggle_emoji(window, cx),
            Tool::Attach => self.choose_files(window, cx),
            Tool::Correct => self.correct_draft(window, cx),
            Tool::Translate => self.translate_draft(window, cx),
            Tool::Language => self.open_language_menu(window, cx),
            Tool::Loop | Tool::Apps => {}
        }
    }

    /// Opens "More options" (its button, tests, the tour): the keyboard goes to its first row.
    pub fn open_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let more = self.fit().more;
        if more.is_empty() || self.more.is_some() {
            return;
        }
        let rows: Vec<Row> = more.iter().map(|tool| self.row(*tool, cx)).collect();
        let compose = cx.entity().downgrade();
        let text = self.editor.read(cx).focus.clone();
        let menu = PopupMenu::build(window, cx, move |menu, window, _| {
            rows.iter()
                .fold(menu.min_w(px(240.0)).action_context(text.clone()), |menu, row| {
                    let compose = compose.clone();
                    let tool = row.tool;
                    menu.item(menu_row(row, &text, window).on_click(move |_, window, cx| {
                        let _ = compose.update(cx, |this, cx| this.take(tool, window, cx));
                    }))
                })
        });
        let dismissed = cx.subscribe_in(&menu, window, |this: &mut Self, _, _: &DismissEvent, _, cx| {
            this.more = None;
            cx.notify();
        });
        menu.focus_handle(cx).focus(window, cx);
        self.more = Some((menu, dismissed));
        cx.notify();
    }

    /// Closes "More options", the keyboard back in the text.
    pub fn close_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.more.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// Whether "More options" is open.
    #[must_use]
    pub fn more_open(&self) -> bool {
        self.more.is_some()
    }

    /// "More options" on the box's row: a menu of the tools in `more`, each with its icon, its key and its state.
    /// Escape closes it and gives the keyboard back to the text. The language menu and the emoji picker of a tool in it
    /// open by this button.
    pub(super) fn more_button(&self, more: &[Tool], cx: &mut Context<Self>) -> AnyElement {
        let open = self.more.is_some();
        let menu = self.more.as_ref().map(|(menu, _)| menu.clone());
        let compose = cx.entity().downgrade();
        let button = icon_button("compose-more", Glyph::MORE, "More options").with_size(Size::Size(px(TOOL)));
        let popover = Popover::new("compose-more-popover")
            .anchor(gpui::Anchor::BottomLeft)
            .appearance(false)
            .trigger(button.selected(open))
            .open(open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = compose.update(cx, |this, cx| {
                    if open {
                        this.open_more(window, cx);
                    } else {
                        this.close_more(window, cx);
                    }
                });
            })
            .content(move |_, _, _| {
                div()
                    .debug_selector(|| "compose-more-menu".to_owned())
                    .children(menu.clone())
            });
        let language = more
            .contains(&Tool::Language)
            .then(|| self.hidden_language_anchor(cx))
            .flatten();
        let emoji_at = more
            .contains(&Tool::Emoji)
            .then(|| crate::chat_view::measured(&self.emoji_at));
        selected_by("compose-more", popover)
            .relative()
            .children(emoji_at)
            .children(language)
            .into_any_element()
    }

    /// The language menu's anchor while its button is in "More options": a face of no width at the right of the More
    /// button, so the menu its row opens shows by it.
    fn hidden_language_anchor(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu: Entity<menu::LanguageMenu> = self.language_menu()?;
        let face = Button::new("compose-language").ghost().with_size(Size::XSmall);
        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .w(px(0.0))
                .h_full()
                .overflow_hidden()
                .child(menu::popover(
                    &menu,
                    "compose-language-popover",
                    gpui::Anchor::BottomLeft,
                    face,
                    cx,
                ))
                .into_any_element(),
        )
    }
}

/// A tool's row in the menu: its icon, its name and its key as bound where the `text` is, `compose-<tool>-row` for
/// tests.
fn menu_row(row: &Row, text: &FocusHandle, window: &Window) -> PopupMenuItem {
    let selector = format!("{}-row", row.tool.selector());
    let label = row.label.clone();
    let key = row
        .tool
        .action()
        .and_then(|action| Kbd::binding_for_action_in(action, text, window));
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div()
            .debug_selector(move || selector.clone())
            .flex_1()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(label.clone())
            .children(key.clone())
    })
    .icon(row.tool.glyph().icon())
    .disabled(row.disabled)
    .checked(row.checked)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: [Tool; 8] = [
        Tool::Format,
        Tool::Emoji,
        Tool::Attach,
        Tool::Loop,
        Tool::Apps,
        Tool::Correct,
        Tool::Translate,
        Tool::Language,
    ];
    const CHANNEL: [Tool; 5] = [
        Tool::Format,
        Tool::Emoji,
        Tool::Correct,
        Tool::Translate,
        Tool::Language,
    ];

    #[test]
    fn a_box_not_drawn_yet_or_wide_keeps_every_tool_on_one_line() {
        for width in [None, Some(px(NARROW_BOX)), Some(px(1200.0))] {
            let fit = fit(width, &CHAT);
            assert!(!fit.stacked);
            assert_eq!(fit.row, CHAT);
            assert!(fit.more.is_empty());
        }
    }

    #[test]
    fn a_box_in_a_call_s_chat_pane_stacks_with_teams_tools_on_its_row() {
        // the Chat pane is 360 px, its box 328 px
        let fit = fit(Some(px(328.0)), &CHAT);
        assert!(fit.stacked);
        assert_eq!(
            fit.row,
            [Tool::Format, Tool::Emoji, Tool::Attach, Tool::Loop, Tool::Apps]
        );
        assert_eq!(fit.more, [Tool::Correct, Tool::Translate, Tool::Language]);
    }

    #[test]
    fn a_thread_s_box_with_room_for_its_text_stays_on_one_line() {
        // the thread's pane is 440 px, its box 408 px
        let fit = fit(Some(px(408.0)), &CHANNEL);
        assert!(!fit.stacked);
        assert_eq!(fit.row, CHANNEL);
    }

    #[test]
    fn a_very_narrow_box_moves_the_last_teams_tools_into_the_menu_too() {
        let fit = fit(Some(px(200.0)), &CHAT);
        assert!(fit.stacked);
        assert_eq!(fit.row, [Tool::Format, Tool::Emoji, Tool::Attach]);
        assert_eq!(fit.more.first(), Some(&Tool::Loop));
        assert_eq!(fit.row.len() + fit.more.len(), CHAT.len());
    }

    #[test]
    fn a_box_with_no_room_for_one_tool_keeps_more_options_and_send() {
        let fit = fit(Some(px(60.0)), &CHAT);
        assert!(fit.row.is_empty());
        assert_eq!(fit.more, CHAT);
    }
}
