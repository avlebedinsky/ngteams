//! A forwarded message as Teams shows it: its content in a card (`rich_text`'s
//! [`Block::Forward`](crate::rich_text::Block::Forward), drawn as the message's own, its pictures too) under a header
//! naming where it came from: the forward arrow, the original's sender (their avatar and name, asked of the engine as
//! any sender's) and the original's time, in full, as a link to the original message.
//!
//! Where it came from is the message's `properties.originalMessageContext` (`teams_proto::chat::forwarded`). The link
//! opens the original in the app when the account has its chat and the store holds the message, else in Teams on the
//! web (the shell's to tell, `Shell::open_original`: [`Original`] goes out as
//! [`ChatViewEvent::OpenOriginal`](crate::ChatViewEvent::OpenOriginal) or
//! [`ChannelViewEvent::OpenOriginal`](crate::ChannelViewEvent::OpenOriginal)).

use chrono::Local;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, rgb,
};
use gpui_component::Size;
use serde_json::{Map, Value};
use teams_proto::chat::{Field, Forwarded, forwarded};

use crate::clock::{self, HourCycle};
use crate::icons::Glyph;
use crate::people::{Face, Shown};
use crate::rich_text::message_web_link;
use crate::search::HitTarget;
use crate::theme::Palette;

/// What a click on a forward's original time runs.
pub type OnOpen = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// The size of the original sender's avatar in the header.
const AVATAR: f32 = 24.0;

/// Where a forwarded message came from, as its header shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardOrigin {
    /// The original's sender's MRI, when the message names one.
    pub sender: Option<String>,
    /// Their name as the view has it (their profile's, else their MRI); none when the message names no sender.
    pub name: Option<SharedString>,
    /// Their avatar beside the name.
    pub face: Face,
    /// When the original was sent (ms).
    pub sent_at: Option<i64>,
    /// The original message, when the message names it in full.
    pub original: Option<Original>,
}

impl ForwardOrigin {
    /// Where `from` says the message came from, its original's sender `shown` as the engine last answered for them
    /// (their profile's name, else their MRI; their picture, else their initials).
    #[must_use]
    pub fn of(from: &Forwarded, shown: Option<&Shown>) -> Self {
        let name = from.sender.as_deref().map(|sender| {
            shown
                .and_then(|shown| shown.name.clone())
                .unwrap_or_else(|| SharedString::from(sender.to_owned()))
        });
        Self {
            sender: from.sender.clone(),
            face: Face {
                named: shown.is_some_and(|shown| shown.name.is_some()),
                picture: shown.and_then(|shown| shown.picture.clone()),
            },
            name,
            sent_at: from.sent_time_ms(),
            original: Original::of(from),
        }
    }
}

/// The original of a forwarded message: where the header's time takes the reader.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Original {
    /// Its chat, its id and its arrival time (its id's, in ms), to open it in the app.
    pub target: HitTarget,
    /// The web client's link to it ([`message_web_link`]), for a chat the account doesn't have.
    pub web_link: Option<String>,
}

impl Original {
    /// The original `forwarded` names: its conversation and its id (digits, its arrival time).
    #[must_use]
    pub fn of(forwarded: &Forwarded) -> Option<Self> {
        let conversation = forwarded.thread_id.clone().filter(|id| !id.is_empty())?;
        let message = forwarded.message_id.clone()?;
        let arrival_time = message
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| message.parse::<i64>().ok())
            .flatten()?;
        let web_link = message_web_link(&conversation, &message);
        Some(Self {
            target: HitTarget {
                conversation,
                message,
                arrival_time,
            },
            web_link,
        })
    }
}

/// Where the message whose `properties` (a stored message's, JSON text) these are was forwarded from; `None` for one
/// that wasn't, and for an origin in a form no capture showed.
#[must_use]
pub fn forwarded_from(properties: Option<&str>) -> Option<Forwarded> {
    let properties = serde_json::from_str::<Map<String, Value>>(properties?).ok()?;
    match forwarded(&properties) {
        Field::Known(forwarded) => Some(forwarded),
        Field::Absent | Field::Unknown(_) => None,
    }
}

/// The header of message `id`'s forward card: the arrow, the original's sender's avatar and name, and its time (as a
/// link that runs `open` when there is one).
#[must_use]
pub fn header(
    id: &str,
    origin: &ForwardOrigin,
    palette: Palette,
    cycle: HourCycle,
    open: Option<OnOpen>,
) -> AnyElement {
    let sender = origin.name.clone().map(|name| {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(origin.face.avatar(&name, Size::Small).size(px(AVATAR)))
            .child(div().child(name))
    });
    let time = origin
        .sent_at
        .map(|ms| clock::full_time(ms, &Local, cycle))
        .filter(|time| !time.is_empty())
        .map(|time| {
            let shown = div()
                .id(ElementId::Name(format!("forward-time-{id}").into()))
                .debug_selector({
                    let id = id.to_owned();
                    move || format!("forward-time-{id}")
                })
                .text_color(rgb(palette.brand))
                .child(time);
            match open {
                Some(open) => shown.underline().cursor_pointer().on_click(open),
                None => shown,
            }
        });
    let selector = format!("forward-header-{id}");
    div()
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .text_color(rgb(palette.muted_foreground))
        .child(Glyph::FORWARD.icon().size(px(16.0)))
        .children(sender)
        .children(time)
        .into_any_element()
}
