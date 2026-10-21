//! The window's root view: the [`Shell`] (the rail, the chat list, the conversation) once it is given one, the
//! account's status alone before that; signed out, only that and what to do. It traces the window's opening and each
//! status it shows (connecting, connected, reconnecting, offline, signed out).

use std::sync::Arc;

use gpui::{
    Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, rgb,
};

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
    models: Models,
    trace: Arc<ViewTrace>,
    shown: Shown,
    shell: Option<Entity<Shell>>,
    _observing: Subscription,
    /// The theme following the window's appearance, while the window is open ([`Root::following`]).
    appearance: Option<Subscription>,
}

impl Root {
    /// The root view over `models`; traces its opening.
    pub fn new(models: Models, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&models.status, |this, status, cx| {
            let shown = Shown::of(status.read(cx));
            if shown != this.shown {
                this.shown = shown;
                this.trace.record(&TraceLine::new("status", shown.name()));
            }
            cx.notify();
        });
        let shown = Shown::of(models.status.read(cx));
        trace.record(&TraceLine::new("window", "opened").kind(shown.name()));
        Self {
            models,
            trace,
            shown,
            shell: None,
            _observing: observing,
            appearance: None,
        }
    }

    /// Shows `shell` in the window.
    #[must_use]
    pub fn with_shell(mut self, shell: Entity<Shell>) -> Self {
        self.shell = Some(shell);
        self
    }

    /// Keeps `appearance` (the theme following the window, [`theme::follow`]) for as long as the view lives.
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
        &self.models
    }

    /// The shell the window shows, if it was given one.
    #[must_use]
    pub fn shell(&self) -> Option<&Entity<Shell>> {
        self.shell.as_ref()
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
        match (&self.shell, self.shown) {
            (Some(shell), shown) if shown != Shown::SignedOut => root.child(shell.clone()),
            _ => root
                .justify_center()
                .items_center()
                .child(status_text(self.models.status.read(cx))),
        }
    }
}
