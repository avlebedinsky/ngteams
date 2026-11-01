//! Offline unit scenarios for the parent module; kept outside runtime coverage sources.

use super::*;

fn at(day: u32, hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 10, day)
        .and_then(|date| date.and_hms_opt(hour, minute, 0))
        .expect("a time")
}

#[test]
fn new_opens_at_the_next_half_hour_for_half_an_hour() {
    let slot = Slot::next_half_hour(at(27, 14, 41));
    assert_eq!((slot.start, slot.end), (at(27, 15, 0), at(27, 15, 30)));
    let slot = Slot::next_half_hour(at(27, 15, 0));
    assert_eq!(slot.start, at(27, 15, 30));
}

#[test]
fn a_slot_moves_whole_and_its_ends_snap() {
    let slot = Slot::new(at(27, 15, 0), at(27, 15, 25));
    assert_eq!(slot.moved(62).start, at(27, 16, 0));
    assert_eq!(slot.moved(62).length(), Duration::minutes(25));
    assert_eq!(slot.end_moved(-40).end, at(27, 15, 15), "kept a step after the start");
    assert_eq!(slot.start_moved(50).start, at(27, 15, 10), "kept a step before the end");
    assert_eq!(
        slot.ending(at(27, 14, 0)).end,
        at(27, 15, 30),
        "an end before the start"
    );
}

#[test]
fn the_time_line_reads_as_teams_writes_it() {
    let slot = Slot::new(at(27, 15, 0), at(27, 15, 25));
    assert_eq!(slot.text(HourCycle::Twelve), "Tue 10/27/2026 3:00 PM - 3:25 PM");
    let all_day = slot.with_all_day(true);
    assert_eq!(all_day.text(HourCycle::Twelve), "Tue 10/27/2026 (all day)");
    assert_eq!((all_day.start, all_day.end), (at(27, 0, 0), at(28, 0, 0)));
}

#[test]
fn only_an_address_is_taken_as_an_attendee() {
    assert!(typed_attendee("bot-b@lab.example.com", true).is_some());
    assert_eq!(
        typed_attendee(" bot-b@lab.example.com; ", true).map(|attendee| attendee.address),
        Some("bot-b@lab.example.com".to_owned())
    );
    for text in [
        "bot-b",
        "a@b",
        "a@b.example.com, c@d.example.com",
        "Bot <b@lab.example.com>",
        "@x.com",
    ] {
        assert!(typed_attendee(text, true).is_none(), "{text}");
    }
}

#[test]
fn a_body_goes_out_as_html_and_comes_back_as_text() {
    assert_eq!(
        text_html("a < b\n\nc"),
        "<div>a &lt; b</div><div><br></div><div>c</div>"
    );
    assert_eq!(
        html_text("<div>a &lt; b</div><div><br></div><div>c</div>"),
        "a < b\n\n\nc"
    );
    assert_eq!(text_html("  "), "");
}

#[test]
fn midnight_rounding_and_date_changes_keep_an_events_duration() {
    let slot = Slot::next_half_hour(at(27, 23, 59));
    assert_eq!((slot.start, slot.end), (at(28, 0, 0), at(28, 0, 30)));
    assert_eq!(snap(at(27, 23, 59)), at(28, 0, 0));
    let overnight = Slot::new(at(27, 23, 30), at(28, 1, 0));
    assert_eq!(
        overnight.text(HourCycle::TwentyFour),
        "Tue 10/27/2026 23:30 - Wed 10/28/2026 01:00"
    );
    let moved = overnight.on(at(29, 0, 0).date());
    assert_eq!((moved.start, moved.end), (at(29, 23, 30), at(30, 1, 0)));
    assert_eq!(moved.length(), Duration::minutes(90));
}

#[test]
fn all_day_uses_exclusive_midnight_ends_and_returns_to_a_working_time() {
    let slot = Slot::new(at(27, 15, 0), at(29, 0, 0)).with_all_day(true);
    assert_eq!((slot.start, slot.end), (at(27, 0, 0), at(29, 0, 0)));
    assert_eq!(slot.with_all_day(true), slot, "turning it on twice preserves its days");
    assert_eq!(
        slot.text(HourCycle::TwentyFour),
        "Tue 10/27/2026 - Wed 10/28/2026 (all day)"
    );
    let timed = slot.with_all_day(false);
    assert!(!timed.all_day);
    assert_eq!((timed.start, timed.end), (at(27, 9, 0), at(27, 9, 30)));
    assert_eq!(timed.with_all_day(false), timed);
    let partial_end = Slot::new(at(27, 15, 0), at(28, 10, 0)).with_all_day(true);
    assert_eq!(partial_end.end, at(29, 0, 0), "the end's partial day is included");
}

#[test]
fn time_choices_cover_the_whole_day_and_label_durations_across_midnight() {
    let choices = day_times(at(27, 0, 0).date());
    assert_eq!(choices.len(), 48);
    assert_eq!(choices.first(), Some(&at(27, 0, 0)));
    assert_eq!(choices.last(), Some(&at(27, 23, 30)));
    assert!(choices.windows(2).all(|pair| pair[1] - pair[0] == DEFAULT_LENGTH));
    let ends = end_times(at(27, 23, 30));
    assert_eq!(ends.len(), 48);
    for (index, time, label) in [
        (0, at(28, 0, 0), "30 min"),
        (1, at(28, 0, 30), "1 hr"),
        (2, at(28, 1, 0), "1.5 hrs"),
        (3, at(28, 1, 30), "2 hrs"),
        (47, at(28, 23, 30), "24 hrs"),
    ] {
        assert_eq!(ends[index], (time, label.to_owned()));
    }
    assert_eq!(reminder_label(Some(17)), "17 minutes before");
    assert_eq!(reminder_label(None), "Don't remind me");
    assert_eq!(day_title(at(27, 0, 0).date()), "Tue, Oct 27, 2026");
}

#[test]
fn a_draft_preserves_the_meeting_details_and_leaves_identity_for_the_engine() {
    let attendee = EventAttendee {
        address: "person@lab.example.test".to_owned(),
        name: Some("Synthetic attendee".to_owned()),
        mri: Some("8:orgid:00000000-0000-4000-8000-00000000000b".to_owned()),
        required: false,
    };
    let recurrence = Recurrence {
        pattern: teams_proto::RecurrencePattern::Daily { interval: 2 },
        start: zoned(at(27, 0, 0)),
        end: Some(zoned(at(30, 0, 0))),
    };
    let fields = Fields {
        subject: "  Synthetic meeting  ".to_owned(),
        slot: Slot::new(at(27, 15, 0), at(27, 16, 0)).with_all_day(true),
        attendees: vec![attendee.clone()],
        location: "  Room 42  ".to_owned(),
        body_html: "<p>Keep <strong>formatting</strong> &amp; text</p>".to_owned(),
        online: true,
        show_as: EventShowAs::WorkingElsewhere,
        reminder: Some(120),
        private: true,
        categories: vec!["Blue category".to_owned(), "Synthetic category".to_owned()],
        recurrence: Some(recurrence.clone()),
    };
    let draft = fields.draft();
    assert_eq!(draft.subject, "Synthetic meeting");
    assert_eq!(draft.location, "Room 42");
    assert_eq!(draft.attendees, [attendee]);
    assert_eq!(draft.body_html, fields.body_html);
    assert_eq!(draft.categories, fields.categories);
    assert_eq!(draft.recurrence, Some(recurrence));
    assert_eq!(draft.show_as, EventShowAs::WorkingElsewhere);
    assert_eq!(draft.reminder_minutes, Some(120));
    assert!(draft.all_day && draft.online && draft.private);
    assert_eq!((draft.start, draft.end), fields.slot.zoned());
    assert_eq!(
        draft.start.ms + i64::from(draft.start.offset_minutes) * 60_000,
        fields.slot.start.and_utc().timestamp_millis(),
        "the instant and its offset reconstruct the form's local time"
    );
    assert!(draft.time_zone.is_none());
    assert!(draft.organizer.mri.is_empty());
    assert!(draft.organizer.name.is_none() && draft.organizer.address.is_none());
}

#[test]
fn attendee_options_and_body_conversion_preserve_only_the_intended_content() {
    let attendee = typed_attendee(" person@lab.example.test, ", false).unwrap();
    assert_eq!(attendee.address, "person@lab.example.test");
    assert!(!attendee.required);
    assert!(attendee.name.is_none() && attendee.mri.is_none());
    for invalid in [
        "a@.example.test",
        "a@example.test.",
        "a@b@example.test",
        "a@x.test;b@y.test",
    ] {
        assert!(typed_attendee(invalid, false).is_none(), "{invalid}");
    }
    assert_eq!(text_html("\"A&B\" > C"), "<div>&quot;A&amp;B&quot; &gt; C</div>");
    assert_eq!(
        html_text("plain &nbsp; &quot;text&quot; &#39;x&#39; &amp; y"),
        "plain   \"text\" 'x' & y"
    );
    assert_eq!(html_text("<P>first</P><p>second</p><unfinished"), "first\nsecond");
    assert_eq!(html_text("no tags"), "no tags");
}
