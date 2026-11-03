//! A meeting's lobby in the call window, as Teams shows it to whoever may admit: a card at the top of the stage,
//! "Waiting in the lobby", each person waiting with Deny and Admit (`docs/protocol/calling.md`, "The lobby"). It shows
//! while the roster tells someone waiting ([`crate::calls::ActiveCall::lobby`]) and goes once nobody does; each
//! button asks the call to let that person in or turn them away ([`Ask::Lobby`]). The account waiting in a lobby itself
//! sees the stage's own words instead ([`crate::calls::LOBBY_WAIT`]).

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
    rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use teams_core::LobbyPerson;

use crate::call_stage::CallStage;
use crate::calls::{ActiveCall, Ask};
use crate::icons::{Glyph, selected_by};
use crate::people;
use crate::theme::Palette;

/// How wide the card is drawn, px.
const CARD_WIDTH: f32 = 320.0;

/// The lobby's card over the stage, while anyone waits; `None` otherwise.
pub(crate) fn card(active: &ActiveCall, palette: Palette, cx: &mut Context<CallStage>) -> Option<AnyElement> {
    if active.lobby.is_empty() {
        return None;
    }
    let rows: Vec<AnyElement> = active
        .lobby
        .iter()
        .enumerate()
        .map(|(index, person)| row(index, person, palette, cx))
        .collect();
    let heading = div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(Glyph::USERS.icon().size(px(16.0)))
        .child("Waiting in the lobby");
    Some(
        div()
            .debug_selector(|| "call-lobby".to_owned())
            .absolute()
            .top_3()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
                div()
                    .w(px(CARD_WIDTH))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .rounded_lg()
                    .shadow_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .text_color(rgb(palette.foreground))
                    .child(heading)
                    .children(rows),
            )
            .into_any_element(),
    )
}

/// One person waiting: their avatar and name, then Deny and Admit.
fn row(index: usize, person: &LobbyPerson, palette: Palette, cx: &mut Context<CallStage>) -> AnyElement {
    let name: SharedString = person.name.clone().unwrap_or_else(|| "Someone".to_owned()).into();
    let (deny_id, admit_id) = (
        SharedString::from(format!("call-lobby-deny-{index}")),
        SharedString::from(format!("call-lobby-admit-{index}")),
    );
    let (for_deny, for_admit) = (person.mri.clone(), person.mri.clone());
    let deny = selected_by(
        deny_id.to_string(),
        Button::new(deny_id)
            .label("Deny")
            .outline()
            .w_full()
            .on_click(cx.listener(move |stage, _, _, cx| {
                stage.ask_call(
                    Ask::Lobby {
                        waiting: for_deny.clone(),
                        admit: false,
                    },
                    cx,
                );
            })),
    )
    .flex_1();
    let admit = selected_by(
        admit_id.to_string(),
        Button::new(admit_id)
            .label("Admit")
            .primary()
            .w_full()
            .on_click(cx.listener(move |stage, _, _, cx| {
                stage.ask_call(
                    Ask::Lobby {
                        waiting: for_admit.clone(),
                        admit: true,
                    },
                    cx,
                );
            })),
    )
    .flex_1();
    div()
        .debug_selector(move || format!("call-lobby-person-{index}"))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(people::avatar(Some(&name), None, gpui_component::Size::Small))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(palette.foreground))
                        .truncate()
                        .child(name.clone()),
                ),
        )
        .child(div().flex().gap_2().child(deny).child(admit))
        .into_any_element()
}
