//! The control socket's reads (`ctl::reads`) and their addressing (`ctl::address`), through the real protocol and
//! server, over a store seeded before the app starts: a 1:1 chat with Bee, a group chat with a topic, and two group
//! chats without one, both named by their members' first names alike. Every id, name and text is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::cell::Cell;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use ctl_support::{ME, UPN, WAIT, ask, connect, launched_with, pump_until, quit, socket_in};
use gpui::{Entity, TestAppContext};
use ngteams::app::Launched;
use serde_json::json;
use teams_core::EngineRequests;
use teams_ctl::ClientError;
use teams_ctl::protocol::{ErrorCode, MessageRow, Request, Response, Unread};
use teams_proto::chat::parse_html;
use teams_store::{Conversation, ConversationKind, Member, Message, NameSource, PictureState, Profile, Store};
use teams_ui::Tray;
use teams_ui::tray::COUNT_DELAY;

const BEE: &str = "8:orgid:00000000-0000-4000-8000-0000000000b1";
const CEE: &str = "8:orgid:00000000-0000-4000-8000-0000000000c1";
const ONE_TO_ONE: &str = "19:00000000-0000-4000-8000-0000000000b1_00000000-0000-4000-8000-0000000000ff@unq.gbl.spaces";
const PROJECT: &str = "19:00000000000000000000000000project@thread.v2";
const TRIO_ONE: &str = "19:0000000000000000000000000000trio1@thread.v2";
const TRIO_TWO: &str = "19:0000000000000000000000000000trio2@thread.v2";

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn conversation(store: &mut Store, id: &str, kind: ConversationKind, topic: Option<&str>, members: &[&str]) {
    store
        .upsert_conversation(&Conversation {
            id: id.to_owned(),
            kind,
            topic: topic.map(str::to_owned),
            version: 1,
            last_activity: None,
        })
        .unwrap();
    let members: Vec<Member> = members.iter().map(|mri| Member::new(mri, None)).collect();
    store.replace_members(id, 1, &members).unwrap();
}

/// A message of `chat` from `sender` at `at` (its id too), indexed for the search by its text.
fn message(store: &mut Store, chat: &str, sender: &str, at: i64, html: &str) {
    let mentions_me = html.contains("<at");
    let properties = mentions_me.then(|| {
        let mentions = json!([{
            "@type": "http://schema.skype.com/Mention",
            "itemid": 0,
            "mri": ME,
            "mentionType": "person",
            "displayName": "Someone",
        }]);
        json!({ "mentions": mentions.to_string() }).to_string()
    });
    let message = Message {
        conversation_id: chat.to_owned(),
        id: at.to_string(),
        version: at,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: None,
        message_type: "RichText/Html".to_owned(),
        content: html.to_owned(),
        properties,
        arrival_time: at,
        compose_time: Some(at),
        edit_time: None,
        delete_time: None,
    };
    store
        .upsert_message_indexed(&message, None, |message| parse_html(&message.content).plain_text())
        .unwrap();
}

/// Fresh profiles: nobody is looked up (no token to look with).
fn profile(store: &mut Store, mri: &str, name: &str) {
    store
        .put_profile(&Profile {
            mri: mri.to_owned(),
            name: Some(name.to_owned()),
            name_source: Some(NameSource::Graph),
            picture: PictureState::None,
            read_at: now_ms(),
        })
        .unwrap();
}

/// The seeded store (see the module docs): Bee's two messages in the 1:1 chat unread (the newest mentions the
/// account), Cee's one in "Project".
fn seed(store: &mut Store) {
    profile(store, ME, "Someone");
    profile(store, BEE, "Bee Example");
    profile(store, CEE, "Cee Example");
    conversation(store, ONE_TO_ONE, ConversationKind::OneToOne, None, &[ME, BEE]);
    conversation(
        store,
        PROJECT,
        ConversationKind::Group,
        Some("Project"),
        &[ME, BEE, CEE],
    );
    conversation(store, TRIO_ONE, ConversationKind::Group, None, &[ME, BEE, CEE]);
    conversation(store, TRIO_TWO, ConversationKind::Group, None, &[ME, CEE, BEE]);
    message(store, ONE_TO_ONE, BEE, 1000, "<p>Hello there</p>");
    message(store, ONE_TO_ONE, ME, 2000, "<p>Hi back</p>");
    let mention = r#"<p><at id="0">Someone</at> line one</p><p>line two</p>"#;
    message(store, ONE_TO_ONE, BEE, 3000, mention);
    message(store, PROJECT, CEE, 5000, "<p>Budget review</p>");
    message(store, TRIO_ONE, ME, 400, "<p>Trio one</p>");
    message(store, TRIO_TWO, ME, 300, "<p>Trio two</p>");
}

struct App {
    cx: TestAppContext,
    socket: std::path::PathBuf,
    launched: Launched,
    requests: EngineRequests,
    asked: Rc<Cell<u32>>,
    _dir: tempfile::TempDir,
}

impl App {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(&dir);
        let cx = TestAppContext::single();
        let asked = Rc::new(Cell::new(0));
        let (launched, requests) = launched_with(&cx, dir.path(), &socket, &asked, seed);
        Self {
            cx,
            socket,
            launched,
            requests,
            asked,
            _dir: dir,
        }
    }

    fn ask(&self, account: Option<&str>, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, account, request)
    }

    fn refused(&self, account: Option<&str>, request: Request) -> teams_ctl::protocol::CtlError {
        match self.ask(account, request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("not refused: {other:?}"),
        }
    }

    fn messages(&self, chat: &str, limit: Option<u32>, before: Option<&str>) -> Vec<MessageRow> {
        let request = Request::Messages {
            chat: chat.to_owned(),
            limit,
            before: before.map(str::to_owned),
        };
        match self.ask(None, request) {
            Ok(Response::Messages { messages }) => messages,
            other => panic!("not messages: {other:?}"),
        }
    }

    fn chat_ids(&self, account: Option<&str>, unread_only: bool, limit: Option<u32>) -> Vec<String> {
        match self.ask(account, Request::Chats { unread_only, limit }) {
            Ok(Response::Chats { chats }) => chats.into_iter().map(|chat| chat.id).collect(),
            other => panic!("not chats: {other:?}"),
        }
    }

    /// The badge's counts of the account.
    fn tray(&self) -> Entity<Tray> {
        self.cx.read(|cx| {
            self.launched
                .desktop
                .read(cx)
                .accounts()
                .read(cx)
                .tile(ME)
                .unwrap()
                .tray
                .clone()
        })
    }

    fn quit(&self) {
        quit(&self.cx);
    }
}

#[test]
fn accounts_list_the_app_s_and_an_account_is_named_by_upn_mri_or_index() {
    let app = App::start();
    let Ok(Response::Accounts { accounts }) = app.ask(None, Request::Accounts) else {
        panic!("no accounts");
    };
    let [account] = accounts.as_slice() else {
        panic!("one account: {accounts:?}");
    };
    assert_eq!(
        (account.index, account.upn.as_str(), account.mri.as_str()),
        (0, UPN, ME)
    );
    assert!(account.shown);
    assert_eq!(account.name.as_deref(), Some("Someone"), "the rail's name of it");

    let all = app.chat_ids(None, false, None);
    assert_eq!(all.len(), 4);
    for named in [UPN.to_uppercase().as_str(), ME, "0", " 0 "] {
        assert_eq!(app.chat_ids(Some(named), false, None), all, "{named}");
    }
    for unknown in ["1", "nobody@example.test", "8:orgid:nobody", "-1"] {
        let error = app.refused(
            Some(unknown),
            Request::Chats {
                unread_only: false,
                limit: None,
            },
        );
        assert_eq!(error.code, ErrorCode::UnknownAccount, "{unknown}: {error:?}");
    }
    app.quit();
}

#[test]
fn chats_are_titled_counted_and_previewed_as_the_chat_list_shows_them() {
    let app = App::start();
    let Ok(Response::Chats { chats }) = app.ask(
        None,
        Request::Chats {
            unread_only: false,
            limit: None,
        },
    ) else {
        panic!("no chats");
    };
    let rows: Vec<(&str, &str, &str, u64, Option<&str>)> = chats
        .iter()
        .map(|chat| {
            (
                chat.id.as_str(),
                chat.title.as_str(),
                chat.chat_type.as_str(),
                chat.unread,
                chat.preview.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (PROJECT, "Project", "group", 1, Some("Budget review")),
            (ONE_TO_ONE, "Bee Example", "one-to-one", 2, Some("Someone line one")),
            (TRIO_ONE, "Bee and Cee", "group", 0, Some("You: Trio one")),
            (TRIO_TWO, "Bee and Cee", "group", 0, Some("You: Trio two")),
        ],
        "the latest activity first"
    );
    assert_eq!(chats[0].last_activity, Some(5000));

    assert_eq!(app.chat_ids(None, true, None), [PROJECT, ONE_TO_ONE], "the unread ones");
    assert_eq!(app.chat_ids(None, true, Some(1)), [PROJECT]);
    assert_eq!(app.chat_ids(None, false, Some(3)), [PROJECT, ONE_TO_ONE, TRIO_ONE]);
    app.quit();
}

#[test]
fn messages_come_newest_first_as_plain_text_from_a_chat_named_by_the_other_person() {
    let app = App::start();
    // a 1:1 chat by the other person's name, in any case
    let messages = app.messages("bee example", None, None);
    let ids: Vec<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    assert_eq!(ids, ["3000", "2000", "1000"]);
    let newest = &messages[0];
    assert_eq!(newest.chat, ONE_TO_ONE);
    assert_eq!(newest.sender.as_deref(), Some(BEE));
    assert_eq!(newest.sender_name.as_deref(), Some("Bee Example"));
    assert_eq!(newest.at, 3000);
    assert!(
        newest.text.contains("line one") && newest.text.contains("line two"),
        "{:?}",
        newest.text
    );
    assert!(newest.mentions_me && !newest.from_me && !newest.edited && !newest.deleted);
    assert!(messages[1].from_me && !messages[1].mentions_me);
    assert_eq!(messages[1].text, "Hi back");

    // by id, a page back from a message, and a limit
    let older: Vec<String> = app
        .messages(ONE_TO_ONE, None, Some("3000"))
        .into_iter()
        .map(|message| message.id)
        .collect();
    assert_eq!(older, ["2000", "1000"]);
    assert_eq!(app.messages(ONE_TO_ONE, Some(1), None).len(), 1);
    let missing = app.refused(
        None,
        Request::Messages {
            chat: ONE_TO_ONE.to_owned(),
            limit: None,
            before: Some("no-such-message".to_owned()),
        },
    );
    assert_eq!(missing.code, ErrorCode::BadRequest);
    app.quit();
}

#[test]
fn a_chat_named_by_no_chat_or_by_several_is_refused_with_the_candidates() {
    let app = App::start();
    let unknown = app.refused(
        None,
        Request::Members {
            chat: "Nobody Here".to_owned(),
        },
    );
    assert_eq!(unknown.code, ErrorCode::UnknownChat);

    let ambiguous = app.refused(
        None,
        Request::Messages {
            chat: "Bee and Cee".to_owned(),
            limit: None,
            before: None,
        },
    );
    assert_eq!(ambiguous.code, ErrorCode::AmbiguousChat);
    let candidates: Vec<(&str, &str)> = ambiguous
        .candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate.title.as_str()))
        .collect();
    assert_eq!(candidates, [(TRIO_ONE, "Bee and Cee"), (TRIO_TWO, "Bee and Cee")]);
    // either one by its id
    assert_eq!(app.messages(TRIO_TWO, None, None)[0].text, "Trio two");
    app.quit();
}

#[test]
fn search_finds_the_listed_chats_messages_and_members_are_named() {
    let app = App::start();
    let Ok(Response::Search { messages }) = app.ask(
        None,
        Request::Search {
            query: "budget".to_owned(),
            limit: None,
        },
    ) else {
        panic!("no hits");
    };
    let hits: Vec<(&str, &str, &str)> = messages
        .iter()
        .map(|hit| (hit.chat.as_str(), hit.id.as_str(), hit.text.as_str()))
        .collect();
    assert_eq!(hits, [(PROJECT, "5000", "Budget review")]);
    assert_eq!(messages[0].sender_name.as_deref(), Some("Cee Example"));
    let short = app.refused(
        None,
        Request::Search {
            query: " ".to_owned(),
            limit: None,
        },
    );
    assert_eq!(short.code, ErrorCode::BadRequest);

    let Ok(Response::Members { members }) = app.ask(
        None,
        Request::Members {
            chat: "project".to_owned(),
        },
    ) else {
        panic!("no members");
    };
    let mut named: Vec<(&str, Option<&str>)> = members
        .iter()
        .map(|member| (member.mri.as_str(), member.name.as_deref()))
        .collect();
    named.sort_unstable();
    assert_eq!(
        named,
        [
            (BEE, Some("Bee Example")),
            (CEE, Some("Cee Example")),
            (ME, Some("Someone")),
        ]
    );
    app.quit();
}

#[test]
fn unread_tells_the_badge_s_counts() {
    let app = App::start();
    let tray = app.tray();
    // the badge counts a moment after the start
    pump_until(&app.cx, COUNT_DELAY, |cx| tray.read(cx).total() == Some(2));

    assert_unread(app.ask(None, Request::Unread));
    app.quit();
}

#[test]
fn unread_asked_before_the_badge_counted_waits_for_its_count() {
    let app = App::start();
    let tray = app.tray();
    assert_eq!(app.cx.read(|cx| tray.read(cx).total()), None, "not counted yet");
    let socket = app.socket.clone();
    let client = thread::spawn(move || connect(&socket)?.request(None, Request::Unread));
    // no time passes: the badge doesn't count, and the request isn't answered 0 meanwhile
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(300) {
        app.cx.run_until_parked();
        thread::sleep(Duration::from_millis(5));
    }
    assert!(!client.is_finished(), "answered before the badge counted");
    // the badge counts a moment after the start, and the request is answered its counts
    pump_until(&app.cx, COUNT_DELAY, |_| client.is_finished());
    assert_unread(client.join().unwrap());
    app.quit();
}

/// The seeded store's unread chats, as the badge counts them.
fn assert_unread(answer: Result<Response, ClientError>) {
    let Ok(Response::Unread(Unread { total, chats })) = answer else {
        panic!("no unread: {answer:?}");
    };
    assert_eq!(total, 2, "the unread chats, as the badge counts them");
    let chats: Vec<(&str, &str, u64)> = chats
        .iter()
        .map(|chat| (chat.chat.as_str(), chat.title.as_str(), chat.count))
        .collect();
    assert_eq!(chats, [(PROJECT, "Project", 1), (ONE_TO_ONE, "Bee Example", 2)]);
}

#[test]
fn an_account_whose_engine_stopped_is_refused_as_stopped() {
    let app = App::start();
    // the first request that needs a token finds the account signed out: its engine stops
    assert!(app.requests.sync().blocking_wait().is_err());
    let start = Instant::now();
    while app.asked.get() == 0 {
        app.cx.run_until_parked();
        assert!(start.elapsed() < WAIT, "the account wasn't signed out in time");
        thread::sleep(Duration::from_millis(10));
    }
    for request in [
        Request::Unread,
        Request::Chats {
            unread_only: false,
            limit: None,
        },
    ] {
        let error = app.refused(None, request);
        assert_eq!(error.code, ErrorCode::Stopped, "{error:?}");
    }
    // the app's own answers go on
    assert!(matches!(
        app.ask(None, Request::Accounts),
        Ok(Response::Accounts { .. })
    ));
    app.quit();
}
