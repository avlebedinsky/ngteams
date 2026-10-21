//! The app's theme: the UI kit's (`gpui-component`), in Teams-like colours and compact sizes, light and dark, following
//! the window's appearance and switching live when it changes.
//!
//! [`init`] sets the kit up once per app with our two themes; [`follow`] keeps the kit's mode in step with a window's
//! appearance (the platform tells a window when the desktop's colour scheme changes: on Linux through the settings
//! portal on the bus the app is given, which in the UI sandbox has none, so the window stays light). Nothing here
//! reads or watches a theme file: the kit's theme directory watcher (`ThemeRegistry::watch_dir`) is never called.

use std::rc::Rc;

use gpui::{App, Global, Subscription, Window, WindowAppearance};
use gpui_component::theme::{Theme, ThemeConfig, ThemeConfigColors, ThemeMode};

/// The light theme's name.
pub const LIGHT: &str = "Teams Light";
/// The dark theme's name.
pub const DARK: &str = "Teams Dark";

/// The base font size, px: compact, as Teams' own (the kit's default is 16). It reaches a window as its rem size
/// only through the kit's root ([`crate::window::KitRoot`]), which sets it each time the window draws; the other
/// values here (radius, colours, shadow) the kit's widgets read from the theme as they draw.
pub const FONT_SIZE: f32 = 14.0;
/// The corner radius of general elements, px.
pub const RADIUS: usize = 4;
/// The corner radius of large elements (dialogs, popovers), px.
pub const RADIUS_LG: usize = 6;

/// One mode's colours, 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The app's background.
    pub background: u32,
    /// Text.
    pub foreground: u32,
    /// Secondary text (times, previews).
    pub muted_foreground: u32,
    /// Raised surfaces: the list's rows, inputs, popovers.
    pub surface: u32,
    /// Borders and separators.
    pub border: u32,
    /// The brand colour: the selected row's mark, buttons, focus.
    pub brand: u32,
    /// The brand colour under the pointer.
    pub brand_hover: u32,
    /// The brand colour pressed.
    pub brand_active: u32,
    /// Text on the brand colour.
    pub on_brand: u32,
    /// A row under the pointer.
    pub hover: u32,
    /// The selected row.
    pub selected: u32,
    /// Selected text.
    pub selection: u32,
    /// The left rail.
    pub rail: u32,
}

/// The light palette.
pub const LIGHT_PALETTE: Palette = Palette {
    background: 0x00f5_f5f5,
    foreground: 0x0024_2424,
    muted_foreground: 0x0061_6161,
    surface: 0x00ff_ffff,
    border: 0x00e0_e0e0,
    brand: 0x005b_5fc7,
    brand_hover: 0x004f_52b2,
    brand_active: 0x0044_4791,
    on_brand: 0x00ff_ffff,
    hover: 0x00f0_f0f0,
    selected: 0x00e8_ebfa,
    selection: 0x00c5_cbfa,
    rail: 0x00eb_ebeb,
};

/// The dark palette.
pub const DARK_PALETTE: Palette = Palette {
    background: 0x001f_1f1f,
    foreground: 0x00ff_ffff,
    muted_foreground: 0x00ad_adad,
    surface: 0x0029_2929,
    border: 0x003d_3d3d,
    brand: 0x007f_85f5,
    brand_hover: 0x0092_99f7,
    brand_active: 0x00a3_a8f8,
    on_brand: 0x0014_1414,
    hover: 0x0033_3333,
    selected: 0x002f_2f4a,
    selection: 0x0044_4791,
    rail: 0x0014_1414,
};

/// `0xRRGGBB` as the kit's `#rrggbb`.
fn hex(color: u32) -> gpui::SharedString {
    format!("#{color:06x}").into()
}

/// The palette of `mode`.
#[must_use]
pub fn palette(mode: ThemeMode) -> Palette {
    if mode.is_dark() { DARK_PALETTE } else { LIGHT_PALETTE }
}

/// The kit's colours from a palette; anything not named keeps the kit's own default for the mode.
fn colors(palette: Palette) -> ThemeConfigColors {
    // assigned one by one: the kit keeps some of its fields private, so there is no struct literal
    let mut colors = ThemeConfigColors::default();
    colors.background = Some(hex(palette.background));
    colors.foreground = Some(hex(palette.foreground));
    colors.border = Some(hex(palette.border));
    colors.muted = Some(hex(palette.hover));
    colors.muted_foreground = Some(hex(palette.muted_foreground));
    colors.accent = Some(hex(palette.hover));
    colors.accent_foreground = Some(hex(palette.foreground));
    colors.primary = Some(hex(palette.brand));
    colors.primary_hover = Some(hex(palette.brand_hover));
    colors.primary_active = Some(hex(palette.brand_active));
    colors.primary_foreground = Some(hex(palette.on_brand));
    colors.secondary = Some(hex(palette.surface));
    colors.secondary_hover = Some(hex(palette.hover));
    colors.secondary_active = Some(hex(palette.selected));
    colors.secondary_foreground = Some(hex(palette.foreground));
    colors.input = Some(hex(palette.border));
    colors.list = Some(hex(palette.surface));
    colors.list_hover = Some(hex(palette.hover));
    colors.list_active = Some(hex(palette.selected));
    colors.list_active_border = Some(hex(palette.brand));
    colors.popover = Some(hex(palette.surface));
    colors.popover_foreground = Some(hex(palette.foreground));
    colors.selection = Some(hex(palette.selection));
    colors.caret = Some(hex(palette.foreground));
    colors.ring = Some(hex(palette.brand));
    colors.sidebar = Some(hex(palette.rail));
    colors.sidebar_foreground = Some(hex(palette.foreground));
    colors.sidebar_border = Some(hex(palette.border));
    colors.title_bar = Some(hex(palette.rail));
    colors.title_bar_border = Some(hex(palette.border));
    colors
}

/// Our theme for `mode`.
#[must_use]
pub fn config(mode: ThemeMode) -> ThemeConfig {
    ThemeConfig {
        is_default: false,
        name: if mode.is_dark() { DARK } else { LIGHT }.into(),
        mode,
        font_size: Some(FONT_SIZE),
        radius: Some(RADIUS),
        radius_lg: Some(RADIUS_LG),
        shadow: Some(false),
        colors: colors(palette(mode)),
        ..ThemeConfig::default()
    }
}

/// Marks the kit as set up with our themes.
struct Installed;

impl Global for Installed {}

/// Sets the kit up for the app (its globals, key bindings and default theme), then installs our two themes, light
/// first. Before any window opens; a later call does nothing.
pub fn init(cx: &mut App) {
    if cx.has_global::<Installed>() {
        return;
    }
    cx.set_global(Installed);
    gpui_component::init(cx);
    let light = Rc::new(config(ThemeMode::Light));
    let dark = Rc::new(config(ThemeMode::Dark));
    Theme::global_mut(cx).light_theme = light;
    Theme::global_mut(cx).dark_theme = dark;
    apply(WindowAppearance::Light, cx);
}

/// Switches the kit to the mode `appearance` names and redraws every window.
pub fn apply(appearance: WindowAppearance, cx: &mut App) {
    Theme::change(appearance, None, cx);
}

/// Applies `window`'s appearance now and again each time the platform says it changed, for as long as the returned
/// subscription is kept.
pub fn follow(window: &mut Window, cx: &mut App) -> Subscription {
    apply(window.appearance(), cx);
    window.observe_window_appearance(|window, cx| apply(window.appearance(), cx))
}

/// The mode the kit is in: light until [`init`] has run.
#[must_use]
pub fn mode(cx: &App) -> ThemeMode {
    cx.try_global::<Theme>().map_or(ThemeMode::Light, |theme| theme.mode)
}

/// The palette windows are drawn with now.
#[must_use]
pub fn current(cx: &App) -> Palette {
    palette(mode(cx))
}
