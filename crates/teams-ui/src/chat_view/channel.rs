//! A team channel's part of the chat view ([`Place::Channel`], [`Place::Thread`]), drawn as Teams' channel draws it:
//!
//! - **Posts and threads.** The channel's messages are read from the store as a chat's are, with the post each belongs
//!   to ([`Core::reply_roots`](crate::Core::reply_roots): the store keeps it from a message's `conversationLink`); the
//!   channel's list shows its posts (a message without a recorded post is a post of its own), a post's replies show
//!   in its thread ([`Place::Thread`]), opened beside the channel by the post's "Reply in thread" or its replies'
//!   summary ([`Replies`]: how many, who replied, when the last came).
//! - **What a channel's messages are besides a chat's**: a meeting scheduled in the channel is Teams' meeting card
//!   ([`MeetingCard`]: its day, its title, struck through once a reply says it is cancelled, its times and place, and
//!   its organizer's Edit), a channel meeting's call is a line titled by its meeting (`Meeting in "<channel>"`, "…
//!   ended"), and the channel's own events (`ThreadActivity/…`: people added or gone, the channel renamed) aren't
//!   posts: the web client reads a channel's posts with `filterSystemMessage=true` and tells those events only in the
//!   channel's info pane.

use std::collections::HashSet;

use chrono::Local;
use gpui::{
    AnyElement, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use teams_proto::ScheduledMeeting;
use teams_store::Message;

use crate::clock::{self, HourCycle};
use crate::forwarded::OnOpen;
use crate::icons::Glyph;
use crate::people::Face;
use crate::theme::Palette;

/// Where a chat view's messages come from and where what it writes goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Place {
    /// A chat: every message of it.
    #[default]
    Chat,
    /// A team's channel: its posts, each with its replies' summary; written to, a message is a new post.
    Channel,
    /// One post's thread in a channel: the post and its replies; written to, a message is a reply, posted to the
    /// post's chain (`teams_proto::reply_chain_id`).
    Thread { post: String },
}

impl Place {
    /// Whether it is a channel's (its posts or a post's thread).
    #[must_use]
    pub fn in_channel(&self) -> bool {
        !matches!(self, Self::Chat)
    }
}

/// The prefix of a channel's own events' types (`ThreadActivity/AddMember`, `ThreadActivity/TopicUpdated`, …): Teams'
/// post list leaves them out.
pub const THREAD_EVENT: &str = "ThreadActivity/";

/// A meeting scheduled in the channel, as its post names it (`teams_proto::ScheduledMeeting`) and the channel's card
/// shows it: its day, its title (struck through once a reply under it says it is cancelled), its times and location,
/// and where it is ("<team> > <channel>").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingCard {
    pub title: SharedString,
    pub start_ms: i64,
    pub end_ms: i64,
    pub location: Option<SharedString>,
    pub cancelled: bool,
    /// The team's and the channel's names, when the view was given them ([`super::ChatView::set_channel_place`]).
    pub place: Option<(SharedString, SharedString)>,
    /// Its id on the team's calendar: what its Edit reads.
    pub ical_uid: String,
    /// The account scheduled it and it still stands: its organizer's Edit shows (not in the read-only mode).
    pub editable: bool,
}

/// A post's replies, as the summary under it shows them: how many, who replied (the latest first, each once, at most
/// [`REPLIERS`]), and when the last came.
#[derive(Clone, Debug, PartialEq)]
pub struct Replies {
    pub count: usize,
    pub people: Vec<(SharedString, Face)>,
    pub last: i64,
}

/// The most repliers a post's summary shows.
pub const REPLIERS: usize = 3;

/// What the channel's messages say of each other, read again with every load: the meetings a reply cancelled, the
/// calls that ended, and where the channel is.
#[derive(Default)]
pub(super) struct ChannelLooks {
    pub(super) place: Option<(SharedString, SharedString)>,
    cancelled: HashSet<String>,
    ended_calls: HashSet<String>,
}

impl ChannelLooks {
    /// Reads the meetings cancelled and the calls ended from `messages`.
    pub(super) fn read(&mut self, messages: &[Message]) {
        self.cancelled = messages
            .iter()
            .filter_map(|message| ScheduledMeeting::of_properties(message.properties.as_deref()?))
            .filter(|meeting| meeting.cancelled)
            .map(|meeting| meeting.ical_uid)
            .collect();
        self.ended_calls = messages
            .iter()
            .filter(|message| message.content.contains("<callEventType>callEnded</callEventType>"))
            .filter_map(|message| xml_text(&message.content, "callId"))
            .collect();
    }

    /// The meeting card a post of a meeting scheduled in the channel is drawn as; `None` for any other message (its
    /// cancellation's reply is drawn as its text). Its Edit only for its organizer (`me`, an MRI), not `read_only`.
    pub(super) fn meeting_card(&self, message: &Message, me: &str, read_only: bool) -> Option<MeetingCard> {
        let meeting =
            ScheduledMeeting::of_properties(message.properties.as_deref()?).filter(|meeting| !meeting.cancelled)?;
        let cancelled = self.cancelled.contains(&meeting.ical_uid);
        let editable = !cancelled && !read_only && meeting.organized_by(me.trim_start_matches("8:orgid:"));
        Some(MeetingCard {
            title: meeting.title.into(),
            start_ms: meeting.start_ms,
            end_ms: meeting.end_ms,
            location: meeting.location.map(SharedString::from),
            cancelled,
            place: self.place.clone(),
            ical_uid: meeting.ical_uid,
            editable,
        })
    }

    /// What a channel meeting's call post says, as Teams' card titles it: the meeting's title (the Meet now pre-join's
    /// subject its post was given, else `Meeting in "<channel>"`), and "ended" once the call is over (the service turns
    /// the post itself into the end); `None` for a post that isn't a meeting's call.
    pub(super) fn meeting_call_line(&self, message: &Message) -> Option<String> {
        if message.message_type != "Event/Call" {
            return None;
        }
        let properties: serde_json::Value = serde_json::from_str(message.properties.as_deref()?).ok()?;
        let meeting: serde_json::Value = match properties.get("meeting")? {
            serde_json::Value::String(text) => serde_json::from_str(text).ok()?,
            object @ serde_json::Value::Object(_) => object.clone(),
            _ => return None,
        };
        let titled = meeting
            .get("meetingtitle")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        let title = titled.or_else(|| {
            self.place
                .as_ref()
                .map(|(_, channel)| format!("Meeting in \"{channel}\""))
        })?;
        let ended = message.content.contains("<callEventType>callEnded</callEventType>")
            || xml_text(&message.content, "callId").is_some_and(|call| self.ended_calls.contains(&call));
        Some(if ended { format!("{title} ended") } else { title })
    }
}

/// The text of the first `<tag>…</tag>` in a call post's `content`, trimmed; `None` when there is none or it is empty.
fn xml_text(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = content.find(&open)? + open.len();
    let end = start + content[start..].find(&format!("</{tag}>"))?;
    Some(content[start..end].trim().to_owned()).filter(|text| !text.is_empty())
}

/// A post's replies' summary under it, as Teams' channel draws it: the repliers' faces overlapping, "N replies" in the
/// brand colour, and when the last came; a click opens the post's thread (`open`). Its test selector is
/// `replies-<post>`.
pub(super) fn replies_line(
    post: &str,
    replies: &Replies,
    palette: Palette,
    cycle: HourCycle,
    open: OnOpen,
) -> AnyElement {
    let faces = replies.people.iter().enumerate().map(|(at, (name, face))| {
        let avatar = face.avatar(name, gpui_component::Size::XSmall).size(px(20.0));
        let tile = div()
            .size(px(22.0))
            .rounded_full()
            .border_1()
            .border_color(rgb(palette.background))
            .flex()
            .items_center()
            .justify_center()
            .child(avatar);
        if at == 0 { tile } else { tile.ml(px(-6.0)) }
    });
    let label = if replies.count == 1 {
        "1 reply".to_owned()
    } else {
        format!("{} replies", replies.count)
    };
    let last = format!("Last reply {}", clock::message_time(replies.last, &Local::now(), cycle));
    let selector = format!("replies-{post}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .mt_1()
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .rounded_md()
        .cursor_pointer()
        .hover(move |row| row.bg(rgb(palette.hover)))
        .on_click(open)
        .child(div().flex().items_center().children(faces))
        .child(
            div()
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(rgb(palette.brand))
                .child(label),
        )
        .child(div().text_xs().text_color(rgb(palette.muted_foreground)).child(last))
        .into_any_element()
}

/// A meeting scheduled in the channel, as Teams' card draws it: the day's tile (its date over its month), the title, the
/// day of the week and the times with the location, and "<team> > <channel>"; once cancelled, no tile, the title struck
/// through and the date in full. Its organizer's Edit (`edit`) on the right.
pub(super) fn meeting_card(
    id: &str,
    card: &MeetingCard,
    palette: Palette,
    cycle: HourCycle,
    edit: Option<OnOpen>,
) -> AnyElement {
    use chrono::TimeZone as _;
    let local = |ms: i64| Local.timestamp_millis_opt(ms).single();
    let (Some(start), Some(end)) = (local(card.start_ms), local(card.end_ms)) else {
        return div().child(card.title.clone()).into_any_element();
    };
    let clock_of = |at: chrono::DateTime<Local>| at.format(cycle.time_of_day()).to_string();
    let day = if card.cancelled {
        start.format("%A, %b %-d").to_string()
    } else {
        start.format("%A").to_string()
    };
    let mut when = format!("{day}, {} - {}", clock_of(start), clock_of(end));
    if let Some(location) = &card.location {
        when.push_str("  |  ");
        when.push_str(location);
    }
    let day_tile = div()
        .w(px(36.0))
        .flex_none()
        .py_1()
        .flex()
        .flex_col()
        .items_center()
        .rounded_md()
        .border_1()
        .border_color(rgb(palette.border))
        .child(
            div()
                .font_weight(gpui::FontWeight::BOLD)
                .child(start.format("%-d").to_string()),
        )
        .child(div().text_xs().child(start.format("%b").to_string().to_uppercase()));
    let title = div().font_weight(gpui::FontWeight::BOLD).child(card.title.clone());
    let title = if card.cancelled { title.line_through() } else { title };
    let place = card
        .place
        .as_ref()
        .map(|(team, channel)| place_line(team, channel, palette));
    // its organizer's Edit, as Teams' card offers it: the meeting's details, to change or cancel it
    let edit = edit.filter(|_| card.editable).map(|edit| {
        let selector = format!("channel-meeting-edit-{id}");
        crate::icons::selected_by(
            &selector,
            Button::new(SharedString::from(selector.clone()))
                .icon(Glyph::PENCIL.icon())
                .label("Edit")
                .outline()
                .small()
                .on_click(edit),
        )
    });
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("channel-meeting-card-{id}"))
        .p_3()
        .flex()
        .gap_3()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.background))
        .children((!card.cancelled).then_some(day_tile))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(title)
                .child(div().text_xs().child(when))
                .children(place),
        )
        .children(edit.map(|edit| div().flex_none().child(edit)))
        .into_any_element()
}

/// A meeting card's "<team> > <channel>": the team's tile, its name, a chevron and the channel's.
fn place_line(team: &SharedString, channel: &SharedString, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .child(
            div()
                .size(px(14.0))
                .rounded_sm()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(6.0))
                .bg(rgb(crate::teams_list::team_colour(team)))
                .text_color(rgb(palette.on_brand))
                .child(crate::teams_list::initials(team)),
        )
        .child(team.clone())
        .child(
            Glyph::CHEVRON_RIGHT
                .icon()
                .size(px(12.0))
                .text_color(rgb(palette.muted_foreground)),
        )
        .child(channel.clone())
}
