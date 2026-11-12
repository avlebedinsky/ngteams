//! Video painting without exposing platform textures to call tiles or inline players. Linux keeps its persistent
//! external texture and GPU completion. Windows uses GPUI's BGRA image path, with one uploaded image per visible
//! surface, evicted on replacement and when the element disappears. Windows traces submission, not GPU completion:
//! GPUI's image API cannot report the latter. A hidden or failed paint never counts as submitted.

use gpui::{App, Bounds, Pixels, Window};
use std::sync::Arc;

#[cfg(unix)]
pub(crate) use gpui::{PixelFrame as Frame, PixelSurface as Surface};
#[cfg(windows)]
#[path = "video_surface/windows.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{Frame, Surface};

pub(crate) enum Outcome {
    #[cfg(unix)]
    Presented,
    #[cfg(windows)]
    Submitted,
}

impl Outcome {
    pub(crate) fn change(&self) -> &'static str {
        match self {
            #[cfg(unix)]
            Self::Presented => "presented",
            #[cfg(windows)]
            Self::Submitted => "submitted",
        }
    }
}

pub(crate) fn frame(
    surface: &Arc<Surface>,
    serial: u64,
    (width, height): (u32, u32),
    pixels: &Arc<Vec<u8>>,
    complete: impl FnOnce(Outcome) + Send + 'static,
) -> Arc<Frame> {
    #[cfg(unix)]
    {
        Arc::new(Frame {
            surface: Arc::clone(surface),
            serial,
            width,
            height,
            pixels: Arc::clone(pixels),
            damage: vec![gpui::PixelRect {
                x: 0,
                y: 0,
                width,
                height,
            }],
            completion: gpui::FrameCompletion::new(move |outcome| {
                if outcome == gpui::FrameOutcome::Presented {
                    complete(Outcome::Presented);
                }
            }),
        })
    }
    #[cfg(windows)]
    {
        windows::frame(surface, serial, (width, height), pixels, complete)
    }
}

pub(crate) fn paint(frame: &Arc<Frame>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
    #[cfg(unix)]
    {
        let _ = cx;
        window.paint_external_surface(bounds, gpui::ExternalSurface::Pixels(Arc::clone(frame)));
    }
    #[cfg(windows)]
    windows::paint(frame, bounds, window, cx);
}

/// Fit a frame inside its bounds without stretching it.
pub(crate) fn fitted(bounds: Bounds<Pixels>, width: u32, height: u32) -> Bounds<Pixels> {
    let side = |pixels: u32| f32::from(u16::try_from(pixels).unwrap_or(u16::MAX)).max(1.0);
    let (box_width, box_height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let scale = (box_width / side(width)).min(box_height / side(height));
    let (drawn_width, drawn_height) = (side(width) * scale, side(height) * scale);
    Bounds {
        origin: gpui::point(
            bounds.origin.x + gpui::px((box_width - drawn_width) / 2.0),
            bounds.origin.y + gpui::px((box_height - drawn_height) / 2.0),
        ),
        size: gpui::size(gpui::px(drawn_width), gpui::px(drawn_height)),
    }
}
