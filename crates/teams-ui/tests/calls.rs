//! Calls in the app (M3 Task 17) on GPUI's test context, over the fake engine (`common`): the chat header's Call (once
//! the calling endpoint is registered) and Join (where an invitation named a running call); the switcher's call
//! commands as they fit the moment, "Test call" among them; the incoming call's banner, ringtone and notification with
//! Answer and Decline, only for an invitation the engine offered, its caller named as the invitation names them, all
//! gone once its ring ends or the account signs out; the in-call view (who it is with, mute, hang up, the duration, the
//! call's state, how it ended, an ask refused while it is up or after, voicemail); one start at a time, its media's
//! setup included; the keys for each; the trace lines for each, never with a name; and a build that shows no call (the
//! personal build's models) showing none of it, its call keys included. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/notices.rs"]
mod notices;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use notices::{FakeServer, chat, message};
use teams_core::VideoOutput;
use teams_core::{
    CallAction, CallActionError, CallId, CallParticipant, CallStatus, CallView, End, EngineError, Event, IncomingCall,
    MessageChange, RegistrationReport, RemoteEnd, Source,
};
use teams_fence::Mri;
use teams_media::devices::pattern::TestPattern;
use teams_store::{Applied, ConversationKind, Member};
use teams_ui::call_alert::{CallAlert, notice_of};
use teams_ui::calls::CallKind;
use teams_ui::notify::NoticeKind;
use teams_ui::ringtone::{Ringer, Ringtone};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::switcher::CallCommand;
use teams_ui::video::{Tile, VideoFeed};
use teams_ui::{Calls, ViewTrace};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const ECHO: &str = "28:00000000-0000-4000-8000-0000000000ec";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
/// The caller's name as an invitation gives it.
const CALLER_NAME: &str = "Bee Caller";

/// A ringer that counts the ringtones it started and the ones stopped.
#[derive(Default)]
struct FakeRinger {
    started: AtomicUsize,
    stopped: Arc<AtomicUsize>,
}

impl FakeRinger {
    fn started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    fn stopped(&self) -> usize {
        self.stopped.load(Ordering::SeqCst)
    }
}

impl Ringer for FakeRinger {
    fn start(&self) -> Ringtone {
        self.started.fetch_add(1, Ordering::SeqCst);
        let stopped = Arc::clone(&self.stopped);
        Ringtone::new(move || {
            stopped.fetch_add(1, Ordering::SeqCst);
        })
    }
}

/// a's store: the a–b chat (b named) and the trio, each with its members.
fn seed(rig: &Rig) {
    rig.core.conversations.lock().unwrap().extend([
        chat(AB, ConversationKind::OneToOne, None),
        chat(TRIO, ConversationKind::Group, Some("Lab trio")),
    ]);
    rig.core.members.lock().unwrap().extend([
        (AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]),
        (
            TRIO.to_owned(),
            vec![
                Member::new(LAB_A, None),
                Member::new(LAB_B, None),
                Member::new(LAB_C, None),
            ],
        ),
    ]);
    rig.core.names.lock().unwrap().extend([
        (LAB_B.to_owned(), "Lab Bee".to_owned()),
        (LAB_C.to_owned(), "Lab Sea".to_owned()),
    ]);
}

/// The rig of a build that shows calls, a's store seeded, the kit set up.
fn calling() -> Rig {
    let rig = Rig::start_with(|cx| teams_ui::Models::with_calls(Some(Mri::new(ECHO)), cx));
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

/// The shell in a window of its own, a's settings opening `chat`.
fn shell_window(
    rig: &Rig,
    chat: Option<&str>,
    trace: Arc<ViewTrace>,
) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let settings = Settings {
        selected_chat: chat.map(str::to_owned),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings))),
        trace,
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    draw(cx);
    (shell, cx)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let found = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn call(number: u64) -> CallId {
    CallId::for_tests(number)
}

fn invitation(number: u64, caller_name: Option<&str>, group_chat: Option<&str>) -> IncomingCall {
    IncomingCall {
        call: call(number),
        caller: Mri::new(LAB_B),
        caller_name: caller_name.map(str::to_owned),
        group_chat: group_chat.map(str::to_owned),
    }
}

fn status(rig: &Rig, number: u64, status: CallStatus) {
    rig.send(Event::Call {
        call: call(number),
        status,
    });
}

fn registered(rig: &Rig) {
    rig.send(Event::CallingRegistration(RegistrationReport::Registered));
}

fn actions(rig: &Rig) -> Vec<CallAction> {
    rig.core.call_actions.lock().unwrap().clone()
}

fn lines(written: &Written, view: &str, change: &str) -> Vec<serde_json::Value> {
    written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == view && line["change"] == change)
        .collect()
}

fn kinds(line: &serde_json::Value) -> Vec<String> {
    line["kinds"]
        .as_array()
        .map(|kinds| {
            kinds
                .iter()
                .filter_map(|kind| kind.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn calls_of(rig: &Rig) -> Entity<Calls> {
    rig.models.calls.clone()
}

/// The incoming call's alert over the rig's calls, ringing on `ringer`, notifying on `server`.
fn alert(rig: &Rig, trace: &Arc<ViewTrace>, ringer: &Arc<FakeRinger>, server: &Arc<FakeServer>) -> Entity<CallAlert> {
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let (trace, ringer, server) = (Arc::clone(trace), Arc::clone(ringer), Arc::clone(server));
    rig.cx.update(|cx| {
        cx.new(|cx| {
            let mut alert = CallAlert::new(core, calls_of(rig), trace, Some(ringer), cx);
            alert.set_server(server, cx);
            alert
        })
    })
}

fn no_name_in(written: &Written) {
    let text = written.text();
    for name in [CALLER_NAME, "Lab Bee", "Lab Sea", "Lab trio"] {
        assert!(!text.contains(name), "{name} reached the trace");
    }
}

#[test]
fn a_chat_s_header_has_call_once_calling_is_registered_and_the_call_shows_until_it_ends() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    assert!(
        !drawn(cx, "chat-call"),
        "no Call before the calling endpoint is registered"
    );
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "none");

    registered(&rig);
    assert!(drawn(cx, "chat-call"));
    assert!(!drawn(cx, "chat-join"));
    click(cx, "chat-call");
    assert_eq!(actions(&rig), [CallAction::Call { chat: AB.into() }]);
    let asked = lines(&written, "call", "asked");
    assert_eq!(
        (asked[0]["conversation"].as_str(), kinds(&asked[0])),
        (Some(AB), vec!["call".to_owned()])
    );

    the_call_shows_as_it_goes(&rig, &shell, cx, &written);
    hang_up_and_dismiss(&rig, cx, &written);
    no_name_in(&written);
}

/// The engine tells the call as it goes: the in-call bar shows it, with who it is with; the header's Call is gone.
fn the_call_shows_as_it_goes(rig: &Rig, shell: &Entity<Shell>, cx: &mut VisualTestContext, written: &Written) {
    status(rig, 1, CallStatus::Placing);
    assert!(drawn(cx, "in-call"));
    assert!(!drawn(cx, "chat-call"), "one call at a time");
    status(rig, 1, CallStatus::Ringing);
    status(rig, 1, CallStatus::Connecting);
    status(rig, 1, CallStatus::Connected);
    let bar = cx.update(|_, cx| shell.read(cx).call_bar().clone());
    let participants = cx.update(|_, cx| bar.read(cx).participants().to_vec());
    assert_eq!(
        participants,
        [(LAB_B.to_owned(), "Lab Bee".into())],
        "who it is with, by name"
    );
    let active = cx.read(|cx| calls_of(rig).read(cx).call().cloned()).expect("up");
    assert!(
        active.connected_at.is_some(),
        "the duration counts from the media's connect"
    );
    let shown: Vec<Vec<String>> = lines(written, "in-call", "shown").iter().map(kinds).collect();
    for state in ["placing", "ringing", "connecting", "connected"] {
        assert!(
            shown
                .iter()
                .any(|kinds| kinds == &["chat", state, "unmuted", "camera-off"]),
            "{state}: {shown:?}"
        );
    }
    let connected = lines(written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(connected["call"], 1);
    assert_eq!(connected["conversation"], AB);
    assert_eq!(connected["people"], serde_json::json!([LAB_B]));
}

/// Hang up: the engine's hang-up; its end shown until dismissed, Call back in the header.
fn hang_up_and_dismiss(rig: &Rig, cx: &mut VisualTestContext, written: &Written) {
    click(cx, "call-hang-up");
    assert_eq!(actions(rig)[1], CallAction::HangUp { call: call(1) });
    status(rig, 1, CallStatus::Leaving);
    status(rig, 1, CallStatus::Ended(End::HungUp));
    assert!(!drawn(cx, "in-call"));
    assert!(drawn(cx, "call-after"));
    assert!(drawn(cx, "chat-call"));
    let ended = lines(written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&ended), ["chat", "ended", "hung-up", "unmuted", "camera-off"]);
    click(cx, "call-dismiss");
    assert!(!drawn(cx, "call-after"));
    assert_eq!(lines(written, "in-call", "hidden").len(), 1);
    let buttons: Vec<Vec<String>> = lines(written, "call-button", "shown").iter().map(kinds).collect();
    assert_eq!(buttons, [["call"], ["none"], ["call"]]);
}

#[test]
fn join_shows_where_an_invitation_named_a_running_call_and_is_forgotten_once_refused() {
    let rig = calling();
    let (trace, written) = trace();
    let (shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "call");

    // a group call's invitation rings and runs out: the call runs on, to be joined
    rig.send(Event::IncomingCall(invitation(4, Some(CALLER_NAME), Some(TRIO))));
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).call_button(cx)),
        "none",
        "while it rings"
    );
    rig.send(Event::RingEnded {
        call: call(4),
        end: RemoteEnd::default(),
    });
    assert!(drawn(cx, "chat-join"));
    assert!(!drawn(cx, "chat-call"));
    click(cx, "chat-join");
    assert_eq!(actions(&rig), [CallAction::Join { chat: TRIO.into() }]);
    assert!(!drawn(cx, "chat-join"), "no second join while the first one goes");
    status(&rig, 1, CallStatus::Joining);
    let joined = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((joined.kind, joined.chat.as_deref()), (CallKind::Join, Some(TRIO)));
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));

    // the engine knows no running call there any more: refused, said, and Join gone
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Call(
            teams_core::CallError::NotJoinable,
        ))));
    cx.simulate_keystrokes("ctrl-shift-j");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 2, "the key asks the same");
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(&refused[0]), ["join", "not-joinable"]);
    assert!(drawn(cx, "call-after"), "why, shown");
    assert!(drawn(cx, "chat-call"), "Call again");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Chat(TRIO.into())
    );
    no_name_in(&written);
}

#[test]
fn the_incoming_call_rings_shows_its_banner_and_notification_and_is_answered_from_either() {
    let rig = calling();
    let (trace, written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let alert = alert(&rig, &trace, &ringer, &server);
    let (_shell, cx) = shell_window(&rig, None, trace);
    registered(&rig);
    let rang = Rang {
        rig: &rig,
        ringer: &ringer,
        server: &server,
        written: &written,
    };
    rang.answered_from_the_notification(&alert, cx);
    rang.declined_from_the_banner(cx);
    rang.ended_by_itself(cx);
    no_name_in(&written);
}

/// An incoming call's test: the rig, the fake ringer and server, the trace.
struct Rang<'a> {
    rig: &'a Rig,
    ringer: &'a FakeRinger,
    server: &'a FakeServer,
    written: &'a Written,
}

impl Rang<'_> {
    /// It rings with its banner, ringtone and notification; Answer on the notification answers it.
    fn answered_from_the_notification(&self, alert: &Entity<CallAlert>, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        rig.send(Event::IncomingCall(invitation(7, Some(CALLER_NAME), None)));
        assert!(drawn(cx, "call-banner"));
        assert!(drawn(cx, "call-answer") && drawn(cx, "call-decline"));
        assert_eq!((ringer.started(), ringer.stopped()), (1, 0), "the ringtone plays");
        let shown = server.shown();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, NoticeKind::Call);
        assert_eq!(
            shown[0].summary, "Bee Caller is calling",
            "the caller's name as the invitation gives it"
        );
        let banner = lines(written, "call-banner", "shown");
        assert_eq!(banner[0]["call"], 7);
        assert_eq!(kinds(&banner[0]), ["one-to-one", "named"]);
        assert_eq!(banner[0]["people"], serde_json::json!([LAB_B]));
        self.answer_on_the_notification(alert, cx);
    }

    /// Answer on the notification: the engine's answer of that invitation; the ring stops once it is told.
    fn answer_on_the_notification(&self, alert: &Entity<CallAlert>, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        let answered = rig
            .cx
            .update(|cx| alert.update(cx, |alert, cx| alert.activated(1, "answer", cx)));
        assert!(answered);
        rig.cx.run_until_parked();
        assert_eq!(actions(rig), [CallAction::Answer { call: call(7) }]);
        status(rig, 7, CallStatus::Answering);
        assert_eq!(ringer.stopped(), 1);
        assert_eq!(server.closed(), [1]);
        assert!(!drawn(cx, "call-banner"));
        assert!(drawn(cx, "in-call"));
        let active = rig.cx.read(|cx| calls_of(rig).read(cx).call().cloned()).unwrap();
        assert_eq!(active.caller_name.as_deref(), Some(CALLER_NAME));
        status(rig, 7, CallStatus::Connected);
        for (view, change) in [
            ("ringtone", "started"),
            ("ringtone", "stopped"),
            ("call-notification", "shown"),
            ("call-notification", "answered"),
            ("call-notification", "closed"),
            ("call-banner", "hidden"),
        ] {
            assert_eq!(lines(written, view, change).len(), 1, "{view} {change}");
        }
        let up = lines(written, "in-call", "shown").last().cloned().unwrap();
        assert_eq!(kinds(&up), ["incoming", "connected", "unmuted", "camera-off"]);
        status(rig, 7, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    }

    /// The next one: Decline on the banner declines it.
    fn declined_from_the_banner(&self, cx: &mut VisualTestContext) {
        let Self {
            rig,
            ringer,
            server,
            written,
        } = *self;
        rig.send(Event::IncomingCall(invitation(8, None, None)));
        assert_eq!(server.shown()[1].summary, "Someone is calling", "no name given");
        click(cx, "call-decline");
        assert_eq!(actions(rig)[1], CallAction::Decline { call: call(8) });
        status(rig, 8, CallStatus::Declining);
        status(rig, 8, CallStatus::Ended(End::Declined));
        assert_eq!((ringer.started(), ringer.stopped()), (2, 2));
        let declined = lines(written, "in-call", "shown").last().cloned().unwrap();
        assert_eq!(
            kinds(&declined),
            ["incoming", "ended", "declined", "unmuted", "camera-off"]
        );
    }

    /// One whose ring ends before anyone acts: all of it gone, nothing asked.
    fn ended_by_itself(&self, cx: &mut VisualTestContext) {
        let Self {
            rig, ringer, server, ..
        } = *self;
        rig.send(Event::IncomingCall(invitation(9, Some(CALLER_NAME), Some(TRIO))));
        assert_eq!(server.shown()[2].body, "Incoming group call");
        rig.send(Event::RingEnded {
            call: call(9),
            end: RemoteEnd::default(),
        });
        assert_eq!((ringer.started(), ringer.stopped()), (3, 3));
        assert!(server.closed().contains(&3));
        assert!(!drawn(cx, "call-banner"));
        assert_eq!(actions(rig).len(), 2, "nothing is answered or declined by itself");
    }
}

#[test]
fn nothing_rings_but_an_invitation_the_engine_offered() {
    let rig = calling();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &Arc::new(ViewTrace::off()), &ringer, &server);
    registered(&rig);
    // a ring's end, a call's status and a voicemail of calls the engine never offered here ring nothing
    rig.send(Event::RingEnded {
        call: call(3),
        end: RemoteEnd::default(),
    });
    status(&rig, 3, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    rig.send(Event::Voicemail { call: call(5) });
    // nor does a message
    rig.send(Event::Message {
        conversation: AB.to_owned(),
        id: "6000".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message(AB, "6000", LAB_B, "<p>call me</p>", 6_000))),
        source: Source::Delivery,
    });
    assert_eq!(ringer.started(), 0);
    assert!(server.shown().is_empty());
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).ringing().is_none()));
    assert!(actions(&rig).is_empty());
    assert!(!rig.core.asked().contains(&"call"), "nothing asked of the engine");
    assert_eq!(rig.shutdowns(), 0);
}

#[test]
fn the_keys_call_answer_decline_mute_and_hang_up() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::Call { chat: AB.into() }]);
    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[1],
        CallAction::Mute {
            call: call(1),
            muted: true
        }
    );
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().muted));
    let muted = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&muted), ["chat", "connected", "muted", "camera-off"]);
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[2],
        CallAction::Mute {
            call: call(1),
            muted: false
        }
    );
    cx.simulate_keystrokes("ctrl-shift-h");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[3], CallAction::HangUp { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    rig.send(Event::IncomingCall(invitation(2, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-shift-s");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[4], CallAction::Answer { call: call(2) });
    status(&rig, 2, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    rig.send(Event::IncomingCall(invitation(3, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-shift-d");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[5], CallAction::Decline { call: call(3) });

    // a key that fits nothing asks nothing of the engine: refused here, said
    rig.send(Event::RingEnded {
        call: call(3),
        end: RemoteEnd::default(),
    });
    cx.simulate_keystrokes("ctrl-shift-h");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 6);
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["hang-up", "no-call"]);
}

#[test]
fn the_switcher_offers_the_call_commands_that_fit_the_moment() {
    let rig = calling();
    let (trace, _written) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    let commands = |cx: &mut VisualTestContext| {
        let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
        cx.update(|_, cx| switcher.read(cx).commands())
    };
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Call", "Test call"]);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("test call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Test call"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig), [CallAction::TestCall { echo: Mri::new(ECHO) }]);
    assert!(cx.update(|_, cx| shell.read(cx).switcher().is_none()), "closed");

    // while it is up: Hang up and Mute
    status(&rig, 1, CallStatus::Connected);
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("hang");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Hang up"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[1], CallAction::HangUp { call: call(1) });
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    // while one rings: Answer and Decline
    rig.send(Event::IncomingCall(invitation(2, Some(CALLER_NAME), None)));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Answer call", "Decline call"]);
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("answer");
    cx.run_until_parked();
    assert_eq!(commands(cx), ["Answer call"]);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(actions(&rig)[2], CallAction::Answer { call: call(2) });
}

#[test]
fn the_in_call_view_tells_a_refusal_a_failure_and_voicemail() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::Media("no relay".to_owned()))));
    click(cx, "chat-call");
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(&refused[0]), ["call", "media"]);
    let told = rig.cx.read(|cx| calls_of(&rig).read(cx).refused().cloned()).unwrap();
    assert!(told.text.contains("no relay"), "{told:?}");
    assert!(drawn(cx, "call-after"));

    // the next: the callee's voicemail takes it (declined there), and a's call says so
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Ringing);
    assert!(!drawn(cx, "call-after"), "a new call clears what the last left");
    status(&rig, 1, CallStatus::Connecting);
    rig.send(Event::Voicemail { call: call(1) });
    let voicemail = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(
        kinds(&voicemail),
        ["chat", "connecting", "unmuted", "camera-off", "voicemail"]
    );
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!(
        teams_ui::calls::status_text(&active),
        "Declined or not answered: voicemail"
    );
    status(&rig, 1, CallStatus::Ended(End::HungUp));

    // the next fails on its media
    click(cx, "chat-call");
    status(&rig, 2, CallStatus::Ringing);
    status(&rig, 2, CallStatus::Ended(End::Failed("media")));
    let failed = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(
        kinds(&failed),
        ["chat", "ended", "failed", "media", "unmuted", "camera-off"]
    );
    let ended = rig.cx.read(|cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!(
        teams_ui::calls::end_text(&ended.end, ended.voicemail),
        "Call failed (media)"
    );
}

#[test]
fn a_build_that_shows_no_call_shows_rings_and_offers_none_of_it() {
    // the personal build's models, until its call rule is built
    let rig = Rig::start();
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    let (trace, written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    rig.send(Event::IncomingCall(invitation(1, Some(CALLER_NAME), None)));
    assert!(!drawn(cx, "chat-call") && !drawn(cx, "call-banner") && !drawn(cx, "in-call"));
    assert_eq!(cx.update(|_, cx| shell.read(cx).call_button(cx)), "none");
    assert_eq!(ringer.started(), 0);
    assert!(server.shown().is_empty());
    for key in [
        "ctrl-shift-c",
        "ctrl-shift-j",
        "ctrl-shift-s",
        "ctrl-shift-d",
        "ctrl-shift-h",
        "ctrl-shift-m",
    ] {
        cx.simulate_keystrokes(key);
    }
    cx.run_until_parked();
    assert!(actions(&rig).is_empty(), "nothing asked of the engine");
    assert!(!drawn(cx, "call-after"), "no refusal shown either");
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).refused().is_none()));
    cx.simulate_keystrokes("ctrl-k");
    cx.simulate_input("call");
    cx.run_until_parked();
    let switcher = cx.update(|_, cx| shell.read(cx).switcher().cloned()).expect("open");
    assert!(cx.update(|_, cx| switcher.read(cx).commands()).is_empty());
    let traced = written.text();
    for view in [
        "call",
        "call-button",
        "call-banner",
        "in-call",
        "ringtone",
        "call-notification",
    ] {
        assert!(!traced.contains(&format!("\"view\":\"{view}\"")), "{view}: {traced}");
    }
}

#[test]
fn a_start_asked_while_one_sets_up_is_refused_and_the_first_keeps_its_chat() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    // the engine answers a start once its media is set up
    *rig.core.hold_calls.lock().unwrap() = true;
    click(cx, "chat-call");
    assert!(!drawn(cx, "chat-call"), "no Call while the first one sets up");
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig),
        [CallAction::Call { chat: AB.into() }],
        "the second waits"
    );
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["call", "busy"]);

    // the first's media is set up and its call told: still the call of its chat
    let held = std::mem::take(&mut *rig.core.held_calls.lock().unwrap());
    assert_eq!(held.len(), 1);
    for answer in held {
        answer.send(Ok(call(1)));
    }
    rig.cx.run_until_parked();
    status(&rig, 1, CallStatus::Placing);
    let active = rig.cx.read(|cx| calls_of(&rig).read(cx).call().cloned()).unwrap();
    assert_eq!((active.kind, active.chat.as_deref()), (CallKind::Chat, Some(AB)));
    let shown = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(shown["conversation"], AB);
}

/// The account signs out (its tokens can't be renewed): the engine is shut down, so no call event comes any more.
fn sign_out(rig: &Rig) {
    rig.cx.update(|cx| {
        rig.bridge
            .update(cx, |bridge, cx| bridge.sign_out("signed out".into(), cx))
            .detach();
    });
    rig.cx.run_until_parked();
}

#[test]
fn a_sign_out_stops_the_ring() {
    let rig = calling();
    let (trace, _written) = trace();
    let ringer = Arc::new(FakeRinger::default());
    let server = Arc::new(FakeServer::default());
    let _alert = alert(&rig, &trace, &ringer, &server);
    let (_shell, cx) = shell_window(&rig, None, trace);
    registered(&rig);
    rig.send(Event::IncomingCall(invitation(7, Some(CALLER_NAME), None)));
    assert_eq!((ringer.started(), ringer.stopped()), (1, 0));
    assert_eq!(server.shown().len(), 1);
    sign_out(&rig);
    assert_eq!(ringer.stopped(), 1, "the ringtone stops");
    assert_eq!(server.closed(), [1], "the notification closes");
    assert!(!drawn(cx, "call-banner"));
    assert!(actions(&rig).is_empty(), "nothing answered or declined");
}

#[test]
fn a_sign_out_shows_the_call_up_ended() {
    let rig = calling();
    let (trace, _written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    sign_out(&rig);
    assert!(!drawn(cx, "in-call"));
    assert!(drawn(cx, "call-after"));
    let ended = rig.cx.read(|cx| calls_of(&rig).read(cx).ended().cloned()).unwrap();
    assert_eq!((ended.id, ended.end), (call(1), End::HungUp));
}

#[test]
fn a_refusal_during_a_call_shows_in_its_bar_and_its_end_replaces_it() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    click(cx, "chat-call");
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "in-call-refused"));
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::NoMedia)));
    cx.simulate_keystrokes("ctrl-shift-m");
    cx.run_until_parked();
    assert!(
        drawn(cx, "in-call") && drawn(cx, "in-call-refused"),
        "told while the call is up"
    );
    cx.simulate_keystrokes("ctrl-shift-j");
    cx.run_until_parked();
    let refused = lines(&written, "call", "refused");
    assert_eq!(kinds(refused.last().unwrap()), ["join", "busy"]);
    assert!(drawn(cx, "in-call-refused"));

    // once it ends, how it ended is what is told
    status(&rig, 1, CallStatus::Ended(End::Declined));
    assert!(drawn(cx, "call-after"));
    let calls = rig.cx.read(|cx| {
        let calls = calls_of(&rig).read(cx);
        (calls.refused().cloned(), calls.ended().map(|ended| ended.end.clone()))
    });
    assert_eq!(calls, (None, Some(End::Declined)));
}

// --- video (M3 Task 18) ---

/// The rig of a build that shows calls with their video, on `feed`.
fn video_calling(feed: &VideoFeed) -> Rig {
    let feed = feed.clone();
    let rig = Rig::start_with(move |cx| teams_ui::Models::with_video_calls(Some(Mri::new(ECHO)), feed, cx));
    seed(&rig);
    rig.cx.update(teams_ui::window::init);
    rig
}

#[test]
fn the_camera_is_off_when_a_call_starts_and_goes_on_and_off_by_its_key_its_button_and_the_switcher() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(drawn(cx, "call-camera"), "the camera's button");
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let up = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&up), ["chat", "connected", "unmuted", "camera-off"]);

    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert_eq!(
        actions(&rig)[1],
        CallAction::Camera {
            call: call(1),
            on: true
        }
    );
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let on = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&on), ["chat", "connected", "unmuted", "camera-on"]);
    click(cx, "call-camera");
    assert_eq!(
        actions(&rig)[2],
        CallAction::Camera {
            call: call(1),
            on: false
        }
    );
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let offered = rig
        .cx
        .read(|cx| CallCommand::fitting(calls_of(&rig).read(cx), Some(AB)));
    assert_eq!(
        offered,
        [CallCommand::HangUp, CallCommand::ToggleMute, CallCommand::ToggleCamera]
    );

    // a camera that went off by itself (its device gone, its encoder failed) is shown off
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert!(rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    rig.send(Event::CameraOff { call: call(1) });
    cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    let off = lines(&written, "in-call", "shown").last().cloned().unwrap();
    assert_eq!(kinds(&off), ["chat", "connected", "unmuted", "camera-off"]);

    // the engine's refusal (a call without video) is told, and the camera stays as it was
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::NoVideo)));
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert!(!rig.cx.read(|cx| calls_of(&rig).read(cx).call().unwrap().camera));
    assert_eq!(
        kinds(lines(&written, "call", "refused").last().unwrap()),
        ["camera", "no-video"]
    );
    no_name_in(&written);
}

#[test]
fn the_feed_s_frames_draw_as_tiles_while_the_call_is_up_and_go_with_it() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "video-tiles"), "no frame yet");

    // the call's media shows the far end's frame and the camera's on the feed (I420, the test pattern)
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    output.remote(0).show(&pattern.frame(1)).unwrap();
    output.preview().unwrap().show(&pattern.frame(2)).unwrap();
    assert!(drawn(cx, "video-tile-remote") && drawn(cx, "video-tile-self"));
    let shown: Vec<Vec<String>> = lines(&written, "video-tile", "shown").iter().map(kinds).collect();
    assert_eq!(shown, [["remote"], ["self"]]);
    let remote = feed.picture(Tile::REMOTE).unwrap();
    assert_eq!(
        (remote.width, remote.height, remote.bgra.len()),
        (320, 180, 320 * 180 * 4)
    );

    // a frame not in I420 isn't taken
    let mut wrong = pattern.frame(3);
    wrong.format = teams_media::devices::PixelFormat::Rgb;
    assert!(output.remote(0).show(&wrong).is_err());

    status(&rig, 1, CallStatus::Ended(End::HungUp));
    assert!(!drawn(cx, "video-tiles"));
    let hidden: Vec<Vec<String>> = lines(&written, "video-tile", "hidden").iter().map(kinds).collect();
    assert_eq!(hidden, [["remote"], ["self"]]);
    assert!(feed.picture(Tile::REMOTE).is_none(), "the call's frames go with it");
}

/// The view of a group call: `who`, each `(MRI, roster name, camera on, line, speaking)`, and whether the account
/// speaks.
fn view(who: &[(&str, &str, bool, Option<usize>, bool)], speaking: bool) -> CallView {
    CallView {
        participants: who
            .iter()
            .map(|(mri, name, video_on, line, speaking)| CallParticipant {
                mri: (*mri).to_owned(),
                name: Some((*name).to_owned()),
                video_on: *video_on,
                line: *line,
                speaking: *speaking,
            })
            .collect(),
        speaking,
    }
}

#[test]
fn a_group_call_s_grid_shows_each_participant_its_video_and_who_speaks() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "call-grid"), "no roster told yet");

    // the roster tells b (camera on, line 1) and c (camera off, line 2, speaking): a cell each and the account's own
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(
            &[
                (LAB_B, "Lab Bee", true, Some(1), false),
                (LAB_C, "Lab Sea", false, Some(2), true),
            ],
            false,
        ),
    });
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    output.remote(1).show(&pattern.frame(1)).unwrap();
    cx.run_until_parked();
    for cell in [
        "call-grid",
        "call-participant-0",
        "call-participant-1",
        "call-participant-self",
    ] {
        assert!(drawn(cx, cell), "{cell}");
    }
    assert!(drawn(cx, "video-tile-line-1"), "b's video on its line");
    assert!(!drawn(cx, "video-tile-line-2"), "no frame of c's yet");
    let cells = lines(&written, "call-participant", "shown");
    assert_eq!(
        cells.iter().map(kinds).collect::<Vec<_>>(),
        [["camera-on", "line-1", "quiet"], ["camera-off", "line-2", "speaking"]]
    );
    assert_eq!(cells[0]["people"], serde_json::json!([LAB_B]));
    assert_eq!(
        kinds(lines(&written, "call-participant", "self").last().unwrap()),
        ["quiet"]
    );

    // c leaves and the account speaks: the grid follows
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&[(LAB_B, "Lab Bee", true, Some(1), false)], true),
    });
    cx.run_until_parked();
    draw(cx);
    assert!(!drawn(cx, "call-participant-1"));
    assert_eq!(
        kinds(lines(&written, "call-participant", "self").last().unwrap()),
        ["speaking"]
    );
    no_name_in(&written);
}

#[test]
fn nine_participants_each_draw_on_a_line_of_their_own() {
    let feed = VideoFeed::new();
    let rig = video_calling(&feed);
    let (trace, _written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(TRIO), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let mris: Vec<String> = (1..=9)
        .map(|n| format!("8:orgid:00000000-0000-4000-8000-0000000001{n:02}"))
        .collect();
    let who: Vec<(&str, &str, bool, Option<usize>, bool)> = mris
        .iter()
        .zip(1..)
        .map(|(mri, line)| (mri.as_str(), "Someone", true, Some(line), false))
        .collect();
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&who, false),
    });
    let pattern = TestPattern::new(320, 180);
    let output: &dyn VideoOutput = &feed;
    for line in 1..=9 {
        output.remote(line).show(&pattern.frame(1)).unwrap();
    }
    // a line past the ninth has no tile: its frames are dropped
    output.remote(10).show(&pattern.frame(1)).unwrap();
    cx.run_until_parked();
    for line in 1..=9 {
        assert!(drawn(cx, &format!("video-tile-line-{line}")), "line {line}");
    }
    assert!(feed.picture(Tile::Line(10)).is_none());
}

#[test]
fn a_build_without_the_codec_has_no_camera_and_its_key_asks_nothing() {
    let rig = calling();
    let (trace, written) = trace();
    let (_shell, cx) = shell_window(&rig, Some(AB), trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    assert!(!drawn(cx, "call-camera"));
    cx.simulate_keystrokes("ctrl-shift-o");
    cx.run_until_parked();
    assert_eq!(actions(&rig).len(), 1, "only the call");
    assert_eq!(
        kinds(lines(&written, "call", "refused").last().unwrap()),
        ["camera", "no-video"]
    );
    let offered = rig
        .cx
        .read(|cx| CallCommand::fitting(calls_of(&rig).read(cx), Some(AB)));
    assert_eq!(offered, [CallCommand::HangUp, CallCommand::ToggleMute]);
}

#[test]
fn a_notification_says_who_calls_as_the_invitation_names_them() {
    let one = notice_of(&invitation(1, Some(CALLER_NAME), None));
    assert_eq!(
        (
            one.kind,
            one.summary.as_str(),
            one.body.as_str(),
            one.conversation.as_str()
        ),
        (NoticeKind::Call, "Bee Caller is calling", "Incoming call", "")
    );
    let group = notice_of(&invitation(2, None, Some(TRIO)));
    assert_eq!(
        (group.summary.as_str(), group.body.as_str(), group.conversation.as_str()),
        ("Someone is calling", "Incoming group call", TRIO)
    );
}
