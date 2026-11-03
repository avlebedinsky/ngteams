//! Scheduling against a synthetic calendar: saves, retries, read-only refusal and the occurrence/series distinction.

#![cfg(test)]

pub mod common;
#[path = "calendar/fixtures.rs"]
mod fixtures;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::NaiveDate;
use common::{Rig, Written};
use fixtures::full;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, px, size};
use teams_core::{EventKind, EventWrite, EventWritten};
use teams_ui::ViewTrace;
use teams_ui::event_form::{EventForm, Saved, Seed, Side, Slot};

fn slot() -> Slot {
    let day = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
    Slot::new(day.and_hms_opt(9, 0, 0).unwrap(), day.and_hms_opt(10, 0, 0).unwrap())
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

fn window(
    rig: &Rig,
    seed: Seed,
) -> (
    Entity<EventForm>,
    &'static mut VisualTestContext,
    Arc<AtomicUsize>,
    Written,
) {
    rig.cx.update(teams_ui::window::init);
    let saved = Arc::new(AtomicUsize::new(0));
    let finished = Arc::clone(&saved);
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let core = rig.core.clone();
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (root, cx) = app.add_window_view(|window, cx| {
        let form = cx.new(|cx| {
            EventForm::new(
                core,
                trace,
                seed,
                Saved::new(move |_| {
                    finished.fetch_add(1, Ordering::SeqCst);
                }),
                window,
                cx,
            )
        });
        teams_ui::window::KitRoot::new(form, window, cx)
    });
    let form = cx.update(|_, cx| root.read(cx).view().clone().downcast::<EventForm>().unwrap());
    cx.simulate_resize(size(px(1400.), px(900.)));
    draw(cx);
    (form, cx, saved, written)
}

fn new_seed() -> Seed {
    Seed::New {
        slot: slot(),
        online: false,
    }
}

#[test]
fn date_time_all_day_and_scheduler_controls_change_the_same_slot() {
    let rig = Rig::start();
    let (form, cx, _, _) = window(&rig, new_seed());
    for selector in ["event-date", "event-start", "event-end"] {
        click(cx, selector);
        cx.simulate_keystrokes("down enter");
        draw(cx);
    }
    let picked = cx.update(|_, cx| form.read(cx).slot());
    assert_eq!(
        picked.start.date(),
        chrono::Local::now().date_naive() - chrono::Duration::days(7)
    );
    assert_eq!(picked.start.time(), chrono::NaiveTime::MIN);
    assert_eq!(picked.length(), chrono::Duration::minutes(30));
    click(cx, "event-all-day");
    assert!(cx.update(|_, cx| form.read(cx).slot().all_day));
    assert!(cx.debug_bounds("event-start").is_none());
    click(cx, "event-all-day");
    assert!(!cx.update(|_, cx| form.read(cx).slot().all_day));
    assert!(cx.debug_bounds("event-start").is_some());
    click(cx, "event-online");
    assert!(cx.update(|_, cx| form.read(cx).online()));
    let scheduler_button = cx.debug_bounds("event-scheduler").unwrap();
    click(cx, "event-scheduler");
    assert_eq!(cx.update(|_, cx| form.read(cx).side()), Side::Scheduler);
    // The open scheduler pane shares this existing selector; keep the toolbar button's own position.
    cx.simulate_click(scheduler_button.center(), Modifiers::none());
    draw(cx);
    assert_eq!(cx.update(|_, cx| form.read(cx).side()), Side::Day);
}

#[test]
fn an_attendee_suggestion_keeps_its_identity_and_suggested_times_move_the_meeting() {
    let rig = Rig::start();
    let attendee = teams_core::Invitable {
        mri: common::LAB_B.to_owned(),
        name: "Synthetic guest".to_owned(),
        address: "guest@lab.example.test".to_owned(),
    };
    rig.core.calendar.lock().unwrap().people.push(attendee.clone());
    rig.core.calendar.lock().unwrap().availability.suggestions = vec![slot().zoned().0.ms + 7_200_000];
    let (form, cx, _, _) = window(&rig, new_seed());
    click(cx, "event-attendees");
    cx.simulate_input("Guest");
    draw(cx);
    cx.executor().advance_clock(std::time::Duration::from_millis(250));
    draw(cx);
    click(cx, "event-suggestion-guest@lab.example.test");
    assert!(
        rig.core
            .calendar
            .lock()
            .unwrap()
            .people_queries
            .contains(&"Guest".to_owned())
    );
    assert_eq!(
        cx.update(|_, cx| form.read(cx).attendees()[0].mri.clone()),
        Some(attendee.mri)
    );
    click(cx, "event-suggested-times");
    cx.simulate_keystrokes("down enter");
    draw(cx);
    assert_eq!(cx.update(|_, cx| form.read(cx).slot()), slot().moved(120));
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one meeting");
    };
    assert_eq!(draft.attendees[0].address, "guest@lab.example.test");
    assert_eq!(draft.start, slot().moved(120).zoned().0);
}

#[test]
fn the_body_format_and_emoji_controls_edit_the_draft_and_ctrl_s_saves_it() {
    let rig = Rig::start();
    let (form, cx, _, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in("Synthetic body", &[], "Details", window, cx);
        });
    });
    draw(cx);
    click(cx, "event-format");
    assert!(cx.update(|_, cx| form.read(cx).formatting()));
    assert!(cx.debug_bounds("format-bar").is_some());
    click(cx, "event-format");
    assert!(!cx.update(|_, cx| form.read(cx).formatting()));
    click(cx, "event-emoji");
    cx.simulate_input("eyes");
    cx.simulate_keystrokes("enter");
    draw(cx);
    assert!(cx.update(|_, cx| form.read(cx).printable(cx).body_text.contains('\u{1f440}')));
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    cx.simulate_keystrokes("ctrl-s");
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one event from Ctrl+S");
    };
    assert!(draft.body_html.contains('\u{1f440}'));
}

#[test]
fn sending_a_new_meeting_preserves_details_and_sends_once_while_a_save_is_in_flight() {
    let rig = Rig::start();
    rig.core.calendar.lock().unwrap().hold_writes = true;
    let (form, cx, saved, written) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in(
                "  Synthetic planning  ",
                &[
                    ("guest@lab.example.test", "Guest", true),
                    ("GUEST@lab.example.test", "Guest optional", false),
                    ("other@lab.example.test", "Other", true),
                ],
                "First line\nSecond line",
                window,
                cx,
            );
            form.remove_attendee("OTHER@lab.example.test", cx);
            form.save(window, cx);
            form.save(window, cx);
        });
    });
    draw(cx);
    {
        let calendar = rig.core.calendar.lock().unwrap();
        let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
            panic!("one creation");
        };
        assert_eq!(draft.subject, "Synthetic planning");
        assert!(draft.online);
        assert_eq!(draft.attendees.len(), 1);
        assert!(!draft.attendees[0].required);
        assert_eq!(draft.attendees[0].name.as_deref(), Some("Guest optional"));
        assert!(draft.body_html.contains("First line") && draft.body_html.contains("Second line"));
    }
    assert_eq!(saved.load(Ordering::SeqCst), 0);
    let answer = rig.core.calendar.lock().unwrap().held_writes.remove(0);
    answer.send(Ok(EventWritten { created: None }));
    draw(cx);
    assert_eq!(saved.load(Ordering::SeqCst), 1);
    assert!(
        written
            .lines()
            .iter()
            .any(|line| line["view"] == "event-form" && line["change"] == "saved")
    );
    assert!(!written.text().contains("Synthetic planning"));
}

#[test]
fn a_failed_save_stays_editable_and_can_be_retried() {
    let rig = Rig::start();
    let (form, cx, saved, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in("Synthetic retry", &[], "Body", window, cx);
            form.save(window, cx);
        });
    });
    draw(cx);
    assert!(cx.debug_bounds("event-problem").is_some());
    assert_eq!(saved.load(Ordering::SeqCst), 0);
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    click(cx, "event-save");
    assert_eq!(rig.core.calendar.lock().unwrap().writes.len(), 2);
    assert_eq!(saved.load(Ordering::SeqCst), 1);
    assert!(cx.debug_bounds("event-problem").is_none());
}

#[test]
fn updating_an_event_preserves_the_original_body_and_unknown_fields_until_edited() {
    let rig = Rig::start();
    let existing = full("existing", 9);
    let body = existing.body_html.clone().unwrap();
    let (form, cx, _, _) = window(&rig, Seed::Change(Box::new(existing)));
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            assert!(form.changes());
            form.set_slot(slot().moved(60), cx);
            form.save(window, cx);
        });
    });
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Update { id, event, draft }] = calendar.writes.as_slice() else {
        panic!("one update");
    };
    assert_eq!(id, "existing");
    assert_eq!(event["syntheticUnknown"], "keep me");
    assert_eq!(draft.body_html, body);
    assert_eq!(draft.categories, ["Blue category"]);
    assert_eq!(draft.reminder_minutes, Some(30));
    assert_eq!(draft.subject, "Synthetic event existing");
}

#[test]
fn the_read_only_form_renders_details_and_never_saves_or_removes_an_event() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (form, cx, saved, _) = window(&rig, Seed::Change(Box::new(full("read-only", 9))));
    assert!(cx.debug_bounds("event-form").is_some());
    assert!(cx.debug_bounds("event-delete").is_none());
    assert!(cx.debug_bounds("event-cancel-meeting").is_none());
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    click(cx, "event-save");
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    assert_eq!(saved.load(Ordering::SeqCst), 0);
    let printable = cx.update(|_, cx| form.read(cx).printable(cx));
    assert_eq!(printable.title, "Synthetic event read-only");
    assert!(printable.body_text.contains("Synthetic body"));
}

#[test]
fn choosing_series_on_an_occurrence_reads_and_updates_the_master_and_can_switch_back() {
    let rig = Rig::start();
    let mut occurrence = full("occurrence", 9);
    occurrence.event.kind = EventKind::Occurrence;
    occurrence.event.series_master_id = Some("master".to_owned());
    let mut master = full("master", 13);
    master.event.kind = EventKind::RecurringMaster;
    rig.core
        .calendar
        .lock()
        .unwrap()
        .full
        .insert("master".to_owned(), master);
    let (form, cx, _, _) = window(&rig, Seed::Change(Box::new(occurrence)));
    cx.update(|window, cx| form.update(cx, |form, cx| form.choose_series(true, window, cx)));
    draw(cx);
    assert!(cx.update(|_, cx| form.read(cx).on_series()));
    assert_eq!(rig.core.calendar.lock().unwrap().full_reads, ["master"]);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    assert!(matches!(&rig.core.calendar.lock().unwrap().writes[0], EventWrite::Update { id, .. } if id == "master"));
    cx.update(|window, cx| form.update(cx, |form, cx| form.choose_series(false, window, cx)));
    draw(cx);
    assert!(!cx.update(|_, cx| form.read(cx).on_series()));
    assert_eq!(cx.update(|_, cx| form.read(cx).slot()), slot());
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    assert!(
        matches!(&rig.core.calendar.lock().unwrap().writes[1], EventWrite::Update { id, .. } if id == "occurrence")
    );
}

#[test]
fn a_new_series_and_the_scheduler_follow_changes_to_the_slot() {
    let rig = Rig::start();
    let (form, cx, _, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.choose_series(true, window, cx);
            form.set_slot(slot().moved(24 * 60), cx);
            form.show_side(Side::Scheduler, cx);
        });
    });
    draw(cx);
    assert_eq!(cx.update(|_, cx| form.read(cx).side()), Side::Scheduler);
    let printable = cx.update(|_, cx| form.read(cx).printable(cx));
    assert!(printable.repeats.unwrap().contains("Wednesday"));
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    assert!(
        matches!(&rig.core.calendar.lock().unwrap().writes[0], EventWrite::Create(draft) if draft.recurrence.is_some())
    );
}

#[test]
fn deleting_an_appointment_requires_the_confirm_action() {
    let rig = Rig::start();
    let (_form, cx, _, _) = window(&rig, Seed::Change(Box::new(full("appointment", 9))));
    click(cx, "event-delete");
    assert!(cx.debug_bounds("event-confirm").is_some());
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    click(cx, "event-confirm-keep");
    assert!(cx.debug_bounds("event-confirm").is_none());
    click(cx, "event-delete");
    click(cx, "event-confirm-go");
    assert!(
        matches!(rig.core.calendar.lock().unwrap().writes.as_slice(), [EventWrite::Delete { id }] if id == "appointment")
    );
}

#[test]
fn an_invalid_pending_attendee_blocks_save_and_can_be_corrected_without_losing_the_draft() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let (form, cx, saved, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in("Keep this draft", &[], "Keep these details", window, cx);
        });
    });
    draw(cx);
    click(cx, "event-attendees");
    cx.simulate_input("unfinished-address");
    draw(cx);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    assert!(
        rig.core.calendar.lock().unwrap().writes.is_empty(),
        "an invalid attendee prevents the write"
    );
    assert_eq!(saved.load(Ordering::SeqCst), 0, "the draft stays open");
    assert!(
        cx.debug_bounds("event-problem").is_some(),
        "validation error remains visible"
    );
    assert!(cx.debug_bounds("event-form").is_some());
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("guest@lab.example.test");
    draw(cx);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one valid creation");
    };
    assert_eq!(draft.subject, "Keep this draft");
    assert!(draft.body_html.contains("Keep these details"));
    assert_eq!(draft.attendees[0].address, "guest@lab.example.test");
    assert_eq!(saved.load(Ordering::SeqCst), 1);
}

#[test]
fn toolbar_choices_are_keyboard_accessible_and_are_saved_in_the_draft() {
    let rig = Rig::start();
    let (form, cx, _, _) = window(&rig, new_seed());
    for selector in [
        "event-show-as",
        "event-reminder",
        "event-category",
        "event-privacy",
        "event-attendee-options",
    ] {
        click(cx, selector);
        cx.simulate_keystrokes("down enter");
        draw(cx);
    }
    assert!(cx.debug_bounds("event-optional-attendees").is_some());
    let printable = cx.update(|_, cx| form.read(cx).printable(cx));
    assert_eq!(printable.show_as, "Free");
    assert_eq!(printable.reminder, "Don't remind me");
    assert_eq!(printable.categories, ["Red category"]);
    assert!(printable.private);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one creation");
    };
    assert_eq!(draft.show_as, teams_core::EventShowAs::Free);
    assert!(draft.reminder_minutes.is_none());
    assert_eq!(draft.categories, ["Red category"]);
    assert!(draft.private);
}

#[test]
fn an_invalid_optional_attendee_keeps_a_valid_required_attendee_for_the_retry() {
    let rig = Rig::start();
    let (form, cx, saved, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in(
                "Synthetic invite",
                &[("known@lab.example.test", "Known", false)],
                "Details",
                window,
                cx,
            );
        });
    });
    draw(cx);
    click(cx, "event-attendees");
    cx.simulate_input("required@lab.example.test");
    draw(cx);
    click(cx, "event-optional-attendees");
    cx.simulate_input("invalid-optional");
    draw(cx);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    assert!(rig.core.calendar.lock().unwrap().writes.is_empty());
    assert_eq!(saved.load(Ordering::SeqCst), 0);
    assert!(cx.debug_bounds("event-problem").is_some());
    assert!(cx.update(|_, cx| {
        form.read(cx)
            .attendees()
            .iter()
            .any(|attendee| attendee.address == "required@lab.example.test" && attendee.required)
    }));
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("optional@lab.example.test");
    draw(cx);
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one creation after correction");
    };
    assert_eq!(draft.attendees.len(), 3);
    assert!(
        draft
            .attendees
            .iter()
            .any(|attendee| attendee.address == "optional@lab.example.test" && !attendee.required)
    );
}

#[test]
fn custom_recurrence_changes_are_saved_only_after_confirmation() {
    let rig = Rig::start();
    let (form, cx, _, _) = window(&rig, new_seed());
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.choose_series(true, window, cx);
            form.open_custom(window, cx);
        });
    });
    draw(cx);
    click(cx, "event-custom-more");
    click(cx, "event-custom-cancel");
    assert!(cx.debug_bounds("event-custom-repeat").is_none());
    assert!(!cx.update(|_, cx| form.read(cx).printable(cx).repeats.unwrap().contains("2 weeks")));
    cx.update(|window, cx| form.update(cx, |form, cx| form.open_custom(window, cx)));
    draw(cx);
    click(cx, "event-custom-unit-day");
    click(cx, "event-custom-unit-month");
    click(cx, "event-custom-on-date");
    click(cx, "event-custom-on-weekday");
    click(cx, "event-custom-unit-year");
    click(cx, "event-custom-on-date");
    click(cx, "event-custom-unit-week");
    click(cx, "event-custom-day-3");
    click(cx, "event-custom-more");
    click(cx, "event-custom-more");
    click(cx, "event-custom-less");
    click(cx, "event-custom-save");
    assert!(cx.debug_bounds("event-custom-repeat").is_none());
    cx.update(|window, cx| form.update(cx, |form, cx| form.save(window, cx)));
    draw(cx);
    let calendar = rig.core.calendar.lock().unwrap();
    let [EventWrite::Create(draft)] = calendar.writes.as_slice() else {
        panic!("one series creation");
    };
    assert!(matches!(&draft.recurrence.as_ref().unwrap().pattern,
        teams_proto::RecurrencePattern::Weekly { interval: 2, days }
            if days.contains(&teams_proto::Weekday::Tuesday) && days.contains(&teams_proto::Weekday::Wednesday)));
}

#[test]
fn a_channel_s_meeting_names_its_channel_and_is_sent_to_the_team_s_calendar() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let place = teams_ui::event_form::MeetingPlace {
        channel: teams_core::MeetingChannel {
            channel: "19:0123456789abcdef0123456789abcdef@thread.tacv2".to_owned(),
            mailbox: "lab-team@lab.example.test".to_owned(),
        },
        team: "Lab team".to_owned(),
        name: "Planning".to_owned(),
    };
    let (form, cx, saved, _) = window(
        &rig,
        Seed::InChannel {
            slot: slot(),
            place: place.clone(),
        },
    );
    // the channel's row in place of the Teams meeting switch: a channel's meeting is always one
    assert!(cx.debug_bounds("event-channel").is_some());
    assert!(cx.debug_bounds("event-online").is_none());
    assert!(cx.update(|_, cx| form.read(cx).online()));
    click(cx, "event-personal-invites");
    assert!(cx.update(|_, cx| form.read(cx).personal_invites()));
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in("Synthetic channel meeting", &[], "", window, cx);
            form.save(window, cx);
        });
    });
    draw(cx);
    {
        let calendar = rig.core.calendar.lock().unwrap();
        let [
            EventWrite::CreateInChannel {
                draft,
                channel,
                personal_invites,
            },
        ] = calendar.writes.as_slice()
        else {
            panic!("one creation in the channel");
        };
        assert_eq!(draft.subject, "Synthetic channel meeting");
        assert!(draft.online);
        assert_eq!(*channel, place.channel);
        assert!(*personal_invites);
    }
    assert_eq!(saved.load(Ordering::SeqCst), 1);
}

fn channel_place() -> teams_ui::event_form::MeetingPlace {
    teams_ui::event_form::MeetingPlace {
        channel: teams_core::MeetingChannel {
            channel: fixtures::CHANNEL.to_owned(),
            mailbox: fixtures::TEAM_MAILBOX.to_owned(),
        },
        team: "Lab team".to_owned(),
        name: "Planning".to_owned(),
    }
}

#[test]
fn a_channel_s_meeting_opened_to_change_sends_its_update_to_the_team_s_calendar() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let full = fixtures::channel_full(common::LAB_A);
    let (form, cx, saved, _) = window(
        &rig,
        Seed::ChangeInChannel {
            full: Box::new(full),
            place: channel_place(),
        },
    );
    // the channel's row, its Send update and its organizer's Cancel meeting; no Teams meeting switch
    assert!(cx.debug_bounds("event-channel").is_some());
    assert!(cx.debug_bounds("event-online").is_none());
    assert!(cx.debug_bounds("event-cancel-meeting").is_some());
    assert!(cx.update(|_, cx| form.read(cx).changes() && form.read(cx).place().is_some()));
    cx.update(|window, cx| {
        form.update(cx, |form, cx| {
            form.fill_in("Synthetic channel meeting renamed", &[], "", window, cx);
        });
    });
    click(cx, "event-personal-invites");
    click(cx, "event-save");
    {
        let calendar = rig.core.calendar.lock().unwrap();
        let [EventWrite::UpdateInChannel { event, draft, channel }] = calendar.writes.as_slice() else {
            panic!("one change in the channel: {:?}", calendar.writes);
        };
        assert_eq!(draft.subject, "Synthetic channel meeting renamed");
        assert_eq!(*channel, channel_place().channel);
        assert_eq!(event["iCalUID"], fixtures::CHANNEL_EVENT);
        assert_eq!(event["inviteAllMembers"], true);
        // the team isn't among whom it invites: the channel's row stands for it
        assert!(
            draft
                .attendees
                .iter()
                .all(|attendee| attendee.address != fixtures::TEAM_MAILBOX)
        );
    }
    assert_eq!(saved.load(Ordering::SeqCst), 1);
}

#[test]
fn a_channel_s_meeting_cancelled_from_its_details_is_cancelled_on_the_team_s_calendar_with_its_note() {
    let rig = Rig::start();
    rig.core
        .calendar
        .lock()
        .unwrap()
        .write_answers
        .push_back(Ok(EventWritten { created: None }));
    let (_form, cx, _, _) = window(
        &rig,
        Seed::ChangeInChannel {
            full: Box::new(fixtures::channel_full(common::LAB_A)),
            place: channel_place(),
        },
    );
    click(cx, "event-cancel-meeting");
    assert!(cx.debug_bounds("event-confirm").is_some());
    click(cx, "event-confirm-go");
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
    assert_eq!(post, "1793680050510");
    assert_eq!(*channel, channel_place().channel);
}
