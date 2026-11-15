//! An appointment and a meeting through their life: made, read by day, week and month, changed, cancelled with a
//! note and deleted, and still there after the client and the server restarted.

use teams_proto::{EventKind, EventResponse};

use super::{Trio, cancel, change, create, delete, invite, meeting_posts, refused, shown, window};
use crate::calendar::{DAY, HOUR, MONDAY, draft};
use crate::support::backends;

const DENTIST: i64 = MONDAY + DAY + 9 * HOUR;
const PLANNING: i64 = MONDAY + 2 * DAY + 14 * HOUR;
/// Friday 2027-01-01, the month view's first day.
const JANUARY: i64 = MONDAY - 3 * DAY;

fn pairs(list: &[(&str, i64)]) -> Vec<(String, i64)> {
    list.iter()
        .map(|(subject, start)| ((*subject).to_owned(), *start))
        .collect()
}

/// a's day, week and month views (`first` and `second` by start, on different days), and b's week (`meeting`).
async fn views_show(trio: &Trio, first: (&str, i64), second: (&str, i64), meeting: (&str, i64)) {
    let day_of = |start: i64| MONDAY + (start - MONDAY).div_euclid(DAY) * DAY;
    let a = &trio.a;
    assert_eq!(shown(&window(a, day_of(first.1), 1).await), pairs(&[first]));
    assert_eq!(shown(&window(a, day_of(second.1), 1).await), pairs(&[second]));
    let both = pairs(&[first, second]);
    assert_eq!(shown(&window(a, MONDAY, 7).await), both);
    assert_eq!(shown(&window(a, JANUARY, 31).await), both);
    assert!(window(a, JANUARY + 31 * DAY, 28).await.is_empty());
    assert_eq!(shown(&window(&trio.b, MONDAY, 7).await), pairs(&[meeting]));
    assert!(window(&trio.c, MONDAY, 7).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_appointment_and_a_meeting_are_made_changed_cancelled_and_deleted_and_outlast_restarts() {
    for backend in backends() {
        let mut trio = Trio::start(backend).await;
        let mut dentist = draft("Dentist", DENTIST, 45);
        dentist.location = "Main street 5".into();
        dentist.body_html = "<p>Bring the x-rays</p>".into();
        let appointment = create(&trio.a, dentist.clone()).await;
        assert_eq!(appointment.kind, EventKind::Single);
        let mut planning = draft("Planning", PLANNING, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b)];
        let meeting = create(&trio.a, planning.clone()).await;
        assert!(meeting.online && meeting.meeting_chat.is_some());
        views_show(
            &trio,
            ("Dentist", DENTIST),
            ("Planning", PLANNING),
            ("Planning", PLANNING),
        )
        .await;

        // changed: the appointment moved two days and shortened, the meeting an hour later and renamed
        dentist.subject = "Dentist, moved".into();
        dentist.start.ms += 2 * DAY;
        dentist.end.ms = dentist.start.ms + 30 * 60_000;
        change(&trio.a, &appointment.id, dentist).await.unwrap();
        planning.subject = "Planning, later".into();
        planning.start.ms += HOUR;
        planning.end.ms += HOUR;
        change(&trio.a, &meeting.id, planning).await.unwrap();
        let moved = (
            ("Planning, later", PLANNING + HOUR),
            ("Dentist, moved", DENTIST + 2 * DAY),
        );
        views_show(&trio, moved.0, moved.1, moved.0).await;

        // the client closed and opened again shows them from its store before it reads, then reads the same
        trio.a.restart().await;
        let kept = trio.a.engine().stored_calendar(MONDAY, MONDAY + 7 * DAY).await.unwrap();
        assert_eq!(shown(&kept), pairs(&[moved.0, moved.1]));
        // the server stopped and started again keeps them, whole
        trio.rig.restart().await;
        views_show(&trio, moved.0, moved.1, moved.0).await;
        let whole = trio.a.engine().calendar_event(appointment.id.clone()).await.unwrap();
        assert_eq!(whole.body_html.as_deref(), Some("<p>Bring the x-rays</p>"));
        assert_eq!(whole.event.location.as_deref(), Some("Main street 5"));
        assert_eq!(
            whole.event.end,
            teams_proto::EventTime::Instant(DENTIST + 2 * DAY + 30 * 60_000)
        );

        // cancelled with a note: gone from its organizer's calendar, cancelled on b's, the note in its chat
        let current = window(&trio.a, MONDAY, 7).await.remove(0);
        cancel(&trio.a, &current, Some("Postponed to next week")).await.unwrap();
        assert_eq!(shown(&window(&trio.a, MONDAY, 7).await), pairs(&[moved.1]));
        let theirs = window(&trio.b, MONDAY, 7).await;
        assert_eq!(theirs.len(), 1, "{theirs:?}");
        assert!(theirs[0].cancelled);
        assert_eq!(theirs[0].subject, "Canceled: Planning, later");
        assert_eq!(theirs[0].response, EventResponse::NotResponded);
        let chat = meeting.meeting_chat.clone().unwrap();
        let posts = meeting_posts(&trio.b, &chat).await;
        assert_eq!(posts.len(), 2, "{posts:?}");
        assert_eq!(
            posts[1],
            (
                "Planning, later".to_owned(),
                true,
                "The meeting \"Planning, later\" is cancelled: Postponed to next week".to_owned()
            )
        );

        // deleted: the appointment by its owner, the cancelled meeting by b; a second delete finds nothing
        delete(&trio.a, &appointment.id).await.unwrap();
        delete(&trio.b, &meeting.id).await.unwrap();
        refused(delete(&trio.a, &appointment.id).await, "404");
        assert!(trio.a.engine().calendar_event(appointment.id.clone()).await.is_err());
        trio.rig.restart().await;
        for user in [&trio.a, &trio.b] {
            assert!(window(user, JANUARY, 31).await.is_empty(), "{}", user.upn);
        }
        trio.stop().await;
    }
}
