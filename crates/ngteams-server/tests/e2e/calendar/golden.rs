//! The server's events beside the synthetic fixture the client's calendar reader is tested with: every key the
//! fixture's meeting has is there, with a value of the same JSON type.

use reqwest::Method;
use serde_json::Value;
use teams_core::EventWrite;
use teams_proto::MIDDLE_TIER_AUDIENCE;

use super::meetings::invited;
use super::{HOUR, MONDAY, Seat, draft, server};
use crate::support::Backend;
use crate::teams::golden::same_shape;
use crate::teams::security::{call, token};

#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_in_the_calendar_view_has_the_shape_of_the_synthetic_fixture() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../../../tests/fixtures/mt/calendar-view.json")).unwrap();
    let golden = &fixture["value"][0];
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let bert = Seat::new(&server, "bert@own.test", "Bert").await;
    let mut review = draft("Code review", MONDAY + 14 * HOUR, 60);
    review.online = true;
    review.body_html = "<p>Bring the diff</p>".into();
    review.attendees = vec![invited("bert@own.test", "Bert", None, true)];
    ada.engine.write_event(EventWrite::Create(review)).await.unwrap();

    let own = token(&server, "bert@own.test", MIDDLE_TIER_AUDIENCE).await;
    let window = "/api/mt/part/own-01/v2.0/me/calendars/default/calendarView\
        ?StartDate=2027-01-04T00:00:00.000Z&EndDate=2027-01-05T00:00:00.000Z&shouldDecryptData=true";
    let (status, answer) = call(&server, &own, Method::GET, window, None).await;
    assert_eq!(status, 200);
    assert_eq!(answer["type"], fixture["type"]);
    let ours = &answer["value"][0];
    // the calendar view names no attendees (as the service's doesn't): they come with the event read whole
    same_shape(ours, golden, &["attendees"], "event");
    assert!(ours.get("attendees").is_none());
    let whole = format!(
        "/api/mt/part/own-01/v2.0/me/calendars/events/{}",
        ours["objectId"].as_str().unwrap()
    );
    let (_, whole) = call(&server, &own, Method::GET, &whole, None).await;
    for (ours, golden) in whole["attendees"]
        .as_array()
        .unwrap()
        .iter()
        .zip(golden["attendees"].as_array().unwrap())
    {
        same_shape(ours, golden, &[], "attendee");
    }
    ada.stop();
    bert.stop();
    server.stop().await;
}
