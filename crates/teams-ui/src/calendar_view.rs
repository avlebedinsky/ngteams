//! The Calendar (the rail's Calendar, Ctrl+4), as Teams' calendar app shows it: across the list and conversation
//! panes, a left pane with the month (Monday first, the days shown marked, today circled, its own month arrows, folded
//! to the week by its chevron), "Add calendar", "My calendars" with the account's calendar ticked and "Show all"; a
//! toolbar with the pane's toggle, Today, the previous and next arrows, the dates shown (a month picker under them), the
//! view switcher (Day, Work week, Week, Month), "…" (Refresh), "Join with an ID" and the split "New"; and the grid:
//! the days' headers (today's in the brand colour under a brand line), the hours with their half-hour lines, the
//! all-day strip, each event a block with a coloured edge (striped while not accepted), its title and organizer, the
//! recurring mark (crossed out on a cancelled occurrence, which is outlined), overlapping events side by side, and the
//! current time's line with its dot; the month view lists each day's events.
//!
//! - **Its events** come from the engine's read of the middle tier's calendar view ([`Core::calendar`]), asked for the
//!   days shown whenever they change, on Refresh and every few minutes while the calendar is shown; what a span read
//!   last is shown at once while it is read again. A span not read in this run is first drawn from the events the
//!   store kept from earlier reads, this run's or an earlier one's ([`Core::stored_calendar`]), and the read's answer
//!   then replaces them: events added, changed, moved, cancelled or gone, occurrences of a series too. A declined
//!   meeting isn't shown, as in Teams.
//! - **An event clicked** opens its card: its title, when, the series it belongs to, where, the organizer and the
//!   account's answer, who is invited, its text; for a Teams meeting the split Join (any time, started or not: its
//!   pre-join, [`crate::prejoin`], which starts the meeting's call when nobody has), "Chat with
//!   participants" (the meeting's chat); Join's menu has Join and "Copy join link". A build that shows no call, and the
//!   read-only mode, offer no Join (and Copy join link beside Chat instead).
//! - **New** (its menu: an event or a Teams meeting), a double click on an empty half hour of the grid, and an
//!   event's Edit (its card's, or a double click on it) open the scheduling form in a window of its own
//!   ([`crate::event_form`]); a save there reads the days shown again. The card of the account's own meeting offers
//!   Cancel meeting, of its appointment Delete, and of a meeting cancelled by its organizer Remove from calendar, each
//!   confirmed first.
//! - **Join with an ID** (`join_id`): the meeting's ID and passcode, the meeting found in the calendar by them and
//!   joined as its card's Join joins it. "Add calendar" (other people's and shared calendars) says it isn't
//!   available yet.
//! - **Keys** while the calendar has the focus: Left and Right the previous and next days, T today, Ctrl+Alt+1–4 the
//!   views, Escape closes a card or the month picker.
//! - **Traced**: `calendar shown` with the view's key, `calendar read` with the view's key when its events came,
//!   `calendar stored` when the store's were drawn first,
//!   `calendar-event opened` (the event's kind), never a subject or a name.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Datelike as _, NaiveDate, NaiveDateTime};
use gpui::{
    AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, ScrollHandle, Styled as _, Task, WeakEntity, Window, div, prelude::FluentBuilder as _,
    px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use teams_core::CalendarEvent;

use crate::bridge::Core;
use crate::calendar::{self, Mode, Shown, Span};
use crate::calls::{Calls, MeetingToJoin};
use crate::clock::HourCycle;
use crate::icons::{Glyph, selected_by};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

mod card;
mod grid;
mod join_id;
mod pane;

pub use join_id::ID_INVALID;

/// One hour's height in the time grid, px.
const HOUR_HEIGHT: f32 = 96.0;
/// The shortest an event's block is drawn, minutes (a quarter-hour event keeps a readable line).
const MIN_MINUTES: i64 = 21;
/// The time gutter's width, px.
const GUTTER: f32 = 60.0;
/// The left pane's width, px.
const PANE_WIDTH: f32 = 280.0;
/// How often the shown events are read again while the calendar is shown.
const REFRESH: StdDuration = StdDuration::from_mins(5);
/// How often the current time's line moves.
const TICK: StdDuration = StdDuration::from_secs(30);
/// The events a month day lists before "+N more".
const MONTH_LISTED: usize = 3;

/// What the calendar tells the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalendarViewEvent {
    /// Open the chat (a meeting's chat).
    OpenChat(String),
    /// Join the running call of the meeting whose chat this is.
    Join(MeetingToJoin),
    /// Join the meeting whose chat this is, its organizer read from the chat (its event's link names none): its
    /// title, and its link to open in the browser when the chat names no meeting.
    JoinChat {
        chat: String,
        title: String,
        link: Option<String>,
    },
    /// Join the meeting with this ID and passcode, which the calendar doesn't have.
    JoinCode(teams_core::MeetingCode),
}

/// One of the calendar's switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Toggle {
    /// The calendar is shown.
    Shown,
    /// The left pane is hidden.
    PaneHidden,
    /// The left pane's month is folded to the anchor's week.
    MiniFolded,
    /// "My calendars" is folded.
    MineFolded,
    /// The account's calendar is unticked: its events aren't shown.
    Hidden,
    /// "Show all" was chosen.
    ShowAll,
    /// The grid scrolls to the working day (or now) when next drawn.
    ScrollDue,
}

/// The switches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Toggles(u8);

impl Default for Toggles {
    fn default() -> Self {
        Self(1 << Toggle::ScrollDue as u8)
    }
}

impl Toggles {
    fn on(self, toggle: Toggle) -> bool {
        self.0 & (1 << toggle as u8) != 0
    }

    fn set(&mut self, toggle: Toggle, on: bool) {
        if on {
            self.0 |= 1 << toggle as u8;
        } else {
            self.0 &= !(1 << toggle as u8);
        }
    }

    fn flip(&mut self, toggle: Toggle) {
        self.set(toggle, !self.on(toggle));
    }
}

/// What a span's read gave.
#[derive(Clone)]
enum Loaded {
    Events(Rc<Vec<Shown>>),
    /// What the store kept of the span, shown until its read answers.
    Stored(Rc<Vec<Shown>>),
    Failed,
}

/// The calendar (see the module docs).
pub struct CalendarView {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    trace: Arc<ViewTrace>,
    focus: FocusHandle,
    mode: Mode,
    /// The day the view is about (Today sets it; the arrows step it).
    anchor: NaiveDate,
    /// The first of the month the left pane's month shows.
    mini_month: NaiveDate,
    /// The pane's and the view's switches ([`Toggle`]).
    toggles: Toggles,
    /// The month picker under the toolbar's dates is open, showing that month.
    picker: Option<NaiveDate>,
    /// What each span read last.
    loaded: HashMap<Span, Loaded>,
    reading: Option<(Span, Task<()>)>,
    /// A read asked again (`again`, after a write) while that span was being read: it runs once that read is over, so
    /// the answer from before the write isn't the last one.
    read_again: bool,
    /// The card of the event clicked.
    card: Option<Shown>,
    /// The card asks to confirm its Cancel meeting, Delete or Remove from calendar, and one is being sent.
    removal: Removal,
    /// What a control that can't act yet says, until closed.
    notice: Option<&'static str>,
    /// Join with an ID's panel, while it is open.
    join_id: Option<join_id::JoinIdForm>,
    scroll: ScrollHandle,
    _ticker: Task<()>,
}

impl EventEmitter<CalendarViewEvent> for CalendarView {}

impl CalendarView {
    /// The calendar, on the work week of today.
    pub fn new(core: Arc<dyn Core>, calls: Entity<Calls>, trace: Arc<ViewTrace>, cx: &mut Context<Self>) -> Self {
        let today = calendar::now().date_naive();
        let ticker = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut since_read = StdDuration::ZERO;
            loop {
                cx.background_executor().timer(TICK).await;
                since_read += TICK;
                let refresh = since_read >= REFRESH;
                if refresh {
                    since_read = StdDuration::ZERO;
                }
                let alive = this.update(cx, |view, cx| view.tick(refresh, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            core,
            calls,
            trace,
            focus: cx.focus_handle(),
            mode: Mode::WorkWeek,
            anchor: today,
            mini_month: today.with_day(1).unwrap_or(today),
            toggles: Toggles::default(),
            picker: None,
            loaded: HashMap::new(),
            reading: None,
            read_again: false,
            card: None,
            removal: Removal::None,
            notice: None,
            join_id: None,
            scroll: ScrollHandle::new(),
            _ticker: ticker,
        }
    }

    /// The calendar is shown (`true`, from the rail) or hidden: shown, it reads the days it shows and takes the focus.
    pub fn set_shown(&mut self, shown: bool, window: &mut Window, cx: &mut Context<Self>) {
        if shown && !self.toggles.on(Toggle::Shown) {
            self.toggles.set(Toggle::ScrollDue, true);
            self.trace
                .record(&TraceLine::new("calendar", "shown").kind(self.mode.key()));
            self.read(true, cx);
            self.focus.focus(window, cx);
        }
        self.toggles.set(Toggle::Shown, shown);
        cx.notify();
    }

    /// Whether the calendar is shown.
    #[must_use]
    pub fn shown(&self) -> bool {
        self.toggles.on(Toggle::Shown)
    }

    /// The view shown.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The day the view is about.
    #[must_use]
    pub fn anchor(&self) -> NaiveDate {
        self.anchor
    }

    /// The days shown.
    #[must_use]
    pub fn span(&self) -> Span {
        Span::of(self.anchor, self.mode)
    }

    /// The events shown, once read (or, until then, as the store kept them).
    #[must_use]
    pub fn events(&self) -> Option<Rc<Vec<Shown>>> {
        match self.loaded.get(&self.span()) {
            Some(Loaded::Events(events) | Loaded::Stored(events)) => Some(Rc::clone(events)),
            _ => None,
        }
    }

    /// Whether the events shown are the store's, their read not answered yet.
    #[must_use]
    pub fn showing_stored(&self) -> bool {
        matches!(self.loaded.get(&self.span()), Some(Loaded::Stored(_)))
    }

    /// The event whose card is open.
    #[must_use]
    pub fn card(&self) -> Option<&Shown> {
        self.card.as_ref()
    }

    /// Shows `mode` around the same day.
    pub fn set_mode(&mut self, mode: Mode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.toggles.set(Toggle::ScrollDue, true);
            self.trace.record(&TraceLine::new("calendar", "shown").kind(mode.key()));
            self.read(false, cx);
            cx.notify();
        }
    }

    /// Shows the days around `date`.
    pub fn go_to(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        self.anchor = date;
        self.mini_month = date.with_day(1).unwrap_or(date);
        self.picker = None;
        self.read(false, cx);
        cx.notify();
    }

    /// Today's days.
    pub fn today(&mut self, cx: &mut Context<Self>) {
        self.toggles.set(Toggle::ScrollDue, true);
        self.go_to(calendar::now().date_naive(), cx);
    }

    /// The previous (`forward` false) or next days.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.go_to(calendar::step(self.anchor, self.mode, forward), cx);
    }

    /// Opens the card of `shown`.
    pub fn open_card(&mut self, shown: Shown, cx: &mut Context<Self>) {
        let kind = if shown.event.online { "meeting" } else { "event" };
        self.trace
            .record(&TraceLine::new("calendar-event", "opened").kind(kind));
        self.card = Some(shown);
        cx.notify();
    }

    /// Closes the event's card.
    pub fn close_card(&mut self, cx: &mut Context<Self>) {
        self.card = None;
        self.removal = Removal::None;
        cx.notify();
    }

    /// The slot New opens with: the next half hour.
    #[must_use]
    pub fn next_slot(&self) -> crate::event_form::Slot {
        crate::event_form::Slot::next_half_hour(calendar::now().naive_local())
    }

    /// Opens the scheduling form on a new event in `slot`, a Teams meeting when `online` (New, its menu, a double click
    /// on an empty half hour).
    pub fn new_event(&mut self, slot: crate::event_form::Slot, online: bool, cx: &mut Context<Self>) {
        let seed = crate::event_form::Seed::New { slot, online };
        crate::event_form::open(
            Arc::clone(&self.core),
            Arc::clone(&self.trace),
            seed,
            Self::saved(cx),
            cx,
        );
    }

    /// Opens the scheduling form on a new meeting in a channel (its header's Schedule a meeting), at the next half hour:
    /// a Teams meeting on its team's calendar, posted to the channel; once sent, the calendar reads its days again.
    pub fn schedule_in_channel(&mut self, place: crate::event_form::MeetingPlace, cx: &mut Context<Self>) {
        let seed = crate::event_form::Seed::InChannel {
            slot: self.next_slot(),
            place,
        };
        crate::event_form::open(
            Arc::clone(&self.core),
            Arc::clone(&self.trace),
            seed,
            Self::saved(cx),
            cx,
        );
    }

    /// Opens the scheduling form on a channel's meeting read whole from its team's calendar (its card's Edit in the
    /// channel): Send update and Cancel meeting go to the team's calendar; once saved, the calendar reads its days again.
    pub fn edit_in_channel(
        &mut self,
        full: teams_core::FullEvent,
        place: crate::event_form::MeetingPlace,
        cx: &mut Context<Self>,
    ) {
        let seed = crate::event_form::Seed::ChangeInChannel {
            full: Box::new(full),
            place,
        };
        crate::event_form::open(
            Arc::clone(&self.core),
            Arc::clone(&self.trace),
            seed,
            Self::saved(cx),
            cx,
        );
    }

    /// Opens the scheduling form on the channel's meeting `channel_meeting` (a calendar card's Edit, a double click on
    /// it): the channel's team found among the account's teams, the meeting read whole from the team's calendar.
    fn edit_channel_event(&mut self, channel_meeting: teams_core::ChannelEventRef, cx: &mut Context<Self>) {
        let core = Arc::clone(&self.core);
        self.card = None;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let read = match place_of(&*core, &channel_meeting.channel).await {
                Some(place) => core
                    .channel_event(place.channel.mailbox.clone(), channel_meeting.ical_uid.clone())
                    .await
                    .map(|full| (full, place)),
                None => Err(teams_core::EngineError::CalendarUnread),
            };
            let _ = this.update(cx, |view, cx| match read {
                Ok((full, place)) => view.edit_in_channel(full, place, cx),
                Err(teams_core::EngineError::Stopped) => {}
                Err(_) => view.say(EVENT_UNREAD, cx),
            });
        })
        .detach();
        cx.notify();
    }

    /// Opens the scheduling form on the event `id`, read whole first (its card's Edit, a double click on it); a
    /// channel's meeting the account scheduled is read from its team's calendar, as Teams opens it.
    pub fn edit_event(&mut self, id: String, cx: &mut Context<Self>) {
        let channel_meeting = self
            .shown_event(&id)
            .and_then(|event| event.channel_meeting.clone())
            .filter(|channel_meeting| channel_meeting.organized_by(self.core.me()));
        if let Some(channel_meeting) = channel_meeting {
            return self.edit_channel_event(channel_meeting, cx);
        }
        let reply = self.core.calendar_event(id);
        self.card = None;
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |view, cx| match answer {
                Ok(full) => {
                    let seed = crate::event_form::Seed::Change(Box::new(full));
                    crate::event_form::open(
                        Arc::clone(&view.core),
                        Arc::clone(&view.trace),
                        seed,
                        Self::saved(cx),
                        cx,
                    );
                }
                Err(teams_core::EngineError::Stopped) => {}
                Err(_) => view.say(EVENT_UNREAD, cx),
            });
        })
        .detach();
        cx.notify();
    }

    /// The ticker's beat while the calendar shows: the current time's line moves, and on a `refresh` the days shown
    /// are read again.
    fn tick(&mut self, refresh: bool, cx: &mut Context<Self>) {
        if self.toggles.on(Toggle::Shown) {
            if refresh {
                self.read(true, cx);
            }
            cx.notify();
        }
    }

    /// What a save in the scheduling form tells this view: read the days shown again.
    fn saved(cx: &mut Context<Self>) -> crate::event_form::Saved {
        let view = cx.entity().downgrade();
        crate::event_form::Saved::new(move |cx| {
            let _ = view.update(cx, |view, cx| {
                view.read(true, cx);
                cx.notify();
            });
        })
    }

    /// Sends the card's confirmed removal of its event: Cancel meeting, Delete or Remove from calendar.
    pub fn remove_carded(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.card else { return };
        let event = &shown.event;
        if own_channel_meeting(event, self.core.me()) && !event.cancelled {
            return self.cancel_channel_carded(cx);
        }
        let write = if event.is_organizer && !event.attendees.is_empty() && !event.cancelled {
            teams_core::EventWrite::Cancel {
                id: event.id.clone(),
                chat: event.meeting_chat.clone(),
                message: None,
            }
        } else {
            teams_core::EventWrite::Delete { id: event.id.clone() }
        };
        self.removal = Removal::Sending;
        let reply = self.core.write_event(write);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |view, cx| {
                view.removal = Removal::None;
                match answer {
                    Ok(_) => {
                        view.card = None;
                        view.read(true, cx);
                    }
                    Err(teams_core::EngineError::Stopped) => {}
                    Err(_) => view.say(EVENT_NOT_REMOVED, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The event `id` as the calendar shows it: its card's, else the spans read.
    fn shown_event(&self, id: &str) -> Option<Rc<CalendarEvent>> {
        if let Some(shown) = self.card.as_ref().filter(|shown| shown.event.id == id) {
            return Some(Rc::clone(&shown.event));
        }
        self.loaded.values().find_map(|loaded| match loaded {
            Loaded::Events(shown) | Loaded::Stored(shown) => shown
                .iter()
                .find(|shown| shown.event.id == id)
                .map(|shown| Rc::clone(&shown.event)),
            Loaded::Failed => None,
        })
    }

    /// The card's Cancel meeting of a channel's meeting the account scheduled: cancelled on its team's calendar, as its
    /// details' Cancel meeting does (everyone on the team told, its channel's post marked).
    fn cancel_channel_carded(&mut self, cx: &mut Context<Self>) {
        let Some((meeting, post)) = self
            .card
            .as_ref()
            .and_then(|shown| teams_core::ScheduledMeeting::of_event(&shown.event))
        else {
            return self.say(EVENT_NOT_REMOVED, cx);
        };
        let Some(channel) = self
            .card
            .as_ref()
            .and_then(|shown| shown.event.channel_meeting.as_ref())
            .map(|channel_meeting| channel_meeting.channel.clone())
        else {
            return;
        };
        self.removal = Removal::Sending;
        let core = Arc::clone(&self.core);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = match place_of(&*core, &channel).await {
                Some(place) => core
                    .write_event(teams_core::EventWrite::CancelInChannel {
                        meeting: Box::new(meeting),
                        channel: place.channel,
                        post,
                        message: None,
                    })
                    .await
                    .map(|_| ()),
                None => Err(teams_core::EngineError::CalendarUnread),
            };
            let _ = this.update(cx, |view, cx| {
                view.removal = Removal::None;
                match answer {
                    Ok(()) => {
                        view.card = None;
                        view.read(true, cx);
                    }
                    Err(teams_core::EngineError::Stopped) => {}
                    Err(_) => view.say(EVENT_NOT_REMOVED, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reads the span shown, unless it was read (`again` reads it anyway) or is being read.
    fn read(&mut self, again: bool, cx: &mut Context<Self>) {
        let span = self.span();
        if self.reading.as_ref().is_some_and(|(reading, _)| *reading == span) {
            self.read_again |= again;
            return;
        }
        if !again && matches!(self.loaded.get(&span), Some(Loaded::Events(_))) {
            return;
        }
        let (start, end) = span.window_ms();
        if !matches!(self.loaded.get(&span), Some(Loaded::Events(_) | Loaded::Stored(_))) {
            let stored = self.core.stored_calendar(start, end);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = stored.await;
                let _ = this.update(cx, |view, cx| view.stored_over(span, answer, cx));
            })
            .detach();
        }
        let reply = self.core.calendar(start, end);
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |view, cx| view.read_over(span, answer, cx));
        });
        self.reading = Some((span, task));
    }

    /// What the store kept of `span` came: shown while its read hasn't answered (an answer that came first is kept, a
    /// failed read gives way to it); nothing kept changes nothing.
    fn stored_over(
        &mut self,
        span: Span,
        answer: Result<Vec<CalendarEvent>, teams_core::EngineError>,
        cx: &mut Context<Self>,
    ) {
        let Ok(events) = answer else { return };
        if events.is_empty() || matches!(self.loaded.get(&span), Some(Loaded::Events(_) | Loaded::Stored(_))) {
            return;
        }
        self.loaded.insert(span, Loaded::Stored(Rc::new(shown(events))));
        self.trace
            .record(&TraceLine::new("calendar", "stored").kind(self.mode.key()));
        cx.notify();
    }

    fn read_over(
        &mut self,
        span: Span,
        answer: Result<Vec<CalendarEvent>, teams_core::EngineError>,
        cx: &mut Context<Self>,
    ) {
        if self.reading.as_ref().is_some_and(|(reading, _)| *reading == span) {
            self.reading = None;
        }
        match answer {
            Ok(events) => {
                self.loaded.insert(span, Loaded::Events(Rc::new(shown(events))));
                self.trace
                    .record(&TraceLine::new("calendar", "read").kind(self.mode.key()));
            }
            // a stopped engine keeps what is shown; a failed read says so while nothing was read (what the store kept
            // stays shown)
            Err(teams_core::EngineError::Stopped) => {}
            Err(_) => {
                self.loaded.entry(span).or_insert(Loaded::Failed);
            }
        }
        // asked again meanwhile, or the view moved on: read what it shows now
        if self.reading.is_none() && (self.read_again || self.span() != span) {
            let again = std::mem::take(&mut self.read_again);
            self.read(again, cx);
        }
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.control && modifiers.alt {
            let mode = match keystroke.key.as_str() {
                "1" => Mode::Day,
                "2" => Mode::WorkWeek,
                "3" => Mode::Week,
                "4" => Mode::Month,
                _ => return,
            };
            cx.stop_propagation();
            self.set_mode(mode, cx);
            return;
        }
        if modifiers.modified() {
            return;
        }
        match keystroke.key.as_str() {
            "left" => self.step(false, cx),
            "right" => self.step(true, cx),
            "t" => self.today(cx),
            "escape" if self.card.is_some() => self.close_card(cx),
            "escape" if self.picker.is_some() => {
                self.picker = None;
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The events of the span shown, when the account's calendar is ticked.
    fn visible(&self) -> Rc<Vec<Shown>> {
        if self.toggles.on(Toggle::Hidden) {
            return Rc::default();
        }
        self.events()
            .or_else(|| {
                // while the span is read, a span read before that holds its days
                let span = self.span();
                self.loaded.iter().find_map(|(read, loaded)| match loaded {
                    Loaded::Events(events) | Loaded::Stored(events)
                        if read.first <= span.first && read.end() >= span.end() =>
                    {
                        Some(Rc::clone(events))
                    }
                    _ => None,
                })
            })
            .unwrap_or_default()
    }

    pub(crate) fn say(&mut self, notice: &'static str, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }
}

/// The events a read gave as the calendar shows them: a declined meeting left out, by start.
fn shown(events: Vec<CalendarEvent>) -> Vec<Shown> {
    let mut shown: Vec<Shown> = events
        .into_iter()
        .filter(calendar::shows)
        .filter_map(|event| Shown::of(Rc::new(event)))
        .collect();
    shown.sort_by_key(|shown| shown.start);
    shown
}

/// Whether `event` is a channel's meeting the account `me` (its MRI) scheduled: its own calendar holds an attendee's
/// copy (`isOrganizer` `false`), but it organizes it, as Teams' card offers it.
#[must_use]
pub fn own_channel_meeting(event: &CalendarEvent, me: &str) -> bool {
    event
        .channel_meeting
        .as_ref()
        .is_some_and(|channel_meeting| channel_meeting.organized_by(me))
}

/// Where the channel `channel` holds a meeting, as the account's teams name it: its team's mailbox and the names the
/// form shows; `None` when no team of the account's (with a mailbox) holds it.
async fn place_of(core: &dyn Core, channel: &str) -> Option<crate::event_form::MeetingPlace> {
    let teams = core.teams().await.ok().flatten()?;
    let team = teams
        .iter()
        .find(|team| team.channels.iter().any(|known| known.id == channel))?;
    let known = team.channels.iter().find(|known| known.id == channel)?;
    Some(crate::event_form::MeetingPlace {
        channel: teams_core::MeetingChannel {
            channel: channel.to_owned(),
            mailbox: team.mailbox.clone()?,
        },
        team: team.name.clone(),
        name: known.name.clone(),
    })
}

/// What Edit says when the event can't be read.
pub const EVENT_UNREAD: &str = "The event couldn't be opened. Check the connection, then try again.";
/// What the card says when its Cancel meeting or Delete didn't go through.
pub const EVENT_NOT_REMOVED: &str = "The event couldn't be canceled or deleted. Check the connection, then try again.";

/// Where the card's Cancel meeting, Delete or Remove from calendar stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Removal {
    #[default]
    None,
    /// Asked: the card asks to confirm it.
    Asked,
    /// Confirmed: being sent.
    Sending,
}
/// What Add calendar says.
pub const ADD_CALENDAR_UNAVAILABLE: &str = "Adding other calendars isn't available yet.";

/// A colour `permille` thousandths of the way from `base` to `over`.
pub(crate) fn mix(base: u32, over: u32, permille: u32) -> u32 {
    let permille = permille.min(1000);
    let (base, over) = (base.to_be_bytes(), over.to_be_bytes());
    let channel =
        |index: usize| (u32::from(base[index]) * (1000 - permille) + u32::from(over[index]) * permille) / 1000;
    (channel(1) << 16) | (channel(2) << 8) | channel(3)
}

/// A thin vertical line between toolbar groups.
fn separator(palette: Palette) -> gpui::Div {
    div().w(px(1.0)).h(px(20.0)).mx_1().bg(rgb(palette.border))
}

/// A time of day on the clock the settings give.
fn time_text(time: NaiveDateTime, cycle: HourCycle) -> String {
    time.format(cycle.time_of_day()).to_string()
}

/// An hour's label in the gutter: "1 PM" or "13:00".
pub(crate) fn hour_label(hour: u32, cycle: HourCycle) -> String {
    match cycle {
        HourCycle::Twelve => {
            let shown = if hour.is_multiple_of(12) { 12 } else { hour % 12 };
            format!("{shown} {}", if hour < 12 { "AM" } else { "PM" })
        }
        HourCycle::TwentyFour => format!("{hour:02}:00"),
    }
}

impl Render for CalendarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let failed = matches!(self.loaded.get(&self.span()), Some(Loaded::Failed)) && self.reading.is_none();
        let failure = failed.then(|| {
            div()
                .debug_selector(|| "calendar-failed".to_owned())
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme::DANGER))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .child("The calendar couldn't be read. Check the connection, then refresh."),
                )
                .child(selected_by(
                    "calendar-retry",
                    Button::new("calendar-retry")
                        .label("Refresh")
                        .small()
                        .outline()
                        .on_click(cx.listener(|this, _, _, cx| this.read(true, cx))),
                ))
        });
        let notice = self.notice_bar(palette, cx);
        let grid = if self.mode == Mode::Month {
            self.month_view(palette, cx)
        } else {
            self.time_grid(palette, cx)
        };
        // another week, day or month fades in from a little below
        let grid = crate::motion::rise(
            grid,
            gpui::SharedString::from(format!("calendar-grid-{:?}-{:?}", self.mode, self.span())),
            8.0,
            cx,
        );
        let picker = self.picker_view(palette, cx);
        let card = self.card_view(palette, cx);
        let join_id = self.join_id_view(palette, cx);
        let main = div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .child(self.toolbar(palette, cx))
            .children(failure)
            .children(notice)
            .child(grid)
            .children(picker)
            .children(card)
            .children(join_id);
        div()
            .debug_selector(|| "calendar".to_owned())
            .key_context("Calendar")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .size_full()
            .flex()
            .when(!self.toggles.on(Toggle::PaneHidden), |calendar| {
                calendar.child(self.left_pane(palette, cx))
            })
            .child(main)
    }
}
