//! The AI notice: one dialog, shown once before any AI use ([`SettingsModel::awaiting_ai_notice`]). It says which AI
//! features the app has, what runs them, who pays for them and where the text goes, and asks at once: keep them on or
//! turn them off. The answer is "Use AI features" in Settings under General ([`crate::settings::Settings::ai_features`]).
//! The shell shows it over everything after the first run's screen.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};

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

/// The AI notice (see the module docs).
pub struct AiNotice {
    model: Entity<SettingsModel>,
    _observing: Subscription,
}

impl AiNotice {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            _observing: observing,
        }
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
    fn answer(&self, id: &'static str, on: bool) -> AnyElement {
        let model = self.model.clone();
        let button = Button::new(id)
            .label(if on { KEEP_ON } else { TURN_OFF })
            .on_click(move |_, _, cx| model.update(cx, |model, cx| model.set_ai_features(on, cx)));
        let button = if on { button.primary() } else { button.outline() };
        selected_by(id, button).into_any_element()
    }
}

impl Render for AiNotice {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        div()
            .debug_selector(|| "ai-notice".to_owned())
            .id("ai-notice")
            .occlude()
            .w(px(480.0))
            .max_w_full()
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
            .child(
                div()
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
                    .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(TITLE)),
            )
            .child(Self::point("features", Glyph::SPARKLES, FEATURES, palette))
            .child(Self::point("providers", Glyph::BOT, PROVIDERS, palette))
            .child(Self::point("data", Glyph::SHIELD, DATA, palette))
            .child(
                div()
                    .debug_selector(|| "ai-notice-later".to_owned())
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(LATER),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(self.answer("ai-notice-off", false))
                    .child(self.answer("ai-notice-on", true)),
            )
    }
}
