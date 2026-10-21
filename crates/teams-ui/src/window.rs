//! The app's one main window.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, Entity, SharedString, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, px,
    size,
};

use crate::models::Models;
use crate::theme;
use crate::trace::ViewTrace;
use crate::view::Root;

/// The lab app's Wayland app id.
pub const LAB_APP_ID: &str = "ms-teams-lab";

/// The window's size when it opens.
const DEFAULT_SIZE: (f32, f32) = (1100.0, 720.0);

/// The window's root: the UI kit's, holding our [`Root`] view. Each time it draws it applies the kit's theme to the
/// window (the font size as the window's rem size, the font family, the background and text colours) and hosts the
/// kit's window-level layers (dialogs, sheets, notifications, tooltips). A window without it keeps GPUI's 16 px rem
/// whatever the theme says, and the kit's dialogs have nowhere to open.
pub type KitRoot = gpui_component::Root;

/// Opens the main window, titled `title`, with app id `app_id`: the kit's root ([`KitRoot`]) holding our root view
/// over `models`; sets the UI kit up first (once per app) and keeps the theme following the window's appearance.
///
/// # Errors
/// The platform can't open a window.
pub fn open(
    cx: &mut App,
    app_id: &str,
    title: SharedString,
    models: Models,
    trace: Arc<ViewTrace>,
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
    theme::init(cx);
    cx.open_window(options, |window, cx| {
        let following = theme::follow(window, cx);
        let view = cx.new(|cx| Root::new(models, trace, cx).following(following));
        cx.new(|cx| KitRoot::new(view, window, cx))
    })
}

/// Our root view in a window [`open`] opened; `None` once the window is gone.
#[must_use]
pub fn content(window: WindowHandle<KitRoot>, cx: &App) -> Option<Entity<Root>> {
    window.read(cx).ok()?.view().clone().downcast::<Root>().ok()
}
