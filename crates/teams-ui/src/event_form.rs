//! The scheduling form ("New event - Calendar"), in a window of its own as Teams opens it: a new event from the
//! calendar's New (or its menu, or a double click on an empty slot of the grid), or an event of the account's opened
//! to change (its card's Edit, or a double click on it).
//!
//! - **Its toolbar**: Event (Series, a repeating event, isn't offered yet), Show as, the reminder, the categories,
//!   private, Print (not offered) and Save (Send while it invites anyone, Send update for a meeting's change); on a
//!   change also Cancel meeting (the organizer's meeting with attendees, with a note for them) or Delete.
//! - **Its form**: the title; the required attendees and, by the attendee options, the optional ones (people the
//!   account knows by name, picked from the suggestions, or any address typed in full); the date and times (a start
//!   and an end every half hour, all day); the location; the Microsoft Teams meeting switch; the text (plain lines; an
//!   event's own text goes back as it came while it isn't edited).
//! - **Its day**: the date's hours beside the form, the new event's block in green over the day's events, moved by a
//!   drag (its top and bottom handles change its start and end) or a click on an empty half hour.
//! - **Save** goes through the engine (`teams_core::calendar`): one write, under the fence; the window closes once it
//!   went through and the calendar reads its days again, and stays open saying why when it didn't.
//! - **Read-only** (`Core::read_only`): the form shows the event and offers no Save, Cancel or Delete.
//! - **Traced**: `event-form opened` (`new` or `change`), `event-form saved` / `not-saved` with the write's kind.

mod day;
pub(crate) mod model;

use std::rc::Rc;
use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate, NaiveDateTime};
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Bounds, Context, Entity, FocusHandle, Focusable as _, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowHandle, WindowOptions, div, prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use serde_json::{Map, Value};
use teams_core::{CalendarEvent, EventAttendee, EventShowAs, EventWrite, FullEvent, Invitable};

use crate::bridge::Core;
use crate::calendar::{self, Shown};
use crate::calendar_view::mix;
use crate::clock;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::window::KitRoot;

pub use model::Slot;
use model::{CATEGORIES, Fields, REMINDERS};

/// How the window opens, px.
const DEFAULT_SIZE: (f32, f32) = (1400.0, 900.0);
const MIN_SIZE: (f32, f32) = (900.0, 640.0);

/// What the form opens on.
#[derive(Clone, Debug)]
pub enum Seed {
    /// A new event in `slot`, a Teams meeting when `online`.
    New { slot: Slot, online: bool },
    /// An event of the account's, read whole, to change.
    Change(Box<FullEvent>),
}

/// What a Save that went through tells the calendar: read its days again.
#[derive(Clone)]
pub struct Saved(Rc<dyn Fn(&mut App)>);

impl Saved {
    #[must_use]
    pub fn new(saved: impl Fn(&mut App) + 'static) -> Self {
        Self(Rc::new(saved))
    }

    /// Nothing to tell (a test's form).
    #[must_use]
    pub fn nothing() -> Self {
        Self::new(|_| {})
    }
}

/// The app id the form's window opens with: the main window's ([`set_app_id`]).
struct AppId(String);

impl Global for AppId {}

/// Opens the form's windows with app id `app_id` (the main window's) from now on.
pub fn set_app_id(app_id: &str, cx: &mut App) {
    cx.set_global(AppId(app_id.to_owned()));
}

/// The form's window, while it is open.
struct Open {
    window: WindowHandle<KitRoot>,
    form: WeakEntity<EventForm>,
}

impl Global for Open {}

/// Opens the form on `seed` in a window of its own, once the update under way is over; an open form's window is
/// brought to the front instead (one form at a time, as one event is edited at a time).
pub fn open(core: Arc<dyn Core>, trace: Arc<ViewTrace>, seed: Seed, saved: Saved, cx: &mut App) {
    cx.defer(move |cx| {
        if let Some((window, _)) = alive(cx) {
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let title: SharedString = match &seed {
            Seed::New { .. } => "New event - Calendar".into(),
            Seed::Change(full) if !full.event.subject.trim().is_empty() => {
                format!("{} - Calendar", full.event.subject.trim()).into()
            }
            Seed::Change(_) => "Event - Calendar".into(),
        };
        let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                ..TitlebarOptions::default()
            }),
            app_id: cx.try_global::<AppId>().map(|id| id.0.clone()),
            window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
            ..WindowOptions::default()
        };
        let mut made = None;
        let opened = cx.open_window(options, |window, cx| {
            let form = cx.new(|cx| EventForm::new(core, trace, seed, saved, window, cx));
            made = Some(form.downgrade());
            cx.new(|cx| KitRoot::new(form, window, cx))
        });
        if let (Ok(window), Some(form)) = (opened, made) {
            cx.set_global(Open { window, form });
        }
    });
}

/// The form, while its window is open.
#[must_use]
pub fn shown(cx: &App) -> Option<Entity<EventForm>> {
    alive(cx).map(|(_, form)| form)
}

/// The form's window, while it is open.
#[must_use]
pub fn window(cx: &App) -> Option<WindowHandle<KitRoot>> {
    alive(cx).map(|(window, _)| window)
}

/// Closes the form's window, if it is open (nothing is saved).
pub fn close(cx: &mut App) {
    let Some((window, _)) = alive(cx) else { return };
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    });
}

fn alive(cx: &App) -> Option<(WindowHandle<KitRoot>, Entity<EventForm>)> {
    let open = cx.try_global::<Open>()?;
    let id = open.window.window_id();
    if !cx.windows().iter().any(|window| window.window_id() == id) {
        return None;
    }
    Some((open.window, open.form.upgrade()?))
}

/// The event a change edits.
#[derive(Clone, Debug)]
struct Editing {
    id: String,
    /// The event as read: a change sends it back with the form's fields over it.
    raw: Map<String, Value>,
    event: CalendarEvent,
}

/// Which list of attendees an attendee box adds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Required,
    Optional,
}

/// What asks the user to confirm before it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Cancel,
    Delete,
}

/// A drag of the new event's block in the day beside the form: what moves, from where, and the slot it began on.
#[derive(Clone, Copy, Debug)]
struct Drag {
    part: day::Part,
    from: Pixels,
    slot: Slot,
}

/// The scheduling form (see the module docs).
pub struct EventForm {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    focus: FocusHandle,
    editing: Option<Editing>,
    title: Entity<InputState>,
    required: Entity<InputState>,
    optional: Entity<InputState>,
    location: Entity<InputState>,
    body: Entity<TextareaState>,
    cancel_note: Entity<InputState>,
    /// The text as the form opened with it, and the event's HTML of it: sent back as it came while unchanged.
    body_seed: (String, Option<String>),
    attendees: Vec<EventAttendee>,
    optional_shown: bool,
    slot: Slot,
    online: bool,
    show_as: EventShowAs,
    reminder: Option<u32>,
    private: bool,
    categories: Vec<String>,
    /// The suggestions under an attendee box, for that box.
    suggestions: Option<(Need, Vec<Invitable>)>,
    suggesting: Option<Task<()>>,
    /// The day beside the form, and its events once read.
    day: NaiveDate,
    day_events: Rc<Vec<Shown>>,
    day_read: Option<Task<()>>,
    day_scroll: ScrollHandle,
    drag: Option<Drag>,
    confirm: Option<Confirm>,
    saving: Option<Task<()>>,
    problem: Option<SharedString>,
    saved: Saved,
    _subscriptions: Vec<Subscription>,
}

impl EventForm {
    /// The form on `seed`, its title focused.
    pub fn new(
        core: Arc<dyn Core>,
        trace: Arc<ViewTrace>,
        seed: Seed,
        saved: Saved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let title = input("Add title", window, cx);
        let required = input("Invite required attendees", window, cx);
        let optional = input("Invite optional attendees", window, cx);
        let location = input("Add a room or location", window, cx);
        let cancel_note = input("Add a message for the attendees (optional)", window, cx);
        let body = cx.new(|cx| TextareaState::new(window, cx).rows(12));
        let subscriptions = vec![
            cx.subscribe_in(
                &required,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    this.attendee_input(Need::Required, event, window, cx);
                },
            ),
            cx.subscribe_in(
                &optional,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    this.attendee_input(Need::Optional, event, window, cx);
                },
            ),
        ];
        let now = calendar::now().naive_local();
        let mut form = Self {
            core,
            trace,
            focus: cx.focus_handle(),
            editing: None,
            title,
            required,
            optional,
            location,
            body,
            cancel_note,
            body_seed: (String::new(), None),
            attendees: Vec::new(),
            optional_shown: false,
            slot: Slot::next_half_hour(now),
            online: false,
            show_as: EventShowAs::Busy,
            reminder: Some(15),
            private: false,
            categories: Vec::new(),
            suggestions: None,
            suggesting: None,
            day: now.date(),
            day_events: Rc::default(),
            day_read: None,
            day_scroll: ScrollHandle::new(),
            drag: None,
            confirm: None,
            saving: None,
            problem: None,
            saved,
            _subscriptions: subscriptions,
        };
        let kind = match seed {
            Seed::New { slot, online } => {
                form.slot = slot;
                form.online = online;
                "new"
            }
            Seed::Change(full) => {
                form.fill(*full, window, cx);
                "change"
            }
        };
        form.day = form.slot.start.date();
        form.day_scroll
            .set_offset(gpui::point(px(0.0), px(-day::scroll_to(form.slot.start.time()))));
        form.trace.record(&TraceLine::new("event-form", "opened").kind(kind));
        form.read_day(cx);
        form.title.update(cx, |title, cx| title.focus(window, cx));
        form
    }

    /// Fills the form from an event read whole.
    fn fill(&mut self, full: FullEvent, window: &mut Window, cx: &mut Context<Self>) {
        let event = full.event.clone();
        if let Some(shown) = Shown::of(Rc::new(event.clone())) {
            self.slot = Slot {
                start: shown.start,
                end: shown.end,
                all_day: event.all_day,
            };
        }
        self.online = event.online;
        self.show_as = match &event.show_as {
            teams_core::ShowAs::Free => EventShowAs::Free,
            teams_core::ShowAs::Tentative => EventShowAs::Tentative,
            teams_core::ShowAs::Away => EventShowAs::Away,
            teams_core::ShowAs::WorkingElsewhere => EventShowAs::WorkingElsewhere,
            teams_core::ShowAs::Busy | teams_core::ShowAs::Other(_) => EventShowAs::Busy,
        };
        self.reminder = full.reminder_minutes;
        self.private = event.private;
        self.categories = full.categories.clone();
        let organizer = event.organizer_address.as_deref().map(str::to_lowercase);
        self.attendees = event
            .attendees
            .iter()
            .filter_map(|attendee| {
                let address = attendee.address.clone()?;
                // the organizer is the account itself: listed by the service, never invited by the form
                (organizer.as_deref() != Some(address.to_lowercase().as_str())).then(|| EventAttendee {
                    required: !full.optional.contains(&address.to_lowercase()),
                    address,
                    name: attendee.name.clone(),
                    mri: None,
                })
            })
            .collect();
        self.optional_shown = self.attendees.iter().any(|attendee| !attendee.required);
        let text = full.body_html.as_deref().map(model::html_text).unwrap_or_default();
        self.body_seed = (text.clone(), full.body_html.clone());
        let subject = event.subject.clone();
        let location = event.location.clone().unwrap_or_default();
        self.title.update(cx, |input, cx| input.set_value(subject, window, cx));
        self.location
            .update(cx, |input, cx| input.set_value(location, window, cx));
        self.body.update(cx, |input, cx| input.set_value(text, window, cx));
        self.editing = Some(Editing {
            id: event.id.clone(),
            raw: full.raw,
            event,
        });
    }

    /// The slot the event takes.
    #[must_use]
    pub fn slot(&self) -> Slot {
        self.slot
    }

    /// Whether it is a Teams meeting.
    #[must_use]
    pub fn online(&self) -> bool {
        self.online
    }

    /// Whom it invites.
    #[must_use]
    pub fn attendees(&self) -> &[EventAttendee] {
        &self.attendees
    }

    /// Whether it changes an event that exists.
    #[must_use]
    pub fn changes(&self) -> bool {
        self.editing.is_some()
    }

    /// What the form holds.
    fn fields(&self, cx: &App) -> Fields {
        let body = self.body.read(cx).value().to_string();
        let body_html = (body == self.body_seed.0).then(|| self.body_seed.1.clone()).flatten();
        Fields {
            subject: self.title.read(cx).value().to_string(),
            slot: self.slot,
            attendees: self.attendees.clone(),
            location: self.location.read(cx).value().to_string(),
            body,
            body_html,
            online: self.online,
            show_as: self.show_as,
            reminder: self.reminder,
            private: self.private,
            categories: self.categories.clone(),
        }
    }

    /// Moves the event to `slot`, and the day beside it to its date.
    pub fn set_slot(&mut self, slot: Slot, cx: &mut Context<Self>) {
        self.slot = slot;
        if slot.start.date() != self.day {
            self.day = slot.start.date();
            self.read_day(cx);
        }
        cx.notify();
    }

    /// Shows `date` beside the form (the event stays where it is).
    fn show_day(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        self.day = date;
        self.read_day(cx);
        cx.notify();
    }

    /// Reads the day beside the form's events.
    fn read_day(&mut self, cx: &mut Context<Self>) {
        let span = calendar::Span::of(self.day, calendar::Mode::Day);
        let (start, end) = span.window_ms();
        let reply = self.core.calendar(start, end);
        let day = self.day;
        self.day_read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |form, cx| {
                if form.day != day {
                    return;
                }
                if let Ok(events) = answer {
                    let editing = form.editing.as_ref().map(|editing| editing.id.clone());
                    let mut shown: Vec<Shown> = events
                        .into_iter()
                        .filter(calendar::shows)
                        .filter(|event| Some(&event.id) != editing.as_ref())
                        .filter_map(|event| Shown::of(Rc::new(event)))
                        .collect();
                    shown.sort_by_key(|shown| shown.start);
                    form.day_events = Rc::new(shown);
                }
                cx.notify();
            });
        }));
    }

    /// An attendee box changed: suggest people whose names hold what it says; Enter takes the first suggestion or a
    /// whole address.
    fn attendee_input(&mut self, need: Need, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        let text = input.read(cx).value().to_string();
        match event {
            InputEvent::PressEnter { .. } => {
                let first = self
                    .suggestions
                    .as_ref()
                    .filter(|(for_need, _)| *for_need == need)
                    .and_then(|(_, found)| found.first().cloned());
                if let Some(person) = first {
                    self.invite_person(&person, need, window, cx);
                } else {
                    self.invite_typed(need, window, cx);
                }
            }
            InputEvent::Change => {
                let query = text.trim().to_owned();
                if query.chars().count() < 2 {
                    self.suggestions = None;
                    self.suggesting = None;
                    cx.notify();
                    return;
                }
                if query.ends_with([';', ',']) {
                    self.invite_typed(need, window, cx);
                    return;
                }
                let reply = self.core.invitable(query.clone());
                self.suggesting = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    let found = reply.await.unwrap_or_default();
                    let _ = this.update(cx, |form, cx| {
                        let input = match need {
                            Need::Required => &form.required,
                            Need::Optional => &form.optional,
                        };
                        // an answer to what the box no longer says is dropped
                        if input.read(cx).value().trim() == query {
                            form.suggestions = Some((need, found));
                            cx.notify();
                        }
                    });
                }));
            }
            _ => {}
        }
    }

    /// Invites the address an attendee box holds, when it is one.
    fn invite_typed(&mut self, need: Need, window: &mut Window, cx: &mut Context<Self>) {
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        let text = input.read(cx).value().to_string();
        match model::typed_attendee(&text, need == Need::Required) {
            Some(attendee) => {
                self.add_attendee(attendee);
                input.update(cx, |input, cx| input.set_value("", window, cx));
                self.suggestions = None;
                self.problem = None;
            }
            None if !text.trim().is_empty() => {
                self.problem = Some("Type a whole email address, or pick someone from the suggestions.".into());
            }
            None => {}
        }
        cx.notify();
    }

    /// Invites `person` from the suggestions.
    fn invite_person(&mut self, person: &Invitable, need: Need, window: &mut Window, cx: &mut Context<Self>) {
        self.add_attendee(EventAttendee {
            address: person.address.clone(),
            name: Some(person.name.clone()),
            mri: Some(person.mri.clone()),
            required: need == Need::Required,
        });
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        input.update(cx, |input, cx| input.set_value("", window, cx));
        self.suggestions = None;
        self.problem = None;
        cx.notify();
    }

    fn add_attendee(&mut self, attendee: EventAttendee) {
        let address = attendee.address.to_lowercase();
        self.attendees.retain(|known| known.address.to_lowercase() != address);
        self.attendees.push(attendee);
    }

    /// Fills the form in as a person typing would: `subject`, each of `invited` (address, name, required), a Teams
    /// meeting with `text` (the UI tour's step).
    pub fn fill_in(
        &mut self,
        subject: &str,
        invited: &[(&str, &str, bool)],
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subject = subject.to_owned();
        self.title.update(cx, |input, cx| input.set_value(subject, window, cx));
        for (address, name, required) in invited {
            self.add_attendee(EventAttendee {
                address: (*address).to_owned(),
                name: Some((*name).to_owned()),
                mri: None,
                required: *required,
            });
            self.optional_shown |= !required;
        }
        let text = text.to_owned();
        self.body.update(cx, |input, cx| input.set_value(text, window, cx));
        self.online = true;
        cx.notify();
    }

    /// Takes the attendee `address` off the invitation.
    pub fn remove_attendee(&mut self, address: &str, cx: &mut Context<Self>) {
        let address = address.to_lowercase();
        self.attendees.retain(|known| known.address.to_lowercase() != address);
        cx.notify();
    }

    /// Whether the form may send anything: not in the read-only mode, not while a save is under way.
    fn may_write(&self) -> bool {
        !self.core.read_only() && self.saving.is_none()
    }

    /// Save (or Send, Send update): the event created or changed, through the engine.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may_write() {
            return;
        }
        // an address still in a box is invited, as Teams does on Send
        for need in [Need::Required, Need::Optional] {
            let input = match need {
                Need::Required => &self.required,
                Need::Optional => &self.optional,
            };
            if !input.read(cx).value().trim().is_empty() {
                self.invite_typed(need, window, cx);
            }
        }
        let draft = self.fields(cx).draft();
        let write = match &self.editing {
            Some(editing) => EventWrite::Update {
                id: editing.id.clone(),
                event: editing.raw.clone(),
                draft,
            },
            None => EventWrite::Create(draft),
        };
        self.send(write, cx);
    }

    /// Cancel meeting or Delete, once confirmed.
    fn remove(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        let Some(editing) = &self.editing else { return };
        if !self.may_write() {
            return;
        }
        let write = match confirm {
            Confirm::Cancel => EventWrite::Cancel {
                id: editing.id.clone(),
                chat: editing.event.meeting_chat.clone(),
                message: Some(self.cancel_note.read(cx).value().to_string()).filter(|note| !note.trim().is_empty()),
            },
            Confirm::Delete => EventWrite::Delete { id: editing.id.clone() },
        };
        self.confirm = None;
        self.send(write, cx);
    }

    fn send(&mut self, write: EventWrite, cx: &mut Context<Self>) {
        let kind = write.kind();
        self.problem = None;
        let reply = self.core.write_event(write);
        self.saving = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |form, cx| {
                form.saving = None;
                match answer {
                    Ok(_) => {
                        form.trace.record(&TraceLine::new("event-form", "saved").kind(kind));
                        (form.saved.0)(cx);
                        form.close(cx);
                    }
                    Err(error) => {
                        form.trace.record(&TraceLine::new("event-form", "not-saved").kind(kind));
                        form.problem = Some(format!("{}", capitalized(&error.to_string())).into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Closes the form's window.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some((window, _)) = alive(cx) {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" if self.confirm.is_some() => self.confirm = None,
            "escape" if self.suggestions.is_some() => self.suggestions = None,
            "s" if keystroke.modifiers.control && !keystroke.modifiers.alt => self.save(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The label of the form's Save: Send update for a change that tells attendees, Send for a new event inviting
    /// anyone, else Save.
    fn save_label(&self) -> &'static str {
        match (&self.editing, self.attendees.is_empty()) {
            (Some(_), false) => "Send update",
            (None, false) => "Send",
            _ => "Save",
        }
    }
}

/// `text` with its first letter capitalized.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A raised box of the form.
fn card(palette: Palette) -> gpui::Div {
    div()
        .rounded_lg()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
}

/// One of the form's rows: its icon, and what it holds over a line.
fn field_row(palette: Palette, glyph: Glyph, content: impl IntoElement, focused: bool) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(40.0))
                .flex_none()
                .flex()
                .justify_center()
                .child(glyph.icon().size(px(18.0)).text_color(rgb(palette.muted_foreground))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h(px(48.0))
                .flex()
                .items_center()
                .gap_2()
                .when(focused, |line| line.border_b_2().border_color(rgb(palette.brand)))
                .when(!focused, |line| line.border_b_1().border_color(rgb(palette.border)))
                .child(content),
        )
}

impl EventForm {
    fn toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let writes = !self.core.read_only();
        let form = cx.entity().downgrade();
        let show_as = self.show_as;
        let show_as_menu = {
            let form = form.clone();
            Button::new("event-show-as")
                .ghost()
                .small()
                .tooltip("Show as")
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(Glyph::CALENDAR_CLOCK.icon().size(px(16.0)))
                        .child(model::show_as_label(show_as))
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    EventShowAs::ALL.iter().fold(menu.min_w(px(200.0)), |menu, choice| {
                        let (form, choice) = (form.clone(), *choice);
                        menu.item(
                            PopupMenuItem::new(model::show_as_label(choice))
                                .checked(choice == show_as)
                                .on_click(move |_, _, cx: &mut App| {
                                    let _ = form.update(cx, |form, cx| {
                                        form.show_as = choice;
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                })
        };
        let reminder = self.reminder;
        let reminder_menu = {
            let form = form.clone();
            Button::new("event-reminder")
                .ghost()
                .small()
                .tooltip(format!("Reminder: {}", model::reminder_label(reminder)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(Glyph::ALARM.icon().size(px(16.0)))
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    REMINDERS.iter().fold(menu.min_w(px(200.0)), |menu, (choice, label)| {
                        let (form, choice) = (form.clone(), *choice);
                        menu.item(PopupMenuItem::new(*label).checked(choice == reminder).on_click(
                            move |_, _, cx: &mut App| {
                                let _ = form.update(cx, |form, cx| {
                                    form.reminder = choice;
                                    cx.notify();
                                });
                            },
                        ))
                    })
                })
        };
        let categories = self.categories.clone();
        let category_menu = {
            let form = form.clone();
            let tip = if categories.is_empty() {
                "Categorize".to_owned()
            } else {
                format!("Categories: {}", categories.join(", "))
            };
            Button::new("event-category")
                .ghost()
                .small()
                .tooltip(tip)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(Glyph::TAG.icon().size(px(16.0)))
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    let mut names: Vec<String> = CATEGORIES.iter().map(|name| (*name).to_owned()).collect();
                    names.extend(
                        categories
                            .iter()
                            .filter(|name| !CATEGORIES.contains(&name.as_str()))
                            .cloned(),
                    );
                    names.into_iter().fold(menu.min_w(px(200.0)), |menu, name| {
                        let form = form.clone();
                        let chosen = categories.contains(&name);
                        menu.item(PopupMenuItem::new(name.clone()).checked(chosen).on_click(
                            move |_, _, cx: &mut App| {
                                let _ = form.update(cx, |form, cx| {
                                    if chosen {
                                        form.categories.retain(|known| *known != name);
                                    } else {
                                        form.categories.push(name.clone());
                                    }
                                    cx.notify();
                                });
                            },
                        ))
                    })
                })
        };
        let private = self.private;
        let privacy_menu = {
            let form = form.clone();
            Button::new("event-privacy")
                .ghost()
                .small()
                .tooltip(if private { "Private" } else { "Not private" })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            if private { Glyph::LOCK } else { Glyph::LOCK_OPEN }
                                .icon()
                                .size(px(16.0)),
                        )
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                )
                .dropdown_menu(move |menu, _, _| {
                    let form = form.clone();
                    menu.min_w(px(200.0))
                        .item(
                            PopupMenuItem::new("Private")
                                .checked(private)
                                .on_click(move |_, _, cx: &mut App| {
                                    let _ = form.update(cx, |form, cx| {
                                        form.private = !form.private;
                                        cx.notify();
                                    });
                                }),
                        )
                })
        };
        let removing = self.editing.as_ref().filter(|_| writes).map(|editing| {
            let meeting = editing.event.is_organizer && !editing.event.attendees.is_empty();
            let (id, label, glyph, confirm) = if meeting {
                (
                    "event-cancel-meeting",
                    "Cancel meeting",
                    Glyph::CALENDAR_CANCEL,
                    Confirm::Cancel,
                )
            } else {
                ("event-delete", "Delete", Glyph::TRASH, Confirm::Delete)
            };
            selected_by(
                id,
                Button::new(id)
                    .ghost()
                    .small()
                    .icon(glyph.icon())
                    .label(label)
                    .text_color(rgb(theme::DANGER))
                    .disabled(self.saving.is_some())
                    .on_click(cx.listener(move |form, _, _, cx| {
                        form.confirm = Some(confirm);
                        cx.notify();
                    })),
            )
        });
        let save_label = self.save_label();
        let save = selected_by(
            "event-save",
            Button::new("event-save")
                .primary()
                .icon(Glyph::SAVE.icon())
                .label(if self.saving.is_some() { "Saving…" } else { save_label })
                .disabled(!self.may_write())
                .tooltip(if writes {
                    "Save (Ctrl+S)"
                } else {
                    "Read-only: nothing is saved"
                })
                .on_click(cx.listener(|form, _, window, cx| form.save(window, cx))),
        );
        let separator = || div().w(px(1.0)).h(px(20.0)).mx_1().bg(rgb(palette.border));
        card(palette)
            .debug_selector(|| "event-toolbar".to_owned())
            .h(px(56.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                "event-kind-event",
                Button::new("event-kind-event")
                    .outline()
                    .small()
                    .icon(Glyph::CALENDAR.icon())
                    .label("Event")
                    .selected(true),
            ))
            .child(selected_by(
                "event-kind-series",
                Button::new("event-kind-series")
                    .ghost()
                    .small()
                    .icon(Glyph::REPEAT.icon())
                    .label("Series")
                    .disabled(true)
                    .tooltip("Repeating events aren't available yet"),
            ))
            .child(separator())
            .child(selected_by("event-show-as", show_as_menu))
            .child(selected_by("event-reminder", reminder_menu))
            .child(selected_by("event-category", category_menu))
            .child(selected_by("event-privacy", privacy_menu))
            .child(separator())
            .child(selected_by(
                "event-print",
                icon_button("event-print", Glyph::PRINTER, "Printing isn't available yet")
                    .small()
                    .disabled(true),
            ))
            .child(div().flex_1())
            .children(removing)
            .child(save)
    }

    fn title_row(&self, palette: Palette, window: &Window, cx: &App) -> gpui::Div {
        let focused = self.title.read(cx).focus_handle(cx).is_focused(window);
        div()
            .debug_selector(|| "event-title".to_owned())
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(40.0))
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .child(Glyph::CALENDAR_ADD.icon().size(px(20.0)).text_color(rgb(palette.brand))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(48.0))
                    .flex()
                    .items_center()
                    .text_xl()
                    .when(focused, |line| line.border_b_2().border_color(rgb(palette.brand)))
                    .when(!focused, |line| line.border_b_1().border_color(rgb(palette.border)))
                    .child(
                        Input::new(&self.title)
                            .appearance(false)
                            .with_size(gpui_component::Size::Large),
                    ),
            )
    }

    fn attendees_row(&self, need: Need, palette: Palette, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let input = match need {
            Need::Required => &self.required,
            Need::Optional => &self.optional,
        };
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let chips: Vec<AnyElement> = self
            .attendees
            .iter()
            .filter(|attendee| attendee.required == (need == Need::Required))
            .map(|attendee| self.chip(attendee, palette, cx))
            .collect();
        let options = (need == Need::Required).then(|| {
            let form = cx.entity().downgrade();
            let shown = self.optional_shown;
            selected_by(
                "event-attendee-options",
                Button::new("event-attendee-options")
                    .ghost()
                    .small()
                    .tooltip("Attendee options")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Glyph::USER_COG.icon().size(px(16.0)))
                            .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                    )
                    .dropdown_menu(move |menu, _, _| {
                        let form = form.clone();
                        menu.min_w(px(220.0))
                            .item(PopupMenuItem::new("Optional attendees").checked(shown).on_click(
                                move |_, _, cx: &mut App| {
                                    let _ = form.update(cx, |form, cx| {
                                        form.optional_shown = !form.optional_shown;
                                        cx.notify();
                                    });
                                },
                            ))
                    }),
            )
        });
        let selector = match need {
            Need::Required => "event-attendees",
            Need::Optional => "event-optional-attendees",
        };
        let content = div()
            .debug_selector(move || selector.to_owned())
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .py_1()
            .children(chips)
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.0))
                    .child(Input::new(input).appearance(false)),
            )
            .children(options);
        let glyph = if need == Need::Required {
            Glyph::USERS
        } else {
            Glyph::USER
        };
        field_row(palette, glyph, content, focused)
    }

    fn chip(&self, attendee: &EventAttendee, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let address = attendee.address.clone();
        let label = attendee.name.clone().unwrap_or_else(|| attendee.address.clone());
        let id = SharedString::from(format!("event-attendee-{}", attendee.address.to_lowercase()));
        div()
            .id(id.clone())
            .h(px(28.0))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .bg(rgb(palette.hover))
            .text_sm()
            .tooltip({
                let address = attendee.address.clone();
                move |window, cx| gpui_component::tooltip::Tooltip::new(address.clone()).build(window, cx)
            })
            .child(
                div()
                    .max_w(px(220.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                icon_button(SharedString::from(format!("{id}-remove")), Glyph::X, "Remove")
                    .xsmall()
                    .on_click(cx.listener(move |form, _, _, cx| form.remove_attendee(&address, cx))),
            )
            .into_any_element()
    }

    /// The suggestions under the attendee box they are for.
    fn suggestions_view(&self, need: Need, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (for_need, found) = self.suggestions.as_ref()?;
        if *for_need != need {
            return None;
        }
        let input = match need {
            Need::Required => &self.required,
            Need::Optional => &self.optional,
        };
        let typed = input.read(cx).value().to_string();
        let typed_row = model::typed_attendee(&typed, true).map(|attendee| {
            div()
                .id("event-suggestion-typed")
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(move |row| row.bg(rgb(palette.hover)))
                .child(Glyph::PLUS.icon().size(px(14.0)))
                .child(format!("Invite {}", attendee.address))
                .on_click(cx.listener(move |form, _, window, cx| form.invite_typed(need, window, cx)))
        });
        let rows: Vec<AnyElement> = found
            .iter()
            .map(|person| {
                let picked = person.clone();
                let selector = format!("event-suggestion-{}", person.address.to_lowercase());
                div()
                    .debug_selector(move || selector.clone())
                    .id(SharedString::from(format!("event-suggestion-{}", person.mri)))
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .child(person.name.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(person.address.clone()),
                    )
                    .on_click(cx.listener(move |form, _, window, cx| form.invite_person(&picked, need, window, cx)))
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() && typed_row.is_none() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "event-suggestions".to_owned())
                .ml(px(52.0))
                .w(px(360.0))
                .py_1()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .children(rows)
                .children(typed_row),
        )
    }

    fn when_row(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let cycle = clock::cycle(cx);
        let slot = self.slot;
        let form = cx.entity().downgrade();
        let date_menu = {
            let form = form.clone();
            Button::new("event-date")
                .ghost()
                .small()
                .tooltip("Date")
                .label(slot.start.format("%a %-m/%-d/%Y").to_string())
                .dropdown_menu(move |menu, _, _| {
                    let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(420.0));
                    (0..42).fold(menu, |menu, offset| {
                        let date = Local::now().date_naive() + Duration::days(offset - 7);
                        let form = form.clone();
                        menu.item(
                            PopupMenuItem::new(date.format("%a %-m/%-d/%Y").to_string())
                                .checked(date == slot.start.date())
                                .on_click(move |_, _, cx: &mut App| {
                                    let _ = form.update(cx, |form, cx| form.set_slot(form.slot.on(date), cx));
                                }),
                        )
                    })
                })
        };
        let time_text = |time: NaiveDateTime| time.format(cycle.time_of_day()).to_string();
        let start_menu = (!slot.all_day).then(|| {
            let form = form.clone();
            Button::new("event-start")
                .ghost()
                .small()
                .tooltip("Start time")
                .label(time_text(slot.start))
                .dropdown_menu(move |menu, _, _| {
                    let menu = menu.min_w(px(160.0)).scrollable(true).max_h(px(420.0));
                    model::day_times(slot.start.date())
                        .into_iter()
                        .fold(menu, |menu, time| {
                            let form = form.clone();
                            menu.item(
                                PopupMenuItem::new(time.format(cycle.time_of_day()).to_string())
                                    .checked(time == slot.start)
                                    .on_click(move |_, _, cx: &mut App| {
                                        let _ = form.update(cx, |form, cx| form.set_slot(form.slot.starting(time), cx));
                                    }),
                            )
                        })
                })
        });
        let end_menu = (!slot.all_day).then(|| {
            let form = form.clone();
            Button::new("event-end")
                .ghost()
                .small()
                .tooltip("End time")
                .label(time_text(slot.end))
                .dropdown_menu(move |menu, _, _| {
                    let menu = menu.min_w(px(200.0)).scrollable(true).max_h(px(420.0));
                    model::end_times(slot.start)
                        .into_iter()
                        .fold(menu, |menu, (time, length)| {
                            let form = form.clone();
                            menu.item(
                                PopupMenuItem::new(format!("{} ({length})", time.format(cycle.time_of_day())))
                                    .checked(time == slot.end)
                                    .on_click(move |_, _, cx: &mut App| {
                                        let _ = form.update(cx, |form, cx| form.set_slot(form.slot.ending(time), cx));
                                    }),
                            )
                        })
                })
        });
        let all_day = Switch::new("event-all-day")
            .checked(slot.all_day)
            .label("All day")
            .on_click(cx.listener(|form, checked: &bool, _, cx| {
                let slot = form.slot.with_all_day(*checked);
                form.set_slot(slot, cx);
            }));
        let content = div()
            .debug_selector(|| "event-when".to_owned())
            .flex_1()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by("event-date", date_menu))
            .children(start_menu.map(|menu| selected_by("event-start", menu)))
            .when(!slot.all_day, |line| line.child("-"))
            .children(end_menu.map(|menu| selected_by("event-end", menu)))
            .child(div().flex_1())
            .child(selected_by("event-all-day", all_day))
            .child(selected_by(
                "event-scheduler",
                Button::new("event-scheduler")
                    .ghost()
                    .small()
                    .icon(Glyph::CALENDAR_DAYS.icon())
                    .label("Scheduler")
                    .disabled(true)
                    .tooltip("The scheduling assistant isn't available yet"),
            ));
        field_row(palette, Glyph::CLOCK, content, false)
    }

    fn location_row(&self, palette: Palette, window: &Window, cx: &App) -> gpui::Div {
        let focused = self.location.read(cx).focus_handle(cx).is_focused(window);
        field_row(
            palette,
            Glyph::MAP_PIN,
            div()
                .debug_selector(|| "event-location".to_owned())
                .flex_1()
                .child(Input::new(&self.location).appearance(false)),
            focused,
        )
    }

    fn online_row(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        // a Teams meeting stays one: its join link and chat were made with it
        let fixed = self.editing.as_ref().is_some_and(|editing| editing.event.online);
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(48.0))
            .child(
                div().w(px(40.0)).flex_none().flex().justify_center().child(
                    Glyph::VIDEO
                        .icon()
                        .size(px(18.0))
                        .text_color(rgb(palette.muted_foreground)),
                ),
            )
            .child(selected_by(
                "event-online",
                Switch::new("event-online")
                    .checked(self.online)
                    .label("Microsoft Teams meeting")
                    .disabled(fixed)
                    .on_click(cx.listener(|form, checked: &bool, _, cx| {
                        form.online = *checked;
                        cx.notify();
                    })),
            ))
    }

    fn body_card(&self, palette: Palette) -> gpui::Div {
        let tool = |id: &'static str, glyph: Glyph, tip: &'static str| {
            selected_by(id, icon_button(id, glyph, tip).small().disabled(true))
        };
        card(palette)
            .flex_1()
            .min_h(px(200.0))
            .p_4()
            .flex()
            .gap_3()
            .child(
                div().w(px(40.0)).flex_none().flex().justify_center().pt_2().child(
                    Glyph::NOTES
                        .icon()
                        .size(px(18.0))
                        .text_color(rgb(palette.muted_foreground)),
                ),
            )
            .child(
                div()
                    .debug_selector(|| "event-body".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .child(
                        div()
                            .flex_1()
                            .p_2()
                            .child(Textarea::new(&self.body).appearance(false).h_full()),
                    )
                    .child(
                        div()
                            .h(px(44.0))
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .border_t_1()
                            .border_color(rgb(palette.border))
                            .bg(rgb(mix(palette.surface, palette.background, 400)))
                            .child(tool(
                                "event-attach",
                                Glyph::PAPERCLIP,
                                "Attaching files isn't available yet",
                            ))
                            .child(tool("event-picture", Glyph::IMAGE, "Pictures aren't available yet"))
                            .child(tool("event-emoji", Glyph::EMOJI, "Emoji aren't available yet"))
                            .child(tool(
                                "event-format",
                                Glyph::TEXT_FORMAT,
                                "Formatting isn't available yet",
                            ))
                            .child(tool("event-loop", Glyph::SUN, "Loop components aren't available yet"))
                            .child(tool(
                                "event-accessibility",
                                Glyph::ACCESSIBILITY,
                                "The accessibility checker isn't available yet",
                            )),
                    ),
            )
    }

    fn problem_bar(&self, palette: Palette) -> Option<gpui::Div> {
        let problem = self.problem.clone()?;
        Some(
            div()
                .debug_selector(|| "event-problem".to_owned())
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme::DANGER))
                .bg(rgb(mix(palette.surface, theme::DANGER, 120)))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
                .child(div().flex_1().text_sm().child(problem)),
        )
    }

    /// The confirmation of Cancel meeting (with a note for the attendees) or Delete, over the form.
    fn confirm_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let confirm = self.confirm?;
        let (title, text, action) = match confirm {
            Confirm::Cancel => (
                "Cancel meeting?",
                "The attendees get a cancellation, with your message if you add one.",
                "Cancel meeting",
            ),
            Confirm::Delete => ("Delete this event?", "It's removed from your calendar.", "Delete"),
        };
        let dialog = div()
            .debug_selector(|| "event-confirm".to_owned())
            .id("event-confirm")
            .occlude()
            .w(px(440.0))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(text))
            .when(confirm == Confirm::Cancel, |dialog| {
                dialog.child(Input::new(&self.cancel_note))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by(
                        "event-confirm-keep",
                        Button::new("event-confirm-keep")
                            .outline()
                            .label("Don't")
                            .on_click(cx.listener(|form, _, _, cx| {
                                form.confirm = None;
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "event-confirm-go",
                        Button::new("event-confirm-go")
                            .danger()
                            .label(action)
                            .on_click(cx.listener(move |form, _, _, cx| form.remove(confirm, cx))),
                    )),
            );
        Some(
            div()
                .id("event-confirm-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .on_click(cx.listener(|form, _, _, cx| {
                    form.confirm = None;
                    cx.notify();
                }))
                .child(dialog),
        )
    }
}

impl Render for EventForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let form_card = card(palette)
            .p_4()
            .pr_6()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.title_row(palette, window, cx))
            .child(self.attendees_row(Need::Required, palette, window, cx))
            .children(self.suggestions_view(Need::Required, palette, cx))
            .when(self.optional_shown, |card| {
                card.child(self.attendees_row(Need::Optional, palette, window, cx))
            })
            .children(
                self.optional_shown
                    .then(|| self.suggestions_view(Need::Optional, palette, cx))
                    .flatten(),
            )
            .child(self.when_row(palette, cx))
            .child(self.location_row(palette, window, cx))
            .child(self.online_row(palette, cx));
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.problem_bar(palette))
            .child(form_card)
            .child(self.body_card(palette));
        let confirm = self.confirm_view(palette, cx);
        div()
            .debug_selector(|| "event-form".to_owned())
            .key_context("EventForm")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .relative()
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .on_mouse_move(cx.listener(Self::dragged))
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|form, _, _, cx| {
                    if form.drag.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(self.toolbar(palette, cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .gap_4()
                    .child(left)
                    .child(self.day_view(palette, cx)),
            )
            .children(confirm)
    }
}
