//! The window's root view: the [`Shell`] (the rail, the sidebar, the conversation) once it is given one, the
//! account's status alone before that; signed out, only that and what to do. It traces the window's opening, each
//! status it shows (connecting, connected, reconnecting, offline, signed out) and each catch-up that ended
//! (`status caught-up`: a live test waits for the first one before it reads what a chat shows).

use std::sync::Arc;

use gpui::{
    App, Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
    div, rgb,
};

use crate::accounts::Accounts;
use crate::models::{Account, Engine, Link, Models, Status};
use crate::shell::Shell;
use crate::theme;
use crate::trace::{TraceLine, ViewTrace};

/// The window's background in the light theme (the sandbox has no settings portal, so its windows stay light),
/// 0xRRGGBB.
pub const BACKGROUND: u32 = theme::LIGHT_PALETTE.background;

/// What the status shows, by name: the trace's `change` for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    /// The push channel isn't up yet since the start.
    Connecting,
    Connected,
    /// The push channel is down and reconnects by itself.
    Reconnecting,
    /// The push channel or the engine stopped for good: nothing new comes.
    Offline,
    SignedOut,
}

impl Shown {
    /// What `status` shows: being signed out first, then a stopped engine, then the push channel.
    #[must_use]
    pub fn of(status: &Status) -> Self {
        match (&status.account, status.engine, status.link) {
            (Account::SignedOut { .. }, _, _) => Self::SignedOut,
            (_, Engine::Stopped, _) | (_, _, Link::Stopped) => Self::Offline,
            (_, _, Link::Starting) => Self::Connecting,
            (_, _, Link::Connected) => Self::Connected,
            (_, _, Link::Down) => Self::Reconnecting,
        }
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Offline => "offline",
            Self::SignedOut => "signed-out",
        }
    }
}

/// The status's text.
#[must_use]
pub fn status_text(status: &Status) -> SharedString {
    match (&status.account, Shown::of(status)) {
        (Account::SignedOut { hint }, _) => format!("Signed out: {hint}").into(),
        (_, Shown::Connecting) => "Connecting…".into(),
        (_, Shown::Connected) => "Connected".into(),
        (_, Shown::Reconnecting) => "Reconnecting…".into(),
        (_, Shown::Offline | Shown::SignedOut) => "Offline".into(),
    }
}

/// The root view: observes the models, traces what it shows.
pub struct Root {
    /// The models whose status the view follows (the first account's), and its observation of them.
    followed: (Models, Subscription),
    trace: Arc<ViewTrace>,
    shown: Shown,
    /// The catch-ups the status counted when it was last traced.
    catch_ups: u64,
    /// The accounts' shells, the first account's first ([`Root::with_shell`], [`Root::add_shell`]).
    shells: Vec<Entity<Shell>>,
    /// The accounts, which say which shell is shown ([`Root::with_accounts`]).
    accounts: Option<(Entity<Accounts>, Subscription)>,
    /// The account shown when the accounts last changed: a switch hands its shell the keyboard.
    shown_account: Option<String>,
    /// The theme following the window's appearance, while the window is open ([`Root::following`]).
    appearance: Option<Subscription>,
}

impl Root {
    /// The root view over `models`; traces its opening.
    pub fn new(models: Models, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = Self::follow(&models, cx);
        let shown = Shown::of(models.status.read(cx));
        let catch_ups = models.status.read(cx).catch_ups;
        trace.record(&TraceLine::new("window", "opened").kind(shown.name()));
        Self {
            followed: (models, observing),
            trace,
            shown,
            catch_ups,
            shells: Vec::new(),
            accounts: None,
            shown_account: None,
            appearance: None,
        }
    }

    /// Follows `models`' status: traced as it changes, the view drawn again.
    fn follow(models: &Models, cx: &mut Context<Self>) -> Subscription {
        cx.observe(&models.status, |this, status, cx| {
            let status = status.read(cx);
            let shown = Shown::of(status);
            if shown != this.shown {
                this.shown = shown;
                this.trace.record(&TraceLine::new("status", shown.name()));
            }
            if status.catch_ups != this.catch_ups {
                this.catch_ups = status.catch_ups;
                this.trace.record(&TraceLine::new("status", "caught-up"));
            }
            cx.notify();
        })
    }

    /// Shows `shell` in the window: the first account's.
    #[must_use]
    pub fn with_shell(mut self, shell: Entity<Shell>) -> Self {
        self.shells.insert(0, shell);
        self
    }

    /// Shows the shell of the account `accounts` show, switched as they are; the shell switched to takes the keyboard.
    #[must_use]
    pub fn with_accounts(mut self, accounts: Entity<Accounts>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe_in(&accounts, window, |this: &mut Self, accounts, window, cx| {
            let shown = accounts.read(cx).active_key().map(str::to_owned);
            if shown != this.shown_account {
                this.shown_account.clone_from(&shown);
                if let Some(shell) = this.shell_in(cx).cloned() {
                    shell.update(cx, |shell, cx| shell.take_focus(window, cx));
                }
            }
            cx.notify();
        });
        self.shown_account = accounts.read(cx).active_key().map(str::to_owned);
        self.accounts = Some((accounts, observing));
        self
    }

    /// Adds another account's shell, shown when the accounts show it.
    pub fn add_shell(&mut self, shell: Entity<Shell>, cx: &mut Context<Self>) {
        self.shells.push(shell);
        cx.notify();
    }

    /// Takes account `key`'s shell out.
    /// Takes account `key`'s shell out. When it was the first account's, the status the view follows (and traces) is
    /// the next first account's from then on.
    pub fn remove_shell(&mut self, key: &str, cx: &mut Context<Self>) {
        let was_first = self.shells.first().is_some_and(|shell| shell.read(cx).account() == key);
        self.shells.retain(|shell| shell.read(cx).account() != key);
        if was_first && let Some(first) = self.shells.first() {
            let models = first.read(cx).models().clone();
            let observing = Self::follow(&models, cx);
            self.shown = Shown::of(models.status.read(cx));
            self.catch_ups = models.status.read(cx).catch_ups;
            self.followed = (models, observing);
        }
        cx.notify();
    }

    /// Keeps `appearance` (the theme following the window and the settings' choice: [`theme::follow`] joined with
    /// [`theme::follow_settings`]) for as long as the view lives.
    #[must_use]
    pub fn following(mut self, appearance: Subscription) -> Self {
        self.appearance = Some(appearance);
        self
    }

    /// Whether the theme follows this view's window ([`crate::window::open`] makes it so).
    #[must_use]
    pub fn follows_appearance(&self) -> bool {
        self.appearance.is_some()
    }

    /// What the status shows now.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shown
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.followed.0
    }

    /// The shell the window shows (the account the accounts show, else the first), if it was given one.
    #[must_use]
    pub fn shell_in(&self, cx: &App) -> Option<&Entity<Shell>> {
        let active = self
            .accounts
            .as_ref()
            .and_then(|(accounts, _)| accounts.read(cx).active_key().map(str::to_owned));
        active
            .and_then(|key| self.shells.iter().find(|shell| shell.read(cx).account() == key))
            .or_else(|| self.shells.first())
    }

    /// The first account's shell, if it was given one; [`Root::shell_in`] is the one shown.
    #[must_use]
    pub fn shell(&self) -> Option<&Entity<Shell>> {
        self.shells.first()
    }

    /// Every account's shell.
    #[must_use]
    pub fn shells(&self) -> &[Entity<Shell>] {
        &self.shells
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::current(cx);
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground));
        // an app of accounts: the shown one's shell, which says itself when it is signed out (its rail switches away)
        if self.accounts.is_some() {
            return match self.shell_in(cx) {
                Some(shell) => root.child(shell.clone()),
                None => root,
            };
        }
        match (self.shells.first(), self.shown) {
            (Some(shell), shown) if shown != Shown::SignedOut => root.child(shell.clone()),
            _ => root
                .justify_center()
                .items_center()
                .child(status_text(self.followed.0.status.read(cx))),
        }
    }
}
