//! The settings view, in the conversation pane: switches for sending typing and read receipts, which messages notify,
//! the idle time and the account's manual status, and what was wrong with the settings file. Each change goes to the
//! [`SettingsModel`] (and so to the file) at once.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::radio::Radio;
use gpui_component::switch::Switch;

use crate::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use crate::theme::{self, Palette};

/// The idle times offered, in minutes (the one set is offered too, if it isn't one of these).
pub const IDLE_CHOICES: [u32; 6] = [1, 5, 10, 15, 30, 60];

/// The settings view (see the module docs).
pub struct SettingsView {
    model: Entity<SettingsModel>,
    _observing: Subscription,
}

impl SettingsView {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        Self {
            model,
            _observing: observing,
        }
    }

    /// A change of the settings, as a control's handler makes it.
    fn changer(&self, change: impl Fn(&mut Settings) + 'static) -> impl Fn(&mut gpui::App) + 'static {
        let model = self.model.clone();
        move |cx| model.update(cx, |model, cx| model.change(&change, cx))
    }

    /// A switch, its test selector its id; `set` writes the value asked for.
    fn switch(&self, id: &'static str, label: &'static str, checked: bool, set: fn(&mut Settings, bool)) -> AnyElement {
        let model = self.model.clone();
        div()
            .debug_selector(move || id.to_owned())
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

    fn switches(&self, settings: &Settings) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.switch(
                "send-typing",
                "Send typing indicators",
                settings.send_typing,
                |settings, on| settings.send_typing = on,
            ))
            .child(self.switch(
                "send-read-receipts",
                "Send read receipts",
                settings.send_read_receipts,
                |settings, on| settings.send_read_receipts = on,
            ))
    }

    /// One radio of a choice, its test selector `<group>-<key>`.
    fn radio(
        &self,
        group: &'static str,
        key: &str,
        label: SharedString,
        checked: bool,
        change: impl Fn(&mut Settings) + 'static,
    ) -> AnyElement {
        let apply = self.changer(change);
        let id: SharedString = format!("{group}-{key}").into();
        let selector = id.to_string();
        div()
            .debug_selector(move || selector)
            .child(
                Radio::new(id)
                    .label(label)
                    .checked(checked)
                    .on_click(move |_, _, cx| apply(cx)),
            )
            .into_any_element()
    }

    fn notifications(&self, settings: &Settings) -> impl IntoElement {
        let radios = Notifications::ALL.map(|level| {
            self.radio(
                "notifications",
                level.key(),
                level.label().into(),
                settings.notifications == level,
                move |settings| settings.notifications = level,
            )
        });
        section("Notifications").child(div().flex().gap_3().children(radios))
    }

    fn idle(&self, settings: &Settings) -> impl IntoElement {
        let mut choices = IDLE_CHOICES.to_vec();
        if !choices.contains(&settings.idle_minutes) {
            choices.push(settings.idle_minutes);
            choices.sort_unstable();
        }
        let radios = choices.into_iter().map(|minutes| {
            self.radio(
                "idle",
                &minutes.to_string(),
                format!("{minutes} min").into(),
                settings.idle_minutes == minutes,
                move |settings| settings.idle_minutes = minutes,
            )
        });
        section("Show me away after").child(div().flex().flex_wrap().gap_3().children(radios))
    }

    fn status(&self, settings: &Settings) -> impl IntoElement {
        let radios = ManualStatus::ALL.map(|status| {
            self.radio(
                "status",
                status.key(),
                status.label().into(),
                settings.manual_status == status,
                move |settings| settings.manual_status = status,
            )
        });
        section("Status").child(div().flex().flex_col().gap_1().children(radios))
    }
}

/// A titled part of the view.
fn section(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
}

/// What was wrong with the settings file, one line each.
fn problems(model: &SettingsModel, palette: Palette) -> impl IntoElement {
    let lines = model
        .problems()
        .iter()
        .map(|problem| div().child(SharedString::from(problem.to_string())));
    div()
        .debug_selector(|| "settings-problems".to_owned())
        .flex()
        .flex_col()
        .gap_1()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .children(lines)
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let model = self.model.read(cx);
        let settings = model.settings().clone();
        let problems = problems(model, palette);
        div()
            .debug_selector(|| "settings".to_owned())
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .max_w(px(640.0))
            .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Settings"))
            .child(problems)
            .child(self.switches(&settings))
            .child(self.notifications(&settings))
            .child(self.idle(&settings))
            .child(self.status(&settings))
    }
}
