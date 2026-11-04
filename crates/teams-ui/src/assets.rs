//! The asset source the app's GPUI runs with. The UI kit draws some of its icons by path (`icons/<name>.svg`, its
//! `IconName`s): a button's loading spinner, a submenu's chevron, a date field's calendar, a select's caret, a
//! checkbox's tick. GPUI's own source serves no path, so without this they are drawn blank.
//!
//! The icons are the kit's default bundle (`gpui-kit-assets`, Apache-2.0, the version `gpui-component` locks): Lucide's
//! SVGs (ISC, the Feather-derived ones MIT: `assets/icons/LICENSE-LUCIDE`) and a few the kit drew from Lucide's. Two
//! kinds are served differently:
//! - the spinner (`icons/loader.svg`, Lucide's spoked loader) is Lucide's ring, as Teams' spinner and the app's own
//!   ([`Glyph::LOADING`](crate::icons::Glyph::LOADING)) are;
//! - the kit's window controls (its title bar's, which the app doesn't use) are drawings of the kit's own whose origin
//!   isn't recorded, so Lucide's close, minus, square and copy stand in for them.
//!
//! The kit's GitHub mark and window resize corner, which none of its components draws, aren't served.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

/// The app's assets: the kit's icons, by the paths it asks for. Given to GPUI with `Application::with_assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

/// Paths served as another of the bundle's icons: the kit's spinner as Teams' ring, its window controls as Lucide's.
const SWAPPED: [(&str, &str); 5] = [
    ("icons/loader.svg", "icons/loader-circle.svg"),
    ("icons/window-close.svg", "icons/close.svg"),
    ("icons/window-minimize.svg", "icons/minus.svg"),
    ("icons/window-maximize.svg", "icons/square.svg"),
    ("icons/window-restore.svg", "icons/copy.svg"),
];

/// The bundle's icons that aren't served: no kit component draws them, and they aren't Lucide's.
pub const WITHHELD: [&str; 2] = ["icons/github.svg", "icons/resize-corner.svg"];

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if WITHHELD.contains(&path) {
            return Ok(None);
        }
        let served = SWAPPED
            .iter()
            .find_map(|(asked, served)| (*asked == path).then_some(*served))
            .unwrap_or(path);
        gpui_kit_assets::Assets.load(served)
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut listed = gpui_kit_assets::Assets.list(path)?;
        listed.retain(|name| !WITHHELD.contains(&name.as_ref()));
        Ok(listed)
    }
}
