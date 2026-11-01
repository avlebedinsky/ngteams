//! Synthetic calendar answers, with floating times so the scenarios are independent of the host time zone.

pub fn full(id: &str, hour: u8) -> teams_core::FullEvent {
    let body = serde_json::json!({
        "objectId": id,
        "subject": format!("Synthetic event {id}"),
        "startTime": format!("2026-10-27T{hour:02}:00:00"),
        "endTime": format!("2026-10-27T{:02}:00:00", hour + 1),
        "isOrganizer": true,
        "myResponseType": "Organizer",
        "showAs": "Busy",
        "bodyContent": "<p>Synthetic <strong>body</strong></p>",
        "isReminderSet": true,
        "reminderMinutesBeforeStart": 30,
        "categories": ["Blue category"],
        "syntheticUnknown": "keep me"
    });
    teams_proto::EventRead::new(id)
        .unwrap()
        .read_answer(&serde_json::to_vec(&body).unwrap())
        .unwrap()
}
