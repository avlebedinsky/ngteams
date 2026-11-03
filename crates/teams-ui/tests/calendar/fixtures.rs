//! Synthetic calendar answers, with floating times so the scenarios are independent of the host time zone.

/// A channel's made-up thread and its team's mailbox, and the made-up calendar id of the meeting scheduled in it.
pub const CHANNEL: &str = "19:0123456789abcdef0123456789abcdef@thread.tacv2";
pub const TEAM_MAILBOX: &str = "lab-team@lab.example.test";
pub const CHANNEL_EVENT: &str = "040000008200e00074c5b7101a82e008";

/// A channel's meeting `organizer` (an MRI) scheduled, as its team's calendar gives it (made up): the team its organizer,
/// the channel, its post and who scheduled it in `skypeTeamsData`.
pub fn channel_full(organizer: &str) -> teams_core::FullEvent {
    let data = serde_json::json!({"cid": CHANNEL, "mid": 1_793_680_050_510_u64, "rid": 1_793_680_050_510_u64,
        "uid": organizer, "type": 0});
    let body = serde_json::json!({
        "objectId": "AQMkMadeUpChannelEvent",
        "iCalUID": CHANNEL_EVENT,
        "subject": "Synthetic channel meeting",
        "startTime": "2026-10-27T09:00:00+00:00",
        "endTime": "2026-10-27T09:30:00+00:00",
        "eventType": "Single",
        "isOnlineMeeting": true,
        "isOrganizer": true,
        "myResponseType": "Organizer",
        "organizerAddress": TEAM_MAILBOX,
        "organizerName": "Lab team",
        "attendees": [{"type": "Organizer", "role": "User", "address": TEAM_MAILBOX, "name": "Lab team"}],
        "showAs": "Busy",
        "bodyContent": "<p>Synthetic join block</p>",
        "skypeTeamsData": data.to_string(),
        "inviteAllMembers": false,
    });
    teams_proto::ChannelEventRead::new(TEAM_MAILBOX, CHANNEL_EVENT)
        .unwrap()
        .read_answer(&serde_json::to_vec(&body).unwrap())
        .unwrap()
}

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
