//! The control socket's message writes (`ctl::writes`), through the real protocol and server, over lab account a's
//! engine on the mock chat service (`teams-mock`, loopback only, through the `test-support` mock origins): each write
//! goes out through a's outbox, its row answered, and the mock tells b what it did. The read-only mode refuses them
//! all and sends nothing. Every id and text is made up or the mock's own.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared harness holds more than this test uses")]
mod ctl_support;

use std::future::{self, Future};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use ctl_support::{ask, quit};
use gpui::{AsyncApp, Entity, TestAppContext};
use ngteams::accounts::{Runner, Started};
use ngteams::accounts_file::{PersonalAccount, PersonalPaths};
use ngteams::app::{self, Launch};
use ngteams::ctl::writes::{self, is_settled};
use ngteams::setup::Setup;
use teams_auth::authz::{AuthzClient, Region};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::{EntraConfig, TokenClient, TokenError};
use teams_core::account::AccountEngine;
use teams_core::engine::{self, EngineOptions, Parts};
use teams_core::{Action, CallFloor, EngineRequests, Event, Outbox};
use teams_ctl::ClientError;
use teams_ctl::protocol::{CtlError, ErrorCode, OutboxRow, OutboxState, Request, Response, WRITE_ANSWER, WRITE_WAIT};
use teams_mock::{AB_CHAT, CHANNEL, CHAT_BASE, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C, NOTIFICATIONS, TENANT};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::{OutboxEntry, OutboxKind, Store};
use teams_ui::bus::BusError;
use teams_ui::{Bridge, SettingsFile, ViewTrace, compose};

const UPN: &str = "bot-a@lab.example.test";

/// a's token, as its session would give it.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

/// The mock, and the app of lab account a on it serving its control socket.
struct Lab {
    mock: ChatServiceMock,
    /// a's engine, beside the app's: to sync a chat the way a delivery would have.
    requests: EngineRequests,
    socket: PathBuf,
    cx: TestAppContext,
    _dir: tempfile::TempDir,
    runtime: tokio::runtime::Runtime,
}

impl Lab {
    /// The app of a on the mock, its store caught up first; `read_only` as `ngteams --read-only` runs.
    fn start(read_only: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let mock = runtime.block_on(ChatServiceMock::start()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let http = || {
            let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
            let http = HttpTransport::with_mock_origin(tokens, CHAT_BASE, &mock.origin()).unwrap();
            if read_only { http.read_only() } else { http }
        };
        let handle = engine::start(Parts {
            calls: None,
            store: Store::open(&dir.path().join("a.sqlite")).unwrap(),
            client: Arc::new(Client::new(http())),
            outbox: Outbox::new(Arc::new(http()), Author::new(Mri::new(LAB_A), "bot-a")),
            push: None,
            options: EngineOptions::default(),
            known_names: Vec::new(),
        })
        .unwrap();
        let requests = handle.requests();
        requests.sync().blocking_wait().unwrap();
        let socket = dir.path().join("run/ngteams/ctl.sock");
        let cx = TestAppContext::single();
        let launch = Launch {
            first: started(handle),
            others: Vec::new(),
            settings: {
                let settings = app::settings(SettingsFile::new(dir.path().join("settings.toml")), None);
                if read_only { settings.read_only() } else { settings }
            },
            bus: Err(BusError::NotGiven),
            sign_in_again: Rc::new(|_| Rc::new(|_| {})),
            runner: Runner {
                setup: Setup::new(PersonalPaths::new(dir.path().join("config"), dir.path().join("data"))).unwrap(),
                floor: CallFloor::new(),
                devices: None,
            },
            read_only,
            trace: Arc::new(ViewTrace::off()),
            ctl: Some(socket.clone()),
        };
        assert!(cx.update(|cx| app::launch(cx, launch)).is_ok(), "the window opens");
        cx.run_until_parked();
        Self {
            mock,
            requests,
            socket,
            cx,
            _dir: dir,
            runtime,
        }
    }

    fn ask(&self, request: Request) -> Result<Response, ClientError> {
        ask(&self.cx, &self.socket, None, request)
    }

    /// The row `request` answers with.
    fn row(&self, request: Request) -> OutboxRow {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Ok(Response::Outbox(row)) => row,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// The error `request` answers with.
    fn refused(&self, request: Request) -> CtlError {
        let asked = format!("{request:?}");
        match self.ask(request) {
            Err(ClientError::Server(error)) => error,
            other => panic!("{asked}: {other:?}"),
        }
    }

    /// What the mock told b since the last time, in a–b, as `(kind, content)`.
    fn told_b(&self) -> Vec<(EventKind, String)> {
        self.mock
            .take_events(LAB_B)
            .into_iter()
            .filter(|event| event.conversation == AB_CHAT && event.from == LAB_A)
            .map(|event| {
                let content = event.resource["content"].as_str().unwrap_or_default().to_owned();
                (event.kind, content)
            })
            .collect()
    }

    fn bridge(&self) -> Entity<Bridge> {
        self.cx.read(app::bridges)[0].clone()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        quit(&self.cx);
    }
}

/// a's engine as the app starts an account: its session only for the sign-out watch, reaching nothing (an empty
/// in-memory store, a loopback port nothing listens on).
fn started(handle: engine::EngineHandle) -> Started<MemorySecretStore> {
    let mut account = PersonalAccount::new(UPN, LAB_A.strip_prefix("8:orgid:").unwrap(), TENANT).unwrap();
    account.name = Some("bot-a".to_owned());
    let session = Session::new(
        UPN,
        MemorySecretStore::new(),
        TokenClient::new(EntraConfig::teams_native().with_authority("http://127.0.0.1:9/")).unwrap(),
        AuthzClient::with_endpoint("http://127.0.0.1:9/authz").unwrap(),
        Region::default(),
    );
    Started {
        account,
        engine: AccountEngine {
            handle,
            session: Arc::new(session),
        },
        calls: None,
    }
}

fn send(text: &str, wait: bool) -> Request {
    Request::Send {
        chat: AB_CHAT.to_owned(),
        text: text.to_owned(),
        wait,
    }
}

fn sent(row: &OutboxRow, write: &str) -> String {
    assert_eq!((row.write.as_str(), row.state), (write, OutboxState::Sent), "{row:?}");
    assert_eq!(row.chat.as_deref(), Some(AB_CHAT), "{row:?}");
    row.message.clone().expect("the message it wrote")
}

#[test]
fn send_edit_react_unreact_and_delete_go_out_through_the_outbox() {
    let lab = Lab::start(false);

    // line breaks kept: each line a paragraph, as the compose box sends them
    let row = lab.row(send("one\ntwo", false));
    let id = sent(&row, "send");
    let told = lab.told_b();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].0, EventKind::Message);
    assert!(told[0].1.contains("<p>one</p><p>two</p>"), "{told:?}");

    let row = lab.row(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        text: "edited".to_owned(),
        wait: true,
    });
    assert_eq!(sent(&row, "edit"), id);
    assert!(matches!(&lab.told_b()[..], [(EventKind::Edit, content)] if content.contains("edited")));

    let reaction = |react: bool| {
        let (chat, message, reaction, wait) = (AB_CHAT.to_owned(), id.clone(), " like ".to_owned(), false);
        if react {
            Request::React {
                chat,
                message,
                reaction,
                wait,
            }
        } else {
            Request::Unreact {
                chat,
                message,
                reaction,
                wait,
            }
        }
    };
    assert_eq!(sent(&lab.row(reaction(true)), "add_reaction"), id);
    assert_eq!(sent(&lab.row(reaction(false)), "remove_reaction"), id);
    let kinds: Vec<EventKind> = lab.told_b().into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(kinds, [EventKind::Reaction, EventKind::Reaction]);

    let row = lab.row(Request::Delete {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        wait: false,
    });
    assert_eq!(sent(&row, "delete"), id);
    assert!(matches!(&lab.told_b()[..], [(EventKind::Delete, _)]));
}

#[test]
fn an_edit_of_a_reply_keeps_its_quote_ahead_of_the_new_text() {
    let lab = Lab::start(false);
    let quoted = lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();
    let message = lab
        .requests
        .message(AB_CHAT, quoted.as_str())
        .blocking_wait()
        .unwrap()
        .expect("b's message stored");
    // a's reply to it, as the compose box sends one
    let quote = compose::Quoted::of(&message, "bot-b").unwrap().node();
    let reply = lab
        .requests
        .write(Action::Send {
            conversation: AB_CHAT.to_owned(),
            text: writes::message_text("my reply", &[quote]).unwrap(),
        })
        .blocking_wait()
        .unwrap();
    let id = sent(&writes::row_of(&reply), "send");
    let itemid = format!("itemid=\"{quoted}\"");
    assert!(
        matches!(&lab.told_b()[..], [(EventKind::Message, content)] if content.contains(&itemid)),
        "the reply quotes b's message"
    );

    let row = lab.row(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: id.clone(),
        text: "edited reply".to_owned(),
        wait: false,
    });
    assert_eq!(sent(&row, "edit"), id);
    let told = lab.told_b();
    let [(EventKind::Edit, content)] = &told[..] else {
        panic!("one edit: {told:?}");
    };
    let quote_at = content.find(&itemid).expect("the quote kept");
    let text_at = content.find("edited reply").expect("the new text");
    assert!(quote_at < text_at, "{content}");
    assert!(!content.contains("my reply"), "{content}");
}

#[test]
fn a_text_that_shows_nothing_or_is_too_long_and_a_message_not_there_are_refused_before_any_row() {
    let lab = Lab::start(false);

    for empty in ["", " \n\t\n "] {
        let error = lab.refused(send(empty, false));
        assert_eq!(error.code, ErrorCode::BadRequest, "{empty:?}");
        assert!(error.message.contains("empty"), "{error:?}");
    }
    let error = lab.refused(send(&"long ".repeat(40_000), false));
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("content longer than"), "{error:?}");

    let error = lab.refused(Request::Edit {
        chat: AB_CHAT.to_owned(),
        message: "1".to_owned(),
        text: "edited".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(error.message.contains("no message 1"), "{error:?}");
    let error = lab.refused(Request::Send {
        chat: "no such chat".to_owned(),
        text: "hi".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::UnknownChat);
    let error = lab.refused(Request::React {
        chat: AB_CHAT.to_owned(),
        message: "1".to_owned(),
        reaction: " ".to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest);
    assert!(lab.told_b().is_empty(), "nothing went out");
}

#[test]
fn read_marks_the_chat_read_up_to_b_s_newest_message_once() {
    let lab = Lab::start(false);
    let read = || Request::MarkRead {
        chat: AB_CHAT.to_owned(),
        wait: false,
    };
    lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();

    let row = lab.row(read());
    assert_eq!(
        (row.write.as_str(), row.state),
        ("read_receipt", OutboxState::Sent),
        "{row:?}"
    );
    let receipts: Vec<EventKind> = lab
        .mock
        .take_events(LAB_B)
        .into_iter()
        .filter(|event| event.conversation == AB_CHAT)
        .map(|event| event.kind)
        .collect();
    assert_eq!(receipts, [EventKind::ReadReceipt]);

    // the horizon moved there: nothing more to mark
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();
    assert_eq!(lab.ask(read()).unwrap(), Response::Done);
}

#[test]
fn read_finds_b_s_message_under_more_than_a_page_of_entries_that_don_t_count() {
    let lab = Lab::start(false);
    let id = lab.mock.post_as(LAB_B, AB_CHAT, "<p>from b</p>").unwrap();
    for _ in 0..=writes::READ_PAGE {
        lab.mock
            .add_message(
                AB_CHAT,
                (LAB_B, "bot-b"),
                "ThreadActivity/TopicUpdate",
                "<topicupdate/>",
            )
            .unwrap();
    }
    lab.requests.sync_conversation(AB_CHAT).blocking_wait().unwrap();

    let row = lab.row(Request::MarkRead {
        chat: AB_CHAT.to_owned(),
        wait: false,
    });
    assert_eq!(
        (row.write.as_str(), row.state),
        ("read_receipt", OutboxState::Sent),
        "{row:?}"
    );
    // read up to b's message, under the entries that came after it
    let message = lab.requests.message(AB_CHAT, id).blocking_wait().unwrap().unwrap();
    let horizon = lab.requests.read_horizon(AB_CHAT).blocking_wait().unwrap().unwrap();
    assert_eq!(horizon.last_read_arrival, message.arrival_time);
}

#[test]
fn read_refuses_a_channel_and_sends_nothing() {
    let lab = Lab::start(false);
    lab.mock
        .add_message(CHANNEL, (LAB_B, "bot-b"), "RichText/Html", "<p>a post from b</p>")
        .unwrap();
    lab.requests.sync_conversation(CHANNEL).blocking_wait().unwrap();
    let writes = || {
        lab.runtime
            .block_on(lab.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() != "GET")
            .count()
    };
    let before = writes();
    let horizon = || lab.requests.read_horizon(CHANNEL).blocking_wait().unwrap();
    let horizon_before = horizon();

    let error = lab.refused(Request::MarkRead {
        chat: CHANNEL.to_owned(),
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::BadRequest, "{error:?}");
    assert!(error.message.contains("channel"), "{error:?}");
    assert_eq!(writes(), before, "no receipt went out");
    assert_eq!(horizon(), horizon_before, "the channel's horizon stays");
}

#[test]
fn new_chat_makes_a_group_chat_and_resend_sends_a_failed_row_again_never_a_sent_one() {
    let lab = Lab::start(false);

    let row = lab.row(Request::NewChat {
        people: vec![LAB_B.to_owned(), format!(" {LAB_C} "), String::new()],
        wait: true,
    });
    assert_eq!(
        (row.write.as_str(), row.state),
        ("create_chat", OutboxState::Sent),
        "{row:?}"
    );
    let made = row.chat.clone().expect("the chat made");
    assert_eq!(row.message.as_deref(), Some(made.as_str()));
    assert!(
        lab.mock
            .take_events(LAB_B)
            .iter()
            .any(|event| event.kind == EventKind::ChatCreated && event.conversation == made),
        "b is told of the chat"
    );
    let nobody = lab.refused(Request::NewChat {
        people: vec![" ".to_owned()],
        wait: false,
    });
    assert_eq!(nobody.code, ErrorCode::BadRequest);
    // a UPN is looked for by the service's people search, which the mock doesn't serve
    let unknown = lab.refused(Request::NewChat {
        people: vec!["nobody@lab.example.test".to_owned()],
        wait: false,
    });
    assert_eq!(unknown.code, ErrorCode::Engine, "{unknown:?}");

    // a's notifications take no message: the send fails for good, its error told; sent again, it fails again
    let failed = lab.row(Request::Send {
        chat: NOTIFICATIONS.to_owned(),
        text: "again".to_owned(),
        wait: true,
    });
    assert_eq!(
        (failed.write.as_str(), failed.state),
        ("send", OutboxState::Failed),
        "{failed:?}"
    );
    assert!(
        failed.error.as_deref().is_some_and(|error| error.contains("403")),
        "{failed:?}"
    );
    let again = lab.row(Request::Resend {
        row: failed.id,
        wait: true,
    });
    assert_eq!((again.id, again.state), (failed.id, OutboxState::Failed), "{again:?}");

    // a row sent is sent once
    let once = lab.row(send("once", false));
    sent(&once, "send");
    let error = lab.refused(Request::Resend {
        row: once.id,
        wait: false,
    });
    assert_eq!(error.code, ErrorCode::Engine, "{error:?}");
}

#[test]
fn the_read_only_app_refuses_every_write_and_sends_nothing() {
    let lab = Lab::start(true);
    let message = || "1".to_owned();
    let chat = || AB_CHAT.to_owned();
    let writes = [
        send("hi", false),
        Request::Edit {
            chat: chat(),
            message: message(),
            text: "hi".to_owned(),
            wait: false,
        },
        Request::Delete {
            chat: chat(),
            message: message(),
            wait: false,
        },
        Request::React {
            chat: chat(),
            message: message(),
            reaction: "like".to_owned(),
            wait: false,
        },
        Request::Unreact {
            chat: chat(),
            message: message(),
            reaction: "like".to_owned(),
            wait: false,
        },
        Request::MarkRead {
            chat: chat(),
            wait: false,
        },
        Request::NewChat {
            people: vec![LAB_B.to_owned()],
            wait: false,
        },
        Request::Resend { row: 1, wait: false },
    ];
    for write in writes {
        assert_eq!(lab.refused(write).code, ErrorCode::RefusedReadOnly);
    }
    let not_reads: Vec<String> = lab
        .runtime
        .block_on(lab.mock.received_requests())
        .iter()
        .filter(|request| request.method.as_str() != "GET")
        .map(|request| format!("{} {}", request.method, request.url.path()))
        .collect();
    assert_eq!(not_reads, Vec::<String>::new());
}

fn entry(id: i64, state: teams_store::OutboxState) -> OutboxEntry {
    OutboxEntry {
        id,
        run_id: 1,
        client_message_id: "1".to_owned(),
        conversation_id: Some(AB_CHAT.to_owned()),
        kind: OutboxKind::Send,
        payload: String::new(),
        state,
        attempts: 1,
        attempts_at_resend: 0,
        failed_checks: 0,
        not_before: None,
        last_error: None,
        message_id: None,
        created_at: 1,
        updated_at: 1,
        unreadable_seen_at: None,
    }
}

/// `writes::settled` of `entry` run on the app's foreground; its outcome once it has one.
fn settling(lab: &Lab, entry: OutboxEntry) -> mpsc::Receiver<Result<OutboxEntry, CtlError>> {
    let bridge = lab.bridge();
    let chats = lab.cx.read(|cx| bridge.read(cx).models().chats.clone());
    let (tell, told) = mpsc::channel();
    lab.cx.update(|cx| {
        cx.spawn(async move |cx: &mut AsyncApp| {
            let _ = tell.send(writes::settled(&chats, entry, cx).await);
        })
        .detach();
    });
    lab.cx.run_until_parked();
    told
}

#[test]
fn a_wait_ends_when_the_engine_tells_the_row_settled_or_at_its_bound() {
    use teams_store::OutboxState as Stored;

    let lab = Lab::start(false);
    let told = settling(&lab, entry(901, Stored::Pending));
    assert!(told.try_recv().is_err(), "a pending row waits");
    let bridge = lab.bridge();
    lab.cx.update(|cx| {
        bridge.update(cx, |bridge, cx| {
            bridge.tell(Event::Outbox(entry(901, Stored::Sending)), cx);
        });
    });
    lab.cx.run_until_parked();
    assert!(told.try_recv().is_err(), "an attempt in flight waits");
    lab.cx.update(|cx| {
        bridge.update(cx, |bridge, cx| {
            bridge.tell(Event::Outbox(entry(901, Stored::Sent)), cx);
        });
    });
    lab.cx.run_until_parked();
    assert_eq!(told.try_recv().unwrap().unwrap().state, Stored::Sent);

    // told already before the wait began
    let told = settling(&lab, entry(901, Stored::Pending));
    assert_eq!(told.try_recv().unwrap().unwrap().state, Stored::Sent);

    let told = settling(&lab, entry(902, Stored::Pending));
    lab.cx.executor().advance_clock(WRITE_WAIT);
    lab.cx.run_until_parked();
    let error = told.try_recv().unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.message.contains("it is queued"), "{error:?}");
}

/// `writes::within_answer` of `write` run on the app's foreground; its outcome once it has one.
fn bounded(
    lab: &Lab,
    write: impl Future<Output = Result<i64, CtlError>> + 'static,
) -> mpsc::Receiver<Result<i64, CtlError>> {
    let (tell, told) = mpsc::channel();
    lab.cx.update(|cx| {
        cx.spawn(async move |cx: &mut AsyncApp| {
            let executor = cx.background_executor().clone();
            let _ = tell.send(writes::within_answer(write, &executor).await);
        })
        .detach();
    });
    lab.cx.run_until_parked();
    told
}

#[test]
fn a_write_the_engine_doesn_t_answer_is_answered_at_its_bound_as_one_that_may_still_go_out() {
    let lab = Lab::start(false);
    assert_eq!(bounded(&lab, future::ready(Ok(7))).try_recv().unwrap().unwrap(), 7);

    let told = bounded(&lab, future::pending());
    lab.cx
        .executor()
        .advance_clock(WRITE_ANSWER.checked_sub(Duration::from_secs(1)).unwrap());
    lab.cx.run_until_parked();
    assert!(told.try_recv().is_err(), "it waits up to its bound");
    lab.cx.executor().advance_clock(Duration::from_secs(1));
    lab.cx.run_until_parked();
    let error = told.try_recv().unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.message.contains("may still go out"), "{error:?}");
    assert!(error.message.contains("--kind outbox"), "{error:?}");
}

#[test]
fn a_row_settles_sent_failed_or_once_it_may_have_arrived() {
    use teams_store::OutboxState as Stored;

    for (state, settled) in [
        (Stored::Pending, false),
        (Stored::Sending, false),
        (Stored::Sent, true),
        (Stored::Denied, true),
        (Stored::Failed, true),
    ] {
        assert_eq!(is_settled(&entry(1, state)), settled, "{state:?}");
    }
    let may_have_arrived = OutboxEntry {
        unreadable_seen_at: Some(1),
        ..entry(1, Stored::Pending)
    };
    assert!(is_settled(&may_have_arrived));
    // a new chat's row names the chat once made
    let made = OutboxEntry {
        kind: OutboxKind::CreateChat,
        conversation_id: None,
        message_id: Some("19:made@thread.v2".to_owned()),
        ..entry(1, Stored::Sent)
    };
    assert_eq!(writes::row_of(&made).chat.as_deref(), Some("19:made@thread.v2"));
}
