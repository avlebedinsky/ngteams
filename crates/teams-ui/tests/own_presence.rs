//! The account's own presence as the app reports it (M2 Task 20), on GPUI's test context over the fake engine
//! (`common`) and its fake clock: Available once the push channel registered and again every refresh while the window
//! is used, nothing once idle (the idle time a setting) and Available again at the next use (the shell's mouse and key
//! input, a key a binding takes included); a manual status from the account menu (or the settings) sent at once,
//! overriding until Automatic is chosen again; nothing on quit; a failed report shown in the account menu and never sent
//! again on its own, a manual status or reset that failed shown as not set and sent again when chosen again in the
//! menu, a fence denial ending the reports. Every id here is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/parts.rs"]
mod parts;

use std::cell::Cell;
use std::future::{self, Future};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use common::{LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, point, px};
use teams_core::{
    AVAILABLE_REFRESH, Availability, Choice, EngineError, Event, OwnStatus, RegistrationReport, Reporting, Transient,
    TransientSent,
};
use teams_proto::{
    Authorized, EndpointRegistration, HttpError, ManualAvailability, Mutating, Read, RegisteredEndpoint,
    Request as ProtoRequest, Response, Transport, register,
};
use teams_trouter::Stop;
use teams_ui::own_presence::{self, OwnPresence, POLICY_TICK};
use teams_ui::settings::{ManualStatus, Notifications, Settings, SettingsModel};
use teams_ui::shell::{self, Shell};
use teams_ui::{ShellParts, ViewTrace};

/// Two of the policy's ticks short of the refresh, and those two ticks.
const JUST_BEFORE_REFRESH: Duration = Duration::from_secs(110);
const TWO_TICKS: Duration = Duration::from_secs(10);
/// One tick short of the refresh.
const REFRESH_LESS_A_TICK: Duration = Duration::from_secs(115);

/// The rig with the own presence over its core, its models and `settings`, tracing to the returned writer.
fn reporting(settings: Settings) -> (Rig, Entity<OwnPresence>, Entity<SettingsModel>, Written) {
    let rig = Rig::start();
    let written = Written::default();
    let trace = Arc::new(ViewTrace::new(written.clone()));
    let parts = parts::parts(&rig, settings, trace);
    let own = own_over(&rig, &parts);
    (rig, own, parts.settings, written)
}

fn own_over(rig: &Rig, parts: &ShellParts) -> Entity<OwnPresence> {
    let (core, models, settings, trace) = (
        Arc::clone(&parts.core),
        parts.models.clone(),
        parts.settings.clone(),
        Arc::clone(&parts.trace),
    );
    rig.cx
        .update(|cx| cx.new(|cx| OwnPresence::new(core, &models, &settings, trace, cx)))
}

/// The own availabilities asked of the engine since the last look.
fn asked(rig: &Rig) -> Vec<OwnStatus> {
    std::mem::take(&mut *rig.core.own_statuses.lock().unwrap())
}

/// Moves the fake clock on by `by` and lets the ticks run.
fn wait(rig: &Rig, by: Duration) {
    rig.cx.executor().advance_clock(by);
    rig.cx.run_until_parked();
}

fn registered(rig: &Rig) {
    rig.send(Event::Registration(RegistrationReport::Registered));
}

fn reporting_of(rig: &Rig, own: &Entity<OwnPresence>) -> Reporting {
    rig.cx.read(|cx| own.read(cx).reporting(cx))
}

fn used(rig: &Rig, own: &Entity<OwnPresence>) {
    rig.cx.update(|cx| own.update(cx, OwnPresence::active));
    rig.cx.run_until_parked();
}

fn choose(rig: &Rig, settings: &Entity<SettingsModel>, status: ManualStatus) {
    rig.cx.update(|cx| {
        settings.update(cx, |model, cx| {
            model.change(|settings| settings.manual_status = status, cx);
        });
    });
    rig.cx.run_until_parked();
}

#[test]
fn available_is_reported_once_registered_and_again_every_refresh_while_used() {
    assert_eq!(TWO_TICKS, POLICY_TICK * 2);
    assert_eq!(JUST_BEFORE_REFRESH + TWO_TICKS, AVAILABLE_REFRESH);
    assert_eq!(REFRESH_LESS_A_TICK + POLICY_TICK, AVAILABLE_REFRESH);
    let (rig, own, _, written) = reporting(Settings::default());
    // nothing before the push channel registered, however long
    wait(&rig, AVAILABLE_REFRESH * 2);
    assert!(asked(&rig).is_empty());
    assert_eq!(reporting_of(&rig, &own), Reporting::Connecting);
    // a registration that failed reports nothing
    rig.send(Event::Registration(RegistrationReport::Failed("HTTP 500".to_owned())));
    assert!(asked(&rig).is_empty());

    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Available);
    for _ in 0..3 {
        used(&rig, &own);
        wait(&rig, JUST_BEFORE_REFRESH);
        assert!(asked(&rig).is_empty(), "not before the refresh");
        wait(&rig, TWO_TICKS);
        assert_eq!(asked(&rig), [OwnStatus::Available]);
    }
    let sent: Vec<_> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "own-presence")
        .collect();
    assert_eq!(sent.len(), 4, "{sent:?}");
    assert!(
        sent.iter()
            .all(|line| line["change"] == "sent" && line["kinds"][0] == "available")
    );
}

#[test]
fn idle_reports_nothing_after_the_set_time_and_the_next_use_reports_available_at_once() {
    let settings = Settings {
        idle_minutes: 3,
        ..Settings::default()
    };
    let (rig, own, model, _) = reporting(settings);
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    wait(&rig, AVAILABLE_REFRESH);
    assert_eq!(asked(&rig), [OwnStatus::Available], "within the 3 minutes");
    // past the idle time without use: nothing is sent, the endpoint is left to lapse
    wait(&rig, Duration::from_secs(60));
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    wait(&rig, AVAILABLE_REFRESH * 5);
    assert!(asked(&rig).is_empty());
    // used again: at once
    used(&rig, &own);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Available);

    // the idle time is a setting, taken at once: shortened to a minute, two minutes after the last use
    wait(&rig, REFRESH_LESS_A_TICK);
    assert!(asked(&rig).is_empty());
    rig.cx
        .update(|cx| model.update(cx, |model, cx| model.change(|settings| settings.idle_minutes = 1, cx)));
    rig.cx.run_until_parked();
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    wait(&rig, AVAILABLE_REFRESH);
    assert!(asked(&rig).is_empty());
}

#[test]
fn a_manual_status_is_sent_when_chosen_and_overrides_until_automatic_is_chosen_again() {
    let (rig, own, settings, written) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    choose(&rig, &settings, ManualStatus::Busy);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Manual(ManualAvailability::Busy));
    // idle doesn't touch it, and the endpoint stays reported
    wait(&rig, AVAILABLE_REFRESH * 4);
    assert_eq!(asked(&rig), [OwnStatus::Available; 4]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Manual(ManualAvailability::Busy));
    // cleared while idle: the reset, then nothing more
    choose(&rig, &settings, ManualStatus::Automatic);
    assert_eq!(asked(&rig), [OwnStatus::ClearManual]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    wait(&rig, AVAILABLE_REFRESH * 2);
    assert!(asked(&rig).is_empty());
    let kinds: Vec<_> = written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "own-presence")
        .map(|line| line["kinds"][0].as_str().unwrap().to_owned())
        .collect();
    assert!(
        kinds.contains(&"busy".to_owned()) && kinds.contains(&"reset".to_owned()),
        "{kinds:?}"
    );
}

#[test]
fn the_saved_manual_status_goes_out_with_the_first_registration_and_automatic_sends_no_reset() {
    let saved = Settings {
        manual_status: ManualStatus::DoNotDisturb,
        ..Settings::default()
    };
    // the own presence held, as the desktop holds it: dropped, it hears nothing
    let (rig, _own, _, _) = reporting(saved);
    registered(&rig);
    assert_eq!(
        asked(&rig),
        [
            OwnStatus::Available,
            OwnStatus::Manual(ManualAvailability::DoNotDisturb)
        ]
    );

    let (rig, _own, _, _) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available], "no reset at the start");
}

#[test]
fn nothing_is_sent_on_quit_or_after_it() {
    let (rig, own, settings, _) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    rig.cx.update(|cx| own.update(cx, OwnPresence::quit));
    rig.cx.run_until_parked();
    assert_eq!(reporting_of(&rig, &own), Reporting::Quit);
    used(&rig, &own);
    choose(&rig, &settings, ManualStatus::Busy);
    wait(&rig, AVAILABLE_REFRESH * 2);
    registered(&rig);
    assert!(asked(&rig).is_empty());
    // the engine's shutdown is the app's, after the quit
    assert_eq!(rig.shutdowns(), 0);
    // no report ever shows as someone else's presence
    assert_eq!(
        rig.cx.read(|cx| rig.models.presence.read(cx).of(LAB_B)),
        Availability::Unknown
    );
    assert_eq!(
        rig.core
            .asked()
            .iter()
            .filter(|asked| **asked == "own_availability")
            .count(),
        1,
        "only the one before the quit"
    );
}

#[test]
fn a_failed_report_is_shown_and_never_sent_again() {
    let (rig, own, settings, written) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Ok(TransientSent::Dropped {
            reason: "HTTP 500; whether it arrived is unknown, so it isn't sent again".to_owned(),
        }));
    choose(&rig, &settings, ManualStatus::Busy);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    let failure = rig.cx.read(|cx| own.read(cx).failure().cloned()).expect("shown");
    assert_eq!(failure.status, OwnStatus::Manual(ManualAvailability::Busy));
    assert!(failure.why.contains("HTTP 500"), "{failure:?}");
    assert!(
        written
            .lines()
            .iter()
            .any(|line| line["view"] == "own-presence" && line["change"] == "failed" && line["kinds"][0] == "busy")
    );
    // not again: only the endpoint's refresh goes on, at its own time
    wait(&rig, AVAILABLE_REFRESH);
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available, OwnStatus::Available]);
    // the failure stays shown until a manual status goes through
    assert!(rig.cx.read(|cx| own.read(cx).failure().is_some()));
    choose(&rig, &settings, ManualStatus::Away);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Away)]);
    assert!(rig.cx.read(|cx| own.read(cx).failure().is_none()));
}

#[test]
fn a_report_that_waited_and_was_dropped_is_shown_and_a_typing_s_isn_t_its() {
    let (rig, own, _, _) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    // a waiting typing that was dropped is no report of the own presence's
    rig.send(Event::Transient(
        Transient::Typing {
            conversation: "19:chat@thread.v2".to_owned(),
        },
        TransientSent::Dropped {
            reason: "a message to its conversation went first".to_owned(),
        },
    ));
    assert!(rig.cx.read(|cx| own.read(cx).failure().is_none()));
    // the engine telling how a waiting endpoint `PUT` went: its `Event::Transient`, which the models turn into the
    // feed's `OwnReport`
    rig.send(Event::Transient(
        Transient::EndpointAvailable { endpoint: endpoint() },
        TransientSent::Dropped {
            reason: "throttled again after waiting once".to_owned(),
        },
    ));
    let failure = rig.cx.read(|cx| own.read(cx).failure().cloned()).expect("shown");
    assert_eq!(failure.status, OwnStatus::Available);
    assert!(failure.why.contains("throttled again"), "{failure:?}");
    // not sent again sooner than the refresh; the refresh that goes through clears it
    wait(&rig, JUST_BEFORE_REFRESH);
    assert!(asked(&rig).is_empty());
    used(&rig, &own);
    wait(&rig, TWO_TICKS);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert!(rig.cx.read(|cx| own.read(cx).failure().is_none()));
}

#[test]
fn a_waiting_manual_status_dropped_later_is_shown_as_not_set() {
    let (rig, own, settings, _) = reporting(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Ok(TransientSent::Waiting { not_before: 0 }));
    choose(&rig, &settings, ManualStatus::Busy);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Manual(ManualAvailability::Busy));
    rig.send(Event::Transient(
        Transient::ManualStatus {
            endpoint: endpoint(),
            status: Some(ManualAvailability::Busy),
        },
        TransientSent::Dropped {
            reason: "throttled again after waiting once".to_owned(),
        },
    ));
    assert_eq!(
        reporting_of(&rig, &own),
        Reporting::NotSet(Choice::Manual(ManualAvailability::Busy))
    );
    let failure = rig.cx.read(|cx| own.read(cx).failure().cloned()).expect("shown");
    assert_eq!(failure.status, OwnStatus::Manual(ManualAvailability::Busy));
}

#[test]
fn going_idle_tells_the_observers_without_any_other_input() {
    let (rig, own, _, _) = reporting(Settings {
        idle_minutes: 1,
        ..Settings::default()
    });
    registered(&rig);
    assert_eq!(reporting_of(&rig, &own), Reporting::Available);
    let told = Rc::new(Cell::new(0_u32));
    let counted = Rc::clone(&told);
    let _observing = rig
        .cx
        .update(|cx| cx.observe(&own, move |_, _| counted.set(counted.get() + 1)));
    // within the idle time: the ticks tell nothing
    wait(&rig, Duration::from_secs(50));
    assert_eq!(told.get(), 0);
    // past it: told once, by the tick that noticed
    wait(&rig, Duration::from_secs(20));
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    assert_eq!(told.get(), 1);
    wait(&rig, Duration::from_secs(60));
    assert_eq!(told.get(), 1, "nothing changed since");
}

#[test]
fn a_fence_denial_ends_the_reports_for_the_run() {
    let (rig, own, settings, _) = reporting(Settings::default());
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Ok(TransientSent::Denied {
            reason: "not the fence's own account".to_owned(),
        }));
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Halted);
    let failure = rig.cx.read(|cx| own.read(cx).failure().cloned()).expect("shown");
    assert!(failure.why.contains("the fence denied it"), "{failure:?}");
    wait(&rig, AVAILABLE_REFRESH * 3);
    choose(&rig, &settings, ManualStatus::Busy);
    registered(&rig);
    assert!(asked(&rig).is_empty());
}

#[test]
fn the_push_channel_stopped_ends_the_reports_and_a_stopped_engine_too() {
    let (rig, own, _, _) = reporting(Settings::default());
    registered(&rig);
    asked(&rig);
    rig.send(Event::PushStopped(Stop::Requested));
    assert_eq!(reporting_of(&rig, &own), Reporting::Connecting);
    wait(&rig, AVAILABLE_REFRESH * 2);
    assert!(asked(&rig).is_empty());

    let (rig, own, _, _) = reporting(Settings::default());
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Stopped));
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Quit);
    assert!(
        rig.cx.read(|cx| own.read(cx).failure().is_none()),
        "a stopped engine isn't a failure to show"
    );
}

// --- the account menu, in a shell's window ---

/// The shell in a window of its own, handed the own presence as the desktop hands it.
fn shell_with_own(settings: Settings) -> (Rig, Entity<Shell>, Entity<OwnPresence>, &'static mut VisualTestContext) {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(&rig, settings, Arc::new(ViewTrace::off()));
    let own = own_over(&rig, &parts);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    let handed = own.clone();
    cx.update(|_, cx| shell.update(cx, |shell, cx| shell.set_own_presence(handed, cx)));
    cx.run_until_parked();
    (rig, shell, own, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

#[test]
fn the_account_menu_sets_a_manual_status_and_shows_what_is_reported() {
    let (rig, shell, _, cx) = shell_with_own(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert!(bounds(cx, "rail-status").is_some());
    assert!(bounds(cx, "status-menu").is_none(), "closed at the start");
    click(cx, "rail-status");
    assert!(cx.update(|_, cx| shell.read(cx).status_menu_open()));
    assert!(bounds(cx, "status-menu-reported").is_some());
    // only Automatic and the captured manual statuses are offered
    for status in ManualStatus::ALL {
        assert!(
            bounds(cx, &format!("status-menu-{}", status.key())).is_some(),
            "{status:?}"
        );
    }
    assert!(bounds(cx, "status-menu-be-right-back").is_none());
    assert!(bounds(cx, "status-menu-appear-offline").is_none());

    click(cx, "status-menu-busy");
    assert!(
        !cx.update(|_, cx| shell.read(cx).status_menu_open()),
        "closed once chosen"
    );
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    let chosen = cx.update(|_, cx| shell.read(cx).settings().read(cx).settings().manual_status);
    assert_eq!(chosen, ManualStatus::Busy, "kept in the settings");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).own_reporting(cx)),
        Some(Reporting::Manual(ManualAvailability::Busy))
    );

    click(cx, "rail-status");
    click(cx, "status-menu-automatic");
    assert_eq!(asked(&rig), [OwnStatus::ClearManual]);
}

#[test]
fn the_account_menu_shows_a_failed_report() {
    let (rig, _, _, cx) = shell_with_own(Settings::default());
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Ok(TransientSent::Dropped {
            reason: "HTTP 403".to_owned(),
        }));
    registered(&rig);
    click(cx, "rail-status");
    assert!(bounds(cx, "status-menu-failure").is_some());
}

/// Opens the account menu and chooses `status` in it.
fn choose_in_menu(cx: &mut VisualTestContext, status: ManualStatus) {
    click(cx, "rail-status");
    click(cx, &format!("status-menu-{}", status.key()));
}

fn shown(cx: &mut VisualTestContext, shell: &Entity<Shell>) -> Reporting {
    cx.update(|_, cx| shell.read(cx).own_reporting(cx)).expect("handed in")
}

fn drop_next(rig: &Rig) {
    rig.core
        .own_answers
        .lock()
        .unwrap()
        .push_back(Ok(TransientSent::Dropped {
            reason: "HTTP 500; whether it arrived is unknown, so it isn't sent again".to_owned(),
        }));
}

#[test]
fn a_failed_manual_status_chosen_again_in_the_account_menu_is_sent_again() {
    let (rig, shell, own, cx) = shell_with_own(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    drop_next(&rig);
    choose_in_menu(cx, ManualStatus::Busy);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    // shown as not set, not as reported: the label, an empty ring for the dot, and the failure
    let not_set = Reporting::NotSet(Choice::Manual(ManualAvailability::Busy));
    assert_eq!(shown(cx, &shell), not_set);
    assert!(own_presence::reporting_label(not_set).contains("not set"));
    assert_eq!(shell::shown_as(not_set), Availability::Unknown);
    click(cx, "rail-status");
    assert!(bounds(cx, "status-menu-reported").is_some());
    assert!(bounds(cx, "status-menu-failure").is_some());
    // nothing goes out on its own, nor on a change of another setting
    wait(&rig, AVAILABLE_REFRESH);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    let model = cx.update(|_, cx| shell.read(cx).settings().clone());
    cx.update(|_, cx| {
        model.update(cx, |model, cx| {
            model.change(|settings| settings.notifications = Notifications::ALL[1], cx);
        });
    });
    cx.run_until_parked();
    assert!(asked(&rig).is_empty());

    // Busy chosen again (the setting unchanged): sent again, once
    click(cx, "status-menu-busy");
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    assert_eq!(shown(cx, &shell), Reporting::Manual(ManualAvailability::Busy));
    assert_eq!(
        shell::shown_as(Reporting::Manual(ManualAvailability::Busy)),
        Availability::Busy
    );
    assert!(
        cx.update(|_, cx| own.read(cx).failure().is_none()),
        "cleared once it went through"
    );
    // gone through: chosen again, nothing
    choose_in_menu(cx, ManualStatus::Busy);
    assert!(asked(&rig).is_empty());
}

#[test]
fn a_failed_reset_chosen_again_in_the_account_menu_is_sent_again() {
    let (rig, shell, _, cx) = shell_with_own(Settings::default());
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    choose_in_menu(cx, ManualStatus::Busy);
    assert_eq!(asked(&rig), [OwnStatus::Manual(ManualAvailability::Busy)]);
    drop_next(&rig);
    choose_in_menu(cx, ManualStatus::Automatic);
    assert_eq!(asked(&rig), [OwnStatus::ClearManual]);
    // the account may still be Busy on the server: not shown as Available
    let not_set = Reporting::NotSet(Choice::Automatic);
    assert_eq!(shown(cx, &shell), not_set);
    assert!(own_presence::reporting_label(not_set).contains("not reset"));
    assert_eq!(shell::shown_as(not_set), Availability::Unknown);
    // Automatic chosen again: the reset again, once
    choose_in_menu(cx, ManualStatus::Automatic);
    assert_eq!(asked(&rig), [OwnStatus::ClearManual]);
    assert_eq!(shown(cx, &shell), Reporting::Available);
    choose_in_menu(cx, ManualStatus::Automatic);
    assert!(asked(&rig).is_empty());
}

#[test]
fn the_window_s_use_brings_available_back_after_idle() {
    let (rig, shell, own, cx) = shell_with_own(Settings {
        idle_minutes: 1,
        ..Settings::default()
    });
    registered(&rig);
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    wait(&rig, Duration::from_secs(90));
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    assert!(asked(&rig).is_empty());
    // the mouse moved over the window
    let _ = bounds(cx, "rail");
    cx.simulate_mouse_move(point(px(300.0), px(200.0)), None, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    assert_eq!(reporting_of(&rig, &own), Reporting::Available);
    // a key typed in it, after idle again
    wait(&rig, Duration::from_secs(90));
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    assert_eq!(asked(&rig), [OwnStatus::Available]);
    // a key a binding takes (Ctrl+K opens the switcher), after idle again: a key listener never hears it
    wait(&rig, Duration::from_secs(90));
    assert_eq!(reporting_of(&rig, &own), Reporting::Inactive);
    cx.simulate_keystrokes("ctrl-k");
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| shell.read(cx).switcher().is_some()),
        "the binding took it"
    );
    assert_eq!(asked(&rig), [OwnStatus::Available]);
}

#[test]
fn the_account_s_own_dot_is_not_one_of_the_others_presence_dots() {
    let (rig, _, _, cx) = shell_with_own(Settings::default());
    registered(&rig);
    assert!(bounds(cx, "rail-status-dot").is_some());
    // the shell's own chats are empty here: any `presence-…` dot would be the account's, and there is none
    assert!(bounds(cx, "presence-available").is_none());
    assert!(bounds(cx, "presence-unknown").is_none());
}

/// An endpoint a registration of this test registered: [`register`] over a fake transport that takes every plain
/// request (a fake registers at all only under `teams-proto`'s `test-support`), the only way to get one.
fn endpoint() -> RegisteredEndpoint {
    let registration = EndpointRegistration::messaging(
        "00000000-0000-4000-8000-0000000000e1",
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/",
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let (_, endpoint) = runtime.block_on(register(&Accepting, &registration)).unwrap();
    endpoint.expect("a registration the fake took")
}

/// A transport answering every plain request 202; it is never asked anything else.
struct Accepting;

impl Transport for Accepting {
    type Error = HttpError;

    fn read<R: Read>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }

    fn send<R: ProtoRequest>(&self, _: &R) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Ok(Response::new(202, Vec::new())))
    }

    fn deliver<R: Mutating + Send>(
        &self,
        _: Authorized<R>,
    ) -> impl Future<Output = Result<Response, HttpError>> + Send {
        future::ready(Err(HttpError::NoRoute))
    }
}
