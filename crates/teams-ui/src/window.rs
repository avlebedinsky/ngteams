//! The app's one main window.

use std::sync::Arc;

use gpui::{
    App, AppContext as _, Bounds, SharedString, TitlebarOptions, WindowBounds, WindowHandle, WindowOptions, px, size,
};

use crate::models::Models;
use crate::theme;
use crate::trace::ViewTrace;
use crate::view::Root;

/// The lab app's Wayland app id.
pub const LAB_APP_ID: &str = "ms-teams-lab";

/// The window's size when it opens.
const DEFAULT_SIZE: (f32, f32) = (1100.0, 720.0);

/// Opens the main window, titled `title`, with app id `app_id`, its root view over `models`; sets the UI kit up
/// first (once per app) and keeps the theme following the window's appearance.
///
/// # Errors
/// The platform can't open a window.
pub fn open(
    cx: &mut App,
    app_id: &str,
    title: SharedString,
    models: Models,
    trace: Arc<ViewTrace>,
) -> gpui::Result<WindowHandle<Root>> {
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
        cx.new(|cx| Root::new(models, trace, cx).following(following))
    })
}
