//! The app on the desktop (M2 Task 14): its main window, its notifications ([`Notifier`]) and its tray item
//! ([`Tray`]), held together.
//!
//! - The window opens over the shell's parts ([`Desktop::open_window`]); closed, it can be opened again from the tray
//!   or a notification, over the same models ([`Desktop::show`]).
//! - **Closing the window keeps the app in the tray** while its tray item is up (a watcher took it); without one,
//!   closing the window quits ([`DesktopEvent::Quit`], which the app acts on: it shuts the engine down, then quits).
//!   The item follows the watcher ([`Hosted`]): a watcher that leaves (a panel restarting, or gone for good) or the
//!   bus lost takes the item down, and a new watcher brings it up again. Taken down while the window is closed, the
//!   window opens again: the app is never left running with no window and no item to bring it back.
//! - A notification activated opens its chat in the window, opened again if it was closed; the tray's "Open" (or a
//!   click on the item) shows the window, its "Quit" quits.
//! - The notifier is told the chat shown in a focused window, which never notifies.
//! - [`Desktop::connect`] connects to the bus the app is given ([`crate::bus`]) and puts the notifications and the
//!   tray item on it; without one, neither is there and nothing else is tried.
//!
//! What it does is traced: `window opened` (the root view's), `window closed`, `tray open`/`quit`, `tray
//! registered`/`gone` (the item up or down), and the notifier's and the tray's own lines.

use std::sync::Arc;

use futures_util::StreamExt as _;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, SharedString, Subscription, Task, WeakEntity,
    WindowHandle, WindowId,
};
use tokio::sync::mpsc;
use zbus::Connection;

use crate::bus::{self, BusError};
use crate::notify::{BusNotifications, Notifier, NotifierEvent};
use crate::shell::{Main, ShellParts};
use crate::trace::TraceLine;
use crate::tray::{BusTray, Hosted, Tray, TrayEvent};
use crate::view::Root;
use crate::window::{self, KitRoot};

/// What the desktop is made of.
#[derive(Clone)]
pub struct DesktopParts {
    /// The window's Wayland app id, and the tray item's id.
    pub app_id: &'static str,
    /// The app's name on the notifications and the tray item.
    pub app_name: SharedString,
    /// The window's title.
    pub title: SharedString,
    pub shell: ShellParts,
}

/// What the desktop asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Quit: the tray's "Quit", or the window closed without a tray item to stay in.
    Quit,
}

/// The app on the desktop (see the module docs). A GPUI entity.
pub struct Desktop {
    parts: DesktopParts,
    window: Option<WindowHandle<KitRoot>>,
    notifier: Entity<Notifier>,
    tray: Entity<Tray>,
    /// The tray item is up: closing the window keeps the app.
    in_tray: bool,
    /// The connection to the bus the app is given, kept for the notifications and the tray item on it.
    bus: Option<Connection>,
    _subscriptions: [Subscription; 2],
}

impl EventEmitter<DesktopEvent> for Desktop {}

/// The chat shown in `window`, if the window has the focus.
fn chat_in_focus(window: WindowHandle<KitRoot>, cx: &mut App) -> Option<String> {
    window
        .update(cx, |kit, window, cx| {
            if !window.is_window_active() {
                return None;
            }
            let root = kit.view().clone().downcast::<Root>().ok()?;
            let shell = root.read(cx).shell()?.clone();
            match shell.read(cx).main() {
                Main::Chat(id) | Main::Channel(id) => Some(id.clone()),
                Main::Nothing | Main::Settings => None,
            }
        })
        .ok()
        .flatten()
}

impl Desktop {
    /// The desktop over `parts`: its notifier and tray, no window yet ([`Desktop::open_window`]) and no bus
    /// ([`Desktop::connect`]).
    pub fn new(parts: DesktopParts, cx: &mut Context<Self>) -> Self {
        let shell = parts.shell.clone();
        let notifier = cx.new(|cx| {
            Notifier::new(
                Arc::clone(&shell.core),
                &shell.models,
                shell.settings.clone(),
                Arc::clone(&shell.trace),
                cx,
            )
        });
        let desktop = cx.entity().downgrade();
        notifier.update(cx, |notifier, _| {
            notifier.set_viewing(Box::new(move |cx| {
                let window = desktop.upgrade()?.read(cx).window?;
                chat_in_focus(window, cx)
            }));
        });
        let opening = cx.subscribe(&notifier, |this: &mut Self, _, event, cx| match event {
            NotifierEvent::Open(id) => this.open_chat(id.clone(), cx),
        });
        let tray = cx.new(|cx| {
            Tray::new(
                Arc::clone(&shell.core),
                shell.models.clone(),
                Arc::clone(&shell.trace),
                cx,
            )
        });
        let desktop = cx.entity().downgrade();
        let closing = cx.on_window_closed(move |cx, id| {
            // a desktop that is gone has no window to watch
            let _ = desktop.update(cx, |this, cx| this.closed(id, cx));
        });
        Self {
            parts,
            window: None,
            notifier,
            tray,
            in_tray: false,
            bus: None,
            _subscriptions: [opening, closing],
        }
    }

    #[must_use]
    pub fn notifier(&self) -> &Entity<Notifier> {
        &self.notifier
    }

    #[must_use]
    pub fn tray(&self) -> &Entity<Tray> {
        &self.tray
    }

    /// The window, while it is open.
    #[must_use]
    pub fn window(&self) -> Option<WindowHandle<KitRoot>> {
        self.window
    }

    /// Whether the tray item is up, so closing the window keeps the app.
    #[must_use]
    pub fn in_tray(&self) -> bool {
        self.in_tray
    }

    /// The tray item is up (a watcher has it), or down again (its watcher left, or the bus was lost): closing the
    /// window keeps the app, or quits it. Down while the window is closed, the window opens again.
    pub fn set_in_tray(&mut self, in_tray: bool, cx: &mut Context<Self>) {
        if self.in_tray == in_tray {
            return;
        }
        self.in_tray = in_tray;
        let change = if in_tray { "registered" } else { "gone" };
        self.parts.shell.trace.record(&TraceLine::new("tray", change));
        if !in_tray && self.window.is_none() {
            self.show(cx);
        }
    }

    /// Opens the main window ([`window::open`]); the one open already, if it is.
    ///
    /// # Errors
    /// The platform can't open a window.
    pub fn open_window(&mut self, cx: &mut Context<Self>) -> gpui::Result<WindowHandle<KitRoot>> {
        if let Some(window) = self.window {
            return Ok(window);
        }
        let DesktopParts {
            app_id, title, shell, ..
        } = self.parts.clone();
        let window = window::open(cx, app_id, title, shell)?;
        self.window = Some(window);
        Ok(window)
    }

    /// Shows the window: brought to the front, or opened again if it was closed. A window that can't be opened
    /// leaves the app as it is (still in the tray).
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = self.window {
            // a window closing as it is asked for is opened again by the next ask
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let _ = self.open_window(cx);
    }

    /// Opens chat `id` in the window, shown first ([`Desktop::show`]).
    pub fn open_chat(&mut self, id: String, cx: &mut Context<Self>) {
        self.show(cx);
        let Some(window) = self.window else { return };
        let _ = window.update(cx, |kit, window, cx| {
            let Ok(root) = kit.view().clone().downcast::<Root>() else {
                return;
            };
            let Some(shell) = root.read(cx).shell().cloned() else {
                return;
            };
            shell.update(cx, |shell, cx| shell.open_chat(id, window, cx));
        });
    }

    fn closed(&mut self, id: WindowId, cx: &mut Context<Self>) {
        if self.window.is_none_or(|window| window.window_id() != id) {
            return;
        }
        self.window = None;
        self.parts.shell.trace.record(&TraceLine::new("window", "closed"));
        if !self.in_tray {
            cx.emit(DesktopEvent::Quit);
        }
    }

    /// What the tray item asked: the window shown, or the app quit.
    pub fn tray_asked(&mut self, event: TrayEvent, cx: &mut Context<Self>) {
        match event {
            TrayEvent::Open => {
                self.parts.shell.trace.record(&TraceLine::new("tray", "open"));
                self.show(cx);
            }
            TrayEvent::Quit => {
                self.parts.shell.trace.record(&TraceLine::new("tray", "quit"));
                cx.emit(DesktopEvent::Quit);
            }
        }
    }

    /// Connects to the bus at `address` (the one the app is given, [`bus::address_from_env`]) and puts the
    /// notifications and the tray item on it. The task ends once both are up or have failed, with what failed (the
    /// app reports it); without an address nothing is connected to.
    pub fn connect(&mut self, address: Result<String, BusError>, cx: &mut Context<Self>) -> Task<Vec<String>> {
        let app_id = self.parts.app_id;
        let app_name = self.parts.app_name.to_string();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let connection = match bus::connect_given(address).await {
                Ok(connection) => connection,
                Err(error) => return vec![error.to_string()],
            };
            let mut problems = Vec::new();
            if let Err(error) = notifications(&this, &connection, &app_name, cx).await {
                problems.push(format!("no notifications: {error}"));
            }
            if let Err(error) = tray(&this, &connection, app_id, &app_name, cx).await {
                problems.push(format!("no tray item: {error}"));
            }
            let _ = this.update(cx, |this, _| this.bus = Some(connection));
            problems
        })
    }
}

/// The notifications on `connection`: the server handed to the notifier, its activations to the notifier.
async fn notifications(
    this: &WeakEntity<Desktop>,
    connection: &Connection,
    app_name: &str,
    cx: &mut AsyncApp,
) -> zbus::Result<()> {
    let (server, mut activations) = BusNotifications::start(connection, app_name).await?;
    let Ok(notifier) = this.update(cx, |this, cx| {
        this.notifier
            .update(cx, |notifier, _| notifier.set_server(Arc::new(server)));
        this.notifier.downgrade()
    }) else {
        return Ok(());
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(id) = activations.next().await {
            if notifier.update(cx, |notifier, cx| notifier.activated(id, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    Ok(())
}

/// The tray item on `connection`: shown the unread total, its asks handed to the desktop.
async fn tray(
    this: &WeakEntity<Desktop>,
    connection: &Connection,
    app_id: &str,
    app_name: &str,
    cx: &mut AsyncApp,
) -> zbus::Result<()> {
    let (events, mut asked) = mpsc::unbounded_channel();
    let (item, mut hosted) = BusTray::start(connection, app_id, app_name, events).await?;
    let _ = this.update(cx, |this, cx| {
        this.set_in_tray(true, cx);
        this.tray.update(cx, |tray, cx| tray.set_icon(Arc::new(item), cx));
    });
    let asking = this.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(event) = asked.recv().await {
            if asking.update(cx, |this, cx| this.tray_asked(event, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
    let this = this.clone();
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(change) = hosted.next().await {
            let up = change == Hosted::Registered;
            if this.update(cx, |this, cx| this.set_in_tray(up, cx)).is_err() {
                return;
            }
        }
        // the bus is lost: the item with it
        let _ = this.update(cx, |this, cx| this.set_in_tray(false, cx));
    })
    .detach();
    Ok(())
}
