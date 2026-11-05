//! The sheet a window's panes float on, over the frame ([`crate::theme::frame`]): rounded, shadowed, a gap from the
//! window's edges, its corners capped in the frame's colour there (GPUI clips only to rectangles, so a pane drawn
//! square to its corner would otherwise show past the rounding). In a theme without depth (High Contrast) it is flat
//! and square, edged by a border. The main window's panes sit on one, and so does the call window's stage (the
//! pre-join, the event form and the sign-in prompt draw cards of their own on the frame instead).

use gpui::{AnyElement, IntoElement as _, ParentElement as _, Styled as _, Window, div, px, rgb};
use gpui_component::Icon;

use crate::theme::{self, Palette};

/// `panes` floated in `holder` as the sheet: `origin` is where the sheet starts in the window (its left and top, px);
/// the holder takes the gap on the right and at the bottom, and on the left too when `gap_left` (a window with no rail
/// beside its sheet).
#[must_use]
pub fn float(
    holder: gpui::Div,
    panes: gpui::Div,
    palette: Palette,
    window: &Window,
    origin: (f32, f32),
    gap_left: bool,
) -> gpui::Div {
    if !palette.depth {
        return holder.child(panes.border_l_1().border_t_1().border_color(rgb(palette.border)));
    }
    let viewport = window.viewport_size();
    let size = (f32::from(viewport.width), f32::from(viewport.height));
    let left = if gap_left { origin.0 + theme::SHEET_GAP } else { origin.0 };
    let (top, right, bottom) = (origin.1, size.0 - theme::SHEET_GAP, size.1 - theme::SHEET_GAP);
    let caps = [
        (Corner::TopLeft, (left, top)),
        (Corner::TopRight, (right, top)),
        (Corner::BottomRight, (right, bottom)),
        (Corner::BottomLeft, (left, bottom)),
    ]
    .map(|(corner, at)| corner.cap(theme::frame_at(palette, size, at)));
    let holder = if gap_left { holder.pl(px(theme::SHEET_GAP)) } else { holder };
    holder.pr(px(theme::SHEET_GAP)).pb(px(theme::SHEET_GAP)).child(
        panes
            .relative()
            .rounded(px(theme::SHEET_RADIUS))
            .shadow(theme::glow(palette, 0x0000_0000, 0.32, 28.0, 8.0))
            .children(caps),
    )
}

/// A corner of the sheet the panes float on.
#[derive(Clone, Copy)]
enum Corner {
    TopLeft,
    TopRight,
    BottomRight,
    BottomLeft,
}

impl Corner {
    /// The frame's colour, `color`, filling this corner outside the sheet's rounding (see [`float`]).
    fn cap(self, color: u32) -> AnyElement {
        // each a square less the quarter circle the sheet's corner is drawn with
        let (drawing, place): (&'static [u8], fn(gpui::Div) -> gpui::Div) = match self {
            Self::TopLeft => (
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M0 0H12A12 12 0 0 0 0 12Z"/></svg>"#,
                |cap| cap.top_0().left_0(),
            ),
            Self::TopRight => (
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M12 0V12A12 12 0 0 0 0 0Z"/></svg>"#,
                |cap| cap.top_0().right_0(),
            ),
            Self::BottomRight => (
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M12 12H0A12 12 0 0 0 12 0Z"/></svg>"#,
                |cap| cap.bottom_0().right_0(),
            ),
            Self::BottomLeft => (
                br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12"><path d="M0 12V0A12 12 0 0 0 12 12Z"/></svg>"#,
                |cap| cap.bottom_0().left_0(),
            ),
        };
        place(div().absolute().size(px(theme::SHEET_RADIUS)))
            .child(
                Icon::default()
                    .data(drawing)
                    .size(px(theme::SHEET_RADIUS))
                    .text_color(rgb(color)),
            )
            .into_any_element()
    }
}

