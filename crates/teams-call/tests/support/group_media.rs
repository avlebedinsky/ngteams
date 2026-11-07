//! A group call's media lines described again (the camera on or off, a screen shared) and its send capabilities, at
//! the call controller's `updateMediaDescriptions` and `applyChannelParameters` links, on synthetic deliveries.

use super::*;
use teams_proto::{ChannelParameters, MediaDescription, MediaDescriptions};

/// An acceptance handing out the call controller's media links: `updateMediaDescriptions` always, and
/// `applyChannelParameters` at `parameters` when given.
fn with_media_links(parameters: Option<&str>) -> Value {
    let mut accepted = acceptance();
    let links = &mut accepted["callAcceptance"]["links"];
    links["updateMediaDescriptions"] = json!(cc("active", "updateMediaDescriptions"));
    if let Some(op) = parameters {
        links["applyChannelParameters"] = json!(cc("mcProxy", op));
    }
    accepted
}

/// The camera on: the main video line both ways, labelled, and a receiving line.
fn camera_on() -> MediaDescriptions {
    MediaDescriptions {
        descriptions: vec![
            MediaDescription {
                mid: "1".into(),
                direction: "sendrecv".into(),
                label: Some("main-video".into()),
            },
            MediaDescription {
                mid: "2".into(),
                direction: "recvonly".into(),
                label: None,
            },
        ],
        request_id: Some(3),
    }
}

fn send_capabilities() -> ChannelParameters {
    ChannelParameters {
        mids: vec!["1".into()],
        media_parameter: r#"{"maxBitrate":1500000}"#.into(),
    }
}

/// The media writes among `effects`: each by its name, with its link and body.
fn media_writes(effects: &[Effect]) -> Vec<(WriteId, &'static str, String, Value)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Send { id, write } => {
                let (url, body) = match write.as_ref() {
                    CallWrite::MediaDescriptions(write) => (write.call_url(), write.body()),
                    CallWrite::ChannelParameters(write) => (write.call_url(), write.body()),
                    _ => (None, None),
                };
                Some((
                    *id,
                    write.name(),
                    url.map(ToString::to_string).unwrap_or_default(),
                    body.unwrap_or(Value::Null),
                ))
            }
            _ => None,
        })
        .collect()
}

fn ended(effects: &[Effect]) -> bool {
    events(effects)
        .iter()
        .any(|event| matches!(event, CallEvent::Ended { .. }))
}

#[test]
fn the_camera_turned_on_describes_the_lines_and_then_gives_the_send_capabilities_and_neither_failing_ends_the_call() {
    let now = Instant::now();
    let (mut calls, call) = up_with(now, &with_media_links(Some("applyChannelParameters")));
    calls.describable(call).unwrap();
    calls
        .describe_media(call, &camera_on(), Some(send_capabilities()))
        .unwrap();
    let described = media_writes(&effects(&mut calls));
    let [
        (lines, "update_media_descriptions", lines_at, lines_body),
        (capabilities, "apply_channel_parameters", capabilities_at, capabilities_body),
    ] = &described[..]
    else {
        panic!("{described:#?}");
    };
    assert_eq!(*lines_at, cc("active", "updateMediaDescriptions"));
    assert_eq!(
        lines_body["UpdateMediaDescriptions"]["mediaDescriptions"],
        json!({"descriptions": [
            {"mid": "1", "direction": "sendrecv", "label": "main-video"},
            {"mid": "2", "direction": "recvonly"},
        ], "requestId": 3}),
        "no share: no negotiation tag"
    );
    assert_eq!(*capabilities_at, cc("mcProxy", "applyChannelParameters"));
    assert_eq!(
        capabilities_body["applyChannelParameters"]["multiChannelParameter"],
        json!({"mids": ["1"], "mediaParameter": r#"{"maxBitrate":1500000}"#})
    );
    calls.on_outcome(*lines, &failed(), now);
    calls.on_outcome(*capabilities, &failed(), now);
    let after = effects(&mut calls);
    assert!(!ended(&after), "{after:#?}");
    assert!(sent(&after).is_empty(), "nothing goes in their place");
    assert_eq!(calls.current(), Some((call, Some(CallState::Connected))));
    calls.describable(call).unwrap();
}

#[test]
fn send_capabilities_go_only_when_given_and_where_the_controller_named_a_link_for_them() {
    let now = Instant::now();
    let (mut calls, call) = up_with(now, &with_media_links(Some("applyChannelParameters")));
    calls.describe_media(call, &camera_on(), None).unwrap();
    let names: Vec<_> = sent(&effects(&mut calls)).into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, ["update_media_descriptions"], "none given");
    let (mut calls, call) = up_with(now, &with_media_links(None));
    calls
        .describe_media(call, &camera_on(), Some(send_capabilities()))
        .unwrap();
    let names: Vec<_> = sent(&effects(&mut calls)).into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, ["update_media_descriptions"], "no link for them");
}

#[test]
fn send_capabilities_that_can_t_be_built_leave_the_descriptions_unsent_too() {
    let now = Instant::now();
    // a link whose last segment is no `applyChannelParameters`: the write doesn't map
    let (mut calls, call) = up_with(now, &with_media_links(Some("channelParameters")));
    let refused = calls.describe_media(call, &camera_on(), Some(send_capabilities()));
    assert!(matches!(refused, Err(CallError::Write(_))), "{refused:?}");
    assert!(sent(&effects(&mut calls)).is_empty(), "nothing was sent");
    // without them the descriptions still go
    calls.describe_media(call, &camera_on(), None).unwrap();
    assert_eq!(sent(&effects(&mut calls)).len(), 1);
}

#[test]
fn nothing_is_described_for_another_call_or_one_whose_controller_named_no_link() {
    let now = Instant::now();
    let (mut bare, id) = up(now);
    assert!(matches!(bare.describable(id), Err(CallError::NotNow)), "no link");
    assert!(matches!(
        bare.describe_media(id, &camera_on(), None),
        Err(CallError::NotNow)
    ));
    assert!(matches!(bare.describe_share(id, &camera_on()), Err(CallError::NotNow)));
    let (mut calls, call) = up_with(now, &with_media_links(None));
    assert!(matches!(calls.describable(CallId(99)), Err(CallError::NoSuchCall)));
    assert!(matches!(
        calls.describe_media(CallId(99), &camera_on(), None),
        Err(CallError::NoSuchCall)
    ));
    assert!(matches!(
        calls.describe_share(CallId(99), &camera_on()),
        Err(CallError::NoSuchCall)
    ));
    assert!(sent(&effects(&mut calls)).is_empty(), "nothing went for {call:?}");
}

#[test]
fn nothing_is_described_while_the_call_rings_or_its_renegotiation_waits_for_its_answer() {
    let now = Instant::now();
    let (mut ringing, call_ringing) = ringing_call(now);
    assert!(
        matches!(ringing.describable(call_ringing), Err(CallError::NotNow)),
        "ringing"
    );
    let mut renegotiable = with_media_links(None);
    renegotiable["callAcceptance"]["links"]["mediaRenegotiation"] = json!(cc("active", "renegotiate"));
    let (mut renegotiated, id) = up_with(now, &renegotiable);
    renegotiated.renegotiate(id, &media()).unwrap();
    let _ = effects(&mut renegotiated);
    assert!(
        matches!(renegotiated.describable(id), Err(CallError::NotNow)),
        "its renegotiation waits for its answer"
    );
}

#[test]
fn a_shared_screen_is_described_tagged_as_the_share_its_attributes_numbered() {
    let now = Instant::now();
    let (mut calls, call) = up_with(now, &with_media_links(None));
    let me = calls
        .call
        .as_ref()
        .and_then(|call| call.me.participant_id.clone())
        .unwrap();
    let sharing = MediaDescriptions {
        descriptions: vec![MediaDescription {
            mid: "3".into(),
            direction: "sendonly".into(),
            label: Some("applicationsharing-video".into()),
        }],
        request_id: Some(4),
    };
    calls.tell_screen_shared(call).unwrap();
    calls.tell_screen_shared(call).unwrap();
    let _ = effects(&mut calls);
    calls.describe_share(call, &sharing).unwrap();
    let described = media_writes(&effects(&mut calls));
    let [(_, "update_media_descriptions", at, body)] = &described[..] else {
        panic!("{described:#?}");
    };
    assert_eq!(*at, cc("active", "updateMediaDescriptions"));
    let described = &body["UpdateMediaDescriptions"]["mediaDescriptions"];
    assert_eq!(
        described["negotiationTag"],
        json!(format!("{me};ss_2")),
        "the second share's"
    );
    assert_eq!(
        described["descriptions"],
        json!([{"mid": "3", "direction": "sendonly", "label": "applicationsharing-video"}])
    );
}

#[test]
fn the_first_share_is_tagged_ss_1_even_before_its_attributes_were_told() {
    let now = Instant::now();
    let (mut calls, call) = up_with(now, &with_media_links(None));
    let me = calls
        .call
        .as_ref()
        .and_then(|call| call.me.participant_id.clone())
        .unwrap();
    calls.describe_share(call, &camera_on()).unwrap();
    let described = media_writes(&effects(&mut calls));
    let [(_, "update_media_descriptions", _, body)] = &described[..] else {
        panic!("{described:#?}");
    };
    assert_eq!(
        body["UpdateMediaDescriptions"]["mediaDescriptions"]["negotiationTag"],
        json!(format!("{me};ss_1"))
    );
}

#[test]
fn a_share_described_at_a_link_the_write_doesn_t_map_sends_nothing() {
    let now = Instant::now();
    let mut accepted = acceptance();
    accepted["callAcceptance"]["links"]["updateMediaDescriptions"] = json!(cc("active", "describe"));
    let (mut calls, call) = up_with(now, &accepted);
    assert!(matches!(
        calls.describe_share(call, &camera_on()),
        Err(CallError::Write(_))
    ));
    assert!(matches!(
        calls.describe_media(call, &camera_on(), None),
        Err(CallError::Write(_))
    ));
    assert!(sent(&effects(&mut calls)).is_empty());
}
