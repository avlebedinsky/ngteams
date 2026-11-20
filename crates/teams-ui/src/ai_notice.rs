//! The AI notice: one dialog, shown once before any AI use ([`SettingsModel::awaiting_ai_notice`]). It says which AI
//! features the app has, what runs them, who pays for them and where the text goes, and asks at once: keep them on or
//! turn them off. The answer is "Use AI features" in Settings under General ([`crate::settings::Settings::ai_features`]).
//! The shell shows it over everything after the first run's screen, except the banner of a call that rings
//! ([`crate::call_view::CallBar`]), as a modal dialog: it takes the keyboard and keeps Tab in itself (the kit's
//! `focus_trap`), and it fits the window at every font size, its points scrolled when they do not fit and its buttons
//! wrapped. A scroll bar shows beside the points whenever they do not fit, so the user sees
//! that more text follows.

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rems, rgb,
};
use gpui_component::FocusTrapElement as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::{Scrollbar, ScrollbarMode};

use crate::icons::{Glyph, selected_by};
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};

pub const TITLE: &str = "AI features";
pub const FEATURES: &str = "ngteams can correct and translate the messages you write, translate chats and live \
                            captions, and summarize meeting recaps with AI.";
pub const PROVIDERS: &str = "The AI runs with your own Claude Code or Codex CLI on this computer, or with an API key \
                             that you add. It uses your own subscription or API account, with its limits and costs.";
pub const DATA: &str = "When you use an AI feature, the text it works on goes to that AI provider: for example, a draft \
                        that you correct or the messages of a chat that you translate. Nothing goes to ngteams or \
                        Microsoft for this.";
pub const LATER: &str = "You can change this later in Settings under General.";
pub const TURN_OFF: &str = "Turn AI features off";
pub const KEEP_ON: &str = "Keep AI features on";

/// The dialog's width, in rems: it grows with the font size, as Teams' dialogs grow with its zoom.
const WIDTH_REMS: f32 = 34.0;

/// The space the dialog leaves to the window's edges, px.
const MARGIN: f32 = 48.0;

/// The scroll bar's track, px: the kit's width, in the dialog's padding right of the points.
const SCROLLBAR_WIDTH: f32 = 16.0;

/// The notice was answered: the shell gives the keyboard back to the window.
pub struct Answered;

/// The AI notice (see the module docs).
pub struct AiNotice {
    model: Entity<SettingsModel>,
    focus: FocusHandle,
    /// The points' scroll position, which the scroll bar shows and moves.
    body: ScrollHandle,
    _observing: Subscription,
}

impl EventEmitter<Answered> for AiNotice {}

impl AiNotice {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            focus: cx.focus_handle(),
            body: ScrollHandle::new(),
            _observing: observing,
        }
    }

    /// The dialog's own focus: the shell gives it the keyboard while the notice waits, and Tab stays in it.
    pub(crate) fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// A point of the notice: an icon and its words; its test selector `ai-notice-<id>`.
    fn point(id: &'static str, glyph: Glyph, words: &'static str, palette: Palette) -> AnyElement {
        div()
            .debug_selector(move || format!("ai-notice-{id}"))
            .w_full()
            .flex()
            .gap_3()
            .child(
                div()
                    .flex_none()
                    .pt(px(2.0))
                    .text_color(rgb(palette.muted_foreground))
                    .child(glyph.icon().size(px(16.0))),
            )
            .child(div().flex_1().min_w_0().text_sm().child(words))
            .into_any_element()
    }

    /// One of the two answers, its test selector its id.
    fn answer(id: &'static str, on: bool, cx: &mut Context<Self>) -> AnyElement {
        let button = Button::new(id)
            .label(if on { KEEP_ON } else { TURN_OFF })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.update(cx, |model, cx| model.set_ai_features(on, cx));
                cx.emit(Answered);
            }));
        let button = if on { button.primary() } else { button.outline() };
        selected_by(id, button).into_any_element()
    }
}

impl Render for AiNotice {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let viewport = window.viewport_size();
        div()
            .debug_selector(|| "ai-notice".to_owned())
            .id("ai-notice")
            .occlude()
            .w(rems(WIDTH_REMS))
            .max_w(viewport.width - px(MARGIN))
            .max_h(viewport.height - px(MARGIN))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(Self::title(palette))
            .child(self.body(palette))
            .child(
                div()
                    .debug_selector(|| "ai-notice-buttons".to_owned())
                    .flex_none()
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_2()
                    .child(Self::answer("ai-notice-off", false, cx))
                    .child(Self::answer("ai-notice-on", true, cx)),
            )
            .focus_trap("ai-notice-trap", &self.focus)
    }
}

impl AiNotice {
    /// The points and the "change this later" line, scrolled when they do not fit. The scroll bar beside them shows
    /// whenever they do not fit; its test selector `ai-notice-scrollbar` is there then (as the last frame laid out).
    fn body(&self, palette: Palette) -> gpui::Div {
        let cut = self.body.max_offset().y > px(0.0);
        let scrollbar = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(-SCROLLBAR_WIDTH))
            .w(px(SCROLLBAR_WIDTH))
            .child(
                Scrollbar::vertical(&self.body)
                    .id("ai-notice-scrollbar")
                    .mode(ScrollbarMode::Always)
                    .viewport_from_layout()
                    // the kit's thumb takes the hover colour, too light to see on the dialog
                    .styles(|styles| {
                        styles
                            .thumb(|thumb| thumb.bg(gpui::Hsla::from(rgb(palette.muted_foreground)).alpha(0.6)))
                            .thumb_hover(|thumb| thumb.bg(gpui::Hsla::from(rgb(palette.muted_foreground))))
                    }),
            );
        let scrollbar = if cut {
            scrollbar.debug_selector(|| "ai-notice-scrollbar".to_owned())
        } else {
            scrollbar
        };
        div()
            .relative()
            .min_h_0()
            .flex_shrink(1.0)
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "ai-notice-body".to_owned())
                    .id("ai-notice-body")
                    .track_scroll(&self.body)
                    .min_h_0()
                    .flex_shrink(1.0)
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(Self::point("features", Glyph::SPARKLES, FEATURES, palette))
                    .child(Self::point("providers", Glyph::BOT, PROVIDERS, palette))
                    .child(Self::point("data", Glyph::SHIELD, DATA, palette))
                    .child(
                        div()
                            .debug_selector(|| "ai-notice-later".to_owned())
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(LATER),
                    ),
            )
            .child(scrollbar)
    }

    /// The icon and the title.
    fn title(palette: Palette) -> gpui::Div {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .size(px(36.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(rgb(palette.selected))
                    .text_color(rgb(palette.brand))
                    .child(Glyph::SPARKLES.icon().size(px(18.0))),
            )
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(TITLE))
    }
}
