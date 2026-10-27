//! The settings view, in the conversation pane: how the app looks (the theme, following the system's light or dark
//! mode with a theme for each or one theme whatever the system says, the font size with its steps and the font
//! family, searchable among the system's), switches for sending typing, read receipts and the account's
//! availability, the calls' video (in a build that has the codec: "OpenH264 Video Codec provided by Cisco Systems,
//! Inc.", as Cisco's binary licence asks beside its control), which messages notify,
//! the idle time, the account's manual status, the calls' microphone, speaker and camera (where the app has calls:
//! "Follow the system", naming the system's default, then every device there is, a device chosen that isn't there
//! shown as not connected), the diagnostic log's level and "Copy diagnostics", and what was wrong with the settings
//! file (or the log). Each change goes to the [`SettingsModel`] (and so to the file, and a device's to the calls) at
//! once.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::IndexPath;
use gpui_component::button::Button;
use gpui_component::radio::Radio;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::switch::Switch;
use teams_log::diagnostics::WARNING;
use teams_media::devices::catalog::{DeviceKind, DeviceList};

use crate::settings::{LogLevel, ManualStatus, Notifications, Settings, SettingsModel};
use crate::theme::{self, Appearance, FONT_SIZE, FONT_SIZES, Palette, THEMES};

/// The font picker's first row: the system's UI font (no family named).
pub const SYSTEM_FONT_LABEL: &str = "System font";

/// The font picker's state.
type FontSelect = SelectState<SearchableVec<SharedString>>;

/// The font picker, its rows (the system's font first, then the families) and the view's subscription to it.
struct Fonts {
    select: Entity<FontSelect>,
    names: Vec<SharedString>,
    _confirmed: Subscription,
}

impl Fonts {
    /// Shows the family `appearance` holds when the picker shows another (another account's view, or the file,
    /// changed it); a family the system doesn't have is offered too, as at the start.
    fn show(&mut self, appearance: &Appearance, window: &mut Window, cx: &mut gpui::App) {
        let shown: SharedString = appearance
            .font_family
            .clone()
            .map_or_else(|| SYSTEM_FONT_LABEL.into(), Into::into);
        if self.select.read(cx).selected_value() == Some(&shown) {
            return;
        }
        let items = (!self.names.contains(&shown)).then(|| {
            self.names.push(shown.clone());
            SearchableVec::new(self.names.clone())
        });
        self.select.update(cx, |select, cx| {
            if let Some(items) = items {
                select.set_items(items, window, cx);
            }
            select.set_selected_value(&shown, window, cx);
        });
    }
}

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
    /// The account whose own settings (its manual status) the view shows ([`SettingsView::for_account`]); `None`: the
    /// first account's.
    account: Option<String>,
    /// The font picker, made the first time the view draws (it needs the window).
    fonts: Option<Fonts>,
    _observing: Subscription,
}

impl SettingsView {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&model, |_, _, cx| cx.notify());
        let video_switch = model.read(cx).shows_video_switch();
        Self {
            model,
            video_switch,
            account: None,
            fonts: None,
            _observing: observing,
        }
    }

    /// The view showing `account`'s own settings (its MRI): an app of several accounts has a view per account.
    #[must_use]
    pub fn for_account(mut self, account: String) -> Self {
        self.account = Some(account);
        self
    }

    /// The family the font picker shows, once the view has drawn it.
    #[must_use]
    pub fn shown_font(&self, cx: &gpui::App) -> Option<SharedString> {
        self.fonts.as_ref()?.select.read(cx).selected_value().cloned()
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

    /// The theme: following the system with a theme for each of its modes, or one theme. Each radio's test selector is
    /// `<side>-<key>`: `light-theme`, `dark-theme` or `theme`, and the theme's key.
    fn themes(&self, appearance: &Appearance) -> impl IntoElement {
        let follow = self.switch(
            "follow-system-theme",
            "Follow the system's light or dark mode",
            appearance.follow_system,
            |settings, on| settings.appearance.follow_system = on,
        );
        let side =
            |group: &'static str, title: &'static str, chosen: &'static str, set: fn(&mut Appearance, &'static str)| {
                let radios = THEMES.iter().map(|spec| {
                    let key = spec.key;
                    self.radio(group, key, spec.name.into(), chosen == key, move |settings| {
                        set(&mut settings.appearance, key);
                    })
                });
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().child(title))
                    .child(div().flex().flex_wrap().gap_3().children(radios))
            };
        let sides = if appearance.follow_system {
            vec![
                side(
                    "light-theme",
                    "When the system is light",
                    appearance.light_theme,
                    |appearance, key| {
                        appearance.light_theme = key;
                    },
                ),
                side(
                    "dark-theme",
                    "When the system is dark",
                    appearance.dark_theme,
                    |appearance, key| {
                        appearance.dark_theme = key;
                    },
                ),
            ]
        } else {
            vec![side("theme", "Theme", appearance.theme, |appearance, key| {
                appearance.theme = key;
            })]
        };
        div().flex().flex_col().gap_3().child(follow).children(sides)
    }

    /// The font size: a step smaller, the size, a step larger and back to the default (Ctrl+-, Ctrl+= and Ctrl+0 do
    /// the same anywhere in the window).
    fn font_size(&self, appearance: &Appearance, palette: Palette) -> impl IntoElement {
        let size = appearance.font_size;
        let step = |id: &'static str, label: &'static str, to: u16, disabled: bool| {
            let apply = self.changer(move |settings| settings.appearance.font_size = to);
            div().debug_selector(move || id.to_owned()).child(
                Button::new(id)
                    .label(label)
                    .outline()
                    .disabled(disabled)
                    .on_click(move |_, _, cx| apply(cx)),
            )
        };
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().child("Font size"))
            .child(step(
                "font-smaller",
                "A−",
                appearance.stepped_font_size(-1),
                size <= *FONT_SIZES.start(),
            ))
            .child(
                div()
                    .debug_selector(|| "font-size".to_owned())
                    .min_w(px(48.0))
                    .child(SharedString::from(format!("{size} px"))),
            )
            .child(step(
                "font-larger",
                "A+",
                appearance.stepped_font_size(1),
                size >= *FONT_SIZES.end(),
            ))
            .child(step("font-reset", "Reset", FONT_SIZE, size == FONT_SIZE))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Ctrl+=, Ctrl+- and Ctrl+0"),
            )
    }

    /// The font picker, made over the system's font families (and a family chosen that isn't there) the first time,
    /// and showing the family the settings hold each time: another account's view, or the file, may have changed it.
    fn font_select(
        &mut self,
        appearance: &Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FontSelect> {
        if let Some(fonts) = &mut self.fonts {
            fonts.show(appearance, window, cx);
            return fonts.select.clone();
        }
        let chosen = appearance.font_family.clone();
        let mut names: Vec<SharedString> = vec![SYSTEM_FONT_LABEL.into()];
        let families = cx.text_system().all_font_names();
        names.extend(
            families
                .iter()
                .filter(|family| !family.starts_with('.'))
                .map(|family| SharedString::from(family.clone())),
        );
        if let Some(missing) = chosen.as_ref().filter(|family| !families.contains(family)) {
            names.push(missing.clone().into());
        }
        let selected = chosen
            .as_deref()
            .map_or(Some(0), |family| names.iter().position(|name| name.as_ref() == family))
            .map(IndexPath::new);
        let select =
            cx.new(|cx| SelectState::new(SearchableVec::new(names.clone()), selected, window, cx).searchable(true));
        let model = self.model.clone();
        let subscription = cx.subscribe(
            &select,
            move |_, _, event: &SelectEvent<SearchableVec<SharedString>>, cx| {
                let SelectEvent::Confirm(Some(name)) = event else {
                    return;
                };
                let family = (name.as_ref() != SYSTEM_FONT_LABEL).then(|| name.to_string());
                model.update(cx, |model, cx| {
                    model.change(|settings| settings.appearance.font_family = family, cx);
                });
            },
        );
        self.fonts = Some(Fonts {
            select: select.clone(),
            names,
            _confirmed: subscription,
        });
        select
    }

    /// How the app looks: the theme, the font size and the font.
    fn appearance(
        &mut self,
        appearance: &Appearance,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let select = self.font_select(appearance, window, cx);
        let font = div().flex().items_center().gap_2().child(div().child("Font")).child(
            div()
                .debug_selector(|| "font-family".to_owned())
                .w(px(280.0))
                .child(Select::new(&select).search_placeholder("Search fonts")),
        );
        section("Appearance")
            .child(self.themes(appearance))
            .child(self.font_size(appearance, palette))
            .child(font)
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
        let account = self
            .account
            .clone()
            .or_else(|| settings.first_account.clone())
            .unwrap_or_default();
        let radios = ManualStatus::ALL.map(|status| {
            let chosen = account.clone();
            self.radio(
                "status",
                status.key(),
                status.label().into(),
                settings.manual_status_of(&account) == status,
                move |settings| settings.set_manual_status(&chosen, status),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let settings = self.model.read(cx).settings().clone();
        let appearance = self.appearance(&settings.appearance, palette, window, cx);
        let model = self.model.read(cx);
        let problems = problems(model, palette);
        let log = self.log(&settings, model, palette);
        let devices = model.devices().map(|list| self.devices(&settings, list));
        // scrolled: the settings outgrow the pane at a large font size or in a small window
        div()
            .id("settings")
            .debug_selector(|| "settings".to_owned())
            .size_full()
            .overflow_y_scroll()
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .max_w(px(640.0))
                    .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Settings"))
                    .child(problems)
                    .child(appearance)
                    .child(self.switches(&settings))
                    .child(self.notifications(&settings))
                    .child(self.idle(&settings))
                    .child(self.status(&settings))
                    .children(devices)
                    .child(log),
            )
    }
}
