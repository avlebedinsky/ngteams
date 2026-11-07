//! The signed-out account's placeholder refuses chat, media, presence and call operations without a running engine;
//! the bridge's tap sees each engine event just before the models apply it.

#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{LAB_B, Rig};
use futures_util::FutureExt as _;
use teams_core::{Action, Availability, CallAction, CallId, EngineError, Event, OwnStatus, Reply, Transient};
use teams_ui::{Core as _, StoppedCore, Tap};

fn stopped<T>(reply: Reply<T>) {
    assert!(matches!(reply.now_or_never(), Some(Err(EngineError::Stopped))));
}

#[test]
fn a_signed_out_account_rejects_chat_presence_media_and_call_operations() {
    let me = "8:orgid:00000000-0000-4000-8000-00000000000a";
    let person = "8:orgid:00000000-0000-4000-8000-00000000000b";
    let core = StoppedCore::new(me);
    assert_eq!(core.me(), me);
    let chat = "19:synthetic@thread.v2";
    stopped(core.conversations(10));
    stopped(core.messages(chat.into(), None, 10));
    stopped(core.message(chat.into(), "message".into()));
    stopped(core.members(chat.into()));
    stopped(core.reactions(chat.into(), "message".into()));
    stopped(core.unread_count(chat.into()));
    stopped(core.person(person.into()));
    stopped(core.presence(person.into()));
    stopped(core.reaction_animation("like".into()));
    stopped(core.message_picture("synthetic-image".into()));
    stopped(core.file_thumbnail("synthetic-file".into()));
    stopped(core.file_stream("synthetic-file".into(), 10, 100));
    stopped(core.file_size("synthetic-file".into()));
    stopped(core.recent_reactions(10));
    stopped(core.frequent_reactions(10));
    stopped(core.read_horizon(chat.into()));
    stopped(core.history_complete(chat.into()));
    stopped(core.outbox_entry(1));
    stopped(core.outbox_unsettled(chat.into()));
    stopped(core.left_by_earlier_runs());
    stopped(core.search("synthetic".into(), vec![chat.into()], 10));
    stopped(core.teams());
    stopped(core.chat_listing());
    stopped(core.sync_conversation(chat.into()));
    stopped(core.reply_roots(chat.into()));
    stopped(core.calendar(0, 1));
    stopped(core.sync());
    stopped(core.load_older(chat.into()));
    stopped(core.write(Action::Delete {
        conversation: chat.into(),
        message_id: "message".into(),
    }));
    stopped(core.resend(1));
    stopped(core.transient(Transient::Typing {
        conversation: chat.into(),
    }));
    stopped(core.own_availability(OwnStatus::Available));
    stopped(core.call(CallAction::Mute {
        call: CallId::for_tests(1),
        muted: true,
    }));
}

fn presence(availability: Availability) -> Event {
    Event::Presence {
        mri: LAB_B.to_owned(),
        availability,
    }
}

/// What a tap saw of each presence event: its availability, and the one the models showed then.
type Seen = Rc<RefCell<Vec<(Availability, Availability)>>>;

fn recording(seen: &Seen, rig: &Rig) -> Tap {
    let (seen, presences) = (Rc::clone(seen), rig.models.presence.clone());
    Rc::new(move |event, cx| {
        if let Event::Presence { availability, .. } = event {
            let shown = presences.read(cx).of(LAB_B);
            seen.borrow_mut().push((availability.clone(), shown));
        }
    })
}

fn shown(rig: &Rig) -> Availability {
    rig.cx.read(|cx| rig.models.presence.read(cx).of(LAB_B))
}

#[test]
fn the_tap_sees_each_event_before_the_models_apply_it_until_it_is_taken_off() {
    let rig = Rig::start();
    // no tap: the models apply it all the same
    rig.send(presence(Availability::Busy));
    assert_eq!(shown(&rig), Availability::Busy);

    let seen = Seen::default();
    let tap = recording(&seen, &rig);
    rig.cx.read(|cx| rig.bridge.read(cx).set_tap(Some(tap)));
    rig.send(presence(Availability::Away));
    assert_eq!(
        *seen.borrow(),
        vec![(Availability::Away, Availability::Busy)],
        "the tap saw it while the models still showed the one before"
    );
    assert_eq!(shown(&rig), Availability::Away);

    // what a test tells the bridge goes the drain's way: the tap, then the models
    rig.cx.update(|cx| {
        rig.bridge
            .update(cx, |bridge, cx| bridge.tell(presence(Availability::DoNotDisturb), cx));
    });
    assert_eq!(
        seen.borrow().last(),
        Some(&(Availability::DoNotDisturb, Availability::Away))
    );
    assert_eq!(shown(&rig), Availability::DoNotDisturb);

    rig.cx.read(|cx| rig.bridge.read(cx).set_tap(None));
    rig.send(presence(Availability::Available));
    assert_eq!(seen.borrow().len(), 2, "a tap taken off sees nothing more");
    assert_eq!(shown(&rig), Availability::Available);
}
