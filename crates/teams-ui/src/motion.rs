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
    AnimationExt as _, AnimationPhase, AnyElement, App, ElementId, IntoElement, SpringAnimation, SpringConfig, Styled,
    ease_out_quint, px,
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

/// How long a ring takes to spread from what rings (a call coming in, a call going out).
const RIPPLE: Duration = Duration::from_millis(1800);

/// Rings of `color` spreading from a round element of `size` px and fading, one after another, for as long as they
/// are drawn under `id`: a call ringing. Drawn absolutely, centred on their parent (which is `relative`); nothing while
/// animations are off.
pub fn ripple(id: &str, color: u32, size: f32, cx: &App) -> Vec<AnyElement> {
    if !on(cx) {
        return Vec::new();
    }
    (0..2_u8)
        .map(|ring| {
            let delay = f32::from(ring) * 0.5;
            gpui::div()
                .absolute()
                .rounded_full()
                .border_2()
                .border_color(gpui::rgb(color))
                .with_animation(
                    gpui::SharedString::from(format!("{id}-ripple-{ring}")),
                    gpui::Animation::new(RIPPLE).repeat(),
                    move |ring, t| {
                        let t = (t + delay).rem_euclid(1.0);
                        let grow = size * 0.6 * t;
                        ring.top(px(-grow / 2.0))
                            .left(px(-grow / 2.0))
                            .size(px(size + grow))
                            .opacity(0.7 * (1.0 - t))
                    },
                )
                .into_any_element()
        })
        .collect()
}

/// How long one breath of a glow takes.
const BREATH: Duration = Duration::from_millis(1400);

/// `element` lit by a glow of the palette's brand colour that swells and settles, for as long as it is drawn under
/// `id` while `lit`: a call's tile while its person speaks. Still (the glow at rest) while animations are off; no glow
/// while not `lit`, nor in a theme without depth (High Contrast stays flat: the tile's own border marks it).
pub fn breathe<E>(
    element: E,
    id: impl Into<ElementId>,
    lit: bool,
    palette: crate::theme::Palette,
    cx: &App,
) -> AnyElement
where
    E: IntoElement + Styled + 'static,
{
    let color = palette.brand;
    let glow = move |swell: f32| {
        vec![gpui::BoxShadow {
            color: gpui::Hsla::from(gpui::rgb(color)).alpha(0.35 + 0.3 * swell),
            offset: gpui::point(px(0.0), px(0.0)),
            blur_radius: px(10.0 + 14.0 * swell),
            spread_radius: px(1.0 + 2.0 * swell),
            inset: false,
        }]
    };
    if !lit || !palette.depth {
        return element.into_any_element();
    }
    if !on(cx) {
        return element.shadow(glow(0.5)).into_any_element();
    }
    element
        .with_animation(id, gpui::Animation::new(BREATH).repeat(), move |element, t| {
            element.shadow(glow((t * std::f32::consts::TAU).sin().mul_add(0.5, 0.5)))
        })
        .into_any_element()
}

/// `element` fading in the first time it is drawn under `id`, in place: for an element placed absolutely, which
/// [`rise`] would move out of its place. Drawn at once while animations are off.
pub fn fade<E>(element: E, id: impl Into<ElementId>, cx: &App) -> AnyElement
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
            gpui::Styled::opacity,
        )
        .into_any_element()
}
