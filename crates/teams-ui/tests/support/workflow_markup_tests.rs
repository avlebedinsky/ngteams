//! Synthetic template markup: fields, bindings, Save's filling and Teams' titles. No tenant data.
use super::*;
use serde_json::json;

const WEEKLY: &str = "805739fdad754e799502b6379900922e";

fn weekly_markup() -> Value {
    json!({"version":"1.0","automation":{"template":{
        "id": WEEKLY,
        "parameters":[
            {"id":"BuiltInFrequency","typeName":"BuiltIn.Frequency","mode":"input","value":"Week"},
            {"id":"BuiltInInterval","typeName":"BuiltIn.Interval","mode":"input"},
            {"id":"BuiltInTimeOfDay","typeName":"BuiltIn.TimeOfDay","mode":"input"},
            {"id":"BuiltInWeekDays","typeName":"BuiltIn.WeekDays","mode":"input","value":["Monday"]},
            {"id":"TeamsGroup","typeName":"Teams.Group","mode":"input"},
            {"id":"TeamsChannel","typeName":"Teams.Channel","mode":"input","dependencies":["TeamsGroup"]},
            {"id":"Subject","typeName":"Outlook.SubjectFilter","mode":"input","optional":true}
        ],
        "editorBindings":[
            {"parameterId":"BuiltInInterval","label":"${resources['x']}"},
            {"parameterId":"TeamsGroup","label":"${resources['y']}"},
            {"parameterId":"TeamsChannel"},
            {"parameterId":"BuiltInTimeOfDay"},
            {"parameterId":"BuiltInWeekDays"}
        ]
    }},
    "dataBindings":{
        "Recurrence":[
            {"source":"${context['parameters']['BuiltInFrequency']}","target":"${json['recurrence']['frequency']}"},
            {"source":"${context['parameters']['BuiltInInterval']}","target":"${json['recurrence']['interval']}"},
            {"source":"${context['parameters']['BuiltInTimeOfDay']}","target":"${json['recurrence']['schedule']}"},
            {"source":"${context['parameters']['BuiltInWeekDays']}","target":"${json['recurrence']['schedule']}"}
        ],
        "Post":[
            {"source":"${context['parameters']['TeamsGroup']}","target":"${json['inputs']['parameters']['body/recipient/groupId']}"},
            {"source":"${context['parameters']['TeamsChannel']}","target":"${json['inputs']['parameters']['body/recipient/channelId']}"},
            {"source":"${context['parameters']['Subject']}","target":"${json['inputs']['parameters']['subject']}"}
        ]
    }})
}

fn definition() -> Value {
    json!({
        "triggers":{"Recurrence":{"type":"Recurrence","recurrence":{"frequency":"Week","interval":1}}},
        "actions":{"Each":{"type":"Foreach","actions":{"Post":{"inputs":{"parameters":{
            "body/recipient/groupId":"","body/recipient/channelId":"","subject":""}}}}}}
    })
}

#[test]
fn fields_follow_the_editor_order_with_teams_labels() {
    let params = params(&weekly_markup());
    let ids: Vec<_> = params.iter().map(|param| param.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "BuiltInInterval",
            "TeamsGroup",
            "TeamsChannel",
            "BuiltInTimeOfDay",
            "BuiltInWeekDays",
            "BuiltInFrequency",
            "Subject"
        ]
    );
    let label = |id: &str| {
        params
            .iter()
            .find(|param| param.id == id)
            .map(|param| param.label.clone())
    };
    assert_eq!(label("BuiltInInterval").as_deref(), Some("Repeat every (weeks)"));
    assert_eq!(label("TeamsGroup").as_deref(), Some("Select team"));
    assert_eq!(label("TeamsChannel").as_deref(), Some("Select channel"));
    assert_eq!(label("BuiltInTimeOfDay").as_deref(), Some("Time"));
    assert_eq!(label("BuiltInWeekDays").as_deref(), Some("Day"));
    assert!(
        params
            .iter()
            .find(|param| param.id == "Subject")
            .is_some_and(|param| param.optional)
    );
    assert_eq!(params[5].kind, Kind::Fixed);
}

#[test]
fn save_fills_each_bound_input_and_drops_an_empty_optional_one() {
    let markup = weekly_markup();
    let mut params = params(&markup);
    for param in &mut params {
        match param.id.as_str() {
            "BuiltInInterval" => param.value = json!(2),
            "BuiltInTimeOfDay" => param.value = json!("20:05"),
            "BuiltInWeekDays" => param.value = json!(["Monday", "Wednesday"]),
            "TeamsGroup" => param.value = json!("group"),
            "TeamsChannel" => param.value = json!("channel"),
            _ => {}
        }
    }
    let mut filled = definition();
    apply(&mut filled, &params, &bindings(&markup));
    let recurrence = &filled["triggers"]["Recurrence"]["recurrence"];
    assert_eq!(recurrence["interval"], 2);
    assert_eq!(recurrence["frequency"], "Week");
    assert_eq!(recurrence["schedule"]["hours"], json!([20]));
    assert_eq!(recurrence["schedule"]["minutes"], json!([5]));
    assert_eq!(recurrence["schedule"]["weekDays"], json!(["Monday", "Wednesday"]));
    let post = &filled["actions"]["Each"]["actions"]["Post"]["inputs"]["parameters"];
    assert_eq!(post["body/recipient/groupId"], "group");
    assert_eq!(post["body/recipient/channelId"], "channel");
    assert!(post.get("subject").is_none(), "an empty optional input is taken out");
}

#[test]
fn people_are_sent_as_their_addresses() {
    let mut people = params(&json!({"automation":{"template":{"id":"t","parameters":[
        {"id":"From","typeName":"Outlook.From","mode":"input","optional":true}]}},
        "dataBindings":{"Mail":[{"source":"${context['parameters']['From']}","target":"${json['inputs']['parameters']['from']}"}]}}));
    people[0].value =
        json!([{"email":"one@example.invalid","name":"One"},{"email":"two@example.invalid","name":"Two"}]);
    let mut filled = json!({"triggers":{"Mail":{"inputs":{"parameters":{}}}}});
    apply(
        &mut filled,
        &people,
        &[Binding {
            node: "Mail".into(),
            param: "From".into(),
            path: vec!["inputs".into(), "parameters".into(), "from".into()],
        }],
    );
    assert_eq!(
        filled["triggers"]["Mail"]["inputs"]["parameters"]["from"],
        "one@example.invalid;two@example.invalid"
    );
}

#[test]
fn the_kept_markup_holds_each_value_and_its_name() {
    let markup = weekly_markup();
    let mut params = params(&markup);
    params[1].value = json!("group");
    params[1].display = Some("Synthetic team".into());
    let kept = filled(
        &markup,
        &params,
        "Automate weekly reminders",
        &json!({"shared_teams":{"api":{"name":"shared_teams"}}}),
    );
    let template = &kept["automation"]["template"];
    assert_eq!(template["displayName"], "Automate weekly reminders");
    let group = template["parameters"]
        .as_array()
        .and_then(|list| list.iter().find(|param| param["id"] == "TeamsGroup"))
        .cloned()
        .unwrap_or_default();
    assert_eq!(group["value"], "group");
    assert_eq!(group["valueDisplayName"], "Synthetic team");
    assert_eq!(
        template["connectionReferences"]["shared_teams"]["source"],
        "NotSpecified"
    );
}

#[test]
fn titles_read_as_teams_words_them() {
    let mut params = params(&weekly_markup());
    for param in &mut params {
        match param.id.as_str() {
            "BuiltInInterval" => param.value = json!(1),
            "BuiltInTimeOfDay" => param.display = Some("8:05 PM".into()),
            "TeamsChannel" => {
                param.value = json!("channel");
                param.display = Some("General".into());
            }
            _ => {}
        }
        if param.id == "BuiltInTimeOfDay" {
            param.value = json!("20:05");
        }
    }
    assert_eq!(
        summary(WEEKLY, "", &params),
        "Send a message to General every 1 week(s) on Monday at 8:05 PM"
    );
    for param in &mut params {
        if param.id == "BuiltInWeekDays" {
            param.value = json!(["Monday", "Friday"]);
        }
        if param.id == "BuiltInInterval" {
            param.value = json!("0");
        }
    }
    assert_eq!(
        summary(WEEKLY, "", &params),
        "Send a message to General every few week(s) on selected days at 8:05 PM"
    );
    assert_eq!(
        summary("8f348e6a1ad64fe8bcb974b02585d987", "", &params),
        "Notify General when a Planner task is completed in plan"
    );
    assert_eq!(
        summary("synthetic", "Send webhook alerts to a channel", &[]),
        "Send webhook alerts to channel"
    );
}

#[test]
fn times_and_intervals_read_as_teams_takes_them() {
    assert_eq!(parse_clock("8:05 PM"), Some((20, 5)));
    assert_eq!(parse_clock("12:30 am"), Some((0, 30)));
    assert_eq!(parse_clock("23:59"), Some((23, 59)));
    assert_eq!(parse_clock("13:00 PM"), None);
    assert_eq!(clock_name(0, 0), "12:00 AM");
    assert_eq!(clock_name(20, 5), "8:05 PM");
    let at = |hour, minute| {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 4)
            .and_then(|day| day.and_hms_opt(hour, minute, 0))
            .unwrap_or_default()
    };
    assert_eq!(default_time(at(20, 4)), (20, 5));
    assert_eq!(default_time(at(23, 58)), (0, 0));
    assert_eq!(interval(&json!(71)), Some(71));
    assert_eq!(interval(&json!("72")), None);
    assert_eq!(interval(&json!("1.5")), None);
    let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap_or_default();
    assert_eq!(
        date_value(day),
        ("2026-11-04T00:00:00".to_owned(), "11/4/2026".to_owned())
    );
    assert_eq!(parse_date("2026-11-04T00:00:00"), Some(day));
}

#[test]
fn a_nested_action_is_found_and_a_recurrence_is_a_schedule() {
    let definition = definition();
    assert!(node(&definition, "Post").is_some());
    assert!(node(&definition, "Recurrence").is_some());
    assert!(node(&definition, "Missing").is_none());
    assert_eq!(recurrence_trigger(&definition).as_deref(), Some("Recurrence"));
}

#[test]
fn the_local_zone_is_found_by_offset_and_city() {
    use super::super::zones::by_offset;
    assert_eq!(by_offset(3 * 3600, Some("Istanbul")).0, "Turkey Standard Time");
    assert_eq!(by_offset(3 * 3600, None).0, "Arabic Standard Time");
    assert_eq!(by_offset(0, Some("London")).0, "GMT Standard Time");
    assert_eq!(by_offset(5 * 3600 + 45 * 60, None).0, "Nepal Standard Time");
    // the names carry standard offsets: Berlin by its standard +01:00, summer or not
    assert_eq!(by_offset(3600, Some("Berlin")).0, "W. Europe Standard Time");
}

#[test]
fn a_known_system_zone_is_its_own_windows_zone() {
    use super::super::zones::{TZDB, by_tzdb};
    // no name shows New York or Chicago: by offset and city they'd be Bogota's and Guatemala's zones
    assert_eq!(by_tzdb("America/New_York").map(|zone| zone.0), Some("Eastern Standard Time"));
    assert_eq!(by_tzdb("America/Chicago").map(|zone| zone.0), Some("Central Standard Time"));
    assert_eq!(by_tzdb("Europe/Berlin").map(|zone| zone.0), Some("W. Europe Standard Time"));
    assert_eq!(by_tzdb("Mars/Olympus_Mons"), None);
    for (tzdb, windows) in TZDB {
        assert_eq!(by_tzdb(tzdb).map(|zone| zone.0), Some(windows), "{tzdb} names a zone Teams offers");
    }
}
