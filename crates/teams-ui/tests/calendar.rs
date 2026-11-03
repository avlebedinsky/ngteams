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
use teams_ui::calendar_view::{CalendarView, CalendarViewEvent};

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

fn shown_ids(view: &Entity<CalendarView>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| {
        view.read(cx)
            .events()
            .map(|events| events.iter().map(|shown| shown.event.id.clone()).collect())
            .unwrap_or_default()
    })
}

#[test]
fn the_stored_events_show_at_once_and_the_read_applies_on_top() {
    let rig = Rig::start();
    let mut moved = event("moved", 9);
    moved.subject = "Before the move".to_owned();
    let mut occurrence = series_meeting(13);
    occurrence.id = "occurrence".to_owned();
    rig.core.calendar.lock().unwrap().stored = vec![event("gone", 11), moved.clone(), occurrence.clone()];
    rig.core.calendar.lock().unwrap().hold = true;
    let (view, cx, written) = window(&rig);
    assert_eq!(
        shown_ids(&view, cx),
        ["moved", "gone", "occurrence"],
        "before any answer"
    );
    assert!(cx.update(|_, cx| view.read(cx).showing_stored()));
    {
        let calendar = rig.core.calendar.lock().unwrap();
        assert_eq!(
            calendar.stored_reads[0], calendar.reads[0],
            "the store asked for the same days the read is"
        );
    }
    assert!(written.text().contains(r#""change":"stored""#));
    draw(cx);
    assert!(cx.debug_bounds("calendar-failed").is_none());

    // the answer: one deleted, one moved and renamed, the occurrence cancelled, one added
    let mut moved_now = event("moved", 15);
    moved_now.subject = "After the move".to_owned();
    let mut cancelled = occurrence;
    cancelled.cancelled = true;
    let answer = rig.core.calendar.lock().unwrap().held.remove(0);
    answer.send(Ok(vec![moved_now, cancelled, event("added", 8)]));
    draw(cx);
    assert_eq!(shown_ids(&view, cx), ["added", "occurrence", "moved"]);
    cx.update(|_, cx| {
        let view = view.read(cx);
        assert!(!view.showing_stored());
        let events = view.events().unwrap();
        assert_eq!(events[2].event.subject, "After the move");
        assert!(events[1].event.cancelled);
    });
}

#[test]
fn a_failed_read_keeps_the_stored_events_and_a_read_span_doesnt_ask_the_store_again() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().stored = vec![event("kept", 9)];
    // going to the day and showing the calendar read it twice
    for _ in 0..2 {
        rig.core
            .calendar
            .lock()
            .unwrap()
            .answers
            .push_back(Err(EngineError::CalendarUnread));
    }
    let (view, cx, _) = window(&rig);
    assert_eq!(shown_ids(&view, cx), ["kept"]);
    assert!(
        cx.debug_bounds("calendar-failed").is_none(),
        "the kept events are shown instead"
    );
    rig.core.calendar.lock().unwrap().events = vec![event("fresh", 10)];
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(false, cx)));
    draw(cx);
    let stored_reads = rig.core.calendar.lock().unwrap().stored_reads.len();
    // back on the first week: still the store's, so it is read again, and the read replaces them
    assert_eq!(shown_ids(&view, cx), ["fresh"]);
    cx.update(|_, cx| view.update(cx, |view, cx| view.step(true, cx)));
    draw(cx);
    assert_eq!(
        rig.core.calendar.lock().unwrap().stored_reads.len(),
        stored_reads,
        "a span read in this run isn't drawn from the store again"
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

/// The account's teams: one holding the made-up channel, with its mailbox.
fn channel_team() -> teams_store::Team {
    teams_store::Team {
        id: fixtures::CHANNEL.to_owned(),
        name: "Lab team".to_owned(),
        archived: false,
        group_id: None,
        mailbox: Some(fixtures::TEAM_MAILBOX.to_owned()),
        owner: true,
        can_add_channel: true,
        can_add_private_channel: true,
        about: teams_store::TeamAbout::default(),
        channels: vec![teams_store::TeamChannel {
            id: fixtures::CHANNEL.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: teams_store::ChannelKind::Standard,
            shown: true,
        }],
    }
}

/// The account's own channel meeting as its calendar holds it: an attendee's copy (`isOrganizer` `false`).
fn own_channel_meeting() -> teams_core::CalendarEvent {
    let mut event = fixtures::channel_full(common::LAB_A).event;
    event.is_organizer = false;
    event.response = EventResponse::Accepted;
    event
}

#[test]
fn the_card_of_a_channel_meeting_the_account_scheduled_edits_it_from_the_team_s_calendar() {
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    rig.core.calendar.lock().unwrap().full.insert(
        fixtures::CHANNEL_EVENT.to_owned(),
        fixtures::channel_full(common::LAB_A),
    );
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(own_channel_meeting())).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card-edit").is_some(), "its organizer's Edit");
    assert!(cx.debug_bounds("calendar-card-remove").is_some(), "and Cancel meeting");
    click(cx, "calendar-card-edit");
    assert_eq!(
        rig.core.calendar.lock().unwrap().full_reads,
        [format!("{}/{}", fixtures::TEAM_MAILBOX, fixtures::CHANNEL_EVENT)]
    );
    let form = cx
        .update(|_, cx| teams_ui::event_form::shown(cx))
        .expect("the form opens");
    cx.update(|_, cx| {
        let form = form.read(cx);
        assert!(form.changes());
        assert_eq!(
            form.place().map(|place| place.channel.mailbox.as_str()),
            Some(fixtures::TEAM_MAILBOX)
        );
    });
    cx.update(|_, cx| teams_ui::event_form::close(cx));
    draw(cx);
}

#[test]
fn the_card_of_a_channel_meeting_the_account_scheduled_cancels_it_on_the_team_s_calendar() {
    let rig = Rig::start();
    *rig.core.teams.lock().unwrap() = Some(vec![channel_team()]);
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(own_channel_meeting())).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    click(cx, "calendar-card-remove");
    click(cx, "calendar-card-remove-confirm");
    let calendar = rig.core.calendar.lock().unwrap();
    let [
        EventWrite::CancelInChannel {
            meeting, channel, post, ..
        },
    ] = calendar.writes.as_slice()
    else {
        panic!("one cancellation in the channel: {:?}", calendar.writes);
    };
    assert_eq!(meeting.ical_uid, fixtures::CHANNEL_EVENT);
    assert_eq!(channel.mailbox, fixtures::TEAM_MAILBOX);
    assert_eq!(post, "1793680050510");
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

/// A Teams meeting nobody started, of someone else's series, as the calendar view gives one.
fn series_meeting(hour: u8) -> teams_core::CalendarEvent {
    let mut meeting = event("series-occurrence", hour);
    meeting.is_organizer = false;
    meeting.online = true;
    meeting.kind = teams_core::EventKind::Occurrence;
    meeting.join_url = Some("https://teams.microsoft.com/l/meetup-join/made-up".to_owned());
    meeting.meeting_chat = Some("19:meeting_MadeUpMeeting0001@thread.v2".to_owned());
    meeting.meeting = Some(teams_core::MeetingInfo {
        organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
        tenant_id: "00000000-0000-4000-8000-0000000000f1".to_owned(),
    });
    meeting
}

fn calling_rig() -> Rig {
    Rig::start_with(|cx| teams_ui::Models::with_calls(None, cx))
}

#[test]
fn a_meetings_card_offers_join_any_time_and_join_goes_to_its_pre_join() {
    let rig = calling_rig();
    let (view, cx, _) = window(&rig);
    let told = Arc::new(std::sync::Mutex::new(Vec::new()));
    let events = Arc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CalendarViewEvent, _| {
            if let CalendarViewEvent::Join(to_join) = event {
                events.lock().unwrap().push(to_join.clone());
            }
        })
        .detach();
    });
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    let title = shown.title();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(
        cx.debug_bounds("calendar-card-join-more").is_some(),
        "its menu's chevron"
    );
    assert!(cx.debug_bounds("calendar-card-chat").is_some());
    assert!(
        cx.debug_bounds("calendar-card-copy-link").is_none(),
        "copying is in Join's menu, as in Teams"
    );
    assert!(
        cx.debug_bounds("calendar-card-not-started").is_none(),
        "no note of waiting"
    );
    click(cx, "calendar-card-join");
    let told = told.lock().unwrap();
    assert_eq!(told.len(), 1);
    assert_eq!(told[0].chat, "19:meeting_MadeUpMeeting0001@thread.v2");
    assert_eq!(told[0].title, title);
    assert!(cx.update(|_, cx| view.read(cx).card().is_none()), "the card gives way");
}

#[test]
fn joins_menu_offers_join_and_copy_join_link() {
    let rig = calling_rig();
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    click(cx, "calendar-card-join-more");
    assert!(cx.debug_bounds("calendar-card-join-now").is_some());
    click(cx, "calendar-card-copy-link");
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
            .as_deref(),
        Some("https://teams.microsoft.com/l/meetup-join/made-up")
    );
}

#[test]
fn the_read_only_card_offers_no_join_but_still_copies_its_link() {
    let rig = calling_rig();
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx, _) = window(&rig);
    let shown = Shown::of(std::rc::Rc::new(series_meeting(9))).unwrap();
    cx.update(|_, cx| view.update(cx, |view, cx| view.open_card(shown, cx)));
    draw(cx);
    assert!(cx.debug_bounds("calendar-card-join").is_none());
    assert!(cx.debug_bounds("calendar-card-not-started").is_none());
    assert!(cx.debug_bounds("calendar-card-chat").is_some());
    assert!(cx.debug_bounds("calendar-card-copy-link").is_some());
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
    // Join with an ID: its panel, an ID that isn't one refused there, closed again
    click(cx, "calendar-join-id");
    assert!(cx.debug_bounds("calendar-join-id-panel").is_some());
    click(cx, "calendar-join-id-join");
    assert!(cx.debug_bounds("calendar-join-id-problem").is_some());
    click(cx, "calendar-join-id-close");
    assert!(cx.debug_bounds("calendar-join-id-panel").is_none());
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
