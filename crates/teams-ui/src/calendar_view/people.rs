//! Other people's calendars, as Teams' calendar adds them: "Add calendar" opens its dialog on "Add from directory" (a
//! person found by name or address among the people the account knows and the directory finds, picked, then Add), and
//! the person's calendar is listed under "People's calendars" in the left pane, ticked to show it, its "…" offering
//! "Show this only" and "Remove". The list is the account's own, kept in the settings ([`PersonCalendar`]).
//!
//! A person's calendar shows what the account may see of it: their busy times (busy, tentative, away, working
//! elsewhere), read for the days shown with the Scheduler's free/busy read (`Core::people_calendars`), each a block in
//! the person's colour that opens no card. The name of what keeps them busy shows only where the service gives it.

use std::collections::HashMap;
use std::rc::Rc;

use chrono::{Duration, Local, TimeZone as _};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};
use teams_core::{CalendarEvent, EventResponse, Invitable, SearchScope};
use teams_proto::{BusyTime, EventAttendee, EventKind, FreeBusy, PersonSchedule, ShowAs};

use super::{CalendarView, Shown, Span, Toggle, pane::calendar_tick};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::settings::{PersonCalendar, SettingsModel};
use crate::theme::Palette;
use crate::trace::TraceLine;

/// The colours people's calendars are drawn in, in the order they were added (Outlook's calendar colours: teal,
/// orange, green, magenta, blue, red, purple, olive). A calendar's colour stands for whose it is, so it is the same
/// in every theme, as Teams' are.
pub const CALENDAR_COLORS: [u32; 8] = [
    0x0003_8387,
    0x00CA_5010,
    0x0049_8205,
    0x00C2_39B3,
    0x0000_78D4,
    0x00D1_3438,
    0x0087_64B8,
    0x0098_6F0B,
];

/// What the dialog says when the person's calendar is already shown.
pub const ALREADY_ADDED: &str = "This calendar is already in your list.";

/// One person's calendar in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub address: String,
    pub name: String,
    /// Ticked: their busy times are shown.
    pub ticked: bool,
}

/// The people's calendars: the list, what was read of each span, and the Add calendar dialog.
#[derive(Default)]
pub(super) struct Others {
    pub(super) list: Vec<Person>,
    /// The busy times read for each span (every person's of the list then).
    read: HashMap<Span, Rc<Vec<Shown>>>,
    reading: Option<(Span, Task<()>)>,
    /// The settings that keep the list.
    settings: Option<Entity<SettingsModel>>,
    pub(super) adding: Option<AddCalendar>,
}

/// The Add calendar dialog.
pub(super) struct AddCalendar {
    query: Entity<InputState>,
    found: Vec<Invitable>,
    /// The person picked, Add's to add.
    picked: Option<Invitable>,
    problem: Option<&'static str>,
    searching: Option<Task<()>>,
}

impl AddCalendar {
    /// Its box.
    pub(super) fn input(&self) -> &Entity<InputState> {
        &self.query
    }
}

impl Others {
    fn color_of(&self, address: &str) -> u32 {
        let index = self
            .list
            .iter()
            .position(|person| person.address.eq_ignore_ascii_case(address))
            .unwrap_or(0);
        CALENDAR_COLORS[index % CALENDAR_COLORS.len()]
    }
}

impl CalendarView {
    /// The settings that keep the account's people's calendars: the list is read from them now.
    pub fn set_settings(&mut self, settings: Entity<SettingsModel>, cx: &mut Context<Self>) {
        let me = self.core.me().to_owned();
        self.others.list = settings
            .read(cx)
            .settings()
            .people_calendars
            .iter()
            .filter(|calendar| calendar.account == me)
            .map(|calendar| Person {
                address: calendar.address.clone(),
                name: calendar.name.clone(),
                ticked: true,
            })
            .collect();
        self.others.settings = Some(settings);
        self.others.read.clear();
        self.read_people(cx);
        cx.notify();
    }

    /// The people's calendars listed.
    #[must_use]
    pub fn people(&self) -> &[Person] {
        &self.others.list
    }

    /// The colour `address`'s calendar is drawn in.
    pub(super) fn person_color(&self, address: &str) -> u32 {
        self.others.color_of(address)
    }

    /// Adds `person`'s calendar (Add calendar's Add): listed, ticked and kept in the settings; their busy times read.
    pub fn add_person(&mut self, person: &Invitable, cx: &mut Context<Self>) -> bool {
        if self
            .others
            .list
            .iter()
            .any(|known| known.address.eq_ignore_ascii_case(&person.address))
        {
            return false;
        }
        let name = if person.name.trim().is_empty() {
            person.address.clone()
        } else {
            person.name.clone()
        };
        self.others.list.push(Person {
            address: person.address.clone(),
            name,
            ticked: true,
        });
        self.trace.record(&TraceLine::new("calendar", "person-added"));
        self.keep_people(cx);
        // what was read, or is being read, was of the list before them
        self.others.read.clear();
        self.others.reading = None;
        self.read_people(cx);
        cx.notify();
        true
    }

    /// Takes `address`'s calendar off the list (its "…" Remove).
    pub fn remove_person(&mut self, address: &str, cx: &mut Context<Self>) {
        self.others
            .list
            .retain(|person| !person.address.eq_ignore_ascii_case(address));
        self.trace.record(&TraceLine::new("calendar", "person-removed"));
        self.keep_people(cx);
        cx.notify();
    }

    /// Ticks `address`'s calendar, or unticks it (`None`: every person's, as "People's calendars" ticks them all).
    pub fn tick_person(&mut self, address: Option<&str>, ticked: bool, cx: &mut Context<Self>) {
        for person in &mut self.others.list {
            if address.is_none_or(|address| person.address.eq_ignore_ascii_case(address)) {
                person.ticked = ticked;
            }
        }
        cx.notify();
    }

    /// "Show this only": `address`'s calendar alone is ticked (`None`: the account's own alone).
    pub fn show_only(&mut self, address: Option<&str>, cx: &mut Context<Self>) {
        self.toggles.set(Toggle::Hidden, address.is_some());
        for person in &mut self.others.list {
            person.ticked = address.is_some_and(|address| person.address.eq_ignore_ascii_case(address));
        }
        cx.notify();
    }

    /// The list kept in the settings, the other accounts' entries left as they were.
    fn keep_people(&self, cx: &mut Context<Self>) {
        let Some(settings) = self.others.settings.clone() else {
            return;
        };
        let me = self.core.me().to_owned();
        let kept: Vec<PersonCalendar> = self
            .others
            .list
            .iter()
            .map(|person| PersonCalendar {
                account: me.clone(),
                address: person.address.clone(),
                name: person.name.clone(),
            })
            .collect();
        settings.update(cx, |model, cx| {
            model.change(
                |settings| {
                    settings.people_calendars.retain(|calendar| calendar.account != me);
                    settings.people_calendars.extend(kept);
                },
                cx,
            );
        });
    }

    /// Reads the people's busy times over the span shown, unless they were read or are being read.
    pub(super) fn read_people(&mut self, cx: &mut Context<Self>) {
        let span = self.span();
        if self.others.list.is_empty()
            || self.others.read.contains_key(&span)
            || self
                .others
                .reading
                .as_ref()
                .is_some_and(|(reading, _)| *reading == span)
        {
            return;
        }
        let people: Vec<EventAttendee> = self
            .others
            .list
            .iter()
            .map(|person| EventAttendee {
                address: person.address.clone(),
                name: Some(person.name.clone()),
                mri: None,
                required: true,
            })
            .collect();
        let (start, _) = span.window_ms();
        let reply = self.core.people_calendars(people, start, span.days);
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |view, cx| view.people_read(span, answer, cx));
        });
        self.others.reading = Some((span, task));
    }

    /// The people's busy times over `span` came: shown in their calendars (a failed read shows none, and is asked
    /// again on the next refresh).
    fn people_read(
        &mut self,
        span: Span,
        answer: Result<Vec<PersonSchedule>, teams_core::EngineError>,
        cx: &mut Context<Self>,
    ) {
        if self
            .others
            .reading
            .as_ref()
            .is_some_and(|(reading, _)| *reading == span)
        {
            self.others.reading = None;
        }
        if let Ok(schedules) = answer {
            let mut shown: Vec<Shown> = schedules
                .iter()
                .flat_map(|schedule| {
                    let owner: Rc<str> = Rc::from(schedule.address.to_ascii_lowercase());
                    schedule.busy.iter().filter_map(move |busy| busy_shown(busy, &owner))
                })
                .collect();
            shown.sort_by_key(|shown| shown.start);
            self.others.read.insert(span, Rc::new(shown));
            self.trace.record(&TraceLine::new("calendar", "people-read"));
        }
        if self.others.reading.is_none() && self.span() != span {
            self.read_people(cx);
        }
        cx.notify();
    }

    /// Reads the people's busy times again (Refresh, the timer).
    pub(super) fn read_people_again(&mut self, cx: &mut Context<Self>) {
        self.others.read.remove(&self.span());
        self.read_people(cx);
    }

    /// The busy times shown of the people ticked (`only`: of that person alone, ticked or not).
    pub(super) fn people_shown(&self, only: Option<&str>) -> Vec<Shown> {
        let Some(read) = self.others.read.get(&self.span()) else {
            return Vec::new();
        };
        read.iter()
            .filter(|shown| {
                let owner = shown.owner.as_deref().unwrap_or_default();
                match only {
                    Some(address) => owner.eq_ignore_ascii_case(address),
                    None => self
                        .others
                        .list
                        .iter()
                        .any(|person| person.ticked && person.address.eq_ignore_ascii_case(owner)),
                }
            })
            .cloned()
            .collect()
    }

    /// Add calendar: the dialog opens on "Add from directory", its box focused.
    pub fn open_add_calendar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Enter a name or email address"));
        query.update(cx, |input, cx| input.focus(window, cx));
        cx.subscribe_in(&query, window, |this: &mut Self, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                let text = input.read(cx).value().trim().to_owned();
                this.search_people(text, cx);
            }
        })
        .detach();
        self.trace.record(&TraceLine::new("calendar", "add-calendar"));
        self.others.adding = Some(AddCalendar {
            query,
            found: Vec::new(),
            picked: None,
            problem: None,
            searching: None,
        });
        cx.notify();
    }

    /// The dialog closes.
    pub fn close_add_calendar(&mut self, cx: &mut Context<Self>) {
        if self.others.adding.take().is_some() {
            cx.notify();
        }
    }

    /// Types `query` in the dialog's box: the people it names are looked for.
    pub fn search_directory(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = &self.others.adding else { return };
        adding.query.update(cx, |input, cx| input.set_value(query, window, cx));
        self.search_people(query.trim().to_owned(), cx);
    }

    /// Looks for the people `query` names, a moment after the typing stops.
    fn search_people(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.others.adding else { return };
        adding.problem = None;
        if query.chars().count() < 2 {
            adding.found.clear();
            adding.searching = None;
            cx.notify();
            return;
        }
        let core = std::sync::Arc::clone(&self.core);
        adding.searching = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(crate::event_form::people::PAUSE).await;
            let known = core.invitable(query.clone());
            let found = core.search_service(query.clone(), SearchScope::Suggestions);
            let known = known.await.unwrap_or_default();
            let found = found.await.unwrap_or_default();
            let mut people = crate::event_form::people::merged(known, &found);
            // a whole address typed is someone too, as the dialog takes one
            if query.contains('@') && !people.iter().any(|person| person.address.eq_ignore_ascii_case(&query)) {
                people.push(Invitable {
                    mri: String::new(),
                    name: query.clone(),
                    address: query.clone(),
                });
            }
            let _ = this.update(cx, |view, cx| {
                let Some(adding) = &mut view.others.adding else { return };
                if adding.query.read(cx).value().trim() == query {
                    adding.found = people;
                    cx.notify();
                }
            });
        }));
    }

    /// Picks `person` in the dialog: Add adds their calendar.
    pub fn pick_person(&mut self, person: Invitable, window: &mut Window, cx: &mut Context<Self>) {
        let Some(adding) = &mut self.others.adding else { return };
        adding.found.clear();
        adding.picked = Some(person);
        adding.query.update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    /// The dialog's Add: the person picked is added, and the dialog closes.
    pub fn add_picked(&mut self, cx: &mut Context<Self>) {
        let Some(picked) = self.others.adding.as_ref().and_then(|adding| adding.picked.clone()) else {
            return;
        };
        if self.add_person(&picked, cx) {
            self.others.adding = None;
        } else if let Some(adding) = &mut self.others.adding {
            adding.problem = Some(ALREADY_ADDED);
        }
        cx.notify();
    }

    /// "People's calendars" in the left pane: its tick and fold, and each person's calendar with its "…".
    pub(super) fn people_group(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if self.others.list.is_empty() {
            return None;
        }
        let open = !self.toggles.on(Toggle::PeopleFolded);
        let all_ticked = self.others.list.iter().all(|person| person.ticked);
        let header = div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                "calendar-people-fold",
                icon_button(
                    "calendar-people-fold",
                    if open {
                        Glyph::CHEVRON_DOWN
                    } else {
                        Glyph::CHEVRON_RIGHT
                    },
                    if open {
                        "Hide people's calendars"
                    } else {
                        "Show people's calendars"
                    },
                )
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggles.flip(Toggle::PeopleFolded);
                    cx.notify();
                })),
            ))
            .child(
                super::pane::calendar_row("calendar-people", palette)
                    .flex_1()
                    .px_1()
                    .child(calendar_tick("calendar-people-tick", all_ticked, palette))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("People's calendars"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.tick_person(None, !all_ticked, cx))),
            );
        let rows = open.then(|| {
            self.others
                .list
                .iter()
                .enumerate()
                .map(|(index, person)| self.person_row(index, person, palette, cx))
                .collect::<Vec<_>>()
        });
        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(header)
                .children(rows.into_iter().flatten()),
        )
    }

    /// A person's calendar in the list: its tick in its colour, its name and its "…".
    fn person_row(&self, index: usize, person: &Person, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let color = self.person_color(&person.address);
        let address = person.address.clone();
        let ticked = person.ticked;
        let tick = div()
            .size(px(18.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .border_1()
            .border_color(rgb(color))
            .when(ticked, |tick| {
                tick.bg(rgb(color))
                    .text_color(rgb(0x00FF_FFFF))
                    .child(Glyph::CHECK.icon().size(px(12.0)))
            });
        let more = person_menu(index, &person.address, cx);
        let selector = format!("calendar-person-{index}");
        div()
            .ml(px(30.0))
            .flex()
            .items_center()
            .child(
                div()
                    .debug_selector(move || selector.clone())
                    .id(SharedString::from(format!("calendar-person-{index}")))
                    .flex_1()
                    .min_w_0()
                    .h(px(36.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .child(tick)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(person.name.clone()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.tick_person(Some(&address), !ticked, cx))),
            )
            .child(selected_by(format!("calendar-person-more-{index}"), more))
            .into_any_element()
    }

    /// The Add calendar dialog, over the calendar: its sections on the left ("Add from directory" chosen), the box,
    /// what it found, the person picked and Add.
    pub(super) fn add_calendar_view(
        &self,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let adding = self.others.adding.as_ref()?;
        let nav = add_nav(palette);
        let main = Self::add_main(adding, palette, cx);
        let dialog = div()
            .debug_selector(|| "calendar-add-dialog".to_owned())
            .id("calendar-add-dialog")
            .occlude()
            .w(px(760.0))
            .h(px(440.0))
            .flex()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(nav)
            .child(main);
        Some(
            div()
                .id("calendar-add-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .on_click(cx.listener(|this, _, _, cx| this.close_add_calendar(cx)))
                .child(dialog),
        )
    }

    /// The dialog's Add from directory: its title and close, what it does, the person picked, the box with what it
    /// found, and Add.
    fn add_main(adding: &AddCalendar, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let found = Self::found_view(adding, palette, cx);
        let picked = adding.picked.as_ref().map(|person| picked_chip(person, palette, cx));
        let problem = adding.problem.map(|problem| {
            div()
                .debug_selector(|| "calendar-add-problem".to_owned())
                .text_sm()
                .text_color(rgb(crate::theme::DANGER))
                .child(SharedString::from(problem))
        });
        let title = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Add from directory"),
            )
            .child(selected_by(
                "calendar-add-close",
                icon_button("calendar-add-close", Glyph::X, "Close")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.close_add_calendar(cx))),
            ));
        let add = Button::new("calendar-add-add")
            .primary()
            .label("Add")
            .disabled(adding.picked.is_none())
            .on_click(cx.listener(|this, _, _, cx| this.add_picked(cx)));
        div()
            .flex_1()
            .min_w_0()
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(title)
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Select a person from your organization's directory to view their calendar."),
            )
            // the person picked takes the box's place, as Teams' does: what the box finds is shown only before a
            // pick, over the Add it leaves disabled
            .child(match picked {
                Some(picked) => picked,
                None => div()
                    .debug_selector(|| "calendar-add-query".to_owned())
                    .relative()
                    .child(Input::new(&adding.query))
                    .children(found.map(|found| gpui::deferred(found).with_priority(1))),
            })
            .children(problem)
            .child(div().flex().child(selected_by("calendar-add-add", add)))
    }

    /// The people the box found, under it.
    fn found_view(adding: &AddCalendar, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if adding.found.is_empty() {
            return None;
        }
        let rows = adding.found.iter().enumerate().map(|(index, person)| {
            let picked = person.clone();
            let name = SharedString::from(person.name.clone());
            div()
                .debug_selector(move || format!("calendar-add-found-{index}"))
                .id(SharedString::from(format!("calendar-add-found-{index}")))
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .cursor_pointer()
                .hover(move |row| row.bg(rgb(palette.hover)))
                .child(crate::people::avatar(Some(&name), None, gpui_component::Size::Small))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().overflow_hidden().text_ellipsis().child(person.name.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(palette.muted_foreground))
                                .child(person.address.clone()),
                        ),
                )
                .on_click(cx.listener(move |this, _, window, cx| this.pick_person(picked.clone(), window, cx)))
        });
        Some(
            div()
                .debug_selector(|| "calendar-add-found".to_owned())
                .absolute()
                .top(px(40.0))
                .left_0()
                .w(px(360.0))
                .py_1()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .child(
                    div()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(palette.brand))
                        .child("Suggested contacts"),
                )
                .children(rows),
        )
    }
}

/// A person's calendar's "…": Show this only, Remove.
fn person_menu(index: usize, address: &str, cx: &mut Context<CalendarView>) -> impl gpui::IntoElement + use<> {
    let view = cx.entity().downgrade();
    let address = address.to_owned();
    icon_button(
        SharedString::from(format!("calendar-person-more-{index}")),
        Glyph::MORE,
        "More options",
    )
    .small()
    .dropdown_menu(move |menu, _, _| {
        let (only, remove) = (view.clone(), view.clone());
        let (only_address, remove_address) = (address.clone(), address.clone());
        menu.min_w(px(180.0))
            .item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(move || format!("calendar-person-only-{index}"))
                        .child("Show this only")
                })
                .on_click(move |_, _, cx: &mut App| {
                    let _ = only.update(cx, |view, cx| view.show_only(Some(&only_address), cx));
                }),
            )
            .item(
                PopupMenuItem::element(move |_, _| {
                    div()
                        .debug_selector(move || format!("calendar-person-remove-{index}"))
                        .child("Remove")
                })
                .on_click(move |_, _, cx: &mut App| {
                    let _ = remove.update(cx, |view, cx| view.remove_person(&remove_address, cx));
                }),
            )
    })
}

/// The dialog's sections on its left: Add from directory, the one it has.
fn add_nav(palette: Palette) -> gpui::Div {
    div()
        .w(px(220.0))
        .flex_none()
        .p_4()
        .flex()
        .flex_col()
        .gap_2()
        .bg(rgb(palette.background))
        .rounded_l_lg()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .mb_2()
                .child("Add calendar"),
        )
        .child(
            div()
                .px_2()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(Glyph::USERS.icon().size(px(16.0)))
                .child("Add from directory"),
        )
}

/// The person picked, as a chip with its remove.
fn picked_chip(person: &Invitable, palette: Palette, cx: &mut Context<CalendarView>) -> gpui::Div {
    let name = SharedString::from(person.name.clone());
    div().flex().child(
        div()
            .debug_selector(|| "calendar-add-picked".to_owned())
            .pl_1()
            .pr_1()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .rounded_full()
            .bg(rgb(palette.selected))
            .child(crate::people::avatar(Some(&name), None, gpui_component::Size::XSmall))
            .child(person.name.clone())
            .child(selected_by(
                "calendar-add-unpick",
                icon_button("calendar-add-unpick", Glyph::X, "Remove")
                    .xsmall()
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(adding) = &mut this.others.adding {
                            adding.picked = None;
                            adding.query.update(cx, |input, cx| input.focus(window, cx));
                        }
                        cx.notify();
                    })),
            )),
    )
}

/// A busy time of `owner`'s as the calendar shows it: an event of theirs, named by what the service said keeps them
/// busy, else by how their time shows ("Busy", "Tentative", "Away", "Working elsewhere"); free time shows nothing.
fn busy_shown(busy: &BusyTime, owner: &Rc<str>) -> Option<Shown> {
    let (show_as, label) = match &busy.status {
        FreeBusy::Free => return None,
        FreeBusy::Tentative => (ShowAs::Tentative, "Tentative"),
        FreeBusy::Busy | FreeBusy::Other(_) => (ShowAs::Busy, "Busy"),
        FreeBusy::Away => (ShowAs::Away, "Away"),
        FreeBusy::WorkingElsewhere => (ShowAs::WorkingElsewhere, "Working elsewhere"),
    };
    let start = Local.timestamp_millis_opt(busy.start_ms).single()?.naive_local();
    let end = Local.timestamp_millis_opt(busy.end_ms).single()?.naive_local();
    let event = CalendarEvent {
        id: format!("person-{owner}-{}", busy.start_ms),
        subject: busy.subject.clone().unwrap_or_else(|| label.to_owned()),
        start: teams_proto::EventTime::Instant(busy.start_ms),
        end: teams_proto::EventTime::Instant(busy.end_ms),
        all_day: end - start >= Duration::hours(24) && start.time() == chrono::NaiveTime::MIN,
        kind: EventKind::Single,
        cancelled: false,
        online: false,
        join_url: None,
        meeting_chat: None,
        meeting: None,
        meeting_code: None,
        location: None,
        organizer_name: None,
        organizer_address: None,
        is_organizer: false,
        response: EventResponse::NoResponse,
        show_as,
        private: false,
        preview: None,
        conference_id: None,
        attendees: Vec::new(),
        series_master_id: None,
        channel_meeting: None,
    };
    let mut shown = Shown::of(Rc::new(event))?;
    shown.owner = Some(Rc::clone(owner));
    Some(shown)
}
