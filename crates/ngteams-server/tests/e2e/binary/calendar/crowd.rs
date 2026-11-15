//! A crowd: sixty more synthetic accounts the operator added, with names in many scripts and with quotes, markup and
//! emoji, all invited to one meeting with an odd subject. Each has it on its calendar, the organizer's roster names
//! each by the name the operator gave, the meeting's chat holds the invitation with its subject, and the Scheduler
//! reads everyone's time.

use reqwest::Method;
use serde_json::Value;
use teams_proto::EventAttendee;

use super::{Trio, create, invite, meeting_posts, raw};
use crate::calendar::{DAY, HOUR, MONDAY, coming_monday, draft};
use crate::support::{PASSWORD, backends};

const PEOPLE: usize = 60;

/// Synthetic display names: scripts, quotes, markup, emoji, a long one.
fn name(n: usize) -> String {
    let odd = [
        "Zoë O'Brien-Łukasiewicz",
        "李小龍",
        "محمد التجريبي",
        "Ada \"The Count\" Lovelace",
        "<b>Bold</b> & <script>x</script>",
        "😀 Smiley Tester",
        "Ἀριστοτέλης",
        "Ñandú Güemes",
    ];
    match n % 10 {
        0..=7 => format!("{} {n}", odd[n % 10]),
        8 => format!("{} {n}", "Long".repeat(30)),
        _ => format!("Plain Person {n}"),
    }
}

fn upn(n: usize) -> String {
    format!("crowd{n:02}@own.test")
}

const SUBJECT: &str = "Q&A <Plan> \"Ünïcødé\" 🚀 — كل شيء";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meeting_of_sixty_people_with_odd_names_reaches_each_with_its_subject_and_names() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        for n in 0..PEOPLE {
            let added = trio
                .rig
                .first
                .cli(&["user", "add", &upn(n), "--name", &name(n)], &format!("{PASSWORD}\n"));
            assert!(added.starts_with(&format!("added {} (", upn(n))), "{added}");
        }
        let start = MONDAY + 2 * DAY + 13 * HOUR;
        let mut all = draft(SUBJECT, start, 60);
        all.online = true;
        all.attendees = vec![invite(&trio.b)];
        all.attendees.extend((0..PEOPLE).map(|n| EventAttendee {
            address: upn(n),
            name: None,
            mri: None,
            required: n % 3 != 0,
        }));
        let created = create(&trio.a, all).await;

        // a's roster: everyone, each by the name the operator gave
        let whole = trio.a.engine().calendar_event(created.id.clone()).await.unwrap();
        let roster: Vec<(String, String, bool)> = whole
            .event
            .attendees
            .iter()
            .map(|one| {
                (
                    one.address.clone().unwrap_or_default(),
                    one.name.clone().unwrap_or_default(),
                    one.optional,
                )
            })
            .collect();
        assert_eq!(roster.len(), PEOPLE + 2, "{roster:?}");
        for n in 0..PEOPLE {
            assert!(
                roster.contains(&(upn(n), name(n), n % 3 == 0)),
                "{} missing: {roster:?}",
                upn(n)
            );
        }

        // each of the sixty has it, with its subject as sent, read with its own token at the other node
        let node = trio.rig.other();
        let view = "/v2.0/me/calendars/default/calendarView?StartDate=2027-01-04T00:00:00.000Z&EndDate=2027-01-11T00:00:00.000Z";
        for n in 0..PEOPLE {
            let (status, answer) = raw(node, &upn(n), Method::GET, view, None).await;
            assert_eq!(status, 200, "{}: {answer}", upn(n));
            let events = answer["value"].as_array().unwrap();
            assert_eq!(events.len(), 1, "{}: {answer}", upn(n));
            assert_eq!(events[0]["subject"], Value::from(SUBJECT), "{}", upn(n));
            assert_eq!(events[0]["objectId"], Value::from(created.id.as_str()), "{}", upn(n));
        }
        let chat = created.meeting_chat.clone().unwrap();
        let posts = meeting_posts(&trio.b, &chat).await;
        assert_eq!(posts, [(SUBJECT.to_owned(), false, "Scheduled a meeting".to_owned())]);
        let details = trio.b.engine().meeting(chat).await.unwrap().unwrap();
        assert_eq!(details.title.as_deref(), Some(SUBJECT));

        // the Scheduler reads all sixty in one ask, in a week clear of the meeting (the read covers five days from a
        // Monday a week or more ahead, which in late December 2026 would be the meeting's own week)
        let monday = coming_monday().max(MONDAY + 7 * DAY);
        let asked: Vec<EventAttendee> = (0..PEOPLE)
            .map(|n| EventAttendee {
                address: upn(n),
                name: Some(name(n)),
                mri: None,
                required: true,
            })
            .collect();
        let read = trio.a.engine().availability(asked, monday, 30).await.unwrap();
        assert_eq!(read.people.len(), PEOPLE + 1);
        for (n, person) in read.people.iter().take(PEOPLE).enumerate() {
            assert_eq!(person.address, upn(n));
            assert!(person.busy.is_empty(), "{}: {:?}", upn(n), person.busy);
        }
        assert!(!read.suggestions.is_empty());
        trio.stop().await;
    }
}
