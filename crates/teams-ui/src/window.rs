//! The app's one main window.

use gpui::{
    App, AppContext as _, Bounds, Entity, SharedString, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, px,
    size,
};

use crate::shell::{Shell, ShellParts};
use crate::view::Root;
use crate::{shell, theme};

/// The lab app's Wayland app id.
pub const LAB_APP_ID: &str = "ms-teams-lab";

/// The window's size when it opens.
const DEFAULT_SIZE: (f32, f32) = (1100.0, 720.0);

/// The window's root: the UI kit's, holding our [`Root`] view. Each time it draws it applies the kit's theme to the
/// window (the font size as the window's rem size, the font family, the background and text colours) and hosts the
/// kit's window-level layers (dialogs, sheets, notifications, tooltips). A window without it keeps GPUI's 16 px rem
/// whatever the theme says, and the kit's dialogs have nowhere to open.
pub type KitRoot = gpui_component::Root;

/// Sets the app up for the views, once per app: the UI kit with our theme ([`theme::init`]) and the shell's keys
/// ([`shell::init`]).
pub fn init(cx: &mut App) {
    theme::init(cx);
    shell::init(cx);
}

/// Opens the main window, titled `title`, with app id `app_id`: the kit's root ([`KitRoot`]) holding our root view,
/// which shows the [`Shell`] over `parts`; sets the app up first ([`init`]) and keeps the theme following the
/// window's appearance.
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
    cx.open_window(options, |window, cx| {
        let following = theme::follow(window, cx);
        let models = parts.models.clone();
        let trace = std::sync::Arc::clone(&parts.trace);
        let shell = cx.new(|cx| Shell::new(parts, window, cx));
        let view = cx.new(|cx| Root::new(models, trace, cx).with_shell(shell).following(following));
        cx.new(|cx| KitRoot::new(view, window, cx))
    })
}

/// Our root view in a window [`open`] opened; `None` once the window is gone.
#[must_use]
pub fn content(window: WindowHandle<KitRoot>, cx: &App) -> Option<Entity<Root>> {
    window.read(cx).ok()?.view().clone().downcast::<Root>().ok()
}
