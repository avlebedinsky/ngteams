//! Manage users: the settings page of an own server's administrator (`docs/server.md`, "Accounts and administration").
//! It lists the server's accounts with a search and more on demand, adds one, and opens one to edit its name, user
//! name, whether it can sign in and whether it is an administrator, to reset its password or to delete it after a
//! confirmation. Microsoft 365 has no counterpart in Teams itself (its accounts are managed in a web admin centre), so
//! the page is drawn with the settings' own boxes and the team dialogs' surfaces.
//!
//! The page asks nothing itself: every read and change goes through the [`UserAdmin`] the app attached for the
//! account, off the foreground, and only an account whose server says it is an administrator gets one. What the
//! server refuses is said in the user's words ([`AdminProblem::words`]).
//!
//! **The keyboard**: Tab walks the search, "Add user" and the list; in the list Up, Down, Home and End move, Enter or
//! Space opens the account. A dialog traps Tab, Enter in a field confirms, Escape closes (Escape and Cancel wait while
//! its change is under way); the search has the keyboard again once a dialog closes.
//!
//! Traced: `user-admin shown`, `listed` (the accounts by id), `dialog`, `done` and `failed`, each with a kind, never
//! a name or a password.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, FocusTrapElement as _, Sizable as _, Size};
use teams_auth::admin::{AdminError, AdminUser, Refusal, UserChanges, UserPage};
use teams_auth::secrets::Secret;

use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard;
use crate::team_dialogs::{buttons, problem, surface, title};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// How long the search waits after the typing.
const PAUSE: Duration = Duration::from_millis(250);
/// The shortest password the server takes.
pub const MIN_PASSWORD: usize = 8;
/// What a password shorter than [`MIN_PASSWORD`] is told, wherever one is typed.
pub const SHORT_PASSWORD: &str = "Use a password of at least 8 characters.";
// the words name the minimum: they change with it
const _: () = assert!(MIN_PASSWORD == 8, "SHORT_PASSWORD names another minimum");
/// What the search asks for when it is empty.
pub const SEARCH_PLACEHOLDER: &str = "Search by name or user name";

/// A new account, as the administrator fills it in.
#[derive(Clone, Debug)]
pub struct NewAccount {
    pub upn: String,
    pub display_name: String,
    pub password: Secret,
    pub admin: bool,
    pub must_change_password: bool,
}

/// Why the server didn't do what the administrator asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdminProblem {
    LastAdmin,
    UpnTaken,
    InvalidUpn,
    InvalidName,
    WeakPassword,
    /// The account is gone (another administrator deleted it).
    NotFound,
    /// This account isn't an administrator any more.
    NotAdmin,
    /// This account is signed out or disabled.
    SignedOut,
    /// The server asks to wait.
    Wait,
    Unreachable,
    Other(String),
}

impl AdminProblem {
    /// The problem in the user's words.
    #[must_use]
    pub fn words(&self) -> SharedString {
        match self {
            Self::LastAdmin => {
                "This is the server's only administrator. Make another user an administrator first.".into()
            }
            Self::UpnTaken => "Another user already has this user name.".into(),
            Self::InvalidUpn => "Enter a user name like name@example.com.".into(),
            Self::InvalidName => "Enter a display name.".into(),
            Self::WeakPassword => SHORT_PASSWORD.into(),
            Self::NotFound => "This user no longer exists.".into(),
            Self::NotAdmin => "You're no longer an administrator of this server.".into(),
            Self::SignedOut => "You're signed out of this server. Sign in again to manage users.".into(),
            Self::Wait => "The server is busy. Wait a moment, then try again.".into(),
            Self::Unreachable => "The server can't be reached. Check your connection, then try again.".into(),
            Self::Other(said) if said.is_empty() => "The server couldn't do this. Try again.".into(),
            Self::Other(said) => said.clone().into(),
        }
    }

    /// Its name in the trace.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::LastAdmin => "last-admin",
            Self::UpnTaken => "upn-taken",
            Self::InvalidUpn => "invalid-upn",
            Self::InvalidName => "invalid-name",
            Self::WeakPassword => "weak-password",
            Self::NotFound => "not-found",
            Self::NotAdmin => "not-admin",
            Self::SignedOut => "signed-out",
            Self::Wait => "wait",
            Self::Unreachable => "unreachable",
            Self::Other(_) => "other",
        }
    }
}

impl std::fmt::Display for AdminProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.words())
    }
}

impl std::error::Error for AdminProblem {}

impl From<AdminError> for AdminProblem {
    fn from(error: AdminError) -> Self {
        match error {
            AdminError::Refused { refusal, message, .. } => match refusal {
                Refusal::LastAdmin => Self::LastAdmin,
                Refusal::UpnTaken => Self::UpnTaken,
                Refusal::InvalidUpn => Self::InvalidUpn,
                Refusal::InvalidName => Self::InvalidName,
                Refusal::WeakPassword => Self::WeakPassword,
                Refusal::NotFound => Self::NotFound,
                Refusal::NotAdmin => Self::NotAdmin,
                Refusal::AccountDisabled => Self::SignedOut,
                _ => Self::Other(message),
            },
            AdminError::Unauthorized => Self::SignedOut,
            AdminError::Throttled { .. } => Self::Wait,
            AdminError::Http(_) => Self::Unreachable,
            other => Self::Other(other.to_string()),
        }
    }
}

/// An own server's account management as the page asks it. Every call blocks until the server answered: the page
/// calls them off the foreground.
pub trait UserAdmin: Send + Sync + 'static {
    /// A page of accounts whose name or user name holds `query`, after the user name `after`.
    ///
    /// # Errors
    /// [`AdminProblem`], as for every call here.
    fn list(&self, query: &str, after: Option<&str>) -> Result<UserPage, AdminProblem>;
    /// Adds an account.
    ///
    /// # Errors
    /// [`AdminProblem`].
    fn add(&self, new: &NewAccount) -> Result<AdminUser, AdminProblem>;
    /// Changes the account `id`.
    ///
    /// # Errors
    /// [`AdminProblem`].
    fn change(&self, id: &str, changes: &UserChanges) -> Result<AdminUser, AdminProblem>;
    /// Deletes the account `id`.
    ///
    /// # Errors
    /// [`AdminProblem`].
    fn delete(&self, id: &str) -> Result<(), AdminProblem>;
    /// Replaces the password of the account `id`.
    ///
    /// # Errors
    /// [`AdminProblem`].
    fn reset_password(&self, id: &str, password: &Secret, must_change: bool) -> Result<(), AdminProblem>;
}

/// Whether `text` reads as a user name (`name@domain`, no spaces).
fn looks_like_upn(text: &str) -> bool {
    text.split_once('@').is_some_and(|(name, domain)| {
        !name.is_empty() && domain.contains('.') && !domain.contains('@') && !text.contains(char::is_whitespace)
    })
}

/// What a dialog is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogKind {
    Add,
    Edit,
    Reset,
    Delete,
}

impl DialogKind {
    fn key(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Edit => "edit",
            Self::Reset => "reset",
            Self::Delete => "delete",
        }
    }
}

/// What a dialog tells the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UserDialogEvent {
    Closed,
    /// Something changed on the server: the list is read again.
    Changed,
    /// The account's dialog of another kind is asked for (its password reset, its deletion).
    Open(DialogKind),
}

/// A dialog's ticks.
#[derive(Clone, Copy, Debug)]
struct Ticks {
    /// "Can sign in" of an account edited.
    enabled: bool,
    admin: bool,
    must_change: bool,
}

/// A dialog over the page (see the module docs).
pub struct UserDialog {
    admin: Arc<dyn UserAdmin>,
    trace: Arc<ViewTrace>,
    kind: DialogKind,
    /// The account it is about; none for a new one.
    user: Option<AdminUser>,
    upn: Entity<InputState>,
    name: Entity<InputState>,
    password: Entity<InputState>,
    ticks: Ticks,
    busy: bool,
    problem: Option<SharedString>,
    /// What the fields held when the problem was said: an edit clears it (the kit's input also tells a change after
    /// an Enter that changed nothing, which must not).
    said_for: (String, String, String),
    /// The password a reset set, shown until the dialog closes.
    given: Option<Secret>,
    /// The dialog itself takes the keyboard at the next draw (the field that had it is gone).
    focus_dialog: bool,
    working: Option<Task<()>>,
    focus: FocusHandle,
    _inputs: [Subscription; 3],
}

impl EventEmitter<UserDialogEvent> for UserDialog {}

impl UserDialog {
    /// The dialog of `kind` about `user`.
    pub fn new(
        (admin, trace): (Arc<dyn UserAdmin>, Arc<ViewTrace>),
        kind: DialogKind,
        user: Option<AdminUser>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let held = user.clone().unwrap_or_else(|| AdminUser {
            id: String::new(),
            upn: String::new(),
            display_name: String::new(),
            enabled: true,
            admin: false,
            must_change_password: true,
        });
        let upn = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("name@example.com")
                .default_value(held.upn.clone())
        });
        let name = cx.new(|cx| InputState::new(window, cx).default_value(held.display_name.clone()));
        let password = cx.new(|cx| InputState::new(window, cx));
        let inputs = [&upn, &name, &password].map(|field| {
            cx.subscribe(field, |this: &mut Self, _, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(cx),
                InputEvent::Change if this.problem.is_some() && this.said_for != this.values(cx) => {
                    this.problem = None;
                    cx.notify();
                }
                _ => {}
            })
        });
        let focus = cx.focus_handle();
        match kind {
            DialogKind::Add => upn.update(cx, |field, cx| field.focus(window, cx)),
            DialogKind::Edit => name.update(cx, |field, cx| field.focus(window, cx)),
            DialogKind::Reset => password.update(cx, |field, cx| field.focus(window, cx)),
            DialogKind::Delete => focus.focus(window, cx),
        }
        trace.record(&TraceLine::new("user-admin", "dialog").kind(kind.key()));
        Self {
            admin,
            trace,
            kind,
            user,
            upn,
            name,
            password,
            ticks: Ticks {
                enabled: held.enabled,
                admin: held.admin,
                must_change: true,
            },
            busy: false,
            problem: None,
            said_for: Default::default(),
            given: None,
            focus_dialog: false,
            working: None,
            focus,
            _inputs: inputs,
        }
    }

    #[must_use]
    pub fn kind(&self) -> DialogKind {
        self.kind
    }

    /// What the dialog says went wrong, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<&SharedString> {
        self.problem.as_ref()
    }

    /// Whether a reset went through and its password shows.
    #[must_use]
    pub fn shows_given(&self) -> bool {
        self.given.is_some()
    }

    /// Fills the form's fields (what a field doesn't show is left).
    pub fn fill(&mut self, upn: &str, name: &str, password: &str, window: &mut Window, cx: &mut Context<Self>) {
        for (field, value) in [(&self.upn, upn), (&self.name, name), (&self.password, password)] {
            let value = value.to_owned();
            field.update(cx, |field, cx| field.set_value(value, window, cx));
        }
        cx.notify();
    }

    pub fn set_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.ticks.enabled = enabled;
        self.problem = None;
        cx.notify();
    }

    pub fn set_admin(&mut self, admin: bool, cx: &mut Context<Self>) {
        self.ticks.admin = admin;
        self.problem = None;
        cx.notify();
    }

    pub fn set_must_change(&mut self, must_change: bool, cx: &mut Context<Self>) {
        self.ticks.must_change = must_change;
        cx.notify();
    }

    /// Puts a password the app made up into the password's field, shown.
    pub fn suggest_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match teams_auth::admin::suggested_password() {
            Ok(password) => {
                let password = password.expose().to_owned();
                self.password
                    .update(cx, |field, cx| field.set_value(password, window, cx));
                self.problem = None;
            }
            Err(_) => self.problem = Some("A password couldn't be made up. Type one.".into()),
        }
        cx.notify();
    }

    fn values(&self, cx: &App) -> (String, String, String) {
        (
            self.upn.read(cx).value().trim().to_owned(),
            self.name.read(cx).value().trim().to_owned(),
            self.password.read(cx).value().to_string(),
        )
    }

    /// What changed of the account edited.
    fn changes(&self, cx: &App) -> UserChanges {
        let (upn, name, _) = self.values(cx);
        let Some(user) = &self.user else {
            return UserChanges::default();
        };
        UserChanges {
            display_name: (name != user.display_name).then_some(name),
            upn: (!upn.eq_ignore_ascii_case(&user.upn)).then_some(upn),
            enabled: (self.ticks.enabled != user.enabled).then_some(self.ticks.enabled),
            admin: (self.ticks.admin != user.admin).then_some(self.ticks.admin),
        }
    }

    /// What keeps the form from being sent, said under it.
    fn form_problem(&self, cx: &App) -> Option<&'static str> {
        let (upn, name, password) = self.values(cx);
        let names = matches!(self.kind, DialogKind::Add | DialogKind::Edit);
        let passwords = matches!(self.kind, DialogKind::Add | DialogKind::Reset);
        if names && !looks_like_upn(&upn) {
            Some("Enter a user name like name@example.com.")
        } else if names && name.is_empty() {
            Some("Enter a display name.")
        } else if passwords && password.chars().count() < MIN_PASSWORD {
            Some(SHORT_PASSWORD)
        } else {
            None
        }
    }

    /// Whether the confirming button is on.
    #[must_use]
    pub fn can_confirm(&self, cx: &App) -> bool {
        !self.busy && (self.kind != DialogKind::Edit || !self.changes(cx).is_empty())
    }

    /// The confirming button, or Enter in a field: the change goes to the server.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        if self.given.is_some() {
            cx.emit(UserDialogEvent::Changed);
            return;
        }
        if !self.can_confirm(cx) {
            return;
        }
        if let Some(words) = self.form_problem(cx) {
            self.say(words.into(), cx);
            return;
        }
        let (upn, display_name, password) = self.values(cx);
        let password = Secret::new(password);
        let admin = Arc::clone(&self.admin);
        let id = self.user.as_ref().map(|user| user.id.clone()).unwrap_or_default();
        let (kind, must_change, changes) = (self.kind, self.ticks.must_change, self.changes(cx));
        let new = NewAccount {
            upn,
            display_name,
            password: password.clone(),
            admin: self.ticks.admin,
            must_change_password: must_change,
        };
        let asked = cx.background_spawn(async move {
            match kind {
                DialogKind::Add => admin.add(&new).map(|_| ()),
                DialogKind::Edit => admin.change(&id, &changes).map(|_| ()),
                DialogKind::Reset => admin.reset_password(&id, &new.password, must_change),
                DialogKind::Delete => admin.delete(&id),
            }
        });
        self.busy = true;
        self.problem = None;
        cx.notify();
        self.working = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = asked.await;
            let _ = this.update(cx, |this, cx| this.answered(answer, password, cx));
        }));
    }

    fn answered(&mut self, answer: Result<(), AdminProblem>, password: Secret, cx: &mut Context<Self>) {
        self.working = None;
        self.busy = false;
        match answer {
            Ok(()) => {
                self.trace
                    .record(&TraceLine::new("user-admin", "done").kind(self.kind.key()));
                if self.kind == DialogKind::Reset {
                    self.given = Some(password);
                    self.focus_dialog = true;
                    cx.notify();
                } else {
                    cx.emit(UserDialogEvent::Changed);
                }
            }
            Err(problem) => {
                self.trace
                    .record(&TraceLine::new("user-admin", "failed").kind(problem.kind()));
                self.say(problem.words(), cx);
            }
        }
    }

    /// Says `words` under the form, for what its fields hold now.
    fn say(&mut self, words: SharedString, cx: &mut Context<Self>) {
        self.said_for = self.values(cx);
        self.problem = Some(words);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                // the write under way is waited for: its answer still reaches the page
                if self.busy {
                    return;
                }
                let closed = if self.given.is_some() {
                    UserDialogEvent::Changed
                } else {
                    UserDialogEvent::Closed
                };
                cx.emit(closed);
            }
            // the password a reset set is read, then Enter closes
            "enter" if self.given.is_some() => {
                cx.stop_propagation();
                cx.emit(UserDialogEvent::Changed);
            }
            _ => {}
        }
    }

    fn shown_name(&self) -> String {
        self.user
            .as_ref()
            .map(|user| {
                if user.display_name.is_empty() {
                    user.upn.clone()
                } else {
                    user.display_name.clone()
                }
            })
            .unwrap_or_default()
    }

    fn label(words: &'static str) -> gpui::Div {
        div().text_sm().child(words)
    }

    fn tick(
        id: &'static str,
        words: &'static str,
        checked: bool,
        set: fn(&mut Self, bool, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        selected_by(
            id,
            Checkbox::new(id)
                .label(words)
                .checked(checked)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| set(this, *checked, cx))),
        )
    }

    fn password_row(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "user-dialog-password".to_owned())
                    .flex_1()
                    .child(Input::new(&self.password).disabled(self.busy)),
            )
            .child(selected_by(
                "user-dialog-suggest",
                icon_button("user-dialog-suggest", Glyph::REFRESH, "Suggest a password")
                    .disabled(self.busy)
                    .on_click(cx.listener(|this, _, window, cx| this.suggest_password(window, cx))),
            ))
    }

    fn names(&self) -> [gpui::Div; 4] {
        [
            Self::label("User name"),
            div()
                .debug_selector(|| "user-dialog-upn".to_owned())
                .child(Input::new(&self.upn).disabled(self.busy)),
            Self::label("Display name"),
            div()
                .debug_selector(|| "user-dialog-name".to_owned())
                .child(Input::new(&self.name).disabled(self.busy)),
        ]
    }

    fn add_form(&self, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let mut form = Vec::from(self.names());
        form.push(Self::label("Password"));
        form.push(self.password_row(cx));
        form.push(Self::tick(
            "user-dialog-must-change",
            "Ask for a new password at the first sign-in",
            self.ticks.must_change,
            Self::set_must_change,
            cx,
        ));
        form.push(Self::tick(
            "user-dialog-admin",
            "Administrator: can manage this server's users",
            self.ticks.admin,
            Self::set_admin,
            cx,
        ));
        form
    }

    fn edit_form(&self, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        let [upn_label, upn, name_label, name] = self.names();
        vec![
            name_label,
            name,
            upn_label,
            upn,
            Self::tick(
                "user-dialog-enabled",
                "Can sign in",
                self.ticks.enabled,
                Self::set_enabled,
                cx,
            ),
            Self::tick(
                "user-dialog-admin",
                "Administrator: can manage this server's users",
                self.ticks.admin,
                Self::set_admin,
                cx,
            ),
        ]
    }

    fn reset_form(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        if let Some(given) = &self.given {
            let copied = given.expose().to_owned();
            return vec![
                div().text_sm().child(format!(
                    "Give {} this password. It isn't shown again.",
                    self.shown_name()
                )),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "user-dialog-given".to_owned())
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(palette.background))
                            .font_family("monospace")
                            .child(given.expose().to_owned()),
                    )
                    .child(selected_by(
                        "user-dialog-copy",
                        icon_button("user-dialog-copy", Glyph::COPY, "Copy password").on_click(
                            move |_, _, cx: &mut App| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                            },
                        ),
                    )),
            ];
        }
        vec![
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("They're signed out everywhere and sign in with the new password."),
            Self::label("New password"),
            self.password_row(cx),
            Self::tick(
                "user-dialog-must-change",
                "Ask for a new password at the next sign-in",
                self.ticks.must_change,
                Self::set_must_change,
                cx,
            ),
        ]
    }

    /// The dialog's title and its confirming button's word.
    fn words(&self) -> (String, &'static str) {
        let name = self.shown_name();
        match self.kind {
            DialogKind::Add => ("Add a user".to_owned(), "Add"),
            DialogKind::Edit => (format!("Edit {name}"), "Save"),
            DialogKind::Reset if self.given.is_some() => ("Password reset".to_owned(), "Done"),
            DialogKind::Reset => (format!("Reset {name}'s password"), "Reset password"),
            DialogKind::Delete => (format!("Delete {name}?"), "Delete"),
        }
    }

    /// An edited account's other actions, at the left of the buttons.
    fn more_actions(&self, cx: &mut Context<Self>) -> Vec<gpui::Div> {
        if self.kind != DialogKind::Edit {
            return Vec::new();
        }
        vec![
            selected_by(
                "user-dialog-reset",
                Button::new("user-dialog-reset")
                    .outline()
                    .label("Reset password")
                    .disabled(self.busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(UserDialogEvent::Open(DialogKind::Reset)))),
            ),
            selected_by(
                "user-dialog-delete",
                Button::new("user-dialog-delete")
                    .danger()
                    .label("Delete user")
                    .disabled(self.busy)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(UserDialogEvent::Open(DialogKind::Delete)))),
            ),
        ]
    }

    fn footer(&self, confirm: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let can_confirm = self.given.is_some() || self.can_confirm(cx);
        let confirming = Button::new("user-dialog-confirm")
            .label(confirm)
            .disabled(!can_confirm)
            .tab_stop(can_confirm)
            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx)));
        let confirming = if self.kind == DialogKind::Delete {
            confirming.danger()
        } else {
            confirming.primary()
        };
        buttons()
            .children(self.more_actions(cx))
            .child(div().flex_1())
            .when(self.given.is_none(), |row| {
                row.child(selected_by(
                    "user-dialog-cancel",
                    Button::new("user-dialog-cancel")
                        .outline()
                        .label("Cancel")
                        .disabled(self.busy)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(UserDialogEvent::Closed))),
                ))
            })
            .child(selected_by("user-dialog-confirm", confirming))
    }
}

impl Render for UserDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        if std::mem::take(&mut self.focus_dialog) {
            self.focus.focus(window, cx);
        }
        let (heading, confirm) = self.words();
        let form = match self.kind {
            DialogKind::Add => self.add_form(cx),
            DialogKind::Edit => self.edit_form(cx),
            DialogKind::Reset => self.reset_form(palette, cx),
            DialogKind::Delete => vec![div().text_sm().child(
                "They're signed out everywhere and can't sign in again. Their messages stay. This can't be undone.",
            )],
        };
        surface("user-dialog", palette, window)
            .w(px(480.0))
            .key_context("UserDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(title(heading))
            .children(form)
            .children(self.problem.clone().map(|words| {
                div()
                    .debug_selector(|| "user-dialog-problem".to_owned())
                    .w_full()
                    .flex()
                    .items_start()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(theme::DANGER))
                    .child(
                        Glyph::CIRCLE_ALERT
                            .icon()
                            .size(px(16.0))
                            .flex_none()
                            .text_color(rgb(theme::DANGER)),
                    )
                    .child(problem("user-dialog-problem-words", words).text_sm().flex_1().min_w_0())
            }))
            .child(self.footer(confirm, cx))
            .focus_trap("user-dialog-trap", &self.focus)
    }
}

/// The Manage users page (see the module docs).
pub struct ManageUsers {
    admin: Arc<dyn UserAdmin>,
    trace: Arc<ViewTrace>,
    /// The administrator's own MRI: its row says "You".
    me: String,
    search: Entity<InputState>,
    users: Vec<AdminUser>,
    next: Option<String>,
    loading: bool,
    problem: Option<AdminProblem>,
    /// The list's cursor row while the list has the keyboard.
    at: usize,
    list_focus: FocusHandle,
    dialog: Option<(Entity<UserDialog>, Subscription)>,
    /// Counts the reads, so an older one's answer is dropped.
    read: u64,
    reading: Option<Task<()>>,
    shown: bool,
    _search: Subscription,
}

impl ManageUsers {
    pub fn new(
        admin: Arc<dyn UserAdmin>,
        trace: Arc<ViewTrace>,
        me: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(SEARCH_PLACEHOLDER));
        let searching = cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.read_from_start(PAUSE, cx);
            }
        });
        Self {
            admin,
            trace,
            me,
            search,
            users: Vec::new(),
            next: None,
            loading: false,
            problem: None,
            at: 0,
            list_focus: cx.focus_handle(),
            dialog: None,
            read: 0,
            reading: None,
            shown: false,
            _search: searching,
        }
    }

    /// The accounts listed.
    #[must_use]
    pub fn users(&self) -> &[AdminUser] {
        &self.users
    }

    /// Whether more accounts follow those listed.
    #[must_use]
    pub fn has_more(&self) -> bool {
        self.next.is_some()
    }

    /// What kept the list from being read, if anything.
    #[must_use]
    pub fn problem(&self) -> Option<&AdminProblem> {
        self.problem.as_ref()
    }

    /// The dialog open over the page.
    #[must_use]
    pub fn dialog(&self) -> Option<&Entity<UserDialog>> {
        self.dialog.as_ref().map(|(dialog, _)| dialog)
    }

    /// The search's field.
    #[must_use]
    pub fn search(&self) -> &Entity<InputState> {
        &self.search
    }

    /// The page shows: its list is read when it wasn't yet.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if !std::mem::replace(&mut self.shown, true) {
            self.trace.record(&TraceLine::new("user-admin", "shown"));
            self.read_from_start(Duration::ZERO, cx);
        }
    }

    /// Reads the list again from its first page, after `pause`.
    fn read_from_start(&mut self, pause: Duration, cx: &mut Context<Self>) {
        self.read_page(None, pause, cx);
    }

    /// "Show more": the page after those listed.
    pub fn read_more(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.next.clone() {
            self.read_page(Some(next), Duration::ZERO, cx);
        }
    }

    fn read_page(&mut self, after: Option<String>, pause: Duration, cx: &mut Context<Self>) {
        self.read += 1;
        let read = self.read;
        self.loading = true;
        cx.notify();
        let admin = Arc::clone(&self.admin);
        let query = self.search.read(cx).value().trim().to_owned();
        self.reading = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(pause).await;
            let more = after.is_some();
            let page = cx
                .background_spawn(async move { admin.list(&query, after.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.read == read {
                    this.listed(page, more, cx);
                }
            });
        }));
    }

    fn listed(&mut self, page: Result<UserPage, AdminProblem>, more: bool, cx: &mut Context<Self>) {
        self.loading = false;
        match page {
            Ok(page) => {
                if more {
                    self.users.extend(page.users);
                } else {
                    self.users = page.users;
                }
                self.next = page.next;
                self.problem = None;
                self.at = self.at.min(self.users.len().saturating_sub(1));
                let ids: Vec<String> = self.users.iter().map(|user| format!("8:orgid:{}", user.id)).collect();
                self.trace
                    .record(&TraceLine::new("user-admin", "listed").people(ids.iter().map(String::as_str)));
            }
            Err(problem) => {
                self.trace
                    .record(&TraceLine::new("user-admin", "failed").kind(problem.kind()));
                self.problem = Some(problem);
            }
        }
        cx.notify();
    }

    /// Opens the dialog of `kind` about `user`.
    pub fn open(&mut self, kind: DialogKind, user: Option<AdminUser>, window: &mut Window, cx: &mut Context<Self>) {
        let parts = (Arc::clone(&self.admin), Arc::clone(&self.trace));
        let about = user.clone();
        let dialog = cx.new(|cx| UserDialog::new(parts, kind, user, window, cx));
        let events = cx.subscribe_in(
            &dialog,
            window,
            move |this: &mut Self, _, event: &UserDialogEvent, window, cx| match event {
                UserDialogEvent::Open(kind) => this.open(*kind, about.clone(), window, cx),
                UserDialogEvent::Closed | UserDialogEvent::Changed => {
                    this.dialog = None;
                    this.search.update(cx, |search, cx| search.focus(window, cx));
                    if *event == UserDialogEvent::Changed {
                        this.read_from_start(Duration::ZERO, cx);
                    }
                    cx.notify();
                }
            },
        );
        self.dialog = Some((dialog, events));
        cx.notify();
    }

    /// Opens the account at `at` of the list.
    pub fn open_row(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(user) = self.users.get(at).cloned() {
            self.at = at;
            self.open(DialogKind::Edit, Some(user), window, cx);
        }
    }

    fn list_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(to) = keyboard::list_step(event, self.users.len(), Some(self.at)) {
            cx.stop_propagation();
            self.at = to;
            cx.notify();
        } else if matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.open_row(self.at, window, cx);
        }
    }

    fn badge(words: &'static str, color: u32) -> gpui::Div {
        div()
            .px_2()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(rgb(color))
            .text_xs()
            .text_color(rgb(color))
            .child(words)
    }

    fn row(&self, at: usize, user: &AdminUser, lit: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let name: SharedString = if user.display_name.is_empty() {
            user.upn.clone().into()
        } else {
            user.display_name.clone().into()
        };
        let own = self.me.ends_with(&user.id);
        let selector = format!("user-row-{}", user.upn);
        div()
            .debug_selector(move || selector.clone())
            .id(("user-row", at))
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .border_1()
            .border_color(rgb(palette.surface))
            .rounded_md()
            .cursor_pointer()
            .hover(move |row| row.bg(rgb(palette.hover)))
            .map(|row| keyboard::lit(row, lit, palette))
            .child(crate::people::avatar(Some(&name), None, Size::Medium))
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
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(user.upn.clone()),
                    ),
            )
            .when(own, |row| row.child(Self::badge("You", palette.muted_foreground)))
            .when(user.admin, |row| row.child(Self::badge("Administrator", palette.brand)))
            .when(user.must_change_password, |row| {
                row.child(Self::badge("New password due", palette.muted_foreground))
            })
            .when(!user.enabled, |row| {
                row.child(Self::badge("Can't sign in", theme::DANGER))
            })
            .child(
                Glyph::CHEVRON_RIGHT
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.open_row(at, window, cx)))
            .into_any_element()
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .debug_selector(|| "users-search".to_owned())
                    .flex_1()
                    .child(Input::new(&self.search).prefix(Glyph::SEARCH.icon().size(px(14.0)))),
            )
            .child(selected_by(
                "users-add",
                Button::new("users-add")
                    .primary()
                    .icon(Glyph::USER_PLUS.icon())
                    .label("Add user")
                    .on_click(cx.listener(|this, _, window, cx| this.open(DialogKind::Add, None, window, cx))),
            ))
    }

    /// What stands in the list's place while it has no rows.
    fn no_rows(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let note = |words: SharedString| {
            div()
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(words)
        };
        if let Some(problem) = &self.problem {
            return note(problem.words())
                .debug_selector(|| "users-problem".to_owned())
                .child(selected_by(
                    "users-retry",
                    Button::new("users-retry")
                        .outline()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.read_from_start(Duration::ZERO, cx))),
                ))
                .into_any_element();
        }
        if self.loading {
            return note("Loading users…".into())
                .debug_selector(|| "users-loading".to_owned())
                .child(Spinner::new().with_size(Size::Medium))
                .into_any_element();
        }
        note("No users match your search.".into())
            .debug_selector(|| "users-none".to_owned())
            .into_any_element()
    }

    fn list(&self, palette: Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        if self.users.is_empty() || self.problem.is_some() {
            return self.no_rows(palette, cx);
        }
        let focused = self.list_focus.is_focused(window);
        let rows: Vec<AnyElement> = self
            .users
            .iter()
            .enumerate()
            .map(|(at, user)| self.row(at, user, focused && at == self.at, palette, cx))
            .collect();
        div()
            .debug_selector(|| "users-list".to_owned())
            .id("users-list")
            .track_focus(&self.list_focus.clone().tab_stop(true))
            .on_key_down(cx.listener(Self::list_key))
            .p_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .children(rows)
            .into_any_element()
    }
}

impl Render for ManageUsers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.show(cx);
        div()
            .debug_selector(|| "manage-users".to_owned())
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Add the people who use this server, and choose who can manage it."),
            )
            .child(self.toolbar(cx))
            .child(self.list(palette, window, cx))
            .when(self.next.is_some() && self.problem.is_none(), |page| {
                page.child(
                    div().flex().justify_center().child(selected_by(
                        "users-more",
                        Button::new("users-more")
                            .outline()
                            .label("Show more")
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.read_more(cx))),
                    )),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{AdminProblem, looks_like_upn};

    #[test]
    fn a_user_name_is_a_name_at_a_domain() {
        assert!(looks_like_upn("someone@example.test"));
        for text in ["", "someone", "@example.test", "some one@example.test", "a@b@c.test"] {
            assert!(!looks_like_upn(text), "{text:?}");
        }
    }

    #[test]
    fn every_problem_has_words_and_a_kind() {
        let problems = [
            AdminProblem::LastAdmin,
            AdminProblem::UpnTaken,
            AdminProblem::InvalidUpn,
            AdminProblem::InvalidName,
            AdminProblem::WeakPassword,
            AdminProblem::NotFound,
            AdminProblem::NotAdmin,
            AdminProblem::SignedOut,
            AdminProblem::Wait,
            AdminProblem::Unreachable,
            AdminProblem::Other(String::new()),
            AdminProblem::Other("The server said so.".to_owned()),
        ];
        let mut kinds = std::collections::BTreeSet::new();
        for problem in &problems {
            assert!(!problem.words().is_empty(), "{problem:?}");
            kinds.insert(problem.kind());
        }
        assert_eq!(kinds.len(), problems.len() - 1);
        assert_eq!(problems[11].words(), "The server said so.");
    }
}
