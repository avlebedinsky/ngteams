//! A node restarted under running clients: on two nodes, the one b and c use stops while a changes a meeting at the
//! other, and the one a uses stops while b answers; each client carries on once its node is back, and nothing is
//! lost. On one node, the node itself.

use teams_core::activity::{ActivityKind, MeetingChange};
use teams_proto::{EventResponse, RsvpAnswer};

use super::{Trio, change, create, invite, respond, shown, window};
use crate::calendar::{HOUR, MONDAY, draft};
use crate::support::backends;

const PLANNING: i64 = MONDAY + 3 * 24 * HOUR + 10 * HOUR;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_restarted_keeps_the_calendar_and_its_clients_carry_on() {
    for backend in backends() {
        let mut trio = Trio::start(backend).await;
        let mut planning = draft("Planning", PLANNING, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b), invite(&trio.c)];
        let created = create(&trio.a, planning.clone()).await;
        respond(&trio.b, &created.id, RsvpAnswer::Accept).await.unwrap();

        // the node of b and c down: a changes the meeting at its own node meanwhile
        let (on_b, on_c) = (trio.b.mark(), trio.c.mark());
        let two = trio.rig.second.is_some();
        let theirs = trio.rig.second.as_mut().unwrap_or(&mut trio.rig.first);
        theirs.stop().await;
        planning.start.ms += HOUR;
        planning.end.ms += HOUR;
        if two {
            change(&trio.a, &created.id, planning.clone()).await.unwrap();
            assert!(trio.b.engine().calendar(MONDAY, MONDAY + 7 * 24 * HOUR).await.is_err());
        }
        let theirs = trio.rig.second.as_mut().unwrap_or(&mut trio.rig.first);
        theirs.start().await;
        if !two {
            change(&trio.a, &created.id, planning.clone()).await.unwrap();
        }
        for (user, mark) in [(&mut trio.b, on_b), (&mut trio.c, on_c)] {
            user.online(mark).await;
            let feed = user
                .feed("the change", |feed| {
                    feed.items
                        .first()
                        .is_some_and(|item| item.kind == ActivityKind::Meeting(MeetingChange::Updated))
                })
                .await;
            assert_eq!(feed.items.len(), 2, "{}: {feed:#?}", user.upn);
            assert_eq!(
                shown(&window(user, MONDAY, 7).await),
                [("Planning".to_owned(), PLANNING + HOUR)]
            );
        }

        // a's node down: b answers at its own node; a reads the answer once its node is back
        trio.rig.first.stop().await;
        if two {
            respond(&trio.b, &created.id, RsvpAnswer::Decline).await.unwrap();
        }
        trio.rig.first.start().await;
        if !two {
            respond(&trio.b, &created.id, RsvpAnswer::Decline).await.unwrap();
        }
        let whole = trio.a.engine().calendar_event(created.id.clone()).await.unwrap();
        let answers: Vec<Option<EventResponse>> = whole.event.attendees.iter().map(|a| a.response.clone()).collect();
        assert_eq!(
            answers,
            [
                Some(EventResponse::Organizer),
                Some(EventResponse::Declined),
                Some(EventResponse::NoResponse)
            ]
        );
        trio.a.restart().await;
        assert_eq!(
            shown(&window(&trio.a, MONDAY, 7).await),
            [("Planning".to_owned(), PLANNING + HOUR)]
        );
        trio.stop().await;
    }
}
