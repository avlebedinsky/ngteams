//! How views move as they change, in one place so the appearance's "Play animations" ([`Appearance::animations`])
//! stills all of it at once. (Off, the theme also sets GPUI's reduced motion, which stills the kit's own menus,
//! dialogs and spinners and animated pictures.)
//!
//! - **Entering** ([`rise`]): an element shown for the first time under its id fades in from a few pixels below: a
//!   chat or channel opened, the newest message, the empty pane's mark. GPUI keeps an animation's start by the
//!   element's id, so a new id (another chat, a new message) plays it again and a redraw doesn't.
//! - **Following a state** ([`toggle`]): a spring from the state's last value toward its new one, kept by the element's
//!   id: a chosen row's mark grows in as the one left shrinks away.
//!
//! [`Appearance::animations`]: crate::theme::Appearance::animations

use std::time::Duration;

use gpui::{
    AnimationExt as _, AnimationPhase, AnyElement, App, ElementId, IntoElement, SpringAnimation, SpringConfig,
    Styled, ease_out_quint, px,
};

/// How long an element takes to come in.
pub const ENTER: Duration = Duration::from_millis(260);

/// A spring that settles quickly with a little overshoot: a mark's growth.
const SNAP: SpringConfig = SpringConfig::new(420.0, 26.0, 1.0);

/// Whether views move: the user's choice (on until the settings say otherwise).
#[must_use]
pub fn on(cx: &App) -> bool {
    crate::theme::animations(cx)
}

/// `element` fading in from `distance` px below the first time it is drawn under `id` (see the module docs); drawn
/// still while animations are off.
pub fn rise<E>(element: E, id: impl Into<ElementId>, distance: f32, cx: &App) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    if !on(cx) {
        return element.into_any_element();
    }
    element
        .with_animation(
            id,
            gpui::Animation::new(ENTER).with_easing(ease_out_quint()),
            move |element, t| element.relative().top(px(distance * (1.0 - t))).opacity(t),
        )
        .into_any_element()
}

/// `element` drawn by `draw` at the phase of a spring that follows `state` (1 on, 0 off; it may overshoot a little),
/// kept under `id`; at the state's own phase while animations are off.
pub fn toggle<E>(
    element: E,
    id: impl Into<ElementId>,
    state: bool,
    cx: &App,
    draw: impl FnOnce(E, f32) -> E + 'static,
) -> AnyElement
where
    E: IntoElement + 'static,
{
    if !on(cx) {
        return draw(element, if state { 1.0 } else { 0.0 }).into_any_element();
    }
    element
        .with_spring(
            id,
            SpringAnimation::new(SNAP).to(state).from(false),
            move |element, phase: AnimationPhase| draw(element, phase.0),
        )
        .into_any_element()
}
