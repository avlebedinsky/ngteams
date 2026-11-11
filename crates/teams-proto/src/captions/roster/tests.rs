use super::*;
use crate::captions::{CAPTION_SERVICE_ALTERNATE_MRI as Y, CAPTION_SERVICE_MRI as X, is_caption_service};
use serde_json::json;

const COMMAND: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep/aks-prod-zz-api.callrecorder.teams.cloud.microsoft:21606/v2/oncommand/00000000-0000-4000-8000-000000000001";
const EARLY: &str = "2026-01-01T00:00:01.9999999Z";
const LATE: &str = "2026-01-01T00:00:02.5Z";

fn endpoint(joined: &str, state: &str) -> Value {
    json!({"endpointMetadata": {"commandUrl": COMMAND, "joinTime": joined, "spokenLanguage": "en-us",
        "processingModes": {"closedCaptions": {"state": state}, "recording": {"state": state}}}})
}

fn full(version: u64, joined: &str, state: &str) -> Value {
    json!({"version": version, "state": "active", "endpoints": {"synthetic": endpoint(joined, state)}})
}

fn apply(roster: &mut CaptionRoster, mri: &str, entry: Value) {
    let mut body = json!({"participants": {}});
    body["participants"][mri] = entry;
    roster.update(&body);
}

#[derive(Clone, Copy, Debug)]
enum Update {
    Full,
    Partial,
    Disconnected,
}

fn update(kind: Update, joined: &str) -> Value {
    match kind {
        Update::Full => full(2, joined, "Active"),
        Update::Partial => json!({"version": 2, "endpoints": {"synthetic": {"endpointMetadata": {
            "processingModes": {"closedCaptions": {"state": "Active"}}
        }}}}),
        Update::Disconnected => json!({"version": 2, "state": "disconnected"}),
    }
}

/// Both entries must be applied, regardless of which one is full or newer. Exercise no prior service, either
/// region alone, and both regions, with full reads, deltas and departures in all pairs and both join orders/ties.
#[test]
fn both_regions_are_merged_before_selecting_the_service() {
    let kinds = [Update::Full, Update::Partial, Update::Disconnected];
    for initial in [0, 1, 2, 3] {
        for (x_join, y_join) in [(EARLY, LATE), (LATE, EARLY), (LATE, LATE)] {
            for x in kinds {
                for y in kinds {
                    check_combination(initial, x_join, y_join, x, y);
                }
            }
        }
    }
}

fn check_combination(initial: u8, x_join: &str, y_join: &str, x: Update, y: Update) {
    let mut roster = CaptionRoster::default();
    if initial & 1 != 0 {
        apply(&mut roster, X, full(1, x_join, "Inactive"));
    }
    if initial & 2 != 0 {
        apply(&mut roster, Y, full(1, y_join, "Inactive"));
    }
    roster.update(&json!({"participants": {X: update(x, x_join), Y: update(y, y_join)}}));
    let active = |kind, bit| match kind {
        Update::Full => true,
        Update::Partial => initial & bit != 0,
        Update::Disconnected => false,
    };
    let expected = match (active(x, 1), active(y, 2)) {
        (true, true) => Some(if x_join >= y_join { X } else { Y }),
        (true, false) => Some(X),
        (false, true) => Some(Y),
        (false, false) => None,
    };
    assert_eq!(
        roster.followed().map(|service| service.mri),
        expected,
        "initial={initial}, joins=({x_join},{y_join}), updates=({x:?},{y:?})"
    );
    assert_eq!(roster.service().map(|service| service.mri), expected);
    if let Some(service) = roster.service() {
        assert_eq!(service.state, "Active");
    }
}

#[test]
fn handover_handback_and_versions_belong_to_each_mri() {
    let mut roster = CaptionRoster::default();
    apply(&mut roster, X, full(20, EARLY, "Inactive"));
    apply(&mut roster, Y, full(1, LATE, "Inactive"));
    assert_eq!(roster.service().unwrap().mri, Y);
    // The old full read cannot hide the followed leg's delta, even with the same endpoint key.
    roster.update(&json!({"participants": {X: full(21, EARLY, "Inactive"), Y: update(Update::Partial, LATE)}}));
    assert_eq!(roster.service().unwrap().state, "Active");
    roster.update(&json!({"participants": {X: full(22, EARLY, "Inactive"),
        Y: json!({"version": 3, "state": "disconnected"})}}));
    assert_eq!(roster.service().unwrap().mri, X);
    // Equal or older versions remain stale after handback; each region retains its history.
    apply(&mut roster, Y, full(2, LATE, "Active"));
    apply(&mut roster, X, json!({"version": 22, "state": "disconnected"}));
    assert_eq!(roster.service().unwrap().mri, X);
    apply(&mut roster, Y, full(4, LATE, "Inactive"));
    assert_eq!(roster.service().unwrap().mri, Y);
    apply(&mut roster, Y, json!({"version": 5, "state": "disconnected"}));
    apply(&mut roster, X, json!({"version": 23, "state": "disconnected"}));
    assert!(roster.followed().is_none());
    apply(&mut roster, Y, json!({"version": 6, "state": "active"}));
    assert!(
        roster.followed().is_none(),
        "a new active delta cannot revive departed endpoints"
    );
}

#[test]
fn newest_endpoint_preserves_join_and_partial_metadata_and_revokes_bad_links() {
    let mut roster = CaptionRoster::default();
    let mut entry = full(1, EARLY, "Inactive");
    entry["endpoints"]["synthetic-newer"] = endpoint(LATE, "Inactive");
    entry["endpoints"]["synthetic-unknown"] = endpoint("unreadable", "Inactive");
    apply(&mut roster, X, entry);
    assert_eq!(roster.service().unwrap().endpoint_id, "synthetic-newer");
    let mut repeated = endpoint(LATE, "Inactive");
    repeated["endpointMetadata"].as_object_mut().unwrap().remove("joinTime");
    apply(
        &mut roster,
        X,
        json!({"version": 2, "state": "active", "endpoints": {"synthetic-newer": repeated}}),
    );
    apply(&mut roster, X, full(3, EARLY, "Active"));
    let delta = json!({"version": 4, "endpoints": {"synthetic-newer": {"endpointMetadata": {
        "spokenLanguage": "en-gb", "processingModes": {"closedCaptions": {"state": "Active"}}
    }}}});
    apply(&mut roster, X, delta);
    let service = roster.service().unwrap();
    assert_eq!(service.joined.as_deref(), Some(LATE));
    assert_eq!(service.spoken_language, "en-gb");
    assert_eq!(service.state, "Active");
    assert_eq!(service.command.as_str(), COMMAND);
    apply(
        &mut roster,
        X,
        json!({"version": 5, "endpoints": {"synthetic-newer": {"endpointMetadata": {
            "commandUrl": "https://outside.example.test/"
        }}}}),
    );
    assert!(
        roster.service().is_none(),
        "invalid link revokes binding rather than falling back to an older endpoint"
    );
    assert_eq!(roster.followed().unwrap().endpoint_id, "synthetic-newer");
}

#[test]
fn service_identity_and_join_time_order_are_preserved() {
    assert!(is_caption_service(X));
    assert!(is_caption_service(&Y.to_uppercase()));
    assert!(!is_caption_service("28:00000000-0000-4000-8000-000000000001"));
    let mut roster = CaptionRoster::default();
    for invalid in [
        json!({}),
        json!({"participants": []}),
        json!({"participants": {"unknown": full(1, LATE, "Active")}}),
    ] {
        roster.update(&invalid);
        assert!(roster.followed().is_none());
    }
    for (joined, expected) in [
        ("2026-01-01T00:00:00.9Z", X),
        ("2026-01-01T00:00:01.1Z", Y),
        ("2026-01-01T00:00:01.1234567890Z", X),
        ("unreadable", X),
    ] {
        let mut roster = CaptionRoster::default();
        apply(&mut roster, X, full(1, "2026-01-01T00:00:01Z", "Inactive"));
        apply(&mut roster, Y, full(1, joined, "Inactive"));
        assert_eq!(roster.service().unwrap().mri, expected);
    }
}

#[test]
fn recording_only_endpoint_survives_the_other_regions_empty_active_delta() {
    let mut roster = CaptionRoster::default();
    let mut entry = full(1, LATE, "Active");
    entry["endpoints"]["synthetic"]["endpointMetadata"]
        .as_object_mut()
        .unwrap()
        .remove("commandUrl");
    apply(&mut roster, Y, entry);
    assert!(roster.service().is_none());
    apply(&mut roster, X, json!({"version": 24, "state": "active"}));
    assert_eq!(roster.followed().unwrap().mri, Y);
    assert_eq!(
        roster.followed().unwrap().metadata["processingModes"]["recording"]["state"],
        "Active"
    );
}

#[test]
fn endpoint_deltas_and_mode_snapshots_stay_with_their_own_participant() {
    let mut roster = CaptionRoster::default();
    let mut partial = full(1, LATE, "Active");
    partial.as_object_mut().unwrap().remove("state");
    apply(&mut roster, Y, partial);
    assert!(roster.followed().is_none());
    apply(&mut roster, Y, json!({"version": 2, "state": "active"}));
    assert_eq!(roster.service().unwrap().mri, Y);
    // A metadata delta without modes retains them; a supplied mode map replaces the previous snapshot.
    apply(
        &mut roster,
        Y,
        json!({"version": 3, "endpoints": {"synthetic": {"endpointMetadata": {
            "spokenLanguage": "en-gb"
        }}}}),
    );
    assert_eq!(roster.service().unwrap().state, "Active");
    apply(
        &mut roster,
        Y,
        json!({"version": 4, "endpoints": {"synthetic": {"endpointMetadata": {
            "processingModes": {"closedCaptions": {"state": "UnknownFutureState"}}
        }}}}),
    );
    assert_eq!(roster.service().unwrap().state, "UnknownFutureState");
    assert!(
        roster.followed().unwrap().metadata["processingModes"]
            .get("recording")
            .is_none()
    );
    apply(&mut roster, X, full(20, EARLY, "Inactive"));
    apply(
        &mut roster,
        Y,
        json!({"version": 5, "endpoints": {"synthetic": {"state": "disconnected"}}}),
    );
    assert_eq!(roster.service().unwrap().mri, X);
    apply(&mut roster, X, json!({"version": 21, "endpoints": {"synthetic": null}}));
    assert!(roster.followed().is_none());
}
