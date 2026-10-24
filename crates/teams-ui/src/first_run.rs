//! The first run's screen (the personal build, M2 Task 27; `docs/personal-build.md`, "The settings that write on
//! their own"): the three writes that happen without a per-item action, typing, read receipts and the account's own
//! availability, each with its switch at Teams' own default (on, presence following the app), and "Confirm". Until it
//! is confirmed none of them is sent ([`SettingsModel::awaiting_first_run`]); afterwards each is a setting like any
//! other, in the settings view. The shell shows it over everything while it waits.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::switch::Switch;

use crate::settings::{Settings, SettingsModel};
use crate::theme;

/// The first run's screen (see the module docs).
pub struct FirstRun {
    model: Entity<SettingsModel>,
    _observing: Subscription,
}

impl FirstRun {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            _observing: observing,
        }
    }

    /// A switch, its test selector `first-run-<id>`; `set` writes the value asked for.
    fn switch(&self, id: &'static str, label: &'static str, checked: bool, set: fn(&mut Settings, bool)) -> AnyElement {
        let model = self.model.clone();
        div()
            .debug_selector(move || format!("first-run-{id}"))
            .child(
                Switch::new(id)
                    .checked(checked)
                    .label(label)
                    .on_click(move |checked: &bool, _, cx| {
                        let checked = *checked;
                        model.update(cx, |model, cx| model.change(|settings| set(settings, checked), cx));
                    }),
            )
            .into_any_element()
    }
}

impl Render for FirstRun {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let settings = self.model.read(cx).settings().clone();
        let model = self.model.clone();
        div()
            .debug_selector(|| "first-run".to_owned())
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(520.0))
            .bg(rgb(palette.background))
            .border_1()
            .border_color(rgb(palette.border))
            .rounded_md()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("What this app sends on its own"),
            )
            .child(div().text_sm().child(
                "These go out without you clicking anything. Nothing of them is sent until you confirm; you can change \
                 each later in Settings.",
            ))
            .child(self.switch(
                "send-typing",
                "Typing indicators while you type",
                settings.send_typing,
                |settings, on| settings.send_typing = on,
            ))
            .child(self.switch(
                "send-read-receipts",
                "Read receipts as you read a chat",
                settings.send_read_receipts,
                |settings, on| settings.send_read_receipts = on,
            ))
            .child(self.switch(
                "send-presence",
                "Your availability, following this app (or the status you choose)",
                settings.send_presence,
                |settings, on| settings.send_presence = on,
            ))
            .child(
                div().debug_selector(|| "first-run-confirm".to_owned()).child(
                    Button::new("first-run-confirm")
                        .primary()
                        .label("Confirm")
                        .on_click(move |_, _, cx| model.update(cx, SettingsModel::confirm_first_run)),
                ),
            )
    }
}
