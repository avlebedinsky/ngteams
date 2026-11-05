//! The menus a recap's player opens over its picture, as Teams' Recap player draws them (dark panels above its
//! buttons, a submenu taking its menu's place under a "<" header):
//!
//! - **Captions** (its CC button): Off, or the transcript's language ([`Captions::label`]).
//! - **Playback speed** (the button that says the speed, "1x"): 2x, 1.8x, 1.5x, 1.2x, 1x and 0.8x ([`RATES`]).
//! - **Settings** (the gear): Caption settings (Size: Small, Medium or Large; Color: Standard, Standard reverse, Purple
//!   or Purple reverse; Background transparency; Revert to default settings), Quality (the picture's height played,
//!   "1080p", the only one the recording's presentation offers) and Keyboard shortcuts (the player's keys).
//!
//! A click outside the menu, or Esc, closes it.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, hsla, px,
};
use teams_media::player::RATES;

use super::captions::{CaptionColor, CaptionSize, CaptionStyle, Captions, swatch};
use super::{ON_VIDEO, VideoPlayer};
use crate::icons::Glyph;

/// A menu of the player's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Captions,
    Speed,
    Settings(Page),
}

/// A page of the settings menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Main,
    Captions,
    Size,
    Color,
    Quality,
    Keys,
}

/// The player's keys, as its Keyboard shortcuts page lists them.
pub const KEYS: [(&str, &str); 8] = [
    ("Play or pause", "Space"),
    ("Skip 10 seconds backward", "Left"),
    ("Skip 10 seconds forward", "Right"),
    ("Mute or unmute sound", "M"),
    ("Turn captions on or off", "C"),
    ("Decrease playback speed", "Z"),
    ("Increase playback speed", "X"),
    ("Close a menu or exit full screen", "Esc"),
];

/// A speed as its button and its menu write it: `1x`, `1.5x`.
#[must_use]
pub fn rate_label(rate: f64) -> String {
    let tenths = (rate * 10.0).round();
    if tenths % 10.0 == 0.0 {
        format!("{}x", tenths / 10.0)
    } else {
        format!("{:.1}x", tenths / 10.0)
    }
}

/// A menu row's height: Teams' 38 px, a little less so a menu fits a recap's player at a common window's size.
const ROW: f32 = 34.0;

/// A row's click: what it does to the player.
type Act = fn(&mut VideoPlayer, &mut Window, &mut Context<VideoPlayer>);

/// A menu's panel: `rows` on a dark, slightly see-through surface over the picture, its bottom above the controls and
/// its right edge `right` of the box's, as tall as its rows up to the box's top (then scrolled), `width` wide.
fn panel(selector: &'static str, right: f32, width: f32, rows: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    let surface = div()
        .id(selector)
        .debug_selector(move || selector.to_owned())
        .w_full()
        .max_h_full()
        .overflow_y_scroll()
        .py_1()
        .rounded_md()
        .bg(hsla(0.0, 0.0, 0.11, 0.94))
        .text_sm()
        .text_color(gpui::rgb(ON_VIDEO))
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .children(rows);
    // the space it may take: from the box's top to the controls (it takes no clicks itself)
    div()
        .absolute()
        .top(px(4.0))
        .bottom(px(46.0))
        .right(px(right))
        .w(px(width))
        .flex()
        .flex_col()
        .justify_end()
        .child(surface)
        .into_any_element()
}

/// A row of a panel: a check (a choice's row: `Some(true)` for the one chosen), its words, what it shows on the right,
/// its click.
fn row(
    selector: String,
    checked: Option<bool>,
    words: impl IntoElement,
    right: Option<AnyElement>,
    click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .h(px(ROW))
        .flex_none()
        .px_3()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.1)))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            click(event, window, cx);
        })
        .when_some(checked, |row, checked| {
            row.child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .when(checked, |mark| mark.child(Glyph::CHECK.icon().size(px(14.0)))),
            )
        })
        .child(div().flex_1().min_w_0().truncate().child(words))
        .children(right)
        .into_any_element()
}

/// A submenu's header: "<" and its title, back to the settings' main page (or the caption settings').
fn header(title: &'static str, back: AnyElement) -> AnyElement {
    div()
        .h(px(ROW))
        .flex_none()
        .px_2()
        .mb_1()
        .flex()
        .items_center()
        .gap_2()
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.15))
        .font_weight(FontWeight::SEMIBOLD)
        .child(back)
        .child(title)
        .into_any_element()
}

/// What a settings row shows on its right: its value and a ">" to its submenu.
fn value(text: impl Into<SharedString>) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(text.into())
        .child(Glyph::CHEVRON_RIGHT.icon().size(px(14.0)))
        .into_any_element()
}

/// The background transparency's switch.
fn switch(on: bool) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(if on { "On" } else { "Off" })
        .child(
            div()
                .w(px(36.0))
                .h(px(18.0))
                .rounded_full()
                .border_1()
                .border_color(gpui::rgb(ON_VIDEO))
                .when(on, |track| {
                    track.bg(gpui::rgb(TEAMS_BRAND)).border_color(gpui::rgb(TEAMS_BRAND))
                })
                .flex()
                .items_center()
                .when(on, Div::justify_end)
                .px(px(2.0))
                .child(div().size(px(12.0)).rounded_full().bg(gpui::rgb(ON_VIDEO))),
        )
        .into_any_element()
}

/// Teams' brand colour, as its player's switches draw "on".
const TEAMS_BRAND: u32 = 0x005b_5fc7;

impl VideoPlayer {
    /// The open menu's panel, above its button (`shift`: the browser's button missing moves the buttons right).
    pub(super) fn menu_panel(&self, shift: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.menu? {
            Menu::Captions => self.captions_panel(shift, cx),
            Menu::Speed => Some(self.speed_panel(shift, cx)),
            Menu::Settings(page) => Some(self.settings_panel(page, cx)),
        }
    }

    fn captions_panel(&self, shift: f32, cx: &mut Context<Self>) -> Option<AnyElement> {
        let captions: &Captions = self.captions.as_ref()?;
        let on = CaptionStyle::current(cx).on;
        let entity = cx.entity();
        let set = move |shown: bool| {
            let entity = entity.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.menu = None;
                    this.show_captions(shown, cx);
                });
            }
        };
        Some(panel(
            "video-captions-menu",
            (30.0 - shift).max(0.0),
            240.0,
            [
                row("video-captions-off".to_owned(), Some(!on), "Off", None, set(false)),
                row(
                    "video-captions-on".to_owned(),
                    Some(on),
                    captions.label(),
                    None,
                    set(true),
                ),
            ],
        ))
    }

    fn speed_panel(&self, shift: f32, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        let rows = RATES.iter().rev().map(|&rate| {
            let entity = entity.clone();
            row(
                format!("video-rate-{}", rate_label(rate)),
                Some((rate - self.rate).abs() < f64::EPSILON),
                rate_label(rate),
                None,
                move |_, _, cx| {
                    entity.update(cx, |this, cx| {
                        this.menu = None;
                        this.set_rate(rate, cx);
                    });
                },
            )
        });
        panel("video-speed-menu", (81.0 - shift).max(0.0), 90.0, rows)
    }

    fn settings_panel(&self, page: Page, cx: &mut Context<Self>) -> AnyElement {
        let style = CaptionStyle::current(cx);
        let width = if page == Page::Keys { 300.0 } else { 270.0 };
        let rows = match page {
            Page::Main => self.main_rows(cx),
            Page::Captions => Self::caption_rows(style, cx),
            Page::Size => Self::size_rows(style, cx),
            Page::Color => Self::color_rows(style, cx),
            Page::Quality => self.quality_rows(cx),
            Page::Keys => Self::key_rows(cx),
        };
        panel("video-settings-menu", 0.0, width, rows)
    }

    /// A row that opens `page` of the settings.
    fn to_page(
        selector: &str,
        words: &'static str,
        right: AnyElement,
        page: Page,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        row(
            format!("video-settings-{selector}"),
            None,
            words,
            Some(right),
            move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.menu = Some(Menu::Settings(page));
                    cx.notify();
                });
            },
        )
    }

    /// A row that does `act` to the player.
    fn acting(
        selector: String,
        checked: Option<bool>,
        words: impl IntoElement,
        right: Option<AnyElement>,
        act: Act,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity();
        row(selector, checked, words, right, move |_, window, cx| {
            entity.update(cx, |this, cx| act(this, window, cx));
        })
    }

    /// The "<" of a submenu's header, back to `page`.
    fn back(page: Page, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity();
        div()
            .id("video-menu-back")
            .debug_selector(|| "video-menu-back".to_owned())
            .size(px(24.0))
            .rounded_md()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.1)))
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                entity.update(cx, |this, cx| {
                    this.menu = Some(Menu::Settings(page));
                    cx.notify();
                });
            })
            .child(Glyph::CHEVRON_LEFT.icon().size(px(14.0)))
            .into_any_element()
    }

    fn main_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = Vec::new();
        if self.captions.is_some() {
            rows.push(Self::to_page(
                "captions",
                "Caption settings",
                value(""),
                Page::Captions,
                cx,
            ));
        }
        if let Some(height) = self.quality() {
            rows.push(Self::to_page(
                "quality",
                "Quality",
                value(format!("{height}p")),
                Page::Quality,
                cx,
            ));
        }
        let entity = cx.entity();
        rows.push(row(
            "video-settings-keys".to_owned(),
            None,
            "Keyboard shortcuts",
            None,
            move |_, _, cx| {
                entity.update(cx, |this, cx| {
                    this.menu = Some(Menu::Settings(Page::Keys));
                    cx.notify();
                });
            },
        ));
        rows
    }

    fn caption_rows(style: CaptionStyle, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            header("Caption settings", Self::back(Page::Main, cx)),
            Self::to_page("size", "Size", value(style.size.label()), Page::Size, cx),
            Self::to_page("color", "Color", value(style.color.label()), Page::Color, cx),
            Self::acting(
                "video-settings-transparency".to_owned(),
                None,
                "Background transparency",
                Some(switch(style.transparent)),
                |_, _, cx| {
                    CaptionStyle::change(cx, |style| style.transparent = !style.transparent);
                    cx.notify();
                },
                cx,
            ),
            Self::acting(
                "video-settings-revert".to_owned(),
                None,
                "Revert to default settings",
                None,
                |_, _, cx| {
                    CaptionStyle::change(cx, CaptionStyle::revert);
                    cx.notify();
                },
                cx,
            ),
        ]
    }

    fn size_rows(style: CaptionStyle, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![header("Size", Self::back(Page::Captions, cx))];
        for size in CaptionSize::ALL {
            let entity = cx.entity();
            rows.push(row(
                format!("video-caption-size-{}", size.key()),
                Some(style.size == size),
                size.label(),
                None,
                move |_, _, cx| {
                    CaptionStyle::change(cx, |style| style.size = size);
                    entity.update(cx, |this, cx| {
                        this.menu = Some(Menu::Settings(Page::Captions));
                        cx.notify();
                    });
                },
            ));
        }
        rows
    }

    fn color_rows(style: CaptionStyle, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![header("Color", Self::back(Page::Captions, cx))];
        for color in CaptionColor::ALL {
            let entity = cx.entity();
            rows.push(row(
                format!("video-caption-color-{}", color.key()),
                Some(style.color == color),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(swatch(color))
                    .child(color.label()),
                None,
                move |_, _, cx| {
                    CaptionStyle::change(cx, |style| style.color = color);
                    entity.update(cx, |this, cx| {
                        this.menu = Some(Menu::Settings(Page::Captions));
                        cx.notify();
                    });
                },
            ));
        }
        rows
    }

    fn quality_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![header("Quality", Self::back(Page::Main, cx))];
        if let Some(height) = self.quality() {
            let words = div()
                .flex()
                .items_start()
                .child(format!("{height}p"))
                .when(height >= 720, |words| {
                    words.child(
                        div()
                            .ml(px(2.0))
                            .text_size(px(8.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("HD"),
                    )
                });
            // the only one played: choosing it closes the menu
            rows.push(Self::acting(
                format!("video-quality-{height}"),
                Some(true),
                words,
                None,
                |this, _, cx| {
                    this.menu = None;
                    cx.notify();
                },
                cx,
            ));
        }
        rows
    }

    fn key_rows(cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut rows = vec![header("Keyboard shortcuts", Self::back(Page::Main, cx))];
        rows.extend(KEYS.iter().map(|(what, key)| {
            div()
                .h(px(28.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .text_xs()
                .child(div().min_w_0().truncate().child(*what))
                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(*key))
                .into_any_element()
        }));
        rows
    }
}
