//! A channel's meeting in the scheduling form: the channel header's Schedule a meeting opens the form with the channel
//! as a row of its own, as Teams shows it (the team's tile, "<team> > <channel>", and the Send personal invites switch
//! with its explanation). The meeting is a Teams meeting on the team's group calendar, posted to the channel
//! (`EventWrite::CreateInChannel`), so the form offers no Teams meeting switch.

use gpui::{
    Context, InteractiveElement as _, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::switch::Switch;
use gpui_component::tooltip::Tooltip;
use teams_core::MeetingChannel;

use super::EventForm;
use crate::icons::{Glyph, selected_by};
use crate::teams_list::{initials, team_colour};
use crate::theme::Palette;

/// The channel a meeting is scheduled in: where it is posted (its thread and its team's mailbox) and what the form
/// names it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingPlace {
    pub channel: MeetingChannel,
    /// The team's name.
    pub team: String,
    /// The channel's name.
    pub name: String,
}

/// What the switch's info says, in Teams' words.
const PERSONAL_INVITES_INFO: &str =
    "Meeting invitations will be added to the personal calendars of all channel members.";

impl EventForm {
    /// The channel's row (see the module docs), when the form schedules a meeting in a channel.
    pub(super) fn channel_row(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let place = self.place.as_ref()?;
        let tile = div()
            .size(px(20.0))
            .flex_none()
            .rounded_sm()
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(8.0))
            .bg(rgb(team_colour(&place.team)))
            .text_color(rgb(palette.on_brand))
            .child(initials(&place.team));
        let named = div()
            .debug_selector(|| "event-channel".to_owned())
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .child(tile)
            .child(div().text_sm().truncate().child(SharedString::from(place.team.clone())))
            .child(
                Glyph::CHEVRON_RIGHT
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .child(div().text_sm().truncate().child(SharedString::from(place.name.clone())));
        let switch = selected_by(
            "event-personal-invites",
            Switch::new("event-personal-invites")
                .checked(self.personal_invites)
                .label("Send personal invites")
                .disabled(self.core.read_only())
                .on_click(cx.listener(|form, checked: &bool, _, cx| {
                    form.personal_invites = *checked;
                    cx.notify();
                })),
        );
        let info = div()
            .id("event-personal-invites-info")
            .debug_selector(|| "event-personal-invites-info".to_owned())
            .child(
                Glyph::INFO
                    .icon()
                    .size(px(16.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .tooltip(|window, cx| Tooltip::new(PERSONAL_INVITES_INFO).build(window, cx));
        Some(super::field_row(
            palette,
            Glyph::HASH,
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_3()
                .child(named)
                .child(switch)
                .child(info),
            false,
        ))
    }
}
