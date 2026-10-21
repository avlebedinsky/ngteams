//! The window's root view. For now an empty window with the account's status in it (connecting, connected, offline,
//! signed out), in the theme's colours; the shell, the chat list and the chat view come in Tasks 8 and 9.

use std::sync::Arc;

use gpui::{
    Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, rgb,
};

use crate::models::{Account, Engine, Link, Models, Status};
use crate::theme;
use crate::trace::{TraceLine, ViewTrace};

/// The window's background in the light theme (the sandbox has no settings portal, so its windows stay light),
/// 0xRRGGBB.
pub const BACKGROUND: u32 = theme::LIGHT_PALETTE.background;

/// What the status line shows, by name: the trace's `change` for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Connecting,
    Connected,
    Offline,
    Stopped,
    SignedOut,
}

impl Shown {
    /// What `status` shows: being signed out first, then a stopped engine, then the push channel.
    #[must_use]
    pub fn of(status: &Status) -> Self {
        match (&status.account, status.engine, status.link) {
            (Account::SignedOut { .. }, _, _) => Self::SignedOut,
            (_, Engine::Stopped, _) | (_, _, Link::Stopped) => Self::Stopped,
            (_, _, Link::Starting) => Self::Connecting,
            (_, _, Link::Connected) => Self::Connected,
            (_, _, Link::Down) => Self::Offline,
        }
    }

    /// The trace's name for it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Offline => "offline",
            Self::Stopped => "stopped",
            Self::SignedOut => "signed-out",
        }
    }
}

/// The status line's text.
#[must_use]
pub fn status_text(status: &Status) -> SharedString {
    match (&status.account, Shown::of(status)) {
        (Account::SignedOut { hint }, _) => format!("Signed out: {hint}").into(),
        (_, Shown::Connecting) => "Connecting…".into(),
        (_, Shown::Connected) => "Connected".into(),
        (_, Shown::Offline) => "Offline: reconnecting…".into(),
        (_, Shown::Stopped | Shown::SignedOut) => "Stopped".into(),
    }
}

/// The root view: observes the models, traces what it shows.
pub struct Root {
    models: Models,
    trace: Arc<ViewTrace>,
    shown: Shown,
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
            _observing: observing,
            appearance: None,
        }
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

    /// What the status line shows now.
    #[must_use]
    pub fn shown(&self) -> Shown {
        self.shown
    }

    #[must_use]
    pub fn models(&self) -> &Models {
        &self.models
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text = status_text(self.models.status.read(cx));
        let colors = theme::current(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground))
            .child(text)
    }
}
