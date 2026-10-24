//! The app's own-presence policy (M2 Task 20), on a clock the test moves: Available (the endpoint `PUT`) once the
//! push channel registered while the user was active within the idle time, sent again every
//! [`AVAILABLE_REFRESH`]; nothing once idle (the endpoint lapses on its own), the `PUT` again at once on the next
//! use; a manual status overrides it until cleared and keeps the endpoint reported, idle or not; the saved manual
//! status applied once at the run's first registration, Automatic at start sending no reset; nothing on quit; a failed
//! report never sent again on its own (a fence denial stops every report for the run), the refresh never brought
//! forward by one; a manual status or reset that failed shown as not set and sent again when the user chooses it again.

use std::time::{Duration, Instant};

use teams_core::own_presence::{AVAILABLE_REFRESH, Choice, OwnPresencePolicy, Reporting};
use teams_core::{EngineError, OwnStatus, TransientSent};
use teams_proto::ManualAvailability;

const IDLE: Duration = Duration::from_mins(5);
/// A second short of the refresh.
const JUST_BEFORE_REFRESH: Duration = Duration::from_secs(119);
const SECOND: Duration = Duration::from_secs(1);

/// A policy started at `start` with `choice` and the 5-minute idle time.
fn started(choice: Choice) -> (OwnPresencePolicy, Instant) {
    let start = Instant::now();
    (OwnPresencePolicy::new(choice, IDLE, start), start)
}

fn sent() -> TransientSent {
    TransientSent::Sent
}

#[test]
fn nothing_is_sent_before_the_push_channel_registered() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.reporting(start), Reporting::Connecting);
    assert!(policy.active(start + SECOND).is_empty());
    assert!(policy.tick(start + AVAILABLE_REFRESH * 3).is_empty());
    assert!(
        policy
            .choose(Choice::Manual(ManualAvailability::Busy), start)
            .is_empty(),
        "kept for the registration"
    );
}

#[test]
fn available_goes_out_at_the_registration_and_again_every_refresh_while_the_window_is_used() {
    let (mut policy, start) = started(Choice::Automatic);
    // the app was just started: that counts as use
    assert_eq!(policy.registered(start + SECOND), [OwnStatus::Available]);
    assert_eq!(policy.reporting(start + SECOND), Reporting::Available);
    // nothing more until the refresh is due, however often it is ticked or used
    assert!(policy.tick(start + SECOND * 30).is_empty());
    assert!(policy.active(start + SECOND * 60).is_empty());
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
    assert_eq!(policy.tick(start + SECOND + AVAILABLE_REFRESH), [OwnStatus::Available]);
    // kept up while the user keeps using the window
    let mut now = start + SECOND + AVAILABLE_REFRESH;
    for _ in 0..5 {
        assert!(policy.active(now + SECOND * 10).is_empty());
        now += AVAILABLE_REFRESH;
        assert_eq!(policy.tick(now), [OwnStatus::Available]);
    }
}

#[test]
fn the_refresh_is_well_inside_the_observed_lapse() {
    // one endpoint `PUT` read Offline 5 min 20 s after it (M2 Task 19, run 2): one refresh dropped still leaves the next
    // one before that
    assert!(AVAILABLE_REFRESH * 2 < Duration::from_secs(320));
}

#[test]
fn idle_sends_nothing_and_lets_the_endpoint_lapse_and_the_next_use_reports_available_at_once() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    assert_eq!(policy.tick(start + AVAILABLE_REFRESH), [OwnStatus::Available]);
    // the idle time passes without use: the refresh due then (and every one after) isn't sent
    let idle_at = start + IDLE;
    assert!(policy.tick(idle_at).is_empty());
    assert_eq!(policy.reporting(idle_at), Reporting::Inactive);
    assert!(policy.tick(start + AVAILABLE_REFRESH * 3).is_empty());
    assert!(policy.tick(start + AVAILABLE_REFRESH * 10).is_empty());
    // a registration while idle (a reconnect, a renewal) brings nothing back
    assert!(policy.registered(start + AVAILABLE_REFRESH * 11).is_empty());
    // the window used again: available at once, then the refresh from there
    let back = start + AVAILABLE_REFRESH * 12;
    assert_eq!(policy.active(back), [OwnStatus::Available]);
    assert_eq!(policy.reporting(back), Reporting::Available);
    assert!(policy.active(back + SECOND).is_empty());
    assert!(policy.tick(back + JUST_BEFORE_REFRESH).is_empty());
    assert_eq!(policy.tick(back + AVAILABLE_REFRESH), [OwnStatus::Available]);
}

#[test]
fn the_idle_time_is_a_setting_taken_at_once() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    // shortened below the time since the last use: idle now, nothing sent
    assert!(
        policy
            .set_idle_after(Duration::from_secs(60), start + SECOND * 90)
            .is_empty()
    );
    assert_eq!(policy.reporting(start + SECOND * 90), Reporting::Inactive);
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
    // lengthened past it again: reported at once
    assert_eq!(
        policy.set_idle_after(Duration::from_mins(30), start + AVAILABLE_REFRESH + SECOND),
        [OwnStatus::Available]
    );
}

#[test]
fn a_new_registration_while_used_reports_available_for_it() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    // the push channel stopped: nothing is reported, whatever the use
    policy.stopped();
    assert_eq!(policy.reporting(start + SECOND), Reporting::Connecting);
    assert!(policy.active(start + SECOND * 2).is_empty());
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
    // a registration again (a new endpoint, a renewal): reported at once, the refresh counted from there
    let again = start + AVAILABLE_REFRESH + SECOND;
    assert_eq!(policy.registered(again), [OwnStatus::Available]);
    assert_eq!(policy.registered(again + SECOND), [OwnStatus::Available]);
    assert!(policy.tick(again + AVAILABLE_REFRESH).is_empty());
    assert_eq!(policy.tick(again + SECOND + AVAILABLE_REFRESH), [OwnStatus::Available]);
}

#[test]
fn a_manual_status_overrides_until_cleared_and_keeps_the_endpoint_reported_when_idle() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy = Choice::Manual(ManualAvailability::Busy);
    assert_eq!(
        policy.choose(busy, start + SECOND),
        [OwnStatus::Manual(ManualAvailability::Busy)]
    );
    assert_eq!(
        policy.reporting(start + SECOND),
        Reporting::Manual(ManualAvailability::Busy)
    );
    // the same choice again sends nothing
    assert!(policy.choose(busy, start + SECOND * 2).is_empty());
    // idle: the endpoint is still kept up (a manual status with no live endpoint reads Offline), no reset, no away
    let mut now = start;
    for _ in 0..5 {
        now += AVAILABLE_REFRESH;
        assert_eq!(policy.tick(now), [OwnStatus::Available]);
    }
    assert_eq!(policy.reporting(now), Reporting::Manual(ManualAvailability::Busy));
    // cleared while idle: the reset, and the endpoint left to lapse
    let cleared = now + SECOND;
    assert_eq!(policy.choose(Choice::Automatic, cleared), [OwnStatus::ClearManual]);
    assert_eq!(policy.reporting(cleared), Reporting::Inactive);
    assert!(policy.tick(now + AVAILABLE_REFRESH).is_empty());
}

#[test]
fn a_manual_status_chosen_while_idle_reports_the_endpoint_at_once() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let idle = start + IDLE + SECOND;
    assert!(policy.tick(idle).is_empty());
    assert_eq!(
        policy.choose(Choice::Manual(ManualAvailability::DoNotDisturb), idle + SECOND),
        [
            OwnStatus::Available,
            OwnStatus::Manual(ManualAvailability::DoNotDisturb)
        ]
    );
    // cleared while used: the reset only, the endpoint's refresh goes on
    let used = idle + SECOND * 2;
    assert!(policy.active(used).is_empty());
    assert_eq!(policy.choose(Choice::Automatic, used), [OwnStatus::ClearManual]);
    assert_eq!(policy.reporting(used), Reporting::Available);
    assert_eq!(policy.tick(idle + SECOND + AVAILABLE_REFRESH), [OwnStatus::Available]);
}

#[test]
fn the_saved_manual_status_is_applied_once_at_the_run_s_first_registration() {
    let (mut policy, start) = started(Choice::Manual(ManualAvailability::Away));
    assert_eq!(
        policy.registered(start + SECOND),
        [OwnStatus::Available, OwnStatus::Manual(ManualAvailability::Away)]
    );
    // a later registration reports the endpoint, not the manual status again
    policy.stopped();
    assert_eq!(policy.registered(start + SECOND * 2), [OwnStatus::Available]);
}

#[test]
fn automatic_at_start_sends_no_reset() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    assert!(policy.choose(Choice::Automatic, start + SECOND).is_empty());
}

#[test]
fn a_choice_made_before_the_registration_takes_the_saved_one_s_place() {
    let (mut policy, start) = started(Choice::Manual(ManualAvailability::Busy));
    assert!(
        policy
            .choose(Choice::Manual(ManualAvailability::DoNotDisturb), start)
            .is_empty()
    );
    assert_eq!(
        policy.registered(start + SECOND),
        [
            OwnStatus::Available,
            OwnStatus::Manual(ManualAvailability::DoNotDisturb)
        ]
    );

    // the saved one cleared before the registration: the reset (an earlier run may have left it set), once
    let (mut policy, start) = started(Choice::Manual(ManualAvailability::Busy));
    assert!(policy.choose(Choice::Automatic, start).is_empty());
    assert_eq!(
        policy.registered(start + SECOND),
        [OwnStatus::Available, OwnStatus::ClearManual]
    );
    policy.stopped();
    assert_eq!(policy.registered(start + SECOND * 2), [OwnStatus::Available]);
}

#[test]
fn quit_sends_nothing_and_stops_every_report() {
    let (mut policy, start) = started(Choice::Manual(ManualAvailability::Busy));
    assert_eq!(policy.registered(start).len(), 2);
    assert!(policy.quit().is_empty(), "no request reports Offline: nothing is sent");
    assert_eq!(policy.reporting(start), Reporting::Quit);
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
    assert!(policy.active(start + AVAILABLE_REFRESH).is_empty());
    assert!(policy.registered(start + AVAILABLE_REFRESH).is_empty());
    assert!(policy.choose(Choice::Automatic, start + AVAILABLE_REFRESH).is_empty());
}

#[test]
fn a_dropped_report_is_never_sent_again_and_the_refresh_isn_t_brought_forward() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let dropped = TransientSent::Dropped {
        reason: "HTTP 500".to_owned(),
    };
    policy.reported(OwnStatus::Available, Ok(&dropped));
    // nothing sooner than the refresh
    assert!(policy.tick(start + SECOND).is_empty());
    assert!(policy.active(start + SECOND * 2).is_empty());
    assert!(policy.tick(start + JUST_BEFORE_REFRESH).is_empty());
    assert_eq!(policy.tick(start + AVAILABLE_REFRESH), [OwnStatus::Available]);

    // a manual status that failed isn't sent again: not on a tick, not on a registration
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    let now = start + AVAILABLE_REFRESH + SECOND;
    assert_eq!(policy.choose(Choice::Manual(ManualAvailability::Busy), now), [busy]);
    policy.reported(busy, Ok(&dropped));
    assert!(policy.tick(now + SECOND).is_empty());
    assert_eq!(policy.registered(now + SECOND * 2), [OwnStatus::Available]);
    assert!(policy.tick(now + AVAILABLE_REFRESH).is_empty());
}

#[test]
fn a_failed_manual_status_is_shown_as_not_set_and_sent_again_when_chosen_again() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy_choice = Choice::Manual(ManualAvailability::Busy);
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    assert_eq!(policy.choose(busy_choice, start), [busy]);
    let dropped = TransientSent::Dropped {
        reason: "HTTP 500".to_owned(),
    };
    policy.reported(busy, Ok(&dropped));
    // not shown as reported: what the account shows isn't known
    assert_eq!(policy.reporting(start + SECOND), Reporting::NotSet(busy_choice));
    assert!(policy.tick(start + SECOND * 2).is_empty());
    // the endpoint stays reported meanwhile
    assert_eq!(policy.tick(start + AVAILABLE_REFRESH), [OwnStatus::Available]);
    // the user chooses it again: sent again, once
    let again = start + AVAILABLE_REFRESH + SECOND;
    assert_eq!(policy.choose(busy_choice, again), [busy]);
    assert_eq!(policy.reporting(again), Reporting::Manual(ManualAvailability::Busy));
    policy.reported(busy, Ok(&sent()));
    assert_eq!(policy.reporting(again), Reporting::Manual(ManualAvailability::Busy));
    // gone through: choosing it again sends nothing
    assert!(policy.choose(busy_choice, again + SECOND).is_empty());

    // one that waited and was then dropped is not set either; an engine error too
    assert_eq!(
        policy.choose(Choice::Manual(ManualAvailability::Away), again + SECOND * 2),
        [OwnStatus::Manual(ManualAvailability::Away)]
    );
    let away = OwnStatus::Manual(ManualAvailability::Away);
    policy.reported(away, Ok(&TransientSent::Waiting { not_before: 0 }));
    assert_eq!(
        policy.reporting(again + SECOND * 2),
        Reporting::Manual(ManualAvailability::Away)
    );
    policy.reported(away, Ok(&dropped));
    assert_eq!(
        policy.reporting(again + SECOND * 2),
        Reporting::NotSet(Choice::Manual(ManualAvailability::Away))
    );
    assert_eq!(
        policy.choose(Choice::Manual(ManualAvailability::Away), again + SECOND * 3),
        [away]
    );
    policy.reported(away, Err(&EngineError::NotAChat));
    assert_eq!(
        policy.reporting(again + SECOND * 3),
        Reporting::NotSet(Choice::Manual(ManualAvailability::Away))
    );
}

#[test]
fn a_failed_reset_is_shown_as_not_set_and_sent_again_when_automatic_is_chosen_again() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    assert_eq!(policy.choose(Choice::Manual(ManualAvailability::Busy), start), [busy]);
    policy.reported(busy, Ok(&sent()));
    assert_eq!(
        policy.choose(Choice::Automatic, start + SECOND),
        [OwnStatus::ClearManual]
    );
    policy.reported(
        OwnStatus::ClearManual,
        Ok(&TransientSent::Dropped {
            reason: "HTTP 500".to_owned(),
        }),
    );
    // the account may still be Busy on the server: not shown as Available
    assert_eq!(
        policy.reporting(start + SECOND * 2),
        Reporting::NotSet(Choice::Automatic)
    );
    assert!(policy.tick(start + SECOND * 3).is_empty());
    assert_eq!(
        policy.choose(Choice::Automatic, start + SECOND * 4),
        [OwnStatus::ClearManual]
    );
    assert_eq!(policy.reporting(start + SECOND * 4), Reporting::Available);
    policy.reported(OwnStatus::ClearManual, Ok(&sent()));
    assert!(policy.choose(Choice::Automatic, start + SECOND * 5).is_empty());
    assert_eq!(policy.reporting(start + SECOND * 5), Reporting::Available);
}

#[test]
fn a_failure_of_an_earlier_choice_doesn_t_mark_the_current_one_not_set() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    let away = Choice::Manual(ManualAvailability::Away);
    assert_eq!(policy.choose(Choice::Manual(ManualAvailability::Busy), start), [busy]);
    assert_eq!(
        policy.choose(away, start + SECOND),
        [OwnStatus::Manual(ManualAvailability::Away)]
    );
    let dropped = TransientSent::Dropped {
        reason: "HTTP 500".to_owned(),
    };
    policy.reported(busy, Ok(&dropped));
    // nor the endpoint's failure
    policy.reported(OwnStatus::Available, Ok(&dropped));
    assert_eq!(
        policy.reporting(start + SECOND * 2),
        Reporting::Manual(ManualAvailability::Away)
    );
    assert!(policy.choose(away, start + SECOND * 2).is_empty());
}

#[test]
fn a_fence_denial_stops_every_report_for_the_run() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let denied = TransientSent::Denied {
        reason: "not the fence's own account".to_owned(),
    };
    policy.reported(OwnStatus::Available, Ok(&denied));
    assert_eq!(policy.reporting(start + SECOND), Reporting::Halted);
    assert!(policy.tick(start + AVAILABLE_REFRESH * 2).is_empty());
    assert!(policy.registered(start + AVAILABLE_REFRESH * 3).is_empty());
    assert!(
        policy
            .choose(Choice::Manual(ManualAvailability::Busy), start + AVAILABLE_REFRESH * 3)
            .is_empty()
    );
}

#[test]
fn a_report_the_engine_had_no_endpoint_for_waits_for_the_next_registration() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    assert_eq!(policy.choose(Choice::Manual(ManualAvailability::Busy), start), [busy]);
    // the push channel stopped before the engine took them (its stop not told yet)
    policy.reported(OwnStatus::Available, Err(&EngineError::NoEndpoint));
    policy.reported(busy, Err(&EngineError::NoEndpoint));
    assert_eq!(policy.reporting(start + SECOND), Reporting::Connecting);
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
    // the choice still stands: it goes out with the next registration, never sent twice
    assert_eq!(
        policy.registered(start + AVAILABLE_REFRESH + SECOND),
        [OwnStatus::Available, busy]
    );
    policy.stopped();
    assert_eq!(
        policy.registered(start + AVAILABLE_REFRESH + SECOND * 2),
        [OwnStatus::Available]
    );
}

#[test]
fn a_report_answered_after_the_choice_changed_doesn_t_bring_the_old_choice_back() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    let busy = OwnStatus::Manual(ManualAvailability::Busy);
    assert_eq!(policy.choose(Choice::Manual(ManualAvailability::Busy), start), [busy]);
    assert_eq!(
        policy.choose(Choice::Automatic, start + SECOND),
        [OwnStatus::ClearManual]
    );
    // Busy's answer comes now: no endpoint (the channel just stopped); it isn't kept for the next registration
    policy.reported(busy, Err(&EngineError::NoEndpoint));
    policy.reported(OwnStatus::ClearManual, Ok(&sent()));
    assert_eq!(policy.registered(start + SECOND * 2), [OwnStatus::Available]);
}

#[test]
fn a_stopped_engine_ends_the_reports() {
    let (mut policy, start) = started(Choice::Automatic);
    assert_eq!(policy.registered(start), [OwnStatus::Available]);
    policy.reported(OwnStatus::Available, Err(&EngineError::Stopped));
    assert_eq!(policy.reporting(start), Reporting::Quit);
    assert!(policy.tick(start + AVAILABLE_REFRESH).is_empty());
}
