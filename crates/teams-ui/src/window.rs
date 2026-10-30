//! The app's one main window.

use gpui::{
    App, AppContext as _, Bounds, Entity, SharedString, Subscription, TitlebarOptions, WindowBounds, WindowHandle,
    WindowOptions, px, size,
};

use crate::accounts::Accounts;
use crate::own_presence::OwnPresence;
use crate::shell::{Shell, ShellParts};
use crate::view::Root;
use crate::{clock, picture_viewer, shell, theme};

/// The lab app's Wayland app id.
pub const LAB_APP_ID: &str = "ngteams-lab";

/// The personal build's Wayland app id (`crates/ngteams`), also the name of its desktop entry
/// (`packaging/com.nickalie.ngteams.desktop`, its `StartupWMClass`).
pub const PERSONAL_APP_ID: &str = "com.nickalie.ngteams";

/// The window's size when it opens.
const DEFAULT_SIZE: (f32, f32) = (1100.0, 720.0);

/// The window's root: the UI kit's, holding our [`Root`] view. Each time it draws it applies the kit's theme to the
/// window (the font size as the window's rem size, the font family, the background and text colours) and hosts the
/// kit's window-level layers (dialogs, sheets, notifications, tooltips). A window without it keeps GPUI's 16 px rem
/// whatever the theme says, and the kit's dialogs have nowhere to open.
pub type KitRoot = gpui_component::Root;

/// Sets the app up for the views, once per app: the UI kit with our theme ([`theme::init`]), the shell's keys
/// ([`shell::init`]) and the picture viewer's ([`picture_viewer::init`]).
pub fn init(cx: &mut App) {
    theme::init(cx);
    shell::init(cx);
    picture_viewer::init(cx);
}

/// Opens the main window, titled `title`, with app id `app_id`: the kit's root ([`KitRoot`]) holding our root view,
/// which shows the [`Shell`] over `parts`; sets the app up first ([`init`]) and keeps the theme following the
/// window's appearance and the settings' choice of how the app looks ([`theme::follow_settings`]) and of the clock times
/// are shown on ([`clock::follow_settings`]).
///
/// # Errors
/// The platform can't open a window.
pub fn open(cx: &mut App, app_id: &str, title: SharedString, parts: ShellParts) -> gpui::Result<WindowHandle<KitRoot>> {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitlebarOptions::default()
        }),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    init(cx);
    picture_viewer::set_app_id(app_id, cx);
    cx.open_window(options, |window, cx| {
        let following = Subscription::join(
            Subscription::join(theme::follow_settings(&parts.settings, cx), theme::follow(window, cx)),
            clock::follow_settings(&parts.settings, cx),
        );
        let models = parts.models.clone();
        let trace = std::sync::Arc::clone(&parts.trace);
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        let view = cx.new(|cx| {
            Root::new(models, trace, cx)
                .with_shell(shell)
                .following(following)
                .tracing_activation(window, cx)
        });
        cx.new(|cx| KitRoot::new(view, window, cx))
    })
}

/// Opens the main window of an app of several accounts: [`open`]'s, with a shell for the first account (`first`) and
/// for each of the others (`rest`), each with its own presence, the one `accounts` shows in front, every shell's rail
/// showing the accounts.
///
/// # Errors
/// The platform can't open a window.
pub fn open_accounts(
    cx: &mut App,
    app_id: &str,
    title: SharedString,
    first: (ShellParts, Entity<OwnPresence>),
    rest: Vec<(ShellParts, Entity<OwnPresence>)>,
    accounts: &Entity<Accounts>,
) -> gpui::Result<WindowHandle<KitRoot>> {
    let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitlebarOptions::default()
        }),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    };
    init(cx);
    picture_viewer::set_app_id(app_id, cx);
    cx.open_window(options, |window, cx| {
        // one settings model for the app: the first account's parts hold it
        let following = Subscription::join(
            Subscription::join(theme::follow_settings(&first.0.settings, cx), theme::follow(window, cx)),
            clock::follow_settings(&first.0.settings, cx),
        );
        let mut shell_of = |(parts, own): (ShellParts, Entity<OwnPresence>), cx: &mut App| {
            let shell = cx.new(|cx| Shell::new(parts, window, cx));
            shell.update(cx, |shell, cx| {
                shell.set_own_presence(own, cx);
                shell.set_accounts(accounts.clone(), cx);
            });
            shell
        };
        let (models, trace) = (first.0.models.clone(), std::sync::Arc::clone(&first.0.trace));
        let first = shell_of(first, cx);
        let rest: Vec<Entity<Shell>> = rest.into_iter().map(|parts| shell_of(parts, cx)).collect();
        // each shell took the keyboard as it was made: the one shown takes it back
        first.update(cx, |shell, cx| shell.take_focus(window, cx));
        let view = cx.new(|cx| {
            let mut root = Root::new(models, trace, cx)
                .with_shell(first)
                .following(following)
                .tracing_activation(window, cx)
                .with_accounts(accounts.clone(), window, cx);
            for shell in rest {
                root.add_shell(shell, cx);
            }
            root
        });
        cx.new(|cx| KitRoot::new(view, window, cx))
    })
}

/// Our root view in a window [`open`] opened; `None` once the window is gone.
#[must_use]
pub fn content(window: WindowHandle<KitRoot>, cx: &App) -> Option<Entity<Root>> {
    window.read(cx).ok()?.view().clone().downcast::<Root>().ok()
}
