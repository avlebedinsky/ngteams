//! The settings view, in the conversation pane: switches for sending typing, read receipts and the account's
//! availability, the calls' video (in a build that has the codec: "OpenH264 Video Codec provided by Cisco Systems,
//! Inc.", as Cisco's binary licence asks beside its control), which messages notify,
//! the idle time, the account's manual status, the calls' microphone, speaker and camera (where the app has calls:
//! "Follow the system", naming the system's default, then every device there is, a device chosen that isn't there
//! shown as not connected), the diagnostic log's level and "Copy diagnostics", and what was wrong with the settings
//! file (or the log). Each change goes to the [`SettingsModel`] (and so to the file, and a device's to the calls) at
//! once.

use gpui::{
    AnyElement, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::button::Button;
use gpui_component::radio::Radio;
use gpui_component::switch::Switch;
use teams_log::diagnostics::WARNING;
use teams_media::devices::catalog::{DeviceKind, DeviceList};

use crate::settings::{LogLevel, ManualStatus, Notifications, Settings, SettingsModel};
use crate::theme::{self, Palette};

/// The idle times offered, in minutes (the one set is offered too, if it isn't one of these).
pub const IDLE_CHOICES: [u32; 6] = [1, 5, 10, 15, 30, 60];

/// What the settings call each kind of device.
#[must_use]
pub fn device_label(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Microphone => "Microphone",
        DeviceKind::Speaker => "Speaker",
        DeviceKind::Camera => "Camera",
    }
}

/// The key of the radio that follows the system (its test selector `<kind>-system`).
pub const FOLLOW_SYSTEM: &str = "system";

/// The label of the calls' video switch: the codec named as Cisco's binary licence asks.
pub const VIDEO_SWITCH: &str = "Video in calls: OpenH264 Video Codec provided by Cisco Systems, Inc.";

/// The settings view (see the module docs).
pub struct SettingsView {
    model: Entity<SettingsModel>,
    /// The build has the video codec: its switch is shown.
    video_switch: bool,
    _observing: Subscription,
}

impl SettingsView {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        let video_switch = model.read(cx).shows_video_switch();
        Self {
            model,
            video_switch,
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
            .child(self.switch(
                "send-presence",
                "Report my availability",
                settings.send_presence,
                |settings, on| settings.send_presence = on,
            ))
            .children(self.video_switch.then(|| {
                self.switch("video-calls", VIDEO_SWITCH, settings.video_calls, |settings, on| {
                    settings.video_calls = on;
                })
            }))
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
        section("Stop reporting me available after").child(div().flex().flex_wrap().gap_3().children(radios))
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

    /// The calls' devices: for each kind, "Follow the system" (naming the system's default), then every device there
    /// is, and a device chosen that isn't there, as not connected. Each radio's test selector is `<kind>-<key>`: the
    /// device's name, or [`FOLLOW_SYSTEM`].
    fn devices(&self, settings: &Settings, list: &DeviceList) -> impl IntoElement {
        let kinds = DeviceKind::ALL.map(|kind| {
            let chosen = settings.device(kind).map(str::to_owned);
            let system = match list.default_of(kind) {
                Some(device) => format!("Follow the system ({})", device.description),
                None => "Follow the system".to_owned(),
            };
            let mut radios = vec![self.radio(
                kind.key(),
                FOLLOW_SYSTEM,
                system.into(),
                chosen.is_none(),
                move |settings| {
                    settings.set_device(kind, None);
                },
            )];
            let mut names: Vec<(String, String)> = list
                .of(kind)
                .map(|device| (device.name.clone(), device.description.clone()))
                .collect();
            if let Some(missing) = chosen.as_ref().filter(|name| list.find(kind, name).is_none()) {
                names.push((missing.clone(), format!("{missing} (not connected)")));
            }
            radios.extend(names.into_iter().map(|(name, label)| {
                let checked = chosen.as_deref() == Some(name.as_str());
                let key = name.clone();
                self.radio(kind.key(), &key, label.into(), checked, move |settings| {
                    settings.set_device(kind, Some(name.clone()));
                })
            }));
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().child(device_label(kind)))
                .children(radios)
        });
        section("Devices").child(div().flex().flex_col().gap_3().children(kinds))
    }

    /// The diagnostic log: its level (changed live; `TEAMS_APP_LOG` overrides it, said here), "Copy diagnostics", and
    /// what the last copy held: said plainly when it has full requests and responses.
    fn log(&self, settings: &Settings, model: &SettingsModel, palette: Palette) -> impl IntoElement {
        let radios = LogLevel::ALL.map(|level| {
            self.radio(
                "log-level",
                level.key(),
                level.label().into(),
                settings.log_level == level,
                move |settings| settings.log_level = level,
            )
        });
        let overridden = teams_log::installed()
            .and_then(teams_log::FileLog::overridden)
            .map(|level| {
                div()
                    .debug_selector(|| "log-level-overridden".to_owned())
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(SharedString::from(format!(
                        "TEAMS_APP_LOG sets the level to {} for this run",
                        level.key()
                    )))
            });
        let copy_model = self.model.clone();
        let copy = div().debug_selector(|| "copy-diagnostics".to_owned()).child(
            Button::new("copy-diagnostics")
                .label("Copy diagnostics")
                .on_click(move |_, _, cx| {
                    copy_model.update(cx, |model, cx| {
                        model.copy_diagnostics(cx);
                    });
                }),
        );
        let copied = model.copied().map(|copied| {
            let text = if copied.has_debug {
                format!("Copied. It {WARNING}.")
            } else {
                "Copied.".to_owned()
            };
            div()
                .debug_selector(|| "diagnostics-copied".to_owned())
                .text_sm()
                .child(SharedString::from(text))
        });
        section("Diagnostic log")
            .child(div().flex().flex_wrap().gap_3().children(radios))
            .children(overridden)
            .child(copy)
            .children(copied)
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
        let log = self.log(&settings, model, palette);
        let devices = model.devices().map(|list| self.devices(&settings, list));
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
            .children(devices)
            .child(log)
    }
}
