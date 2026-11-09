//! The sounds' settings, where Teams has them and more besides: a "Sound" box on the Notifications page ("Play sounds
//! when I'm busy or in a meeting", as Teams words it, and the pack chosen as a whole), a row per event of the
//! notifications' and of the calls' (a switch, which sound, a volume and a preview, as Teams' dropdown and play button
//! do for its three), and on the Calls page Teams' "Choose a ringtone for you", the incoming call's row again.
//!
//! An event's picker offers the chosen pack's sound, every pack that has one, and "Use my own file…", a WAV, OGG or MP3 kept as
//! the user's own ([`crate::sounds::pack::import_single`]). Packs are added from a folder or a `.zip` and removed here;
//! each picker's test selector is the event's ([`SoundEvent::picker`]), its rows `<that>-<key>`.

use std::path::Path;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    PathPromptOptions, SharedString, Styled as _, WeakEntity, div, px, rgb,
};
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, Sizable as _};

use super::{Apply, MENU_HEIGHT, Pick, SettingsView, caption, group, row, row_with};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::settings::Settings;
use crate::sounds::pack;
use crate::sounds::synth::PACKS;
use crate::sounds::{ImportKind, MAX_VOLUME, PackError, PackInfo, SoundEvent, SoundSource, VOLUME_STEP};
use crate::theme::{self, Palette};

/// The key of an event's picker row that plays the chosen pack's sound.
pub const FROM_PACK: &str = "pack";
/// The key of an event's picker row that asks for the user's own file.
pub const OWN_FILE: &str = "own-file";
/// How wide an event's picker is, px.
const SOUND_PICKER_WIDTH: f32 = 150.0;

/// What the user is adding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adding {
    Pack(ImportKind),
    File(SoundEvent),
}

impl SettingsView {
    /// The Notifications page's sound boxes.
    pub(super) fn sound_groups(
        &self,
        settings: &Settings,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let (notifications, calls): (Vec<_>, Vec<_>) =
            SoundEvent::ALL.into_iter().partition(|event| event.is_notification());
        let rows = |events: Vec<SoundEvent>, cx: &mut Context<Self>| {
            events
                .into_iter()
                .map(|event| self.event_row(event, settings, palette, cx))
                .collect()
        };
        let notifications = rows(notifications, cx);
        let calls = rows(calls, cx);
        vec![
            group(Some("Sound"), self.sound_rows(settings, palette, cx), palette),
            group(Some("Notification sounds"), notifications, palette),
            group(Some("Call sounds"), calls, palette),
        ]
    }

    /// The Calls page's ringtone box, as Teams has it there.
    pub(super) fn ringtone_group(&self, settings: &Settings, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let row = self.titled_event_row(
            SoundEvent::IncomingCall,
            "Choose a ringtone for you".into(),
            settings,
            palette,
            cx,
        );
        group(Some("Ringtone"), vec![row], palette)
    }

    /// The pack, the master volume, adding and removing packs, and whether sounds play while busy.
    fn sound_rows(&self, settings: &Settings, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let model = self.model.read(cx);
        let can_add = model.sound_packs_dir().is_some();
        let packs = model.sound_packs().to_vec();
        let mut rows = vec![
            row(
                "Sound pack",
                Some("The set of sounds the app plays. Any single sound can be changed below."),
                super::picker("sound-pack", self.pack_picks(settings, &packs), palette),
                palette,
            ),
            row(
                "Volume",
                Some("How loud every sound is, on top of each sound's own volume."),
                self.volume_stepper("sound-volume", settings.sounds.volume, palette, |settings, volume| {
                    settings.sounds.volume = volume;
                }),
                palette,
            ),
            row(
                "Play sounds when I'm busy or in a meeting",
                Some("Off: new messages and the ringtone stay quiet while you are busy or in a call."),
                self.switch("sounds-while-busy", settings.sounds.while_busy, |settings, on| {
                    settings.sounds.while_busy = on;
                }),
                palette,
            ),
        ];
        if can_add {
            rows.push(row(
                "Add a sound pack",
                Some("A folder or .zip file with a pack.toml and WAV, OGG or MP3 sounds, at most 30 seconds each."),
                self.add_buttons(cx),
                palette,
            ));
        }
        rows.extend(self.adding_problem.clone().map(|problem| {
            caption("sound-pack-problem", theme::DANGER)
                .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)))
                .child(problem)
                .into_any_element()
        }));
        rows.extend(packs.iter().map(|pack| Self::imported_row(pack, palette, cx)));
        rows
    }

    /// One pick per pack, the imported ones after the app's.
    fn pack_picks(&self, settings: &Settings, packs: &[PackInfo]) -> Vec<Pick> {
        let builtin = PACKS.iter().map(|pack| {
            (
                SoundSource::Builtin(pack.key),
                pack.key.to_owned(),
                SharedString::from(pack.name),
            )
        });
        let imported = packs.iter().map(|pack| {
            (
                SoundSource::Imported(pack.id.clone()),
                pack.id.clone(),
                SharedString::from(pack.name.clone()),
            )
        });
        builtin
            .chain(imported)
            .map(|(source, key, label)| Pick {
                key,
                label,
                status: None,
                chosen: settings.sounds.pack == source,
                apply: self.changer(move |settings| settings.sounds.pack = source.clone()),
            })
            .collect()
    }

    /// "Folder" and "Zip file", working while one import runs.
    fn add_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.adding.is_some();
        let button = |id: &'static str, glyph: Glyph, label: &'static str, kind: ImportKind, cx: &mut Context<Self>| {
            selected_by(
                id,
                Button::new(id)
                    .icon(glyph.icon())
                    .label(label)
                    .outline()
                    .small()
                    .disabled(busy)
                    .on_click(cx.listener(move |this, _, _, cx| this.add(Adding::Pack(kind), cx))),
            )
        };
        div()
            .flex()
            .gap_2()
            .child(button(
                "sound-pack-add-folder",
                Glyph::FOLDER,
                "Folder",
                ImportKind::Folder,
                cx,
            ))
            .child(button(
                "sound-pack-add-archive",
                Glyph::FILE_ARCHIVE,
                "Zip file",
                ImportKind::Archive,
                cx,
            ))
            .into_any_element()
    }

    /// An imported pack: its name, who made it and how many sounds it has, with Remove.
    fn imported_row(pack: &PackInfo, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let count = pack.events.len();
        let sounds = if count == 1 {
            "1 sound".to_owned()
        } else {
            format!("{count} sounds")
        };
        let description = match &pack.author {
            Some(author) => format!("By {author}. {sounds}."),
            None => format!("{sounds}."),
        };
        let id: SharedString = format!("sound-pack-remove-{}", pack.id).into();
        let remove = pack.id.clone();
        let button = icon_button(id.clone(), Glyph::TRASH, "Remove sound pack")
            .small()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.update(cx, |model, cx| model.remove_sound_pack(&remove, cx));
            }));
        row_with(
            pack.name.clone().into(),
            Some(description.into()),
            selected_by(id.to_string(), button),
            palette,
        )
    }

    fn event_row(
        &self,
        event: SoundEvent,
        settings: &Settings,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.titled_event_row(event, event.label().into(), settings, palette, cx)
    }

    /// An event's row under `title`: its switch, which sound, its volume and its preview.
    fn titled_event_row(
        &self,
        event: SoundEvent,
        title: SharedString,
        settings: &Settings,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sound = settings.sounds.event(event);
        let model = self.model.clone();
        let id: SharedString = format!("sound-on-{}", event.key()).into();
        let switch = selected_by(
            id.to_string(),
            Switch::new(id).checked(sound.on).on_click(move |on: &bool, _, cx| {
                let on = *on;
                model.update(cx, |model, cx| {
                    model.change(
                        |settings| settings.sounds.change_event(event, |sound| sound.on = on),
                        cx,
                    );
                });
            }),
        );
        let controls = div()
            .flex()
            .items_center()
            .gap_3()
            .child(switch)
            .child(self.sound_picker(event, settings, palette, cx))
            .child(self.volume_stepper(
                &format!("sound-volume-{}", event.key()),
                sound.volume,
                palette,
                move |settings, volume| settings.sounds.change_event(event, |sound| sound.volume = volume),
            ))
            .children(self.preview_button(event, cx));
        row_with(title, None, controls, palette)
    }

    /// −, the volume and +, in steps of [`VOLUME_STEP`]; `<id>-down`, `<id>` and `<id>-up` are the test selectors.
    fn volume_stepper(
        &self,
        id: &str,
        volume: u8,
        palette: Palette,
        set: impl Fn(&mut Settings, u8) + Clone + 'static,
    ) -> AnyElement {
        let step = |suffix: &str, glyph: Glyph, tip: &'static str, to: u8, disabled: bool| {
            let set = set.clone();
            let apply = self.changer(move |settings| set(settings, to));
            let selector = format!("{id}-{suffix}");
            selected_by(
                selector.clone(),
                icon_button(SharedString::from(selector), glyph, tip)
                    .xsmall()
                    .disabled(disabled)
                    .on_click(move |_, _, cx| apply(cx)),
            )
        };
        let shown = id.to_owned();
        div()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(rgb(palette.border))
            .child(step(
                "down",
                Glyph::MINUS,
                "Quieter",
                volume.saturating_sub(VOLUME_STEP),
                volume == 0,
            ))
            .child(
                div()
                    .debug_selector(move || shown.clone())
                    .w(px(40.0))
                    .flex()
                    .justify_center()
                    .text_sm()
                    .child(SharedString::from(format!("{volume}%"))),
            )
            .child(step(
                "up",
                Glyph::PLUS,
                "Louder",
                volume.saturating_add(VOLUME_STEP).min(MAX_VOLUME),
                volume >= MAX_VOLUME,
            ))
            .into_any_element()
    }

    /// Play, or Stop while this event's preview plays; none without a speaker.
    fn preview_button(&self, event: SoundEvent, cx: &mut Context<Self>) -> Option<AnyElement> {
        let player = self.model.read(cx).sound_player()?.clone();
        let playing = player.previewing() == Some(event);
        let (glyph, tip) = if playing {
            (Glyph::RECORD_STOP, "Stop")
        } else {
            (Glyph::PLAY, "Play this sound")
        };
        let id = format!("sound-preview-{}", event.key());
        Some(
            selected_by(
                id.clone(),
                icon_button(SharedString::from(id), glyph, tip)
                    .small()
                    .on_click(move |_, _, _| {
                        if playing {
                            player.stop_preview();
                        } else {
                            player.preview(event);
                        }
                    }),
            )
            .into_any_element(),
        )
    }

    /// Which sound `event` plays: the chosen pack's, every pack that has one, and the user's own file.
    fn sound_picker(
        &self,
        event: SoundEvent,
        settings: &Settings,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picks = Rc::new(self.sound_picks(event, settings, cx));
        let shown = picks
            .iter()
            .find(|pick| pick.chosen)
            .map_or_else(|| SharedString::from("Pack's sound"), |pick| pick.label.clone());
        let face = div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .min_w_0()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(shown),
            )
            .child(
                Glyph::CHEVRON_DOWN
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            );
        let group = event.picker();
        let view: WeakEntity<Self> = cx.entity().downgrade();
        selected_by(
            group,
            Button::new(group)
                .outline()
                .small()
                .w(px(SOUND_PICKER_WIDTH))
                .child(face)
                .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, _| {
                    let menu = menu.min_w(px(200.0)).scrollable(true).max_h(px(MENU_HEIGHT));
                    let view = view.clone();
                    let own = own_file_item(group, event, view);
                    picks
                        .iter()
                        .fold(menu, |menu, pick| menu.item(super::pick_item(group, pick, palette)))
                        .item(own)
                }),
        )
        .into_any_element()
    }

    /// `event`'s picks: the chosen pack's sound, then every pack with a sound for it.
    fn sound_picks(&self, event: SoundEvent, settings: &Settings, cx: &App) -> Vec<Pick> {
        let chosen = settings.sounds.event(event).sound;
        let set = |sound: Option<SoundSource>| -> Apply {
            self.changer(move |settings| {
                let sound = sound.clone();
                settings.sounds.change_event(event, |kept| kept.sound = sound);
            })
        };
        let mut picks = vec![Pick {
            key: FROM_PACK.to_owned(),
            label: "Pack's sound".into(),
            status: None,
            chosen: chosen.is_none(),
            apply: set(None),
        }];
        picks.extend(PACKS.iter().map(|pack| {
            let source = SoundSource::Builtin(pack.key);
            Pick {
                key: pack.key.to_owned(),
                label: pack.name.into(),
                status: None,
                chosen: chosen.as_ref() == Some(&source),
                apply: set(Some(source)),
            }
        }));
        picks.extend(
            self.model
                .read(cx)
                .sound_packs()
                .iter()
                .filter(|pack| pack.events.contains(&event))
                .map(|pack| {
                    let source = SoundSource::Imported(pack.id.clone());
                    Pick {
                        key: pack.id.clone(),
                        label: pack.name.clone().into(),
                        status: None,
                        chosen: chosen.as_ref() == Some(&source),
                        apply: set(Some(source)),
                    }
                }),
        );
        picks
    }

    /// The user picks what `adding` asks for; it is imported off the window's thread.
    pub fn add(&mut self, adding: Adding, cx: &mut Context<Self>) {
        if self.adding.is_some() {
            return;
        }
        let Some(dir) = self.model.read(cx).sound_packs_dir() else {
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: adding != Adding::Pack(ImportKind::Folder),
            directories: adding == Adding::Pack(ImportKind::Folder),
            multiple: false,
            prompt: Some("Add".into()),
        });
        self.adding = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let source = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let imported = match source {
                Some(source) => Some(
                    cx.background_spawn(async move { import_blocking(&source, adding, &dir) })
                        .await,
                ),
                // cancelled, or the chooser failed: adding works again
                None => None,
            };
            let _ = this.update(cx, |this, cx| this.finish_adding(imported, adding, cx));
        }));
        cx.notify();
    }

    /// An import done: the new sounds chosen and listed, or what is wrong with it said.
    fn finish_adding(&mut self, imported: Option<Result<PackInfo, PackError>>, adding: Adding, cx: &mut Context<Self>) {
        self.adding = None;
        match imported {
            Some(Ok(pack)) => {
                self.adding_problem = None;
                let source = SoundSource::Imported(pack.id);
                self.model.update(cx, |model, cx| {
                    model.change(
                        |settings| match adding {
                            Adding::Pack(_) => settings.sounds.pack = source,
                            Adding::File(event) => {
                                settings.sounds.change_event(event, |sound| sound.sound = Some(source));
                            }
                        },
                        cx,
                    );
                    // A replacement can keep the same source id while changing the file behind it.
                    model.apply_sounds();
                    model.reload_sound_packs(cx);
                });
            }
            Some(Err(error)) => {
                log::warn!("a sound pack can't be imported: {error}");
                self.adding_problem = Some(error.to_string().into());
            }
            None => {}
        }
        cx.notify();
    }
}

/// Imports `source` as `adding` says, into `dir`.
fn import_blocking(source: &Path, adding: Adding, dir: &Path) -> Result<PackInfo, PackError> {
    match adding {
        Adding::Pack(kind) => pack::import(source, kind, dir),
        Adding::File(event) => pack::import_single(source, event, dir),
    }
}

/// "Use my own file…" at the end of an event's picker.
fn own_file_item(group: &'static str, event: SoundEvent, view: WeakEntity<SettingsView>) -> PopupMenuItem {
    let selector = format!("{group}-{OWN_FILE}");
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div().debug_selector(move || selector.clone()).child("Use my own file…")
    })
    .on_click(move |_, _, cx| {
        // a view gone with its window has nothing to add to
        let _ = view.update(cx, |view, cx| view.add(Adding::File(event), cx));
    })
}
