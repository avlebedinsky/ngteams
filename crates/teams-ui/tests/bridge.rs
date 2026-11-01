//! The signed-out account's placeholder refuses chat, media, presence and call operations without a running engine.

#![cfg(test)]

use futures_util::FutureExt as _;
use teams_core::{Action, CallAction, CallId, EngineError, OwnStatus, Reply, Transient};
use teams_ui::{Core as _, StoppedCore};

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
