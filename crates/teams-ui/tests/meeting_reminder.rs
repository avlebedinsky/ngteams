//! A calendar meeting's reminder on GPUI's test context and its clock, over the fake engine (`common`) and a fake
//! notification server: a meeting is reminded at its reminder time and again as it starts, once each; a run started
//! after the reminder time reminds at once; an event the calendar view gives no reminder for is read whole first; a
//! declined, cancelled or offline one, the switch off and Do not disturb remind of nothing; a meeting that moves closes
//! its notice; Snooze, Dismiss and Join do what they say, from the desktop notice and from the window's card, also
//! when Snooze ends after the start or the server answers after a Dismiss; a reminder changed during the run is read
//! again; the calendar is read from the service without the calendar view. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;
#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
#[path = "common/notices.rs"]
mod notices;

use std::sync::{Arc, Mutex};

use common::{Rig, Written};
use futures_util::future::BoxFuture;
use gpui::{AppContext as _, Entity};
use notices::FakeServer;
use teams_core::{CalendarEvent, EventKind, EventReminder, EventResponse, EventTime, MeetingInfo, ShowAs};
use teams_ui::ViewTrace;
use teams_ui::meeting_alert::MeetingJoin;
use teams_ui::meeting_reminder::{self, MeetingReminders, ReminderEvent, Stage, TICK};
use teams_ui::models::Models;
use teams_ui::notify::{DISMISS_ACTION, JOIN_ACTION, Notice, NoticeKind, NotificationServer, SNOOZE_ACTION};
use teams_ui::settings::{ManualStatus, Settings, SettingsModel};

const MEETING: &str = "19:meeting_MadeUpReview01@thread.v2";
const MINUTE: i64 = 60_000;

struct Reminding {
    rig: Rig,
    reminders: Entity<MeetingReminders>,
    server: Arc<FakeServer>,
    asked: Arc<Mutex<Vec<ReminderEvent>>>,
    written: Written,
    /// Lets one held answer of the server go (`start_held`).
    release: Option<async_channel::Sender<()>>,
}

/// A notification server slow to answer: each show is recorded at once, its id answered once released.
struct HeldServer {
    inner: Arc<FakeServer>,
    gate: async_channel::Receiver<()>,
}

impl NotificationServer for HeldServer {
    fn show(&self, notice: &Notice, replaces: u32) -> BoxFuture<'static, Result<u32, String>> {
        let shown = self.inner.show(notice, replaces);
        let gate = self.gate.clone();
        Box::pin(async move {
            let _ = gate.recv().await;
            shown.await
        })
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        self.inner.close(id)
    }
}

/// The account's online meeting `id` from `start` (ms) for half an hour, its reminder `reminder`.
fn meeting(id: &str, start: i64, reminder: EventReminder) -> CalendarEvent {
    CalendarEvent {
        id: id.to_owned(),
        subject: "Planning".to_owned(),
        start: EventTime::Instant(start),
        end: EventTime::Instant(start + 30 * MINUTE),
        all_day: false,
        kind: EventKind::Single,
        cancelled: false,
        online: true,
        join_url: Some("https://teams.microsoft.com/l/meetup-join/made-up".to_owned()),
        meeting_chat: Some(MEETING.to_owned()),
        meeting: Some(MeetingInfo {
            organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
            tenant_id: "00000000-0000-4000-8000-0000000000d0".to_owned(),
        }),
        meeting_code: None,
        location: None,
        organizer_name: Some("Bee".to_owned()),
        organizer_address: None,
        is_organizer: false,
        response: EventResponse::Accepted,
        show_as: ShowAs::Busy,
        private: false,
        preview: None,
        conference_id: None,
        attendees: Vec::new(),
        series_master_id: None,
        channel_meeting: None,
        reminder,
    }
}

impl Reminding {
    /// The reminders over a calendar of the events `events` makes from now (ms) on the test clock.
    fn start(settings: Settings, events: impl FnOnce(i64) -> Vec<CalendarEvent>) -> Self {
        Self::start_on(settings, events, false)
    }

    /// As [`Self::start`], the server answering each show only when [`Self::release`] lets it.
    fn start_held(settings: Settings, events: impl FnOnce(i64) -> Vec<CalendarEvent>) -> Self {
        Self::start_on(settings, events, true)
    }

    fn start_on(settings: Settings, events: impl FnOnce(i64) -> Vec<CalendarEvent>, held: bool) -> Self {
        let rig = Rig::start_with(|cx| Models::with_calls(None, cx));
        let now = rig.cx.read(meeting_reminder::wall_ms);
        let events = events(now);
        {
            let mut calendar = rig.core.calendar.lock().unwrap();
            calendar.events.clone_from(&events);
            calendar.stored = events;
        }
        let written = Written::default();
        let trace = Arc::new(ViewTrace::new(written.clone()));
        let server = Arc::new(FakeServer::default());
        let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
        let settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings)));
        let calls = rig.models.calls.clone();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&asked);
        let (release, gate) = async_channel::unbounded();
        let given: Arc<dyn NotificationServer> = if held {
            Arc::new(HeldServer {
                inner: Arc::clone(&server),
                gate,
            })
        } else {
            server.clone()
        };
        let reminders = rig.cx.update(|cx| {
            let reminders = cx.new(|cx| {
                let mut reminders = MeetingReminders::new(core, settings, calls, trace, cx);
                reminders.set_server(given);
                reminders
            });
            cx.subscribe(&reminders, move |_, event, _| {
                recorded.lock().unwrap().push(event.clone());
            })
            .detach();
            reminders
        });
        rig.cx.run_until_parked();
        Self {
            rig,
            reminders,
            server,
            asked,
            written,
            release: held.then_some(release),
        }
    }

    /// The held server answers its oldest show.
    fn release(&self) {
        if let Some(release) = &self.release {
            release.try_send(()).unwrap();
        }
        self.rig.cx.run_until_parked();
    }

    /// The account's event `id` reads whole as `event`, its reminder `minutes` before.
    fn whole(&self, event: CalendarEvent, minutes: u32) {
        self.rig.core.calendar.lock().unwrap().full.insert(
            event.id.clone(),
            teams_core::FullEvent {
                event,
                raw: serde_json::Map::new(),
                body_html: None,
                reminder_minutes: Some(minutes),
                categories: Vec::new(),
                optional: Vec::new(),
                recurrence: None,
            },
        );
    }

    fn snooze(&self, minutes: u32) {
        let occasion = self.cards()[0].occasion.clone();
        self.rig.cx.update(|cx| {
            self.reminders
                .update(cx, |reminders, cx| reminders.snooze(&occasion, minutes, cx));
        });
        self.rig.cx.run_until_parked();
    }

    fn now(&self) -> i64 {
        self.rig.cx.read(meeting_reminder::wall_ms)
    }

    /// The clock moved on by `minutes`, a tick at a time.
    fn wait(&self, minutes: u64) {
        let ticks = minutes * 60 / TICK.as_secs();
        for _ in 0..ticks {
            self.rig.cx.executor().advance_clock(TICK);
            self.rig.cx.run_until_parked();
        }
    }

    /// The calendar holds `events` from now on, the store as well.
    fn calendar(&self, events: Vec<CalendarEvent>) {
        let mut calendar = self.rig.core.calendar.lock().unwrap();
        calendar.events.clone_from(&events);
        calendar.stored = events;
    }

    fn bodies(&self) -> Vec<String> {
        self.server.shown().into_iter().map(|notice| notice.body).collect()
    }

    fn cards(&self) -> Vec<meeting_reminder::ReminderCard> {
        self.rig.cx.read(|cx| self.reminders.read(cx).cards(cx))
    }

    fn activate(&self, id: u32, action: &str) -> bool {
        let taken = self.rig.cx.update(|cx| {
            self.reminders
                .update(cx, |reminders, cx| reminders.activated(id, action, cx))
        });
        self.rig.cx.run_until_parked();
        taken
    }
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or_default()
}

#[test]
fn a_meeting_is_reminded_at_its_reminder_time_and_again_as_it_starts_once_each() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 16 * MINUTE, EventReminder::Before(15))]
    });
    assert!(r.server.shown().is_empty(), "16 minutes before: not yet");
    // the service's calendar was read: the calendar view was never opened
    assert!(!r.rig.core.calendar.lock().unwrap().reads.is_empty());
    r.wait(1);
    let shown = r.server.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].kind, NoticeKind::Reminder);
    assert_eq!(shown[0].summary, "Planning");
    assert_eq!(shown[0].conversation, MEETING);
    assert_eq!(first_line(&shown[0].body), "Starts in 15 minutes");
    one_card_before_it_starts(&r);
    r.wait(10);
    assert_eq!(r.server.shown().len(), 1, "once before it starts");
    r.wait(5);
    let bodies = r.bodies();
    assert_eq!(bodies.len(), 2);
    assert_eq!(first_line(&bodies[1]), "Starting now");
    // the start's notice takes the reminder's place
    assert!(matches!(
        r.server.asked().last(),
        Some(notices::Asked::Show { replaces: 1, .. })
    ));
    r.wait(10);
    assert_eq!(r.server.shown().len(), 2, "once as it starts");
    traced_without_its_title(&r);
}

/// The window shows the reminder as one card, Join offered.
fn one_card_before_it_starts(r: &Reminding) {
    let cards = r.cards();
    assert_eq!(cards.len(), 1);
    assert_eq!((cards[0].stage, cards[0].joinable), (Stage::Before, true));
}

/// The trace names the chat and what happened, never the title.
fn traced_without_its_title(r: &Reminding) {
    let text = r.written.text();
    assert!(text.contains("meeting-reminder") && text.contains("shown"), "{text}");
    assert!(!text.contains("Planning"), "{text}");
}

#[test]
fn a_run_started_after_the_reminder_time_reminds_at_once() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))]
    });
    assert_eq!(
        r.bodies().iter().map(|body| first_line(body)).collect::<Vec<_>>(),
        ["Starts in 5 minutes"]
    );
}

#[test]
fn a_meeting_whose_read_whole_fails_is_reminded_at_outlook_s_default() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 10 * MINUTE, EventReminder::Unknown)]
    });
    assert_eq!(r.rig.core.calendar.lock().unwrap().full_reads, ["planning"]);
    assert_eq!(
        r.bodies().iter().map(|body| first_line(body)).collect::<Vec<_>>(),
        ["Starts in 10 minutes"]
    );
}

#[test]
fn an_event_read_whole_gives_its_reminder() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("later", now + 30 * MINUTE, EventReminder::Unknown)]
    });
    let now = r.now();
    r.whole(meeting("planning", now + 10 * MINUTE, EventReminder::Before(5)), 5);
    r.calendar(vec![meeting("planning", now + 10 * MINUTE, EventReminder::Unknown)]);
    r.wait(4);
    assert!(r.server.shown().is_empty(), "6 minutes before a 5-minute reminder");
    r.wait(1);
    assert_eq!(
        r.bodies().iter().map(|body| first_line(body)).collect::<Vec<_>>(),
        ["Starts in 5 minutes"]
    );
}

#[test]
fn declined_cancelled_offline_or_reminderless_meetings_remind_of_nothing() {
    let r = Reminding::start(Settings::default(), |now| {
        let at = now + 5 * MINUTE;
        let mut declined = meeting("declined", at, EventReminder::Before(15));
        declined.response = EventResponse::Declined;
        let mut cancelled = meeting("cancelled", at, EventReminder::Before(15));
        cancelled.cancelled = true;
        let mut offline = meeting("offline", at, EventReminder::Before(15));
        offline.online = false;
        let mut all_day = meeting("all-day", at, EventReminder::Before(15));
        all_day.all_day = true;
        vec![
            declined,
            cancelled,
            offline,
            all_day,
            meeting("off", at, EventReminder::Off),
        ]
    });
    r.wait(10);
    assert!(r.server.shown().is_empty(), "{:?}", r.server.shown());
}

#[test]
fn the_switch_off_or_do_not_disturb_remind_of_nothing() {
    let off = Reminding::start(
        Settings {
            meeting_reminders: false,
            ..Settings::default()
        },
        |now| vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))],
    );
    assert!(off.server.shown().is_empty());
    assert!(off.cards().is_empty());
    let mut busy = Settings::default();
    busy.set_manual_status(common::LAB_A, ManualStatus::DoNotDisturb);
    let dnd = Reminding::start(busy, |now| {
        vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))]
    });
    assert!(dnd.server.shown().is_empty());
}

#[test]
fn a_meeting_that_moves_or_is_cancelled_closes_its_reminder() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))]
    });
    assert_eq!(r.server.shown().len(), 1);
    let now = r.now();
    r.calendar(vec![meeting("planning", now + 60 * MINUTE, EventReminder::Before(15))]);
    r.wait(1);
    assert_eq!(r.server.closed(), [1]);
    assert!(r.cards().is_empty());
    // moved to start in ten minutes: a new meeting, reminded anew
    let now = r.now();
    r.calendar(vec![meeting("planning", now + 10 * MINUTE, EventReminder::Before(15))]);
    r.wait(1);
    assert_eq!(r.server.shown().len(), 2);
    let mut cancelled = meeting("planning", now + 10 * MINUTE, EventReminder::Before(15));
    cancelled.cancelled = true;
    r.calendar(vec![cancelled]);
    r.wait(1);
    assert_eq!(r.server.closed(), [1, 2]);
}

#[test]
fn snooze_reminds_again_after_its_time_and_dismiss_ends_the_reminders() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 14 * MINUTE, EventReminder::Before(15))]
    });
    assert!(r.activate(1, SNOOZE_ACTION));
    assert_eq!(r.server.closed(), [1]);
    assert!(r.cards().is_empty());
    r.wait(4);
    assert_eq!(r.server.shown().len(), 1, "snoozed for five minutes");
    r.wait(2);
    assert_eq!(r.server.shown().len(), 2, "reminded again");
    // the card's Snooze "until it starts", then Dismiss at the start
    r.snooze(0);
    r.wait(7);
    assert_eq!(r.server.shown().len(), 2, "snoozed until it starts");
    r.wait(1);
    assert_eq!(r.server.shown().len(), 3, "reminded as it starts");
    assert_eq!(first_line(&r.bodies()[2]), "Starting now");
    assert!(r.activate(3, DISMISS_ACTION));
    r.wait(10);
    assert_eq!(r.server.shown().len(), 3);
    assert!(r.cards().is_empty());
    assert!(!r.activate(99, DISMISS_ACTION), "not a reminder's");
}

#[test]
fn join_goes_into_the_meeting_as_the_calendar_s_join_does() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))]
    });
    assert!(r.activate(1, JOIN_ACTION));
    assert_eq!(
        *r.asked.lock().unwrap(),
        [ReminderEvent::Join(MeetingJoin::Calendar {
            chat: Some(MEETING.to_owned()),
            meeting: Some(MeetingInfo {
                organizer_id: "00000000-0000-4000-8000-00000000000b".to_owned(),
                tenant_id: "00000000-0000-4000-8000-0000000000d0".to_owned(),
            }),
            title: "Planning".to_owned(),
            link: Some("https://teams.microsoft.com/l/meetup-join/made-up".to_owned()),
        })]
    );
    assert_eq!(r.server.closed(), [1]);
    r.wait(10);
    assert_eq!(r.server.shown().len(), 1, "joined: no start notice");
}

#[test]
fn a_snooze_that_ends_well_after_the_start_reminds_again() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + MINUTE, EventReminder::Before(15))]
    });
    r.wait(1);
    assert_eq!(first_line(&r.bodies()[1]), "Starting now");
    r.wait(6);
    assert!(r.activate(1, SNOOZE_ACTION));
    r.wait(4);
    assert_eq!(r.server.shown().len(), 2, "snoozed for five minutes");
    r.wait(2);
    assert_eq!(r.server.shown().len(), 3, "reminded again 11 minutes after the start");
    assert_eq!(first_line(&r.bodies()[2]), "Starting now");
    // the card's longest Snooze, from 12 to 27 minutes after the start
    r.snooze(15);
    r.wait(14);
    assert_eq!(r.server.shown().len(), 3);
    r.wait(2);
    assert_eq!(r.server.shown().len(), 4, "reminded again until the meeting ends");
}

#[test]
fn a_reminder_changed_during_the_run_is_read_again() {
    let r = Reminding::start(Settings::default(), |now| {
        vec![meeting("planning", now + 34 * MINUTE, EventReminder::Unknown)]
    });
    assert_eq!(r.rig.core.calendar.lock().unwrap().full_reads, ["planning"]);
    // the first read failed (Outlook's 15 minutes); the organizer then set 30 minutes
    let now = r.now();
    r.whole(meeting("planning", now + 34 * MINUTE, EventReminder::Before(30)), 30);
    r.wait(4);
    assert!(r.server.shown().is_empty(), "the read is not stale yet");
    r.wait(2);
    assert_eq!(r.rig.core.calendar.lock().unwrap().full_reads.len(), 2);
    assert_eq!(
        r.bodies().iter().map(|body| first_line(body)).collect::<Vec<_>>(),
        ["Starts in 29 minutes"]
    );
}

#[test]
fn a_notice_the_server_answers_after_a_dismiss_is_closed() {
    let r = Reminding::start_held(Settings::default(), |now| {
        vec![meeting("planning", now + 5 * MINUTE, EventReminder::Before(15))]
    });
    assert_eq!(r.server.shown().len(), 1);
    let occasion = r.cards()[0].occasion.clone();
    r.rig.cx.update(|cx| {
        r.reminders.update(cx, |reminders, cx| reminders.dismiss(&occasion, cx));
    });
    r.rig.cx.run_until_parked();
    assert!(r.server.closed().is_empty(), "the server didn't name it yet");
    r.release();
    assert_eq!(r.server.closed(), [1]);
    assert!(!r.activate(1, DISMISS_ACTION), "not kept as the reminder's notice");
}
