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
//!   last is shown at once while it is read again. A declined meeting isn't shown, as in Teams.
//! - **An event clicked** opens its card: its title, when, the series it belongs to, where, the organizer and the
//!   account's answer, who is invited, its text; for a Teams meeting Join (the call machinery's join of the meeting's
//!   running call, offered only once a call there is known: [`crate::calls::Calls::joinable`]), "Chat with
//!   participants" (the meeting's chat) and "Copy link". A build that shows no call, and the read-only mode, offer no
//!   Join.
//! - **New and Join with an ID** need writes no capture has shown (a meeting's creation, a meeting's lookup by its id
//!   and passcode): they say so rather than send a guess. So do "Add calendar" (other people's and shared calendars).
//! - **Keys** while the calendar has the focus: Left and Right the previous and next days, T today, Ctrl+Alt+1–4 the
//!   views, Escape closes a card or the month picker.
//! - **Traced**: `calendar shown` with the view's key, `calendar read` with the view's key when its events came,
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
use crate::calls::Calls;
use crate::clock::HourCycle;
use crate::icons::{Glyph, selected_by};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

mod card;
mod grid;
mod pane;

/// One hour's height in the time grid, px.
const HOUR_HEIGHT: f32 = 96.0;
/// The shortest an event's block is drawn, minutes (a quarter-hour event keeps a readable line).
const MIN_MINUTES: i64 = 21;
/// The time gutter's width, px.
const GUTTER: f32 = 60.0;
/// The left pane's width, px.
const PANE_WIDTH: f32 = 280.0;
/// How often the shown events are read again while the calendar is shown.
const REFRESH: StdDuration = StdDuration::from_secs(5 * 60);
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
    Join(String),
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
    /// The card of the event clicked.
    card: Option<Shown>,
    /// What a control that can't act yet says, until closed.
    notice: Option<&'static str>,
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
                let alive = this.update(cx, |view, cx| {
                    if view.toggles.on(Toggle::Shown) {
                        if refresh {
                            view.read(true, cx);
                        }
                        cx.notify();
                    }
                });
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
            card: None,
            notice: None,
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

    /// The events shown, once read.
    #[must_use]
    pub fn events(&self) -> Option<Rc<Vec<Shown>>> {
        match self.loaded.get(&self.span()) {
            Some(Loaded::Events(events)) => Some(Rc::clone(events)),
            _ => None,
        }
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
        cx.notify();
    }

    /// Reads the span shown, unless it was read (`again` reads it anyway) or is being read.
    fn read(&mut self, again: bool, cx: &mut Context<Self>) {
        let span = self.span();
        if self.reading.as_ref().is_some_and(|(reading, _)| *reading == span) {
            return;
        }
        if !again && matches!(self.loaded.get(&span), Some(Loaded::Events(_))) {
            return;
        }
        let (start, end) = span.window_ms();
        let reply = self.core.calendar(start, end);
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |view, cx| view.read_over(span, answer, cx));
        });
        self.reading = Some((span, task));
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
                let mut shown: Vec<Shown> = events
                    .into_iter()
                    .filter(calendar::shows)
                    .filter_map(|event| Shown::of(Rc::new(event)))
                    .collect();
                shown.sort_by_key(|shown| shown.start);
                self.loaded.insert(span, Loaded::Events(Rc::new(shown)));
                self.trace
                    .record(&TraceLine::new("calendar", "read").kind(self.mode.key()));
            }
            // a stopped engine keeps what is shown; a failed read says so while nothing was read
            Err(teams_core::EngineError::Stopped) => {}
            Err(_) => {
                self.loaded.entry(span).or_insert(Loaded::Failed);
            }
        }
        // the view moved on meanwhile: read what it shows now
        if self.reading.is_none() && self.span() != span {
            self.read(false, cx);
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
                    Loaded::Events(events) if read.first <= span.first && read.end() >= span.end() => {
                        Some(Rc::clone(events))
                    }
                    _ => None,
                })
            })
            .unwrap_or_default()
    }

    fn say(&mut self, notice: &'static str, cx: &mut Context<Self>) {
        self.notice = Some(notice);
        cx.notify();
    }
}

/// What New says: a meeting's creation was never captured.
pub const NEW_UNAVAILABLE: &str =
    "Scheduling a meeting from here isn't available yet. Schedule it in Teams on the web or in Outlook.";
/// What Join with an ID says: joining by a meeting's id and passcode was never captured.
pub const JOIN_ID_UNAVAILABLE: &str =
    "Joining by a meeting ID isn't available yet. Open the meeting from your calendar or its link instead.";
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
fn hour_label(hour: u32, cycle: HourCycle) -> String {
    match cycle {
        HourCycle::Twelve => {
            let shown = if hour % 12 == 0 { 12 } else { hour % 12 };
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
        let picker = self.picker_view(palette, cx);
        let card = self.card_view(palette, cx);
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
            .children(card);
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
