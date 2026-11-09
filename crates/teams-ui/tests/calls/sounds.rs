//! A call's sounds ([`CallSounds`], [`CallAlert`]): each state of a call makes its sound once, the ringback plays while
//! the call rings, and a ringtone the settings keep quiet isn't started.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Entity};
use teams_core::{CallStatus, End, RemoteEnd};
use teams_ui::call_sounds::CallSounds;
use teams_ui::ringtone::Ringtone;
use teams_ui::settings::attach_sounds;
use teams_ui::sounds::{LARGE_CALL, SoundEvent, SoundPlayer};
use teams_ui::{Settings, SettingsModel};

use super::{
    AB, LAB_B, Rig, ViewTrace, call, calling, calls_of, click, invitation, registered, shell_window, status, view,
};

/// A player that keeps what it was told.
#[derive(Default)]
struct Told {
    played: Mutex<Vec<SoundEvent>>,
    loops: Mutex<usize>,
    stopped: Arc<Mutex<usize>>,
}

impl Told {
    fn played(&self) -> Vec<SoundEvent> {
        std::mem::take(&mut self.played.lock().expect("not poisoned"))
    }
}

impl SoundPlayer for Told {
    fn apply(&self, _: &Settings, _: Option<PathBuf>) {}

    fn play(&self, event: SoundEvent, _: &str, _: bool) {
        self.played.lock().expect("not poisoned").push(event);
    }

    fn looped(&self, _: SoundEvent) -> Ringtone {
        *self.loops.lock().expect("not poisoned") += 1;
        let stopped = Arc::clone(&self.stopped);
        Ringtone::new(move || *stopped.lock().expect("not poisoned") += 1)
    }

    fn preview(&self, _: SoundEvent) {}

    fn stop_preview(&self) {}

    fn previewing(&self) -> Option<SoundEvent> {
        None
    }

    fn watch_previewing(&self, _: Box<dyn Fn() + Send + Sync>) {}
}

const ME: &str = "8:orgid:me";

fn sounds(rig: &Rig) -> (Entity<CallSounds>, Arc<Told>) {
    let told = Arc::new(Told::default());
    let player = Arc::clone(&told);
    let calls = calls_of(rig);
    let entity = rig.cx.update(|cx| {
        let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
        attach_sounds(&settings, player, cx);
        cx.new(|cx| CallSounds::new(ME.to_owned(), calls, settings, cx))
    });
    (entity, told)
}

/// A call of the account's asked for and placed, the rig's call number 1.
fn placed(rig: &Rig) -> &'static mut gpui::VisualTestContext {
    let (_shell, cx) = shell_window(rig, Some(AB), Arc::new(ViewTrace::off()));
    registered(rig);
    click(cx, "chat-call");
    status(rig, 1, CallStatus::Placing);
    cx
}

#[test]
fn a_call_rings_back_connects_and_ends_with_its_sounds() {
    let rig = calling();
    let (entity, told) = sounds(&rig);
    placed(&rig);
    status(&rig, 1, CallStatus::Ringing);
    rig.cx.run_until_parked();
    assert!(rig.cx.read(|cx| entity.read(cx).ringing_back()));
    assert_eq!(*told.loops.lock().expect("not poisoned"), 1);
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert!(!rig.cx.read(|cx| entity.read(cx).ringing_back()));
    assert_eq!(*told.stopped.lock().expect("not poisoned"), 1);
    assert_eq!(told.played(), [SoundEvent::CallJoined]);
    status(&rig, 1, CallStatus::Reconnecting);
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    assert_eq!(told.played(), [SoundEvent::Reconnecting, SoundEvent::Reconnected]);
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    rig.cx.run_until_parked();
    assert_eq!(told.played(), [SoundEvent::CallEnded]);
}

#[test]
fn a_call_that_never_connected_makes_no_end_sound() {
    let rig = calling();
    let (_entity, told) = sounds(&rig);
    placed(&rig);
    status(&rig, 1, CallStatus::Ended(End::HungUp));
    rig.cx.run_until_parked();
    assert_eq!(told.played(), []);
}

#[test]
fn people_coming_and_going_sound_but_not_the_first_roster() {
    let rig = calling();
    let (_entity, told) = sounds(&rig);
    placed(&rig);
    status(&rig, 1, CallStatus::Connected);
    let roster = |who: &[(&str, &str, bool, Option<usize>, bool)]| {
        rig.send(teams_core::Event::CallParticipants {
            call: call(1),
            view: view(who, false),
        });
        rig.cx.run_until_parked();
    };
    rig.cx.run_until_parked();
    told.played();
    roster(&[("8:a", "A", false, None, false)]);
    assert_eq!(told.played(), [], "the roster first told");
    roster(&[("8:a", "A", false, None, false), ("8:b", "B", false, None, false)]);
    assert_eq!(told.played(), [SoundEvent::PersonJoined]);
    roster(&[("8:b", "B", false, None, false)]);
    assert_eq!(told.played(), [SoundEvent::PersonLeft]);
    let many: Vec<String> = (0..=LARGE_CALL).map(|index| format!("8:p{index}")).collect();
    let crowd: Vec<(&str, &str, bool, Option<usize>, bool)> =
        many.iter().map(|mri| (mri.as_str(), "P", false, None, false)).collect();
    roster(&crowd);
    assert_eq!(told.played(), [], "a large call stays quiet");
}

#[test]
fn someone_waiting_makes_its_sound_once() {
    let rig = calling();
    let (_entity, told) = sounds(&rig);
    placed(&rig);
    status(&rig, 1, CallStatus::Connected);
    rig.cx.run_until_parked();
    told.played();
    for _ in 0..2 {
        rig.send(teams_core::Event::Lobby {
            call: call(1),
            waiting: vec![teams_core::LobbyPerson {
                mri: teams_proto::Mri::new(LAB_B),
                name: None,
            }],
        });
        rig.cx.run_until_parked();
    }
    assert_eq!(told.played(), [SoundEvent::LobbyRequest]);
}

#[test]
fn an_invitation_that_rings_out_is_missed_unless_the_account_ended_it() {
    let rig = calling();
    let (_entity, told) = sounds(&rig);
    registered(&rig);
    rig.send(teams_core::Event::IncomingCall(invitation(2, None, None)));
    rig.send(teams_core::Event::RingEnded {
        call: call(2),
        end: RemoteEnd::default(),
    });
    rig.cx.run_until_parked();
    assert_eq!(told.played(), [SoundEvent::MissedCall]);
    rig.send(teams_core::Event::IncomingCall(invitation(3, None, None)));
    rig.send(teams_core::Event::RingEnded {
        call: call(3),
        end: RemoteEnd {
            by: Some(teams_proto::Mri::new(ME)),
            ..RemoteEnd::default()
        },
    });
    rig.cx.run_until_parked();
    assert_eq!(told.played(), [], "taken on another device");
}
