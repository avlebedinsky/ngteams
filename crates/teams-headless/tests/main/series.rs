//! The series command orchestrates the actual calendar API, with synthetic profile and recurrence answers.

#![cfg(test)]

use super::*;
use serde_json::{Value, json};
use teams_core::{EventAttendee, EventWrite};
use teams_mock::LAB_A;

const PROFILE: &str = "fetchShortProfile";
const EVENT: &str = "/api/mt/part/amer-03/v2.0/me/calendars/events/SyntheticSeries";
const VIEW: &str = "/api/mt/part/amer-03/v2.0/me/calendars/default/calendarView";

async fn profile(lab: &TestLab) {
    lab.mock
        .answer_next(
            "POST",
            PROFILE,
            200,
            &json!({
                "type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity",
                "value": [{"mri": LAB_A, "email": "bot-a@lab.example.test"}],
            }),
        )
        .await;
}

fn master() -> Value {
    json!({
        "objectId": "SyntheticSeries", "subject": "lab series probe", "eventType": "RecurringMaster",
        "startTime": "2026-11-02T06:00:00Z", "endTime": "2026-11-02T06:30:00Z",
        "organizerAddress": "bot-a@lab.example.test",
        "attendees": [{"address": "bot-a@lab.example.test"}],
        "eventRecurrencePattern": {"patternType": 2, "weekly": {"interval": 1, "daysOfTheWeek": [1]}},
        "eventRecurrenceRange": {"startDate": "2026-11-02T00:00:00Z", "endDate": "2026-11-23T00:00:00Z"},
    })
}

async fn initialized() -> TestLab {
    let lab = TestLab::new().await;
    profile(&lab).await;
    let handle = lab.start_engine(false).await.unwrap();
    let mut forbidden = teams_headless::series::probe_draft(now_ms());
    forbidden.attendees = vec![EventAttendee {
        address: "outsider@synthetic.example".to_owned(),
        name: None,
        mri: None,
        required: true,
    }];
    assert!(matches!(
        handle.write_event(EventWrite::Create(forbidden)).await,
        Err(teams_core::EngineError::EventNotSaved { .. })
    ));
    handle.shutdown();
    lab
}

async fn prepared() -> TestLab {
    let lab = initialized().await;
    lab.mock
        .serve_bytes(EVENT, "application/json", serde_json::to_vec(&master()).unwrap())
        .await;
    lab
}

async fn creation(lab: &TestLab) {
    profile(lab).await;
    lab.mock
        .answer_next("POST", "events", 201, &json!({"value": [master()]}))
        .await;
}

#[tokio::test]
async fn the_series_dispatch_creates_reads_schedules_and_deletes_or_keeps_its_own_series() {
    let lab = prepared().await;
    creation(&lab).await;
    let (outcome, output) = dispatch_args(&lab, &["series"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{output}");
    for line in [
        "series create ok",
        "series read kind=RecurringMaster",
        "scheduler people=",
        "series delete ok",
    ] {
        assert!(output.contains(line), "{output}");
    }
    creation(&lab).await;
    let (outcome, output) = dispatch_args(&lab, &["series", "--keep"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{output}");
    assert!(output.contains("series kept 1"));
    assert!(!output.contains("series delete ok"));
}

#[tokio::test]
async fn cleaning_up_deduplicates_occurrences_and_reports_a_failed_creation() {
    let lab = prepared().await;
    let mut occurrence = master();
    occurrence["eventType"] = json!("Occurrence");
    occurrence["objectId"] = json!("OccurrenceOne");
    occurrence["seriesMasterId"] = json!("SyntheticSeries");
    let mut second = occurrence.clone();
    second["objectId"] = json!("OccurrenceTwo");
    lab.mock
        .serve_bytes(
            VIEW,
            "application/json",
            serde_json::to_vec(&json!({"value": [occurrence, second]})).unwrap(),
        )
        .await;
    let (outcome, output) = dispatch_args(&lab, &["series", "--cleanup"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{output}");
    assert!(output.contains("occurrences=2 series=1"), "{output}");
    assert_eq!(output.matches("series delete ok").count(), 1);
    profile(&lab).await;
    lab.mock.answer_next("POST", "events", 403, &Value::Null).await;
    let (outcome, output) = dispatch_args(&lab, &["series"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE);
    assert!(output.contains("series create failed"));
}

#[tokio::test]
async fn cleanup_fails_if_the_calendar_cannot_be_read_and_deletes_nothing() {
    let lab = prepared().await;
    lab.mock.answer_next("GET", "calendarView", 403, &Value::Null).await;
    let (outcome, output) = dispatch_args(&lab, &["series", "--cleanup"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE, "{output}");
    assert!(output.contains("series calendar read failed"), "{output}");
    assert!(
        lab.mock
            .received_requests()
            .await
            .iter()
            .all(|request| request.method.as_str() != "DELETE")
    );
}

#[tokio::test]
async fn cleanup_succeeds_when_the_calendar_was_read_and_contains_no_probe_series() {
    let lab = prepared().await;
    let (outcome, output) = dispatch_args(&lab, &["series", "--cleanup"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::SUCCESS, "{output}");
    assert!(output.contains("occurrences=0 series=0"), "{output}");
    assert!(
        lab.mock
            .received_requests()
            .await
            .iter()
            .all(|request| request.method.as_str() != "DELETE")
    );
}

#[tokio::test]
async fn read_scheduler_and_delete_failures_each_fail_the_probe_without_skipping_cleanup() {
    let lab = initialized().await;
    creation(&lab).await;
    lab.mock.answer_next("GET", EVENT, 403, &Value::Null).await;
    lab.mock
        .serve_bytes(EVENT, "application/json", serde_json::to_vec(&master()).unwrap())
        .await;
    for endpoint in ["schedules", "findMeetingTimes"] {
        lab.mock.answer_next("PUT", endpoint, 403, &Value::Null).await;
    }
    lab.mock
        .answer_next("DELETE", "SyntheticSeries", 403, &Value::Null)
        .await;
    let (outcome, output) = dispatch_args(&lab, &["series"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE, "{output}");
    for expected in [
        "series create ok",
        "series read failed",
        "scheduler failed",
        "series delete failed",
        "series why",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    let requests = lab.mock.received_requests().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "DELETE")
            .count(),
        1
    );
    assert!(!output.contains("series delete ok"));
}

#[tokio::test]
async fn an_unread_create_answer_still_finds_the_probe_but_missing_recurrence_is_reported_as_failure() {
    let lab = initialized().await;
    let mut incomplete = master();
    let object = incomplete.as_object_mut().unwrap();
    object.remove("eventRecurrencePattern");
    object.remove("eventRecurrenceRange");
    incomplete["attendees"] = json!([{}]);
    lab.mock
        .serve_bytes(EVENT, "application/json", serde_json::to_vec(&incomplete).unwrap())
        .await;
    let mut occurrence = incomplete.clone();
    occurrence["eventType"] = json!("Occurrence");
    occurrence["objectId"] = json!("SyntheticOccurrence");
    occurrence["seriesMasterId"] = json!("SyntheticSeries");
    lab.mock
        .serve_bytes(
            VIEW,
            "application/json",
            serde_json::to_vec(&json!({"value":[occurrence]})).unwrap(),
        )
        .await;
    profile(&lab).await;
    lab.mock.answer_next("POST", "events", 201, &json!({"value":[]})).await;
    let (outcome, output) = dispatch_args(&lab, &["series", "--keep"], None).await;
    assert_eq!(outcome.unwrap(), ExitCode::FAILURE, "{output}");
    for expected in [
        "series create ok answer=unread",
        "occurrences=1 series=1",
        "pattern=None",
        "ends=None",
        "attendees=Some(Some([None]))",
        "eventRecurrencePattern=none",
        "eventRecurrenceRange=none",
        "series kept 1",
    ] {
        assert!(output.contains(expected), "{output}");
    }
    assert!(
        lab.mock
            .received_requests()
            .await
            .iter()
            .all(|request| request.method.as_str() != "DELETE")
    );
}

#[tokio::test]
async fn create_calendar_read_failure_fails_after_exactly_one_cleanup_without_resending_the_create() {
    let lab = prepared().await;
    creation(&lab).await;
    lab.mock.answer_next("GET", "calendarView", 403, &Value::Null).await;
    let (outcome, output) = dispatch_args(&lab, &["series"], None).await;
    assert!(
        output.contains("series create ok") && output.contains("series calendar read failed"),
        "{output}"
    );
    assert_eq!(
        output.matches("series delete ok").count(),
        1,
        "the known created series must still be cleaned up: {output}"
    );
    let requests = lab.mock.received_requests().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/default/events"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "DELETE" && request.url.path().ends_with("/SyntheticSeries"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "GET" && request.url.path().ends_with("/calendarView"))
            .count(),
        1
    );
    assert_eq!(
        outcome.unwrap(),
        ExitCode::FAILURE,
        "an unread calendar cannot make the probe successful: {output}"
    );
}
