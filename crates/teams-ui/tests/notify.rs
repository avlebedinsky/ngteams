//! The notifications on GPUI's test context, over the fake engine (`common`) and a fake notification server that
//! records what it is asked: which messages notify (someone else's, delivered, or brought by a catch-up or a replay
//! after the push channel was last heard before it went down and unread; never the account's own, one the store
//! already had, history, an edit; the "notifications" setting; a chat shown in a focused window, asked again before
//! its notifications show; channels and own streams), a mention marked, more than three of one chat collapsed into
//! one (still marked when an earlier one mentioned the account), activating one opening its chat, a chat read (its
//! horizon moved, or a catch-up changed it) closing its notifications, a chat opened while its messages were looked
//! at showing none of them.
//! The same server on the sandbox's real private bus is `tests/desktop_bus.rs`. Every id and name is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;
#[path = "common/notices.rs"]
mod notices;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity};
use notices::{Asked, FakeServer, chat, message};
use teams_core::{CLOCK_SKEW_MS, Event, MessageChange, Source};
use teams_store::{Applied, ConversationKind, Member, Message, ReadHorizon};
use teams_trouter::Down;
use teams_ui::ViewTrace;
use teams_ui::notify::{GATHER, Notice, NoticeKind, Notifier, NotifierEvent, may_notify};
use teams_ui::settings::{Notifications, Settings, SettingsModel};

const LAB_C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const TRIO: &str = "19:0000000000000000000000000000trio@thread.v2";
const CHANNEL: &str = "19:channel0001@thread.tacv2";
const NOTES: &str = "48:notes";

/// The notifier over a rig, its server, what it opened and the chat a test says is shown in a focused window.
struct Notifying {
    rig: Rig,
    notifier: Entity<Notifier>,
    server: Arc<FakeServer>,
    opened: Arc<Mutex<Vec<String>>>,
    viewing: Arc<Mutex<Option<String>>>,
    settings: Entity<SettingsModel>,
    written: Written,
}

fn now_ms() -> i64 {
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

/// `message` with a mention of `mri` in its properties, as the service stores one.
fn mentioning(mut message: Message, mri: &str) -> Message {
    message.properties = Some(format!(
        "{{\"mentions\":\"[{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":0,\\\"mri\\\":\
         \\\"{mri}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab A\\\"}}]\"}}"
    ));
    message
}

impl Notifying {
    fn start(settings: Settings) -> Self {
        let rig = Rig::start();
        rig.core.conversations.lock().unwrap().extend([
            chat(AB, ConversationKind::OneToOne, None),
            chat(TRIO, ConversationKind::Group, Some("Lab trio")),
            chat(CHANNEL, ConversationKind::Channel, Some("General")),
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
            (LAB_B.to_owned(), "Bee".to_owned()),
            (LAB_C.to_owned(), "Cee".to_owned()),
        ]);
        let written = Written::default();
        let trace = Arc::new(ViewTrace::new(written.clone()));
        let server = Arc::new(FakeServer::default());
        let viewing = Arc::new(Mutex::new(None::<String>));
        let opened = Arc::new(Mutex::new(Vec::new()));
        let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
        let settings = rig.cx.update(|cx| cx.new(|_| SettingsModel::in_memory(settings)));
        let models = rig.models.clone();
        let (shown, given, recorded) = (Arc::clone(&viewing), Arc::clone(&server), Arc::clone(&opened));
        let notifier = rig.cx.update(|cx| {
            let notifier = cx.new(|cx| {
                let mut notifier = Notifier::new(core, &models, settings.clone(), trace, cx);
                notifier.set_server(given);
                notifier.set_viewing(Box::new(move |_| shown.lock().unwrap().clone()));
                notifier
            });
            cx.subscribe(&notifier, move |_, event, _| match event {
                NotifierEvent::Open(id) => recorded.lock().unwrap().push(id.clone()),
                NotifierEvent::OpenChannel(id) => recorded.lock().unwrap().push(format!("channel {id}")),
            })
            .detach();
            notifier
        });
        Self {
            rig,
            notifier,
            server,
            opened,
            viewing,
            settings,
            written,
        }
    }

    /// `message` newly stored in its conversation, brought by `source`.
    fn arrive(&self, message: Message, source: Source) {
        self.rig.send(Event::Message {
            conversation: message.conversation_id.clone(),
            id: message.id.clone(),
            change: MessageChange::New,
            applied: Applied::Inserted,
            message: Some(Box::new(message)),
            source,
        });
    }

    /// Lets the gathering run its course.
    fn settle(&self) {
        self.rig.cx.executor().advance_clock(GATHER);
        self.rig.cx.run_until_parked();
    }

    fn shown_lines(&self) -> Vec<(String, String)> {
        self.written
            .lines()
            .into_iter()
            .filter(|line| line["view"] == "notification" && line["change"] == "shown")
            .map(|line| {
                (
                    line["conversation"].as_str().unwrap().to_owned(),
                    line["kinds"][0].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

#[test]
fn a_delivered_message_from_someone_else_notifies_with_its_name_and_first_line() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        message(AB, "1", LAB_B, "<p>lunch at <b>noon</b>?</p><p>second line</p>", 5_000),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(
        n.server.shown(),
        [Notice {
            conversation: AB.to_owned(),
            kind: NoticeKind::Message,
            summary: "Bee".to_owned(),
            body: "lunch at noon?".to_owned(),
        }]
    );
    // a group chat names its sender and the chat
    n.arrive(message(TRIO, "2", LAB_C, "<p>hi all</p>", 6_000), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown()[1].summary, "Cee in Lab trio");
    // the trace says which chat and what kind, never a name or text
    assert_eq!(
        n.shown_lines(),
        [
            (AB.to_owned(), "message".to_owned()),
            (TRIO.to_owned(), "message".to_owned())
        ]
    );
    let text = n.written.text();
    assert!(!text.contains("Bee") && !text.contains("lunch"), "{text}");
    // a notification writes nothing: the engine was only read, and runs on
    let asked = n.rig.core.asked();
    assert!(
        asked
            .iter()
            .all(|asked| !matches!(*asked, "write" | "transient" | "resend" | "sync")),
        "{asked:?}"
    );
    assert_eq!(n.rig.shutdowns(), 0);
}

#[test]
fn the_account_s_own_a_replay_from_before_the_start_one_already_stored_history_and_an_edit_never_notify() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_A, "<p>mine</p>", 5_000), Source::Delivery);
    n.arrive(message(AB, "2", LAB_B, "<p>again</p>", 5_001), Source::Replay);
    n.arrive(message(AB, "3", LAB_B, "<p>older</p>", 5_002), Source::History);
    for (change, applied) in [
        (MessageChange::New, Applied::Ignored),
        (MessageChange::New, Applied::Updated),
        (MessageChange::Edit, Applied::Updated),
        (MessageChange::Reaction, Applied::Updated),
    ] {
        n.rig.send(Event::Message {
            conversation: AB.to_owned(),
            id: "4".to_owned(),
            change,
            applied,
            message: Some(Box::new(message(AB, "4", LAB_B, "<p>known</p>", 5_003))),
            source: Source::Delivery,
        });
    }
    let mut deleted = message(AB, "5", LAB_B, "", 5_004);
    deleted.delete_time = Some(5_005);
    n.arrive(deleted, Source::Delivery);
    let mut event = message(AB, "6", LAB_B, "<addmember/>", 5_006);
    event.message_type = "ThreadActivity/AddMember".to_owned();
    n.arrive(event, Source::Delivery);
    n.settle();
    assert_eq!(n.server.asked(), []);
    assert!(!may_notify(
        LAB_A,
        AB,
        &message(AB, "1", LAB_A, "x", 1),
        Source::Delivery
    ));
    assert!(may_notify(LAB_A, AB, &message(AB, "1", LAB_B, "x", 1), Source::CatchUp));
}

#[test]
fn a_mention_is_marked_and_mentions_only_shows_only_mentions() {
    let n = Notifying::start(Settings {
        notifications: Notifications::Mentions,
        ..Settings::default()
    });
    n.arrive(message(TRIO, "1", LAB_B, "<p>no mention</p>", 5_000), Source::Delivery);
    n.arrive(
        mentioning(message(TRIO, "2", LAB_B, "<p>see this</p>", 5_001), LAB_A),
        Source::Delivery,
    );
    // a mention of someone else isn't one of the account
    n.arrive(
        mentioning(message(TRIO, "3", LAB_B, "<p>for c</p>", 5_002), LAB_C),
        Source::Delivery,
    );
    n.arrive(
        mentioning(message(AB, "4", LAB_B, "<p>you</p>", 5_003), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 2, "{shown:?}");
    assert_eq!(
        (shown[0].kind, shown[0].summary.as_str(), shown[0].body.as_str()),
        (NoticeKind::Mention, "Bee mentioned you in Lab trio", "see this")
    );
    assert_eq!(
        (shown[1].kind, shown[1].summary.as_str()),
        (NoticeKind::Mention, "Bee mentioned you")
    );
    assert_eq!(n.shown_lines()[0].1, "mention");
}

#[test]
fn notifications_off_shows_none_and_the_setting_is_read_as_it_now_stands() {
    let n = Notifying::start(Settings {
        notifications: Notifications::Off,
        ..Settings::default()
    });
    n.arrive(
        mentioning(message(AB, "1", LAB_B, "<p>you</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(n.server.asked(), []);
    n.rig.cx.update(|cx| {
        n.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.notifications = Notifications::All, cx);
        });
    });
    n.arrive(message(AB, "2", LAB_B, "<p>now</p>", 5_001), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 1);
}

#[test]
fn the_chat_shown_in_a_focused_window_never_notifies() {
    let n = Notifying::start(Settings::default());
    *n.viewing.lock().unwrap() = Some(AB.to_owned());
    n.arrive(message(AB, "1", LAB_B, "<p>seen</p>", 5_000), Source::Delivery);
    n.arrive(message(TRIO, "2", LAB_C, "<p>elsewhere</p>", 5_001), Source::Delivery);
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].conversation, TRIO);
    *n.viewing.lock().unwrap() = None;
    n.arrive(message(AB, "3", LAB_B, "<p>away now</p>", 5_002), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 2);
}

#[test]
fn channels_notify_only_mentions_and_own_streams_never() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        message(CHANNEL, "1", LAB_B, "<p>to the team</p>", 5_000),
        Source::Delivery,
    );
    n.arrive(message(NOTES, "2", LAB_B, "<p>a note</p>", 5_001), Source::Delivery);
    n.arrive(
        mentioning(message(CHANNEL, "3", LAB_B, "<p>you there</p>", 5_002), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(
        (shown[0].conversation.as_str(), shown[0].kind),
        (CHANNEL, NoticeKind::Mention)
    );
    // activated, it opens the channel read-only, never as a chat
    let id = n.rig.cx.read(|cx| n.notifier.read(cx).shown())[0].0;
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(id, cx)));
    n.rig.cx.run_until_parked();
    assert_eq!(*n.opened.lock().unwrap(), [format!("channel {CHANNEL}")]);
}

/// `millis` milliseconds after the epoch.
fn at(millis: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(u64::try_from(millis).unwrap())
}

#[test]
fn a_catch_up_s_message_notifies_only_if_it_came_after_the_push_channel_was_last_heard_and_is_unread() {
    let n = Notifying::start(Settings::default());
    let started = n.rig.cx.read(|cx| n.notifier.read(cx).cutoff());
    // before the first connect: since the start
    n.arrive(
        message(AB, "1", LAB_B, "<p>while the app was off</p>", started - 60_000),
        Source::CatchUp,
    );
    n.settle();
    assert_eq!(n.server.asked(), []);
    n.rig.send(Event::Connected {
        endpoint: "endpoint-1".to_owned(),
    });
    // a suspend: the socket was last heard twenty minutes ago, its missing pong noticed only now
    let heard = now_ms() - 20 * 60_000;
    n.rig.send(Event::Disconnected {
        down: Down::PongTimeout,
        heard: Some(at(heard)),
    });
    let cutoff = n.rig.cx.read(|cx| n.notifier.read(cx).cutoff());
    assert_eq!(
        cutoff,
        heard - CLOCK_SKEW_MS,
        "the cutoff is when it was last heard, not when the drop was told"
    );
    // a socket that never connected moves nothing
    n.rig.send(Event::Disconnected {
        down: Down::Closed,
        heard: None,
    });
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).cutoff()), cutoff);
    n.rig.core.horizons.lock().unwrap().insert(
        TRIO.to_owned(),
        ReadHorizon {
            conversation_id: TRIO.to_owned(),
            user: LAB_A.to_owned(),
            last_read_arrival: heard + 180_000,
            updated: heard + 180_000,
            raw: None,
        },
    );
    n.arrive(
        message(AB, "2", LAB_B, "<p>before the drop</p>", cutoff - 1),
        Source::CatchUp,
    );
    // after it was last heard, before the drop was noticed
    n.arrive(
        message(AB, "3", LAB_B, "<p>missed in the silence</p>", heard + 60_000),
        Source::CatchUp,
    );
    // read on another device: at or before the horizon
    n.arrive(
        message(TRIO, "4", LAB_C, "<p>read elsewhere</p>", heard + 120_000),
        Source::CatchUp,
    );
    n.arrive(
        message(TRIO, "5", LAB_C, "<p>not read</p>", heard + 240_000),
        Source::CatchUp,
    );
    // pushed again on the reconnect, never stored before: missed as well
    n.arrive(
        message(AB, "6", LAB_B, "<p>replayed</p>", heard + 300_000),
        Source::Replay,
    );
    n.settle();
    let bodies: Vec<String> = n.server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["missed in the silence", "replayed", "not read"]);
}

#[test]
fn more_than_three_in_one_chat_collapse_into_one() {
    let n = Notifying::start(Settings::default());
    // a catch-up of messages that arrived before the app started notifies nothing, even five at once
    for id in 1..=5 {
        n.arrive(
            message(TRIO, &id.to_string(), LAB_C, "<p>burst</p>", 5_000 + id),
            Source::CatchUp,
        );
    }
    n.settle();
    let asked = n.server.asked();
    assert!(asked.is_empty(), "arrived before the start: {asked:?}");

    // five at once that arrived after the start, unread: straight into one
    let n = Notifying::start(Settings::default());
    let base = now_ms() + 60_000;
    for id in 1..=5 {
        n.arrive(
            message(TRIO, &id.to_string(), LAB_C, "<p>burst</p>", base + id),
            Source::CatchUp,
        );
    }
    n.settle();
    let shown = n.server.shown();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(
        (shown[0].kind, shown[0].summary.as_str(), shown[0].body.as_str()),
        (NoticeKind::Collapsed, "Lab trio", "5 new messages")
    );

    // two, then two more: the two singles give way to one of four, which a fifth replaces
    let n = Notifying::start(Settings::default());
    for id in 1..=2 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>one</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.shown().len(), 2);
    for id in 3..=4 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>more</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.closed(), [1, 2]);
    let collapsed = n.server.asked().last().cloned().unwrap();
    let Asked::Show {
        notice,
        replaces: 0,
        id,
    } = collapsed
    else {
        panic!("{collapsed:?}");
    };
    assert_eq!(
        (notice.kind, notice.body.as_str()),
        (NoticeKind::Collapsed, "4 new messages")
    );
    n.arrive(
        mentioning(message(AB, "5", LAB_B, "<p>and you</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    let replaced = n.server.asked().last().cloned().unwrap();
    assert_eq!(
        replaced,
        Asked::Show {
            notice: Notice {
                conversation: AB.to_owned(),
                kind: NoticeKind::Collapsed,
                summary: "Bee".to_owned(),
                body: "5 new messages, one or more mentioning you".to_owned(),
            },
            replaces: id,
            id,
        }
    );
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), [(id, AB.to_owned())]);
}

#[test]
fn a_mention_shown_before_still_marks_the_notification_its_chat_collapses_into() {
    let n = Notifying::start(Settings::default());
    n.arrive(
        mentioning(message(AB, "1", LAB_B, "<p>you there?</p>", 5_000), LAB_A),
        Source::Delivery,
    );
    n.settle();
    assert_eq!(n.server.shown()[0].kind, NoticeKind::Mention);
    for id in 2..=4 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>more</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    let collapsed = n.server.shown().last().cloned().unwrap();
    assert_eq!(
        (collapsed.kind, collapsed.body.as_str()),
        (NoticeKind::Collapsed, "4 new messages, one or more mentioning you")
    );
    // the next replaces it, still marked
    n.arrive(message(AB, "5", LAB_B, "<p>and more</p>", 5_000), Source::Delivery);
    n.settle();
    assert_eq!(
        n.server.shown().last().unwrap().body,
        "5 new messages, one or more mentioning you"
    );
    // once read, the chat starts again: no mark left over
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    for id in 6..=9 {
        n.arrive(
            message(AB, &id.to_string(), LAB_B, "<p>later</p>", 5_000),
            Source::Delivery,
        );
    }
    n.settle();
    assert_eq!(n.server.shown().last().unwrap().body, "4 new messages");
}

#[test]
fn a_chat_shown_or_opened_while_its_messages_are_looked_at_shows_none_of_them() {
    let n = Notifying::start(Settings::default());
    n.rig.core.hold_people.lock().unwrap().insert(LAB_B.to_owned());
    // every name asked answered, one ask after another (the chat's name, then the sender's)
    let release = |n: &Notifying| {
        let mut answered = 0;
        loop {
            let held: Vec<_> = n.rig.core.held.lock().unwrap().drain(..).collect();
            if held.is_empty() {
                break;
            }
            for (answer, person) in held {
                answer.send(Ok(person));
                answered += 1;
            }
            n.rig.cx.run_until_parked();
        }
        assert!(answered > 0, "nothing held");
    };
    // the names are asked; the chat is focused before the answers come
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    *n.viewing.lock().unwrap() = Some(AB.to_owned());
    release(&n);
    assert_eq!(n.server.asked(), []);

    // one shown, then another waits for its sender's name while the first is activated: the chat was opened after
    // the second came, so it shows nothing
    *n.viewing.lock().unwrap() = None;
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.settle();
    release(&n);
    let first = n.rig.cx.read(|cx| n.notifier.read(cx).shown())[0].0;
    n.arrive(message(AB, "3", LAB_B, "<p>three</p>", 5_002), Source::Delivery);
    n.settle();
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(first, cx)));
    release(&n);
    let bodies: Vec<String> = n.server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["two"]);
    // a later one shows again
    n.arrive(message(AB, "4", LAB_B, "<p>four</p>", 5_003), Source::Delivery);
    n.settle();
    release(&n);
    assert_eq!(n.server.shown().last().unwrap().body, "four");
}

#[test]
fn activating_one_opens_its_chat_and_closes_the_chat_s_others() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.arrive(message(TRIO, "3", LAB_C, "<p>three</p>", 5_002), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().len(), 3);
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(2, cx)));
    n.rig.cx.run_until_parked();
    assert_eq!(*n.opened.lock().unwrap(), [AB]);
    assert_eq!(n.server.closed(), [1, 2]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), [(3, TRIO.to_owned())]);
    // an id it never showed opens nothing
    n.rig
        .cx
        .update(|cx| n.notifier.update(cx, |notifier, cx| notifier.activated(99, cx)));
    assert_eq!(n.opened.lock().unwrap().len(), 1);
    let opened: Vec<_> = n
        .written
        .lines()
        .into_iter()
        .filter(|line| line["view"] == "notification" && line["change"] == "opened")
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["conversation"], AB);
    // the chat starts its count again: one more is shown on its own
    n.arrive(message(AB, "4", LAB_B, "<p>four</p>", 5_003), Source::Delivery);
    n.settle();
    assert_eq!(n.server.shown().last().unwrap().kind, NoticeKind::Message);
}

#[test]
fn a_chat_read_to_its_end_closes_its_notifications() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 1);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [] as [u32; 0]);
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ReadHorizon {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [1]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), []);
}

#[test]
fn a_chat_a_catch_up_finds_read_closes_its_notifications() {
    let n = Notifying::start(Settings::default());
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    // read on another device while the push channel was down: the catch-up stores the horizon and tells the chat
    // changed
    n.rig.core.unread.lock().unwrap().insert(AB.to_owned(), 0);
    n.rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
    assert_eq!(n.server.closed(), [1]);
    assert_eq!(n.rig.cx.read(|cx| n.notifier.read(cx).shown()), []);
}

#[test]
fn what_comes_before_a_server_is_given_is_never_shown() {
    let n = Notifying::start(Settings::default());
    let late = n.rig.cx.update(|cx| {
        let core = n.rig.bridge.read(cx).core();
        let trace = Arc::new(ViewTrace::off());
        cx.new(|cx| Notifier::new(core, &n.rig.models, n.settings.clone(), trace, cx))
    });
    n.arrive(message(AB, "1", LAB_B, "<p>one</p>", 5_000), Source::Delivery);
    n.settle();
    assert!(!n.rig.cx.read(|cx| late.read(cx).has_server()));
    let server = Arc::new(FakeServer::default());
    let given = Arc::clone(&server);
    n.rig.cx.update(|cx| late.update(cx, |late, _| late.set_server(given)));
    n.settle();
    assert_eq!(server.asked(), [], "dropped, not held for later");
    n.arrive(message(AB, "2", LAB_B, "<p>two</p>", 5_001), Source::Delivery);
    n.settle();
    let bodies: Vec<String> = server.shown().into_iter().map(|notice| notice.body).collect();
    assert_eq!(bodies, ["two"]);
}
