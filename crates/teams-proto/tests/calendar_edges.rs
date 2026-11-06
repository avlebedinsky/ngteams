//! Synthetic calendar answers exercising the public reader contracts, without a service or tenant.

#![cfg(test)]

use serde_json::{Value, json};
use teams_proto::{
    Attendee, CalendarAnswerError, CalendarEvent, CalendarView, EventKind, EventRead, EventResponse, EventTime,
    FloatingTime, MAX_CALENDAR_WINDOW_MS, Read, Service, ShowAs, TimeZoneSettings, join_url_chat,
};

fn event() -> Value {
    json!({
        "_synthetic": true,
        "objectId": "synthetic-event",
        "startTime": "2026-10-26T14:00:00Z",
        "endTime": "2026-10-26T15:00:00Z"
    })
}

fn created(value: &Value) -> CalendarEvent {
    CalendarEvent::created(&serde_json::to_vec(value).unwrap()).expect("a readable synthetic event")
}

#[test]
fn windows_keep_exact_bounds_and_refuse_reversed_wide_or_unrepresentable_ranges() {
    let view = CalendarView::between(-1, 0).unwrap();
    assert_eq!(view.endpoint().query()[0].1, "1969-12-31T23:59:59.999Z");
    assert_eq!(view.endpoint().query()[1].1, "1970-01-01T00:00:00.000Z");
    assert!(CalendarView::between(0, MAX_CALENDAR_WINDOW_MS).is_some());
    for (start, end) in [
        (1, 0),
        (0, 0),
        (0, MAX_CALENDAR_WINDOW_MS + 1),
        (i64::MAX - 1, i64::MAX),
        (i64::MIN, i64::MIN + 1),
    ] {
        assert!(CalendarView::between(start, end).is_none(), "{start}..{end}");
    }
}

#[test]
fn creation_answers_accept_the_three_envelopes_and_only_the_first_created_event() {
    let raw = event();
    let expected = created(&raw);
    assert_eq!(created(&json!({"_synthetic":true,"value":raw})), expected);
    assert_eq!(created(&json!({"_synthetic":true,"value":[raw, event()]})), expected);
    for value in [
        Value::Null,
        json!([]),
        json!({"value":[]}),
        json!({"value":[null,event()]}),
    ] {
        assert!(
            CalendarEvent::created(&serde_json::to_vec(&value).unwrap()).is_none(),
            "{value}"
        );
    }
    assert!(CalendarEvent::created(b"{").is_none());
}

#[test]
fn a_created_event_is_kept_as_its_entry_and_a_series_master_is_not() {
    let raw = event();
    let body = serde_json::to_vec(&json!({"_synthetic":true,"value":[raw.clone()]})).unwrap();
    let (event, entry) = CalendarEvent::created_kept(&body).expect("kept");
    assert_eq!(event, created(&raw));
    assert_eq!(serde_json::from_str::<Value>(&entry).unwrap(), raw);
    let mut master = raw;
    master["eventType"] = json!("RecurringMaster");
    assert!(CalendarEvent::created_kept(&serde_json::to_vec(&master).unwrap()).is_none());
    assert!(CalendarEvent::created_kept(b"{").is_none());
}

#[test]
fn unreadable_required_fields_are_counted_without_dropping_valid_neighbors() {
    let mut invalid = vec![Value::Null, json!([]), json!(false)];
    for field in ["objectId", "startTime", "endTime"] {
        let mut absent = event();
        absent.as_object_mut().unwrap().remove(field);
        invalid.push(absent);
        for value in [Value::Null, json!(42), json!(" ")] {
            let mut entry = event();
            entry[field] = value;
            invalid.push(entry);
        }
    }
    let skipped = invalid.len();
    let mut master = event();
    master["eventType"] = json!("RecurringMaster");
    let mut entries = vec![event(), master];
    entries.extend(invalid);
    let mut last = event();
    last["objectId"] = json!("last");
    entries.push(last);
    let answer =
        CalendarView::read_answer(&serde_json::to_vec(&json!({"_synthetic":true,"value":entries})).unwrap()).unwrap();
    assert_eq!(answer.skipped, skipped);
    assert_eq!(
        answer.events.iter().map(|event| event.id.as_str()).collect::<Vec<_>>(),
        ["synthetic-event", "last"]
    );
    for bytes in [b"{".as_slice(), b"\xff", br#"{"value":null}"#, br#"{"value":{}}"#] {
        assert_eq!(CalendarView::read_answer(bytes), Err(CalendarAnswerError::Malformed));
    }
}

#[test]
fn optional_values_are_trimmed_and_boolean_flags_are_kept() {
    let mut value = event();
    value["objectId"] = json!(" synthetic-event ");
    value["subject"] = json!(" Appointment ");
    value["location"] = json!({"displayName":" Conference room "});
    value["organizerName"] = json!(" Organizer ");
    value["organizerAddress"] = json!(" organizer@example.invalid ");
    value["bodyPreview"] = json!(" Preview ");
    value["onlineMeetingConferenceId"] = json!(" 12345 ");
    value["seriesMasterId"] = json!(" series ");
    for field in [
        "isAllDayEvent",
        "isCancelled",
        "isOrganizer",
        "isPrivate",
        "isOnlineMeeting",
    ] {
        value[field] = json!(true);
    }
    let read = created(&value);
    assert_eq!(read.id, "synthetic-event");
    assert_eq!(read.subject, "Appointment");
    assert_eq!(read.location.as_deref(), Some("Conference room"));
    assert_eq!(read.organizer_name.as_deref(), Some("Organizer"));
    assert_eq!(read.organizer_address.as_deref(), Some("organizer@example.invalid"));
    assert_eq!(read.preview.as_deref(), Some("Preview"));
    assert_eq!(read.conference_id.as_deref(), Some("12345"));
    assert_eq!(read.series_master_id.as_deref(), Some("series"));
    assert!(read.all_day && read.cancelled && read.is_organizer && read.private && read.online);
}

#[test]
fn mistyped_optional_fields_stay_absent() {
    let mut wrong = event();
    for field in [
        "subject",
        "organizerName",
        "organizerAddress",
        "bodyPreview",
        "onlineMeetingConferenceId",
        "seriesMasterId",
    ] {
        wrong[field] = json!(5);
    }
    wrong["location"] = json!({"displayName":" "});
    wrong["attendees"] = json!({});
    wrong["isAllDayEvent"] = json!("true");
    let read = created(&wrong);
    assert!(read.subject.is_empty());
    assert_eq!(
        (
            read.organizer_name,
            read.organizer_address,
            read.preview,
            read.conference_id,
            read.series_master_id
        ),
        (None, None, None, None, None)
    );
    assert_eq!(read.location, None);
    assert!(read.attendees.is_empty());
    assert!(!read.all_day && !read.cancelled && !read.is_organizer && !read.private && !read.online);
}

#[test]
fn event_kinds_and_responses_preserve_unknown_values() {
    for (wire, kind, recurring) in [
        ("Single", EventKind::Single, false),
        ("Occurrence", EventKind::Occurrence, true),
        ("Exception", EventKind::Exception, true),
        ("RecurringMaster", EventKind::RecurringMaster, true),
        ("FutureKind", EventKind::Other("FutureKind".into()), false),
    ] {
        let mut value = event();
        value["eventType"] = json!(wire);
        let read = created(&value);
        assert_eq!(read.kind, kind);
        assert_eq!(read.kind.recurring(), recurring);
    }
    for (wire, expected) in [
        ("Organizer", EventResponse::Organizer),
        ("Accepted", EventResponse::Accepted),
        ("Tentative", EventResponse::Tentative),
        ("TentativelyAccepted", EventResponse::Tentative),
        ("Declined", EventResponse::Declined),
        ("NotResponded", EventResponse::NotResponded),
        ("None", EventResponse::NoResponse),
        ("FutureResponse", EventResponse::Other("FutureResponse".into())),
    ] {
        let mut value = event();
        value["myResponseType"] = json!(wire);
        assert_eq!(created(&value).response, expected);
    }
    let absent = created(&event());
    assert_eq!(absent.kind, EventKind::Other(String::new()));
    assert_eq!(absent.response, EventResponse::Other(String::new()));
}

#[test]
fn availability_values_preserve_unrecognized_wire_values() {
    for (wire, expected) in [
        ("Free", ShowAs::Free),
        ("Tentative", ShowAs::Tentative),
        ("Busy", ShowAs::Busy),
        ("OOF", ShowAs::Away),
        ("Oof", ShowAs::Away),
        ("WorkingElsewhere", ShowAs::WorkingElsewhere),
        ("FutureAvailability", ShowAs::Other("FutureAvailability".into())),
    ] {
        let mut value = event();
        value["showAs"] = json!(wire);
        assert_eq!(created(&value).show_as, expected);
    }
    assert_eq!(created(&event()).show_as, ShowAs::Other(String::new()));
}

#[test]
fn attendees_accept_nested_and_flat_names_or_addresses_and_drop_unreadable_entries() {
    let mut value = event();
    value["attendees"] = json!([
        {"emailAddress":{"name":" Nested ","address":" nested@example.invalid "}},
        {"displayName":" Alias ","smtpAddress":" alias@example.invalid "},
        {"email":" only-address@example.invalid "},
        {"name":" Name only "},
        {"name":" ","address":55},
        {"emailAddress":null}, null, 5
    ]);
    assert_eq!(
        created(&value).attendees,
        [
            Attendee {
                name: Some("Nested".into()),
                address: Some("nested@example.invalid".into()),
                ..Attendee::default()
            },
            Attendee {
                name: Some("Alias".into()),
                address: Some("alias@example.invalid".into()),
                ..Attendee::default()
            },
            Attendee {
                name: None,
                address: Some("only-address@example.invalid".into()),
                ..Attendee::default()
            },
            Attendee {
                name: Some("Name only".into()),
                address: None,
                ..Attendee::default()
            },
        ]
    );
}

#[test]
fn meeting_chat_uses_valid_embedded_data_before_the_join_link() {
    let mut value = event();
    value["joinOnlineMeetingUrl"] = json!("https://teams.microsoft.com/l/meetup-join/19%3alink%40thread.skype/0");
    value["skypeTeamsData"] = json!(r#"{"_synthetic":true,"cid":"19:data@thread.tacv2"}"#);
    let read = created(&value);
    assert!(read.online);
    assert_eq!(read.meeting_chat.as_deref(), Some("19:data@thread.tacv2"));
    for data in ["{", "{}", r#"{"cid":5}"#, r#"{"cid":"28:wrong@thread.v2"}"#] {
        value["skypeTeamsData"] = json!(data);
        assert_eq!(created(&value).meeting_chat.as_deref(), Some("19:link@thread.skype"));
    }
    value["joinOnlineMeetingUrl"] = json!("http://teams.microsoft.com/l/meetup-join/19%3alink%40thread.skype/0");
    let read = created(&value);
    assert_eq!(read.join_url, None);
    assert_eq!(read.meeting_chat, None);
    assert!(!read.online);
}

#[test]
fn join_links_reject_unreadable_or_non_meeting_thread_segments() {
    for suffix in ["v2", "skype", "tacv2"] {
        let url =
            format!("https://teams.microsoft.com/l/meetup-join/19%3asynthetic%40thread.{suffix}?context=synthetic");
        assert_eq!(join_url_chat(&url), Some(format!("19:synthetic@thread.{suffix}")));
    }
    for segment in [
        "",
        "%FF",
        "28%3afoo%40thread.v2",
        "19%3afoo%40thread.unknown",
        "19%3afoo%20bar%40thread.v2",
        "19%3afoo%2fbar%40thread.v2",
        "19%3afoo%3fbar%40thread.v2",
        "19%3afoo%23bar%40thread.v2",
    ] {
        let url = format!("https://teams.microsoft.com/l/meetup-join/{segment}/0");
        assert_eq!(join_url_chat(&url), None, "{segment}");
    }
    assert_eq!(join_url_chat("https://teams.microsoft.com/other"), None);
}

#[test]
fn floating_times_keep_components_and_invalid_clock_shapes_are_skipped() {
    for text in [
        "2026-10-26T14:30",
        "2026-10-26 14:30:00",
        " 2026-10-26T14:30:00.1234567 ",
    ] {
        let mut value = event();
        value["startTime"] = json!(text);
        assert_eq!(
            created(&value).start,
            EventTime::Floating(FloatingTime {
                year: 2026,
                month: 10,
                day: 26,
                hour: 14,
                minute: 30,
                second: 0
            })
        );
    }
    for text in [
        "2026-00-26T14:30",
        "2026-10-00T14:30",
        "2026-10-32T14:30",
        "2026-10-26T24:00",
        "2026-10-26T14:60",
        "2026-10-26T14:30:61",
        "2026-10-26T14:30:00.extra",
        "2026-10-26T14:30:00:01",
        "2026-10-26-01T14:30",
        "2026-10-26Tclock",
        "tomorrow",
    ] {
        let mut value = event();
        value["startTime"] = json!(text);
        assert!(
            CalendarEvent::created(&serde_json::to_vec(&value).unwrap()).is_none(),
            "{text}"
        );
    }
}

#[test]
fn a_full_event_preserves_raw_fields_and_reads_form_metadata() {
    let mut value = event();
    value["body"] = json!({"content":"<p>Synthetic body</p>"});
    value["isReminderSet"] = json!(true);
    value["reminderMinutesBeforeStart"] = json!(15);
    value["categories"] = json!(["Synthetic", 7, null, "Planning"]);
    value["attendees"] = json!([
        {"type":"Optional","emailAddress":{"address":"OPTIONAL@EXAMPLE.INVALID"}},
        {"type":"Required","emailAddress":{"address":"required@example.invalid"}},
        {"type":"Optional","address":"FLAT@EXAMPLE.INVALID"},
        {"type":"Optional","emailAddress":{"name":"No address"}}
    ]);
    value["futureField"] = json!({"kept":true});
    value["eventRecurrencePattern"] = json!({"patternType":1,"daily":{"interval":2}});
    value["eventRecurrenceRange"] = json!({"startDate":"2026-10-26T00:00:00Z"});
    let request = EventRead::new("synthetic-event").unwrap();
    assert_eq!(request.service(), Service::MiddleTier);
    assert_eq!(
        request.endpoint().segments(),
        ["v2.0", "me", "calendars", "events", "synthetic-event"]
    );
    assert_eq!(
        request.endpoint().query(),
        &[("shouldDecryptData".into(), "true".into())]
    );
    let read = request.read_answer(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(read.raw, *value.as_object().unwrap());
    assert_eq!(read.event.id, "synthetic-event");
    assert_eq!(read.body_html.as_deref(), Some("<p>Synthetic body</p>"));
    assert_eq!(read.reminder_minutes, Some(15));
    assert_eq!(read.categories, ["Synthetic", "Planning"]);
    assert_eq!(read.optional, ["optional@example.invalid", "flat@example.invalid"]);
    assert_eq!(
        read.recurrence.unwrap().pattern.wire(),
        json!({"patternType":1,"daily":{"interval":2}})
    );
    value["bodyContent"] = json!("<p>Preferred</p>");
    let wrapped = request
        .read_answer(&serde_json::to_vec(&json!({"_synthetic":true,"value":value})).unwrap())
        .unwrap();
    assert_eq!(wrapped.body_html.as_deref(), Some("<p>Preferred</p>"));
}

#[test]
fn full_event_refusals_do_not_turn_wrong_ids_or_shapes_into_empty_events() {
    assert!(EventRead::new("").is_none());
    assert!(EventRead::new(" \t").is_none());
    let request = EventRead::new("another-event").unwrap();
    assert_eq!(
        request.read_answer(&serde_json::to_vec(&event()).unwrap()),
        Err(CalendarAnswerError::Malformed)
    );
    for body in [
        b"{".as_slice(),
        b"null",
        b"[]",
        br#"{"value":{}}"#,
        br#"{"objectId":"another-event"}"#,
    ] {
        let error = request.read_answer(body).unwrap_err();
        assert_eq!(error, CalendarAnswerError::Malformed);
        assert!(!error.to_string().contains("another-event"));
    }
}

#[test]
fn form_metadata_defaults_and_reminder_integer_bounds_are_preserved() {
    let request = EventRead::new("synthetic-event").unwrap();
    let read = request.read_answer(&serde_json::to_vec(&event()).unwrap()).unwrap();
    assert_eq!(read.body_html, None);
    assert_eq!(read.reminder_minutes, None);
    assert!(read.categories.is_empty() && read.optional.is_empty());
    assert_eq!(read.recurrence, None);
    for (set, minutes, expected) in [
        (true, json!(0), Some(0)),
        (true, json!(u32::MAX), Some(u32::MAX)),
        (true, json!(u64::from(u32::MAX) + 1), None),
        (true, json!(-1), None),
        (true, json!("15"), None),
        (false, json!(15), None),
    ] {
        let mut value = event();
        value["isReminderSet"] = json!(set);
        value["reminderMinutesBeforeStart"] = minutes;
        value["categories"] = json!("not a list");
        value["attendees"] = json!(false);
        let read = request.read_answer(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(read.reminder_minutes, expected);
        assert!(read.categories.is_empty() && read.optional.is_empty());
    }
}

#[test]
fn timezone_settings_read_local_names_and_fallback_without_guessing() {
    let request = TimeZoneSettings::new("Europe/Istanbul");
    assert_eq!(request.service(), Service::MiddleTier);
    assert_eq!(
        request.endpoint().segments(),
        ["beta", "me", "calendarEvents", "timeZoneSettingsWithOffset"]
    );
    assert_eq!(
        request.endpoint().query(),
        &[("timezone".into(), "Europe/Istanbul".into())]
    );
    for (body, expected) in [
        (
            json!({"local":{"timeZone":" Local zone "},"timeZone":"Remote zone"}),
            Some("Local zone"),
        ),
        (json!({"timeZone":" Top zone "}), Some("Top zone")),
        (json!({"local":null,"timeZone":"Top zone"}), Some("Top zone")),
        (json!({"local":{"timeZone":" "}}), None),
        (json!({"timeZone":17}), None),
        (json!([]), None),
    ] {
        assert_eq!(
            TimeZoneSettings::read_answer(&serde_json::to_vec(&body).unwrap()).as_deref(),
            expected
        );
    }
    assert_eq!(TimeZoneSettings::read_answer(b"{"), None);
}

#[test]
fn an_event_read_whole_gives_each_attendee_s_answer_and_whether_they_are_optional() {
    let value = json!({
        "objectId": "AAMkMadeUp",
        "subject": "Synthetic",
        "startTime": "2026-11-06T10:00:00+00:00",
        "endTime": "2026-11-06T10:30:00+00:00",
        "attendees": [
            {"name": "Accepting", "address": "yes@example.invalid", "role": "User",
             "status": {"response": "Accepted", "time": "2026-11-06T05:52:15.2453471+00:00"}},
            {"name": "Maybe", "address": "maybe@example.invalid", "type": "Optional",
             "status": {"response": "TentativelyAccepted"}},
            {"name": "Silent", "address": "silent@example.invalid", "status": {"response": "None"}},
            {"name": "Unasked", "address": "unasked@example.invalid"}
        ]
    });
    let read = EventRead::new("AAMkMadeUp")
        .unwrap()
        .read_answer(&serde_json::to_vec(&value).unwrap())
        .unwrap();
    let answers: Vec<(Option<EventResponse>, bool)> = read
        .event
        .attendees
        .iter()
        .map(|attendee| (attendee.response.clone(), attendee.optional))
        .collect();
    assert_eq!(
        answers,
        [
            (Some(EventResponse::Accepted), false),
            (Some(EventResponse::Tentative), true),
            (Some(EventResponse::NoResponse), false),
            (None, false),
        ]
    );
}
