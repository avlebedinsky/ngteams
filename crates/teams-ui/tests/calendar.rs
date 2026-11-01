//! The calendar's navigation, refresh and event-card actions against a synthetic engine on GPUI's test platform.

#![cfg(test)]

pub mod common;
#[path = "calendar/fixtures.rs"]
mod fixtures;

use std::sync::Arc;

use chrono::NaiveDate;
use common::{Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{EngineError, EventResponse, EventWrite, EventWritten};
use teams_ui::ViewTrace;
use teams_ui::calendar::{Mode, Shown};
use teams_ui::calendar_view::CalendarView;

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, day).unwrap()
}

fn event(id: &str, hour: u8) -> teams_core::CalendarEvent {
    fixtures::full(id, hour).event
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    draw(cx);
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn window(rig: &Rig) -> (Entity<CalendarView>, &'static mut VisualTestContext, Written) {
    rig.cx.update(teams_ui::window::init);
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let core = rig.core.clone();
    let calls = rig.models.calls.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let view = cx.new(|cx| CalendarView::new(core, calls, trace, cx));
        teams_ui::window::KitRoot::new(view, window, cx)
    });
    let view = cx.update(|_, cx| root.read(cx).view().clone().downcast::<CalendarView>().unwrap());
    cx.simulate_resize(size(px(1300.), px(850.)));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.go_to(date(27), cx);
            view.set_shown(true, window, cx);
        });
    });
    draw(cx);
    (view, cx, written)
}

#[test]
fn the_calendar_filters_declined_events_sorts_reads_and_navigates_by_keyboard() {
    let rig = Rig::start();
    let mut declined = event("declined", 11);
    declined.response = EventResponse::Declined;
    rig.core.calendar.lock().unwrap().events = vec![event("late", 14), declined, event("early", 9)];
    let (view, cx, written) = window(&rig);
    cx.update(|_, cx| {
        let view = view.read(cx);
        let events = view.events().unwrap();
        assert_eq!(
            events.iter().map(|shown| shown.event.id.as_str()).collect::<Vec<_>>(),
            ["early", "late"]
        );
        assert!(view.shown());
        assert_eq!(view.mode(), Mode::WorkWeek);
    });
    assert!(cx.debug_bounds("calendar-grid").is_some());
    cx.simulate_keystrokes("ctrl-alt-4");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Month);
    assert!(cx.debug_bounds("calendar-month").is_some());
    cx.simulate_keystrokes("right");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).anchor()),
        NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()
    );
    cx.simulate_keystrokes("left");
    draw(cx);
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(1));
    click(cx, "calendar-pane-toggle");
    assert!(cx.debug_bounds("calendar-pane").is_none());
    click(cx, "calendar-pane-toggle");
    assert!(cx.debug_bounds("calendar-pane").is_some());
    assert!(!written.text().contains("Synthetic event"), "trace carries no subjects");
}

#[test]
fn a_failed_read_retries_and_a_stopped_refresh_keeps_the_previous_events() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::CalendarUnread));
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::CalendarUnread));
    let (view, cx, _) = window(&rig);
    assert!(cx.debug_bounds("calendar-failed").is_some());
    rig.core.calendar.lock().unwrap().events = vec![event("recovered", 9)];
    click(cx, "calendar-retry");
    assert!(cx.debug_bounds("calendar-failed").is_none());
    assert_eq!(cx.update(|_, cx| view.read(cx).events().unwrap().len()), 1);
    rig.core
        .calendar
        .lock()
        .unwrap()
        .answers
        .push_back(Err(EngineError::Stopped));
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.set_shown(false, window, cx);
            view.set_shown(true, window, cx);
        });
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| view.read(cx).events().unwrap()[0].event.id.clone()),
        "recovered"
    );
}

#[test]
fn a_refresh_requested_during_a_read_runs_after_the_first_answer() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().hold = true;
    let (view, cx, _) = window(&rig);
    assert_eq!(
        rig.core.calendar.lock().unwrap().reads.len(),
        1,
        "same-span reads coalesce"
    );
    let answer = rig.core.calendar.lock().unwrap().held.remove(0);
    rig.core.calendar.lock().unwrap().hold = false;
    rig.core.calendar.lock().unwrap().events = vec![event("latest", 10)];
    answer.send(Ok(vec![event("stale", 9)]));
    draw(cx);
    assert_eq!(
        rig.core.calendar.lock().unwrap().reads.len(),
        2,
        "showing requested a fresh read"
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).events().unwrap()[0].event.id.clone()),
        "latest"
    );
}

#[test]
fn canceling_an_organizers_meeting_requires_confirmation_and_refreshes_after_success() {
    let rig = Rig::start();
    let mut meeting = event("meeting", 9);
    meeting.attendees.push(teams_proto::Attendee {
        name: Some("Synthetic attendee".to_owned()),
        address: Some("guest@lab.example.test".to_owned()),
    });
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(meeting)).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown.clone(), cx)));
    draw(cx);
    click(cx, "calendar-card-remove");
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    click(cx, "calendar-card-keep");
    assert!(cx.debug_bounds("calendar-card-confirm").is_none());
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.close_card(cx);
            view.open_card(shown, cx);
        });
    });
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    assert!(
        matches!(rig.core.calendar.lock().unwrap().writes.as_slice(), [EventWrite::Cancel { id, .. }] if id == "meeting")
    );
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
}

#[test]
fn the_read_only_card_offers_no_edit_or_remove_and_escape_closes_it() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(event("read-only", 9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card").is_some());
    assert!(cx.debug_bounds("calendar-card-edit").is_none());
    assert!(cx.debug_bounds("calendar-card-remove").is_none());
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn calendar_controls_toggle_visibility_choose_dates_and_explain_unavailable_actions() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().events = vec![event("visible", 9)];
    let (view, cx, _) = window(&rig);
    click(cx, "calendar-mini-next");
    click(cx, "calendar-mini-previous");
    click(cx, "calendar-mini-fold");
    click(cx, "calendar-fold");
    click(cx, "calendar-show-all");
    click(cx, "calendar-show-all");
    click(cx, "calendar-mine-fold");
    assert!(cx.debug_bounds("calendar-own").is_none());
    click(cx, "calendar-mine-fold");
    assert!(cx.debug_bounds("calendar-own").is_some());
    click(cx, "calendar-own");
    assert!(cx.debug_bounds("calendar-event-visible").is_none());
    click(cx, "calendar-own");
    click(cx, "calendar-dates");
    assert!(cx.debug_bounds("calendar-picker").is_some());
    click(cx, "calendar-picker-next");
    click(cx, "calendar-picker-previous");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.debug_bounds("calendar-picker").is_none());
    click(cx, "calendar-add");
    assert!(cx.debug_bounds("calendar-notice").is_some());
    click(cx, "calendar-notice-close");
    click(cx, "calendar-join-id");
    assert!(cx.debug_bounds("calendar-notice").is_some());
    click(cx, "calendar-notice-close");
    click(cx, "calendar-mode");
    click(cx, "calendar-mode-day");
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    click(cx, "calendar-next");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(28));
    click(cx, "calendar-previous");
    assert_eq!(cx.update(|_, cx| view.read(cx).anchor()), date(27));
}

#[test]
fn all_day_recurring_and_canceled_events_remain_navigable_in_week_and_month_views() {
    let rig = Rig::start();
    let mut all_day = event("all-day", 0);
    all_day.all_day = true;
    let mut tentative = event("tentative", 9);
    tentative.is_organizer = false;
    tentative.organizer_name = Some("Synthetic organizer".to_owned());
    tentative.response = EventResponse::Tentative;
    tentative.kind = teams_proto::EventKind::Occurrence;
    let mut canceled = event("canceled", 10);
    canceled.cancelled = true;
    let mut short = event("short", 12);
    short.is_organizer = false;
    short.organizer_name = Some("Another synthetic organizer".to_owned());
    let teams_proto::EventTime::Floating(end) = &mut short.end else {
        unreachable!()
    };
    end.hour = 12;
    end.minute = 15;
    rig.core.calendar.lock().unwrap().events = vec![all_day, tentative, canceled, short];
    let (view, cx, _) = window(&rig);
    assert!(cx.debug_bounds("calendar-all-day").is_some());
    click(cx, "calendar-event-all-day");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card().unwrap().event.id.clone()),
        "all-day"
    );
    cx.update(|_, cx| view.update(cx, CalendarView::close_card));
    click(cx, "calendar-day-2026-10-27");
    assert_eq!(cx.update(|_, cx| view.read(cx).mode()), Mode::Day);
    click(cx, "calendar-event-tentative");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).card().unwrap().event.id.clone()),
        "tentative"
    );
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            view.close_card(cx);
            view.set_mode(Mode::Month, cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("calendar-event-canceled").is_some());
    click(cx, "calendar-event-canceled");
    assert!(cx.update(|_, cx| view.read(cx).card().unwrap().event.cancelled));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}

#[test]
fn double_clicking_an_empty_half_hour_opens_an_unsaved_draft_at_that_time() {
    let rig = Rig::start();
    let (_, cx, _) = window(&rig);
    let at = cx.debug_bounds("calendar-slot-2026-10-27-20").unwrap().center();
    cx.simulate_event(gpui::MouseDownEvent {
        position: at,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        button: gpui::MouseButton::Left,
        modifiers: Modifiers::none(),
        click_count: 2,
    });
    draw(cx);
    cx.update(|_, cx| {
        let form = teams_ui::event_form::shown(cx).unwrap();
        assert_eq!(form.read(cx).slot().start, date(27).and_hms_opt(10, 0, 0).unwrap());
        teams_ui::event_form::close(cx);
    });
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
}
