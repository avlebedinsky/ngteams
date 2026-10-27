//! Teams' presence badge on an avatar's corner, shared by the views that show one (a message's sender in the chat
//! view; the chat list's rows can take it too): the state's colour ([`presence::fill`]) with its mark (a check for
//! available, a clock for away and be right back, a bar for do not disturb; busy plain), offline a hollow grey ring
//! with a grey cross, each ringed in the background so it stands off the avatar. Its debug selector is the dot's
//! (`presence-<state>`).

use gpui::{Div, ParentElement as _, Styled as _, div, px, rgb};
use teams_core::Availability;

use crate::icons::Glyph;
use crate::presence;
use crate::theme::Palette;

/// The badge's diameter beside a message's 32 px avatar, px, its ring included.
pub const MESSAGE: f32 = 14.0;

/// The badge of `availability`, `size` px across; `None` for a presence that isn't known (or a word this build
/// doesn't name): nothing is drawn rather than a ring that says nothing.
#[must_use]
pub fn badge(availability: &Availability, size: f32, palette: Palette) -> Option<Div> {
    let fill = presence::fill(availability)?;
    let mark = match availability {
        Availability::Available | Availability::AvailableIdle => Some(Glyph::CHECK),
        Availability::Away | Availability::BeRightBack => Some(Glyph::CLOCK),
        Availability::DoNotDisturb => Some(Glyph::MINUS),
        Availability::Offline => Some(Glyph::X),
        Availability::Busy | Availability::BusyIdle | Availability::Other(_) | Availability::Unknown => None,
    };
    let badge = presence::dot(availability, palette)
        .size(px(size))
        .border_2()
        .flex()
        .items_center()
        .justify_center();
    // offline is Teams' hollow grey ring with a grey cross; every other state its colour filled, its mark cut out
    if *availability == Availability::Offline {
        let ring = div()
            .size_full()
            .rounded_full()
            .border_1()
            .border_color(rgb(fill))
            .flex()
            .items_center()
            .justify_center()
            .children(mark.map(|mark| mark.icon().size(px(size - 6.0)).text_color(rgb(fill))));
        return Some(badge.bg(rgb(palette.background)).child(ring));
    }
    Some(
        badge
            .bg(rgb(fill))
            .children(mark.map(|mark| mark.icon().size(px(size - 4.0)).text_color(rgb(palette.background)))),
    )
}
