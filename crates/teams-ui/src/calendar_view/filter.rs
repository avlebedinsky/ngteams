//! The calendar's Filter, as Teams' calendar toolbar has it: a drop-down of the kinds of event shown, each ticked while
//! shown (meetings the account organizes and those it is invited to, Teams meetings and the rest, how each shows the
//! account's time, single events and series' occurrences), and Clear filters; the button is marked, its tip saying
//! "Filter applied", while anything is hidden. Only what the calendar view's events say is filtered on (it names no
//! attendees, so an appointment and a meeting the account organizes aren't told apart).

use gpui::{
    App, Context, InteractiveElement as _, IntoElement as _, ParentElement as _, Styled as _, WeakEntity, div,
    prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Selectable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use teams_core::{CalendarEvent, ShowAs};

use super::CalendarView;
use crate::icons::{Glyph, selected_by};
use crate::theme::Palette;

/// A kind of event the Filter shows or hides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shows {
    /// Events the account organizes.
    Organized,
    /// Meetings the account is invited to.
    Invited,
    TeamsMeetings,
    /// Events that aren't Teams meetings.
    Others,
    Free,
    Tentative,
    Busy,
    Away,
    WorkingElsewhere,
    Single,
    /// Occurrences of a series.
    Recurring,
}

impl Shows {
    /// The Filter's groups: a heading and its kinds, each with its selector key and words.
    const GROUPS: [Group; 4] = [
        (
            "Meetings",
            &[
                (Self::Organized, "organized", "Organized by me"),
                (Self::Invited, "invited", "I'm invited to"),
            ],
        ),
        (
            "Online",
            &[
                (Self::TeamsMeetings, "teams", "Teams meetings"),
                (Self::Others, "others", "Other events"),
            ],
        ),
        (
            "Show as",
            &[
                (Self::Free, "free", "Free"),
                (Self::Tentative, "tentative", "Tentative"),
                (Self::Busy, "busy", "Busy"),
                (Self::Away, "away", "Away"),
                (Self::WorkingElsewhere, "elsewhere", "Working elsewhere"),
            ],
        ),
        (
            "Recurrence",
            &[
                (Self::Single, "single", "Single events"),
                (Self::Recurring, "recurring", "Series"),
            ],
        ),
    ];

    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// One of the Filter's groups: its heading, and each kind with its selector key and words.
type Group = (&'static str, &'static [(Shows, &'static str, &'static str)]);

/// The kinds hidden (none by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hidden(u16);

impl Hidden {
    /// Whether `kind` is hidden.
    #[must_use]
    pub fn hides(self, kind: Shows) -> bool {
        self.0 & kind.bit() != 0
    }

    /// Whether anything is hidden.
    #[must_use]
    pub fn any(self) -> bool {
        self.0 != 0
    }

    fn flip(&mut self, kind: Shows) {
        self.0 ^= kind.bit();
    }

    /// Whether `event` is shown: none of the kinds it is hidden.
    #[must_use]
    pub fn shows(self, event: &CalendarEvent) -> bool {
        let whose = if event.is_organizer {
            Shows::Organized
        } else {
            Shows::Invited
        };
        let online = if event.online {
            Shows::TeamsMeetings
        } else {
            Shows::Others
        };
        let time = match event.show_as {
            ShowAs::Free => Some(Shows::Free),
            ShowAs::Tentative => Some(Shows::Tentative),
            ShowAs::Busy => Some(Shows::Busy),
            ShowAs::Away => Some(Shows::Away),
            ShowAs::WorkingElsewhere => Some(Shows::WorkingElsewhere),
            ShowAs::Other(_) => None,
        };
        let series = if event.kind.recurring() {
            Shows::Recurring
        } else {
            Shows::Single
        };
        [Some(whose), Some(online), time, Some(series)]
            .into_iter()
            .flatten()
            .all(|kind| !self.hides(kind))
    }
}

impl CalendarView {
    /// What the Filter hides.
    #[must_use]
    pub fn hidden(&self) -> Hidden {
        self.hidden
    }

    /// Shows `kind` again, or hides it.
    pub fn flip_shown(&mut self, kind: Shows, cx: &mut Context<Self>) {
        self.hidden.flip(kind);
        cx.notify();
    }

    /// Shows every kind again.
    pub fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.hidden = Hidden::default();
        cx.notify();
    }

    /// The toolbar's Filter: its button (marked while anything is hidden) and its drop-down.
    pub(super) fn filter_menu(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().downgrade();
        let hidden = self.hidden;
        let tip = if hidden.any() {
            "Filter applied: some events are hidden"
        } else {
            "Filter the events shown"
        };
        selected_by(
            "calendar-filter",
            Button::new("calendar-filter")
                .ghost()
                .selected(hidden.any())
                .tooltip(tip)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Glyph::LIST_FILTER.icon().size(px(16.0)))
                        .child("Filter")
                        // "Filter applied" is told by its mark: the toolbar has no room for the longer words
                        .when(hidden.any(), |face| {
                            face.child(div().size(px(6.0)).rounded_full().bg(rgb(palette.brand)))
                        })
                        .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
                )
                .dropdown_menu(move |menu, _, _| filter_items(menu, &view, hidden, palette)),
        )
        .into_any_element()
    }
}

/// The Filter's drop-down: Clear filters, then each group's heading and its kinds, ticked while shown.
fn filter_items(menu: PopupMenu, view: &WeakEntity<CalendarView>, hidden: Hidden, palette: Palette) -> PopupMenu {
    let clear = view.clone();
    let mut menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(520.0)).item(
        PopupMenuItem::element(|_, _| {
            div()
                .debug_selector(|| "calendar-filter-clear".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .child(Glyph::X.icon().size(px(14.0)))
                .child("Clear filters")
        })
        .on_click(move |_, _, cx: &mut App| {
            let _ = clear.update(cx, CalendarView::clear_filters);
        }),
    );
    for (heading, kinds) in Shows::GROUPS {
        menu = menu.separator().label(heading);
        for &(kind, key, label) in kinds {
            let view = view.clone();
            let selector = format!("calendar-filter-{key}");
            let ticked = !hidden.hides(kind);
            let row = move |_: &mut gpui::Window, _: &mut App| filter_row(selector.clone(), label, ticked, palette);
            menu = menu.item(PopupMenuItem::element(row).on_click(move |_, _, cx: &mut App| {
                let _ = view.update(cx, |view, cx| view.flip_shown(kind, cx));
            }));
        }
    }
    menu
}

/// A row of the Filter: its tick box (the calendar list's) and words.
fn filter_row(selector: String, label: &'static str, ticked: bool, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_2()
        .child(super::pane::calendar_tick("calendar-filter-tick", ticked, palette))
        .child(label)
}
