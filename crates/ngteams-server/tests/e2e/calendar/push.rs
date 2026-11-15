//! A meeting on the push channel: the invitation in the meeting's chat and the item in the activity feed reach
//! the invited account's socket live, through the client's own push channel, and nobody else's.

use std::time::Duration;

use serde_json::Value;
use teams_core::EventWrite;
use teams_proto::ScheduledMeeting;
use teams_trouter::{ChatEvent, PushEvent};

use super::meetings::invited;
use super::{HOUR, MONDAY, Seat, draft, server};
use crate::support::backends;
use crate::trouter::{Account, Push, delivery, registered_push};

const FEED: &str = "48:notifications";

/// The next new message on the socket: its conversation, its text and its properties.
async fn message(push: &mut Push) -> (String, String, Value) {
    loop {
        if let ChatEvent::Message { conversation, message } = delivery(push).await.event {
            return (
                conversation,
                message.content.clone(),
                Value::Object(message.properties.clone()),
            );
        }
    }
}

/// The next two new messages, the feed's item first.
async fn told(push: &mut Push) -> [(String, String, Value); 2] {
    let (first, second) = (message(push).await, message(push).await);
    if first.0 == FEED {
        [first, second]
    } else {
        [second, first]
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_meetings_invitation_and_cancellation_reach_the_invited_account_live() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan").await;
        let mut on_bert = registered_push(&Account::sign_in(&server, "bert@own.test", &bert.id).await, &server).await;
        let mut on_dan = registered_push(&Account::sign_in(&server, "dan@own.test", &dan.id).await, &server).await;
        let start = MONDAY + 14 * HOUR;

        let mut planning = draft("Planning", start, 30);
        planning.online = true;
        planning.attendees = vec![invited("bert@own.test", "Bert", Some(&bert.mri), true)];
        let written = ada.engine.write_event(EventWrite::Create(planning)).await.unwrap();
        let created = written.created.unwrap();
        let chat = created.meeting_chat.clone().unwrap();

        let [item, invitation] = told(&mut on_bert).await;
        assert_eq!(item.0, FEED);
        let activity = &item.2["activity"];
        assert_eq!(activity["activityType"], "msGraph");
        assert_eq!(activity["activitySubtype"], "privateMeetingCreated");
        assert_eq!(activity["sourceUserId"], ada.mri.as_str());
        assert_eq!(activity["sourceUserImDisplayName"], "Ada");
        assert_eq!(activity["messagePreview"], "Planning");
        let opens = activity["customTapAction"].as_str().unwrap();
        assert!(
            opens.ends_with(&format!("/l/meeting/details/?eventId={}", created.id)),
            "{opens}"
        );
        let time = activity["activityContext"]["location"].as_str().unwrap();
        assert!(
            time.contains("<StartDateTime>2027-01-04T14:00:00.0000000Z</StartDateTime>"),
            "{time}"
        );
        assert_eq!(
            (invitation.0.as_str(), invitation.1.as_str()),
            (chat.as_str(), "Scheduled a meeting")
        );
        let named = ScheduledMeeting::of_properties(&invitation.2.to_string()).unwrap();
        assert_eq!((named.title.as_str(), named.start_ms), ("Planning", start));

        // its cancellation the same way
        ada.engine
            .write_event(EventWrite::Cancel {
                id: created.id.clone(),
                chat: Some(chat.clone()),
                message: Some("Postponed".into()),
            })
            .await
            .unwrap();
        let [item, cancellation] = told(&mut on_bert).await;
        assert_eq!(item.2["activity"]["activitySubtype"], "privateMeetingCanceled");
        assert_eq!(cancellation.0, chat);
        assert!(
            ScheduledMeeting::of_properties(&cancellation.2.to_string())
                .unwrap()
                .cancelled
        );

        // nothing of it reaches an account it doesn't invite
        let quiet = on_dan
            .quiet(Duration::from_millis(500), |event| {
                matches!(event, PushEvent::Delivery(_))
            })
            .await;
        assert!(quiet, "{backend:?}");
        on_bert.stop().await;
        on_dan.stop().await;
        for seat in [ada, bert, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}
