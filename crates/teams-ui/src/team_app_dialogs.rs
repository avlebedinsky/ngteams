//! Two more dialogs of Manage team, as Teams has them (`docs/protocol/teams.md`), in Teams' words:
//!
//! - [`TeamPictureDialog`]: "Change team picture": the picture, Upload (a PNG or JPEG picked from the disk) and Remove
//!   (the picture Teams makes from the team's name, [`Core::default_team_picture`]), then Save.
//! - [`AddAppDialog`]: "Get more apps": the apps the store offers ([`Core::app_store`]), Search, and Add on one, which
//!   puts it on the team. An app that brings a bot says so.
//!
//! A dialog tells the shell by a [`TeamDialogEvent`]. Nothing here sends anything but through [`Core`], and only on its
//! own confirming button.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Image, ImageFormat,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, PathPromptOptions, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Disableable as _, FocusTrapElement as _, Sizable as _};
use teams_proto::{RawBody, StoreApp, TeamWrite};
use teams_store::Team;

use crate::bridge::Core;
use crate::icons::{Glyph, selected_by};
use crate::manage_team::app_tile;
use crate::team_dialogs::{TeamDialogEvent, buttons, problem, surface, title, waiting};
use crate::teams_list::TeamsList;
use crate::theme::{self, Palette};

/// The largest picture Upload takes, as Teams' own limit for a team's picture.
pub const PICTURE_LIMIT: usize = 4 * 1024 * 1024;

/// The format of a picture's bytes, by their first ones: only PNG and JPEG, as Teams' Upload takes.
#[must_use]
pub fn picture_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, ..] => Some(ImageFormat::Jpeg),
        _ => None,
    }
}

/// "Change team picture" (see the module docs).
pub struct TeamPictureDialog {
    core: Arc<dyn Core>,
    teams: Entity<TeamsList>,
    team: Team,
    /// The picture chosen, waiting for Save.
    chosen: Option<Vec<u8>>,
    reading: bool,
    saving: bool,
    problem: Option<&'static str>,
    working: Option<Task<()>>,
    focus: FocusHandle,
}

impl EventEmitter<TeamDialogEvent> for TeamPictureDialog {}

impl TeamPictureDialog {
    /// The dialog on `team`.
    pub fn new(
        core: Arc<dyn Core>,
        teams: Entity<TeamsList>,
        team: Team,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self {
            core,
            teams,
            team,
            chosen: None,
            reading: false,
            saving: false,
            problem: None,
            working: None,
            focus,
        }
    }

    /// The picture chosen, waiting for Save.
    #[must_use]
    pub fn chosen(&self) -> Option<&[u8]> {
        self.chosen.as_deref()
    }

    /// A picture's bytes chosen: taken when PNG or JPEG and not too large, else said why not.
    pub fn choose(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if bytes.len() > PICTURE_LIMIT {
            self.problem = Some("The picture is too large. Choose one under 4 MB.");
        } else if picture_format(&bytes).is_none() {
            self.problem = Some("That file can't be used. Choose a .png, .jpg or .jpeg picture.");
        } else {
            self.problem = None;
            self.chosen = Some(bytes);
        }
        cx.notify();
    }

    /// Upload: a picture picked from the disk.
    pub fn upload(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Upload".into()),
        });
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let path: Option<PathBuf> = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            let Some(path) = path else {
                return;
            };
            let read = cx.background_spawn(async move { std::fs::read(path) }).await;
            let _ = this.update(cx, |this, cx| match read {
                Ok(bytes) => this.choose(bytes, cx),
                Err(error) => {
                    log::info!("a team's picture couldn't be read: {error}");
                    this.problem = Some("That file can't be read.");
                    cx.notify();
                }
            });
        }));
    }

    /// Remove: the picture Teams makes from the team's name, read now, chosen.
    pub fn remove(&mut self, cx: &mut Context<Self>) {
        self.reading = true;
        cx.notify();
        let reply = self.core.default_team_picture(self.team.name.clone());
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let picture = reply.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.reading = false;
                if let Some(bytes) = picture {
                    this.choose(bytes, cx);
                } else {
                    this.problem = Some("We couldn't remove the picture. Please try again.");
                    cx.notify();
                }
            });
        }));
    }

    /// Save: the chosen picture becomes the team's.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let (Some(bytes), Some(group)) = (self.chosen.clone(), self.team.group_id.clone()) else {
            return;
        };
        self.saving = true;
        self.problem = None;
        cx.notify();
        let write = TeamWrite::SetPicture {
            team: self.team.id.clone(),
            group,
            picture: RawBody::from(bytes.clone()),
        };
        let reply = self.core.team_write(write, None);
        let team = self.team.id.clone();
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.saved(answer.is_ok(), team, bytes, cx));
        }));
    }

    /// Save's answer: the new picture shows at once and the dialog closes, or it says what went wrong.
    fn saved(&mut self, went: bool, team: String, bytes: Vec<u8>, cx: &mut Context<Self>) {
        self.saving = false;
        if !went {
            self.problem = Some("We couldn't update the team picture. Please try again.");
            cx.notify();
            return;
        }
        if let Some(format) = picture_format(&bytes) {
            let image = Arc::new(Image::from_bytes(format, bytes));
            self.teams
                .update(cx, |teams, cx| teams.picture_changed(team.clone(), image, cx));
        }
        cx.emit(TeamDialogEvent::Changed {
            team,
            words: "Team picture updated",
        });
    }

    fn preview(&self, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let face = div().size(px(196.0)).flex_none().overflow_hidden().rounded_md();
        match self
            .chosen
            .as_ref()
            .and_then(|bytes| Some((picture_format(bytes)?, bytes.clone())))
        {
            Some((format, bytes)) => face
                .debug_selector(|| "team-picture-chosen".to_owned())
                .child(gpui::img(Arc::new(Image::from_bytes(format, bytes))).size_full())
                .into_any_element(),
            None => face
                .child(self.teams.read(cx).avatar(&self.team, 196.0, false, palette))
                .into_any_element(),
        }
    }
}

impl Render for TeamPictureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let busy = self.saving || self.reading;
        let can_save = self.chosen.is_some() && !busy;
        surface("team-picture-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    cx.emit(TeamDialogEvent::Closed);
                }
            }))
            .child(title("Change team picture"))
            .child(div().flex().justify_center().pt_4().child(self.preview(palette, cx)))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .gap_3()
                    .child(selected_by(
                        "team-picture-upload",
                        Button::new("team-picture-upload")
                            .ghost()
                            .icon(Glyph::UPLOAD.icon())
                            .label("Upload")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.upload(cx))),
                    ))
                    .child(selected_by(
                        "team-picture-remove",
                        Button::new("team-picture-remove")
                            .ghost()
                            .icon(Glyph::TRASH.icon())
                            .label("Remove")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                    )),
            )
            .children(self.problem.map(|words| problem("team-picture-problem", words)))
            .child(
                buttons()
                    .child(selected_by(
                        "team-picture-close",
                        Button::new("team-picture-close")
                            .outline()
                            .label("Close")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamDialogEvent::Closed))),
                    ))
                    .child(selected_by(
                        "team-picture-save",
                        Button::new("team-picture-save")
                            .primary()
                            .label(if self.saving { "Saving…" } else { "Save" })
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    )),
            )
            .focus_trap("team-picture-dialog-trap", &self.focus)
    }
}

/// How many apps the dialog lists at most.
const MOST_APPS: usize = 60;

/// "Get more apps" (see the module docs).
pub struct AddAppDialog {
    core: Arc<dyn Core>,
    team: Team,
    search: Entity<InputState>,
    apps: Option<Vec<StoreApp>>,
    /// The installed apps' ids: their Add says Added.
    installed: Vec<String>,
    adding: Option<String>,
    added: Vec<String>,
    problem: Option<&'static str>,
    _reading: Option<Task<()>>,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _typed: Subscription,
}

impl EventEmitter<TeamDialogEvent> for AddAppDialog {}

impl AddAppDialog {
    /// The dialog on `team`, the store being read; `installed` are the ids of the apps on it.
    pub fn new(
        core: Arc<dyn Core>,
        team: Team,
        installed: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search apps"));
        let typed = cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify());
        search.update(cx, |search, cx| search.focus(window, cx));
        let reply = core.app_store();
        let reading = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let apps = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.apps = Some(apps);
                cx.notify();
            });
        });
        Self {
            core,
            team,
            search,
            apps: None,
            installed,
            adding: None,
            added: Vec::new(),
            problem: None,
            _reading: Some(reading),
            working: None,
            focus: cx.focus_handle(),
            _typed: typed,
        }
    }

    /// The apps listed: those whose name or maker holds the search's words, by name.
    #[must_use]
    pub fn shown(&self, cx: &gpui::App) -> Vec<StoreApp> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        let mut apps: Vec<StoreApp> = self
            .apps
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|store| {
                query.is_empty()
                    || store.app.name.to_lowercase().contains(&query)
                    || store.app.developer.to_lowercase().contains(&query)
            })
            .collect();
        apps.sort_by_key(|store| store.app.name.to_lowercase());
        apps.truncate(MOST_APPS);
        apps
    }

    /// Add on an app: it goes onto the team.
    pub fn add(&mut self, store: StoreApp, cx: &mut Context<Self>) {
        let id = store.app.id.clone();
        self.adding = Some(id.clone());
        self.problem = None;
        cx.notify();
        let reply = self.core.team_write(
            TeamWrite::AddApp {
                team: self.team.id.clone(),
                app: store.definition,
            },
            None,
        );
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.adding = None;
                if answer.is_ok() {
                    this.added.push(id);
                } else {
                    this.problem = Some("The app wasn't added. Please try again.");
                }
                cx.notify();
            });
        }));
    }

    /// Done: the dialog closes, Manage team reading its apps again when one was added.
    pub fn done(&mut self, cx: &mut Context<Self>) {
        if self.added.is_empty() {
            cx.emit(TeamDialogEvent::Closed);
        } else {
            cx.emit(TeamDialogEvent::Changed {
                team: self.team.id.clone(),
                words: "App added to the team",
            });
        }
    }

    fn app_row(&self, at: usize, store: &StoreApp, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = store.app.id.clone();
        let on_team = self.installed.contains(&id) || self.added.contains(&id);
        let adding = self.adding.as_deref() == Some(id.as_str());
        let picked = store.clone();
        let read_only = self.core.read_only();
        div()
            .debug_selector(move || format!("add-app-{at}"))
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(app_tile(&store.app, 36.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(if store.has_bot {
                                format!("{} · Adds a bot to the team", store.app.developer)
                            } else {
                                store.app.developer.clone()
                            }),
                    ),
            )
            .when(!read_only, |row| {
                row.child(selected_by(
                    format!("add-app-add-{at}"),
                    Button::new(SharedString::from(format!("add-app-add-{at}")))
                        .small()
                        .when(!on_team, ButtonVariants::primary)
                        .when(on_team, Button::outline)
                        .label(if on_team {
                            "Added"
                        } else if adding {
                            "Adding…"
                        } else {
                            "Add"
                        })
                        .disabled(on_team || self.adding.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| this.add(picked.clone(), cx))),
                ))
            })
            .into_any_element()
    }
}

impl Render for AddAppDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let shown = self.shown(cx);
        let rows: Vec<AnyElement> = shown
            .iter()
            .enumerate()
            .map(|(at, store)| self.app_row(at, store, palette, cx))
            .collect();
        let list: AnyElement = match &self.apps {
            None => waiting("add-app-loading", "Loading apps…", palette).into_any_element(),
            Some(_) if rows.is_empty() => div()
                .debug_selector(|| "add-app-none".to_owned())
                .py_8()
                .text_center()
                .text_color(rgb(palette.muted_foreground))
                .child("No apps match your search.")
                .into_any_element(),
            Some(_) => div()
                .id("add-app-list")
                .max_h(px(380.0))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows)
                .into_any_element(),
        };
        surface("add-app-dialog", palette, window)
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.done(cx);
                }
            }))
            .child(title(format!("Add an app to {}", self.team.name)))
            .child(selected_by("add-app-search", Input::new(&self.search)))
            .child(list)
            .children(self.problem.map(|words| problem("add-app-problem", words)))
            .child(
                buttons().child(selected_by(
                    "add-app-done",
                    Button::new("add-app-done")
                        .primary()
                        .label("Done")
                        .on_click(cx.listener(|this, _, _, cx| this.done(cx))),
                )),
            )
            .focus_trap("add-app-dialog-trap", &self.focus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_png_and_jpeg_pictures_are_taken() {
        assert_eq!(picture_format(b"\x89PNG\r\n"), Some(ImageFormat::Png));
        assert_eq!(picture_format(&[0xff, 0xd8, 0xff]), Some(ImageFormat::Jpeg));
        assert_eq!(picture_format(b"GIF89a"), None);
        assert_eq!(picture_format(b""), None);
    }
}
