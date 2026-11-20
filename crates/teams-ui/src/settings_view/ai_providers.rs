//! The settings' "AI providers" box on the General page: the providers that each AI setting can pick
//! ([`crate::local_ai::providers`]), in the user's order.
//!
//! - **A row per provider**: its name, its kind and its state (a command-line tool found on this computer or not; an
//!   API's key saved or not, and the base URL of an OpenAI-compatible server), Move up and Move down, Edit, Remove
//!   (an API only: a command-line tool can only be turned off) and its on/off switch.
//! - **Add provider**: the three API kinds, and a command-line tool that the list does not hold. A new API opens its
//!   editor and joins the list on Save.
//! - **The editor**, under its row: the name, the base URL, the default model, the API key (masked; empty keeps the
//!   saved key), "Test connection" with the values typed, Save and Cancel. A command-line tool has its name only.
//!
//! Each change writes the whole list (the tools found too) to the settings, so the order and the switches hold. A key
//! goes only to the keyring ([`AiKeys`]), off the UI thread, and the provider shows its key saved only when the
//! keyring took it. No key goes to the settings, a debug selector, a log line or a `Debug` output.
//!
//! Test selectors: `ai-providers` (the box), `ai-provider-<id>` (a row) with its `-switch`, `-up`, `-down`, `-edit`
//! and `-remove`; `ai-provider-add` and its rows `ai-provider-add-<kind>`; the editor's `ai-provider-name`,
//! `ai-provider-base-url`, `ai-provider-model`, `ai-provider-key`, `ai-provider-remove-key`, `ai-provider-test`,
//! `ai-provider-save` and `ai-provider-cancel`.

use std::sync::Arc;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use teams_auth::secrets::Secret;

use super::{group, row, row_colored, row_with};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::local_ai::providers::{self, AiKeys, ApiModels, KeyVault as _, MAX_NAME, MemoryKeys};
use crate::local_ai::{Backend, Model, Provider, ToolChoice, Tools, Work};
use crate::settings::SettingsModel;
use crate::theme::{self, Palette};

/// The API kinds that "Add provider" always offers.
const API_KINDS: [Backend; 3] = [Backend::Anthropic, Backend::OpenAi, Backend::Compatible];
/// Every work that keeps an AI choice.
const WORKS: [Work; 5] = [Work::Captions, Work::Summary, Work::Chat, Work::Correction, Work::Draft];
/// How wide an editor's field is, px.
const FIELD_WIDTH: f32 = 320.0;
/// What an OpenAI-compatible server's base URL looks like.
const COMPATIBLE_EXAMPLE: &str = "http://localhost:11434/v1";

/// What "Test connection" came to.
enum Tested {
    Running,
    /// The API listed this many models.
    Connected(usize),
    Failed(SharedString),
}

impl Tested {
    /// What the editor's "Test connection" row says of it.
    fn note(&self) -> SharedString {
        match self {
            Self::Running => "Testing the connection\u{2026}".into(),
            Self::Connected(1) => "Connected: 1 model".into(),
            Self::Connected(count) => format!("Connected: {count} models").into(),
            Self::Failed(why) => why.clone(),
        }
    }
}

/// What the editor waits for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Busy {
    Saving,
    RemovingKey,
}

/// The open editor of one provider (see the module docs).
struct Editor {
    /// The provider as it was when the editor opened (a new one: as it will join the list).
    provider: Provider,
    /// It is not in the list yet.
    new: bool,
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
    /// Why the last Save or key removal did not work.
    problem: Option<SharedString>,
    tested: Option<Tested>,
    testing: Option<Task<()>>,
    busy: Option<(Busy, Task<()>)>,
    _events: [Subscription; 4],
}

impl Editor {
    fn open(provider: Provider, new: bool, window: &mut Window, cx: &mut Context<AiProviders>) -> Self {
        let kind = provider.kind;
        let base_placeholder = if kind == Backend::Compatible {
            COMPATIBLE_EXAMPLE
        } else {
            providers::default_base_url(kind)
        };
        let key_placeholder = if kind == Backend::Compatible {
            "Optional"
        } else {
            "Paste your API key"
        };
        let name = field(&provider.name, kind.name(), false, window, cx);
        let base_url = field(
            provider.base_url.as_deref().unwrap_or_default(),
            base_placeholder,
            false,
            window,
            cx,
        );
        let model = field(
            provider.model.as_deref().unwrap_or_default(),
            "Optional",
            false,
            window,
            cx,
        );
        let key = field("", key_placeholder, true, window, cx);
        let events = [&name, &base_url, &model, &key].map(|input| {
            cx.subscribe_in(input, window, |this: &mut AiProviders, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save(cx);
                }
            })
        });
        name.update(cx, |name, cx| name.focus(window, cx));
        Self {
            provider,
            new,
            name,
            base_url,
            model,
            key,
            problem: None,
            tested: None,
            testing: None,
            busy: None,
            _events: events,
        }
    }

    fn busy(&self) -> Option<Busy> {
        self.busy.as_ref().map(|(busy, _)| *busy)
    }

    /// The provider as the fields make it, or what is wrong with them.
    fn edited(&self, cx: &App) -> Result<Provider, SharedString> {
        let text = |input: &Entity<InputState>| input.read(cx).value().trim().to_owned();
        edited(
            &self.provider,
            &text(&self.name),
            &text(&self.base_url),
            &text(&self.model),
        )
    }

    /// The key typed, if any.
    fn typed_key(&self, cx: &App) -> Option<Secret> {
        let typed = self.key.read(cx).value().trim().to_owned();
        (!typed.is_empty() && !self.provider.kind.is_cli()).then(|| Secret::new(typed))
    }
}

/// An editor's field.
fn field(
    value: &str,
    placeholder: &'static str,
    masked: bool,
    window: &mut Window,
    cx: &mut Context<AiProviders>,
) -> Entity<InputState> {
    let value = value.to_owned();
    cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder(placeholder)
            .masked(masked)
            .default_value(value)
    })
}

/// `provider` with the name, base URL and default model typed (each trimmed; an empty one is none), or what is wrong
/// with them.
///
/// # Errors
/// The name is empty or too long, an OpenAI-compatible server has no base URL, the base URL is not an `http` or
/// `https` address, or the model's name has a shape that no model has.
pub fn edited(provider: &Provider, name: &str, base_url: &str, model: &str) -> Result<Provider, SharedString> {
    if name.is_empty() {
        return Err("Type a name for the provider.".into());
    }
    if name.len() > MAX_NAME {
        return Err(format!("Use a shorter name: at most {MAX_NAME} bytes.").into());
    }
    let mut edited = Provider {
        name: name.to_owned(),
        ..provider.clone()
    };
    if provider.kind.is_cli() {
        return Ok(edited);
    }
    edited.base_url = match base_url {
        "" if provider.kind == Backend::Compatible => {
            return Err(format!("Type the base URL of the server's API, for example {COMPATIBLE_EXAMPLE}.").into());
        }
        "" => None,
        url if providers::base_url_shaped(url) => Some(url.to_owned()),
        _ => return Err("The base URL must be an http:// or https:// address with a server name.".into()),
    };
    edited.model = match model {
        "" => None,
        typed => match Model::from_key(typed) {
            Some(Model::Named(model)) => Some(model),
            _ => return Err("A model's name has no spaces and does not start with a dash.".into()),
        },
    };
    Ok(edited)
}

/// What a provider's row says under its name: its kind and its state.
fn state(provider: &Provider, tools: &Tools) -> String {
    let kind = provider.kind.name();
    if provider.kind.is_cli() {
        let found = if tools.program(provider.kind).is_some() {
            "Found on this computer"
        } else {
            "Not found"
        };
        return format!("{kind} \u{b7} {found}");
    }
    let key = if provider.has_key { "Key saved" } else { "No key" };
    match (&provider.base_url, provider.kind) {
        (Some(url), Backend::Compatible) => format!("{kind} \u{b7} {key} \u{b7} {url}"),
        _ => format!("{kind} \u{b7} {key}"),
    }
}

/// The box (see the module docs).
pub struct AiProviders {
    model: Entity<SettingsModel>,
    editor: Option<Editor>,
    /// Why a removed provider's key stays in the keyring.
    problem: Option<SharedString>,
    _observing: [Subscription; 2],
}

impl AiProviders {
    pub fn new(model: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = [
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe_global::<Tools>(|_, cx| cx.notify()),
        ];
        Self {
            model,
            editor: None,
            problem: None,
            _observing: observing,
        }
    }

    /// The providers shown: the settings' list, with the tools found that it does not hold ([`providers::effective`]).
    pub fn list(&self, cx: &mut App) -> Vec<Provider> {
        let tools = Tools::of(cx);
        providers::effective(&self.model.read(cx).settings().ai_providers, &tools)
    }

    /// The id of the provider whose editor is open.
    #[must_use]
    pub fn editing(&self) -> Option<&str> {
        self.editor.as_ref().map(|editor| editor.provider.id.as_str())
    }

    fn write(&self, list: Vec<Provider>, cx: &mut Context<Self>) {
        self.model.update(cx, |model, cx| {
            model.change(|settings| settings.ai_providers = list, cx);
        });
    }

    /// Turns the provider `id` on or off.
    pub fn set_enabled(&mut self, id: &str, on: bool, cx: &mut Context<Self>) {
        let mut list = self.list(cx);
        if let Some(provider) = list.iter_mut().find(|provider| provider.id == id) {
            provider.enabled = on;
            self.write(list, cx);
        }
    }

    /// Moves the provider `id` one place up (or down) in the order.
    pub fn move_by(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let mut list = self.list(cx);
        let Some(at) = list.iter().position(|provider| provider.id == id) else {
            return;
        };
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        if let Some(to) = to.filter(|to| *to < list.len()) {
            list.swap(at, to);
            self.write(list, cx);
        }
    }

    /// Removes the API provider `id`, its key and its listed models.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let mut list = self.list(cx);
        let Some(at) = list
            .iter()
            .position(|provider| provider.id == id && !provider.kind.is_cli())
        else {
            return;
        };
        let removed = list.remove(at);
        if self.editing() == Some(id) {
            self.editor = None;
        }
        self.write(list, cx);
        ApiModels::forget(id, cx);
        if removed.has_key {
            let keys = AiKeys::of(cx);
            let deleted = cx.background_executor().spawn({
                let id = removed.id.clone();
                async move { keys.delete(&id) }
            });
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let deleted = deleted.await;
                let _ = this.update(cx, |this, cx| this.removed_key(&removed.name, deleted, cx));
            })
            .detach();
        }
    }

    /// The keyring's answer to a removed provider's key.
    fn removed_key(&mut self, name: &str, deleted: Result<(), String>, cx: &mut Context<Self>) {
        if let Err(why) = deleted {
            self.problem = Some(format!("The key of {name} stays in the keyring: {why}").into());
            cx.notify();
        }
    }

    /// "Add provider": a command-line tool joins the list at once; an API opens its editor.
    pub fn add(&mut self, kind: Backend, window: &mut Window, cx: &mut Context<Self>) {
        let mut list = self.list(cx);
        if kind.is_cli() {
            if !list.iter().any(|provider| provider.kind == kind) {
                list.push(Provider::cli(kind));
                self.write(list, cx);
            }
            return;
        }
        let taken = self.taken(list, cx);
        let provider = Provider::new_api(kind, &taken);
        self.editor = Some(Editor::open(provider, true, window, cx));
        cx.notify();
    }

    /// `list` and the ids that an AI setting still names: a new provider takes none of them.
    fn taken(&self, mut list: Vec<Provider>, cx: &App) -> Vec<Provider> {
        let settings = self.model.read(cx).settings();
        for work in WORKS {
            if let ToolChoice::Only(id) = &settings.ai(work).tool
                && !list.iter().any(|provider| provider.id == *id)
            {
                list.push(Provider {
                    id: id.clone(),
                    ..Provider::new_api(Backend::Compatible, &[])
                });
            }
        }
        list
    }

    /// Opens the editor of the provider `id`.
    pub fn edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self.list(cx).into_iter().find(|provider| provider.id == id) else {
            return;
        };
        self.editor = Some(Editor::open(provider, false, window, cx));
        cx.notify();
    }

    /// Closes the editor; nothing typed is kept.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editor = None;
        cx.notify();
    }

    /// Save: the provider as typed joins (or replaces its entry in) the list; a key typed goes to the keyring first.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else { return };
        if editor.busy().is_some() {
            return;
        }
        let provider = match editor.edited(cx) {
            Ok(provider) => provider,
            Err(problem) => {
                editor.problem = Some(problem);
                cx.notify();
                return;
            }
        };
        let Some(key) = editor.typed_key(cx) else {
            self.commit(&provider, false, cx);
            return;
        };
        let keys = AiKeys::of(cx);
        let id = provider.id.clone();
        let put = cx.background_executor().spawn(async move { keys.put(&id, &key) });
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let put = put.await;
            let _ = this.update(cx, |this, cx| this.key_saved(provider, put, cx));
        });
        editor.problem = None;
        editor.busy = Some((Busy::Saving, task));
        cx.notify();
    }

    /// The keyring's answer to a Save's key: the provider saved with its key, or the editor says why not.
    fn key_saved(&mut self, mut provider: Provider, put: Result<(), String>, cx: &mut Context<Self>) {
        match put {
            Ok(()) => {
                provider.has_key = true;
                self.commit(&provider, true, cx);
            }
            Err(why) => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.busy = None;
                    editor.problem = Some(format!("The keyring did not keep the key: {why}").into());
                }
                cx.notify();
            }
        }
    }

    /// Writes `provider` into the list and closes its editor; its models are listed again when its key or base URL
    /// changed, or when it is new. A provider in the list takes only what the editor sets (name, base URL, model, a
    /// key saved): its switch and the rest stay as they are now, also when they changed while the editor was open.
    fn commit(&mut self, provider: &Provider, key_changed: bool, cx: &mut Context<Self>) {
        let mut list = self.list(cx);
        let held = list.iter().position(|held| held.id == provider.id);
        let relist =
            !provider.kind.is_cli() && (key_changed || held.is_none_or(|at| list[at].base_url != provider.base_url));
        match held {
            Some(at) => {
                let held = &mut list[at];
                held.name.clone_from(&provider.name);
                held.base_url.clone_from(&provider.base_url);
                held.model.clone_from(&provider.model);
                if key_changed {
                    held.has_key = provider.has_key;
                }
            }
            None => list.push(provider.clone()),
        }
        if self.editing() == Some(provider.id.as_str()) {
            self.editor = None;
        }
        self.write(list, cx);
        if relist {
            ApiModels::relist(provider, cx);
        }
        cx.notify();
    }

    /// "Remove key": the saved key goes from the keyring, and the provider has none from then on.
    pub fn remove_key(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut() else { return };
        if editor.busy().is_some() || !editor.provider.has_key {
            return;
        }
        let keys = AiKeys::of(cx);
        let id = editor.provider.id.clone();
        let deleted = cx.background_executor().spawn({
            let id = id.clone();
            async move { keys.delete(&id) }
        });
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let deleted = deleted.await;
            let _ = this.update(cx, |this, cx| this.key_removed(&id, deleted, cx));
        });
        editor.problem = None;
        editor.busy = Some((Busy::RemovingKey, task));
        cx.notify();
    }

    fn key_removed(&mut self, id: &str, deleted: Result<(), String>, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_mut().filter(|editor| editor.provider.id == id) else {
            return;
        };
        editor.busy = None;
        if let Err(why) = deleted {
            editor.problem = Some(format!("The keyring did not remove the key: {why}").into());
            cx.notify();
            return;
        }
        editor.provider.has_key = false;
        let mut list = self.list(cx);
        if let Some(provider) = list.iter_mut().find(|provider| provider.id == id) {
            provider.has_key = false;
            let provider = provider.clone();
            self.write(list, cx);
            ApiModels::relist(&provider, cx);
        }
        cx.notify();
    }

    /// "Test connection": asks the API for its models with the values typed (a key typed, else the one saved), in
    /// the background.
    pub fn test_connection(&mut self, cx: &mut Context<Self>) {
        if !crate::local_ai::enabled(cx) {
            return;
        }
        let Some(editor) = self.editor.as_mut() else { return };
        let mut provider = match editor.edited(cx) {
            Ok(provider) => provider,
            Err(problem) => {
                editor.tested = Some(Tested::Failed(problem));
                cx.notify();
                return;
            }
        };
        let keys = match editor.typed_key(cx) {
            Some(key) => {
                let typed = MemoryKeys::default();
                let _ = typed.put(&provider.id, &key);
                provider.has_key = true;
                AiKeys::new(Arc::new(typed))
            }
            None if provider.has_key || provider.kind == Backend::Compatible => AiKeys::of(cx),
            None => {
                editor.tested = Some(Tested::Failed("Type the API key to test the connection.".into()));
                cx.notify();
                return;
            }
        };
        let stop = ApiModels::stop_flag(cx);
        let listed = cx
            .background_executor()
            .spawn(async move { ApiModels::list_unless(&stop, &provider, &keys) });
        editor.testing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let tested = match listed.await {
                Ok(models) => Tested::Connected(models.len()),
                Err(failure) => Tested::Failed(failure.message().into()),
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(editor) = this.editor.as_mut() {
                    editor.tested = Some(tested);
                    editor.testing = None;
                }
                cx.notify();
            });
        }));
        editor.tested = Some(Tested::Running);
        cx.notify();
    }

    /// What "Test connection" says now, if anything.
    #[must_use]
    pub fn tested(&self) -> Option<SharedString> {
        self.editor.as_ref()?.tested.as_ref().map(Tested::note)
    }

    /// What the open editor says is wrong, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<SharedString> {
        self.editor
            .as_ref()
            .and_then(|editor| editor.problem.clone())
            .or_else(|| self.problem.clone())
    }
}

// --- drawing ---

impl AiProviders {
    /// A provider's row: its name and state, its order, Edit, Remove and its switch; `place` is its index, the list's
    /// length, and whether its editor is open.
    fn provider_row(
        provider: &Provider,
        place: (usize, usize, bool),
        tools: &Tools,
        palette: Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (at, count, edited) = place;
        let id = provider.id.clone();
        let base = format!("ai-provider-{id}");
        let action = |suffix: &str, glyph: Glyph, tip: &'static str| {
            let selector = format!("{base}-{suffix}");
            (
                selector.clone(),
                icon_button(SharedString::from(selector), glyph, tip).small(),
            )
        };
        let on = |act: fn(&mut Self, &str, &mut Context<Self>)| {
            let id = id.clone();
            cx.listener(
                move |this: &mut Self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                    act(this, &id, cx);
                },
            )
        };
        let (up, up_button) = action("up", Glyph::ARROW_UP, "Move up");
        let (down, down_button) = action("down", Glyph::ARROW_DOWN, "Move down");
        let (edit, edit_button) = action("edit", Glyph::PENCIL, "Edit");
        let editing = {
            let id = id.clone();
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.edit(&id, window, cx))
        };
        let controls = div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                up,
                up_button
                    .disabled(at == 0)
                    .on_click(on(|this, id, cx| this.move_by(id, true, cx))),
            ))
            .child(selected_by(
                down,
                down_button
                    .disabled(at + 1 == count)
                    .on_click(on(|this, id, cx| this.move_by(id, false, cx))),
            ))
            .child(selected_by(edit, edit_button.selected(edited).on_click(editing)))
            .child(Self::remove_control(provider, &base, cx))
            .child(div().ml_3().child(Self::switch_control(provider, &base, cx)));
        div()
            .debug_selector(move || base.clone())
            .child(row_with(
                provider.name.clone().into(),
                Some(state(provider, tools).into()),
                controls,
                palette,
            ))
            .into_any_element()
    }

    /// Remove, for an API; for a command-line tool a hidden one, which keeps the switches in one column.
    fn remove_control(provider: &Provider, base: &str, cx: &Context<Self>) -> AnyElement {
        if provider.kind.is_cli() {
            return div()
                .invisible()
                .child(
                    icon_button(SharedString::from(format!("{base}-no-remove")), Glyph::TRASH, "")
                        .small()
                        .disabled(true),
                )
                .into_any_element();
        }
        let id = provider.id.clone();
        let selector = format!("{base}-remove");
        selected_by(
            selector.clone(),
            icon_button(SharedString::from(selector), Glyph::TRASH, "Remove provider")
                .small()
                .text_color(rgb(theme::DANGER))
                .on_click(cx.listener(move |this, _, _, cx| this.remove(&id, cx))),
        )
        .into_any_element()
    }

    /// The provider's on/off switch.
    fn switch_control(provider: &Provider, base: &str, cx: &Context<Self>) -> AnyElement {
        let id = provider.id.clone();
        let selector = format!("{base}-switch");
        selected_by(
            selector.clone(),
            Switch::new(SharedString::from(selector))
                .checked(provider.enabled)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| this.set_enabled(&id, *checked, cx))),
        )
        .into_any_element()
    }

    /// "Add provider" and its dropdown of kinds.
    fn add_row(list: &[Provider], palette: Palette, cx: &Context<Self>) -> AnyElement {
        let mut kinds = API_KINDS.to_vec();
        kinds.extend(
            Backend::CLI
                .into_iter()
                .filter(|kind| !list.iter().any(|provider| provider.kind == *kind)),
        );
        let this = cx.entity().downgrade();
        let button = Button::new("ai-provider-add")
            .icon(Glyph::PLUS.icon())
            .label("Add provider")
            .outline()
            .small()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                kinds.iter().fold(menu.min_w(px(220.0)), |menu, kind| {
                    let (kind, this) = (*kind, this.clone());
                    let selector = format!("ai-provider-add-{}", kind.key());
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let selector = selector.clone();
                            div().debug_selector(move || selector.clone()).child(kind.name())
                        })
                        .on_click(move |_, window, cx| {
                            let _ = this.update(cx, |this, cx| this.add(kind, window, cx));
                        }),
                    )
                })
            });
        row(
            "Add provider",
            Some("An API that the app asks with your key, or a command-line tool on this computer."),
            selected_by("ai-provider-add", button),
            palette,
        )
    }

    /// The open editor's rows, under its provider's row.
    fn editor_rows(editor: &Editor, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let kind = editor.provider.kind;
        let busy = editor.busy();
        let mut rows = Vec::new();
        if editor.new {
            rows.push(
                div()
                    .px_4()
                    .pt_3()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("New {}", kind.name()))
                    .into_any_element(),
            );
        }
        rows.push(noted(
            "Name",
            "What the AI settings call this provider.".into(),
            palette.muted_foreground,
            input("ai-provider-name", &editor.name, busy.is_some(), false),
        ));
        if !kind.is_cli() {
            rows.extend(Self::api_rows(editor, palette, cx));
        }
        rows.push(Self::footer(editor, palette, cx));
        let rows = rows.into_iter().enumerate().map(|(index, row)| {
            div()
                .when(index > 0 && !(editor.new && index == 1), |row| {
                    row.border_t_1().border_color(rgb(palette.border))
                })
                .child(row)
        });
        div()
            .debug_selector(|| "ai-provider-editor".to_owned())
            .bg(rgb(palette.background))
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    }

    /// An API editor's base URL, model, key and connection test.
    fn api_rows(editor: &Editor, palette: Palette, cx: &Context<Self>) -> Vec<AnyElement> {
        let kind = editor.provider.kind;
        let busy = editor.busy();
        let muted = palette.muted_foreground;
        let base_url_note: SharedString = if kind == Backend::Compatible {
            "The address of the server's OpenAI-compatible API.".into()
        } else {
            format!("Leave it empty to use {}.", providers::default_base_url(kind)).into()
        };
        let model_note: SharedString = if kind == Backend::Compatible {
            "The model that the AI settings ask for when they name none.".into()
        } else {
            format!(
                "What \u{201c}{}'s default\u{201d} asks for in an AI setting.",
                editor.provider.name
            )
            .into()
        };
        let key_note: SharedString = match (editor.provider.has_key, kind) {
            (true, _) => "A key is saved in the system keyring. Type a new one to replace it.".into(),
            (false, Backend::Compatible) => "Only if the server asks for one. It goes to the system keyring.".into(),
            (false, _) => "It goes to the system keyring, never to the settings file.".into(),
        };
        let remove_key = editor.provider.has_key.then(|| {
            selected_by(
                "ai-provider-remove-key",
                Button::new("ai-provider-remove-key")
                    .label("Remove key")
                    .outline()
                    .small()
                    .spinning(busy == Some(Busy::RemovingKey))
                    .disabled(busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.remove_key(cx))),
            )
        });
        let key = div().flex().items_center().gap_2().children(remove_key).child(input(
            "ai-provider-key",
            &editor.key,
            busy.is_some(),
            true,
        ));
        vec![
            noted(
                "Base URL",
                base_url_note,
                muted,
                input("ai-provider-base-url", &editor.base_url, busy.is_some(), false),
            ),
            noted(
                "Default model",
                model_note,
                muted,
                input("ai-provider-model", &editor.model, busy.is_some(), false),
            ),
            noted("API key", key_note, muted, key),
            Self::test_row(editor, palette, cx),
        ]
    }

    /// "Test connection", and what it came to.
    fn test_row(editor: &Editor, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let running = matches!(editor.tested, Some(Tested::Running));
        let (note, color): (SharedString, u32) = match &editor.tested {
            None => (
                "Ask the API for its models with the values above.".into(),
                palette.muted_foreground,
            ),
            Some(tested) => (
                tested.note(),
                match tested {
                    Tested::Running => palette.muted_foreground,
                    Tested::Connected(_) => palette.foreground,
                    Tested::Failed(_) => theme::DANGER,
                },
            ),
        };
        let mut button = Button::new("ai-provider-test")
            .label("Test connection")
            .outline()
            .small()
            .spinning(running)
            .disabled(running)
            .on_click(cx.listener(|this, _, _, cx| this.test_connection(cx)));
        if matches!(editor.tested, Some(Tested::Connected(_))) {
            button = button.icon(Glyph::CIRCLE_CHECK.icon());
        }
        noted("Test connection", note, color, selected_by("ai-provider-test", button))
    }

    /// What is wrong, then Cancel and Save.
    fn footer(editor: &Editor, palette: Palette, cx: &Context<Self>) -> AnyElement {
        let busy = editor.busy();
        div()
            .px_4()
            .py_2p5()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "ai-provider-problem".to_owned())
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(theme::DANGER))
                    .children(editor.problem.clone()),
            )
            .child(selected_by(
                "ai-provider-cancel",
                Button::new("ai-provider-cancel")
                    .label("Cancel")
                    .outline()
                    .small()
                    .disabled(busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            ))
            .child(selected_by(
                "ai-provider-save",
                Button::new("ai-provider-save")
                    .label("Save")
                    .primary()
                    .small()
                    .spinning(busy == Some(Busy::Saving))
                    .disabled(busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            ))
            .text_color(rgb(palette.foreground))
            .into_any_element()
    }
}

/// An editor's field, its test selector `selector`.
fn input(selector: &'static str, state: &Entity<InputState>, disabled: bool, masked: bool) -> AnyElement {
    let field = Input::new(state).disabled(disabled);
    let field = if masked { field.mask_toggle() } else { field };
    div()
        .debug_selector(move || selector.to_owned())
        .w(px(FIELD_WIDTH))
        .child(field)
        .into_any_element()
}

/// An editor's row: its name and a note in `color` leading, its control trailing.
fn noted(title: &'static str, note: SharedString, color: u32, control: impl IntoElement) -> AnyElement {
    row_colored(title.into(), Some(note), color, control)
}

impl Render for AiProviders {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let list = self.list(cx);
        let tools = Tools::of(cx);
        let mut rows = Vec::new();
        if list.is_empty() {
            rows.push(row(
                "No AI providers",
                Some("Install Claude Code (claude) or the Codex CLI (codex), or add an API with your key."),
                div(),
                palette,
            ));
        }
        for (at, provider) in list.iter().enumerate() {
            let edited = self.editing() == Some(provider.id.as_str());
            rows.push(Self::provider_row(
                provider,
                (at, list.len(), edited),
                &tools,
                palette,
                cx,
            ));
            if let Some(editor) = self
                .editor
                .as_ref()
                .filter(|editor| editor.provider.id == provider.id && !editor.new)
            {
                rows.push(Self::editor_rows(editor, palette, cx));
            }
        }
        if let Some(editor) = self.editor.as_ref().filter(|editor| editor.new) {
            rows.push(Self::editor_rows(editor, palette, cx));
        }
        rows.push(Self::add_row(&list, palette, cx));
        if let Some(problem) = self.problem.clone() {
            rows.push(
                div()
                    .px_4()
                    .py_2p5()
                    .text_sm()
                    .text_color(rgb(theme::DANGER))
                    .child(problem)
                    .into_any_element(),
            );
        }
        div()
            .debug_selector(|| "ai-providers".to_owned())
            .child(group(Some("AI providers"), rows, palette))
    }
}
