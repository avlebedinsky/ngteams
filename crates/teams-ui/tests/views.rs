//! The bridge, the models and the root view on GPUI's test context, over a fake engine (`common`): the drain applies
//! each event to the models on the foreground and every view observing a model sees it; views reach the engine only
//! through `Core`, and a query answers through its `Reply`; shutdown ends the drain and shuts the engine down once;
//! a sign-out shows its hint and stops the engine; the view trace holds ids, kinds and times, never a message's text
//! or a person's name.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use std::sync::Arc;

use common::{CHAT, LAB_B, Rig, Watcher, Written, conversation, message, outbox_row};
use gpui::{AppContext as _, TestAppContext};
use teams_core::{Event, MessageChange, Source};
use teams_store::{Applied, Conversation, OutboxState};
use teams_trouter::Down;
use teams_ui::trace::TraceLine;
use teams_ui::view::{Shown, status_text};
use teams_ui::{Account, Bridge, Engine, Link, Root, TraceId, ViewTrace};

fn changed(conversation: &str) -> Event {
    Event::ConversationChanged {
        conversation: conversation.to_owned(),
    }
}

#[test]
fn two_views_observing_one_model_both_see_an_event() {
    let rig = Rig::start();
    let chats = rig.models.chats.clone();
    let first = rig.cx.update(|cx| cx.new(|cx| Watcher::on(&chats, cx)));
    let second = rig.cx.update(|cx| cx.new(|cx| Watcher::on(&chats, cx)));

    rig.send(changed(CHAT));

    for watcher in [&first, &second] {
        assert_eq!(rig.cx.read(|cx| watcher.read(cx).seen), 1);
    }
    let at = rig.cx.read(|cx| rig.models.chats.read(cx).changed_at(CHAT));
    assert!(at > 0, "the conversation's change is kept");
    assert_eq!(
        rig.cx
            .read(|cx| rig.models.chats.read(cx).changed_at("19:other@thread.v2")),
        0
    );
}

#[test]
fn each_event_reaches_its_model() {
    let rig = Rig::start();
    let status = |rig: &Rig| rig.cx.read(|cx| rig.models.status.read(cx).clone());
    assert_eq!(status(&rig).link, Link::Starting);

    rig.send(Event::Connected {
        endpoint: "00000000-0000-4000-8000-0000000000e1".to_owned(),
    });
    assert_eq!(status(&rig).link, Link::Connected);
    rig.send(Event::Disconnected(Down::Closed));
    assert_eq!(status(&rig).link, Link::Down);

    rig.send(Event::Typing {
        conversation: CHAT.to_owned(),
        from: Some(LAB_B.to_owned()),
        typing: true,
    });
    let typing = rig.cx.read(|cx| {
        rig.models
            .chats
            .read(cx)
            .typing(CHAT)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    assert_eq!(typing, [LAB_B]);
    rig.send(Event::Typing {
        conversation: CHAT.to_owned(),
        from: Some(LAB_B.to_owned()),
        typing: false,
    });
    assert_eq!(rig.cx.read(|cx| rig.models.chats.read(cx).typing(CHAT).count()), 0);

    let before = rig.cx.read(|cx| rig.models.chats.read(cx).revision());
    rig.send(Event::Outbox(outbox_row(7, OutboxState::Pending)));
    rig.cx.read(|cx| {
        let chats = rig.models.chats.read(cx);
        assert_eq!(chats.outbox_row(7).map(|row| row.state), Some(OutboxState::Pending));
        assert!(chats.revision() > before, "the row's conversation changed");
        assert_eq!(chats.changed_at(CHAT), chats.revision());
    });
    rig.send(Event::ReadHorizon {
        conversation: CHAT.to_owned(),
    });
    rig.send(Event::Message {
        conversation: CHAT.to_owned(),
        id: "1767225600000".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message("1767225600000", "hello", "Lab B"))),
        source: Source::Delivery,
    });
    assert_eq!(rig.cx.read(|cx| rig.models.chats.read(cx).revision()), before + 3);
}

#[test]
fn the_engine_ending_its_events_marks_it_stopped() {
    let Rig {
        cx,
        events,
        models,
        shutdowns,
        ..
    } = Rig::start();

    drop(events);
    cx.run_until_parked();

    assert_eq!(cx.read(|cx| models.status.read(cx).engine), Engine::Stopped);
    assert_eq!(
        shutdowns.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "not shut down yet"
    );
}

#[test]
fn shutdown_ends_the_drain_and_shuts_the_engine_down_once() {
    let rig = Rig::start();
    rig.send(changed(CHAT));

    rig.cx.update(|cx| rig.bridge.update(cx, Bridge::shutdown).detach());
    rig.cx.run_until_parked();

    assert_eq!(rig.shutdowns(), 1);
    assert!(!rig.cx.read(|cx| rig.bridge.read(cx).running()));
    assert_eq!(rig.cx.read(|cx| rig.models.status.read(cx).engine), Engine::Stopped);
    // the drain is gone: an event the engine still had is never applied
    assert!(
        rig.events.send(changed("19:after@thread.v2")).is_err(),
        "the receiver went with the engine"
    );
    rig.cx.run_until_parked();
    assert_eq!(
        rig.cx
            .read(|cx| rig.models.chats.read(cx).changed_at("19:after@thread.v2")),
        0
    );

    rig.cx.update(|cx| rig.bridge.update(cx, Bridge::shutdown).detach());
    rig.cx.run_until_parked();
    assert_eq!(rig.shutdowns(), 1, "a second shutdown has nothing left to do");
}

/// A view that asks the conversation list once, as views will: through `Core`, awaiting its `Reply`.
struct ListView {
    shown: Option<Vec<Conversation>>,
}

#[test]
fn a_view_asks_through_core_and_its_query_answers_through_its_reply() {
    let rig = Rig::start();
    rig.core
        .conversations
        .lock()
        .expect("unpoisoned")
        .extend([conversation(CHAT), conversation("19:second@thread.v2")]);
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());

    let view = rig.cx.update(|cx| {
        cx.new(|cx| {
            let reply = core.conversations(1);
            cx.spawn(async move |this, cx| {
                let answer = reply.await;
                this.update(cx, |view: &mut ListView, cx| {
                    view.shown = answer.ok();
                    cx.notify();
                })
            })
            .detach();
            ListView { shown: None }
        })
    });
    rig.cx.run_until_parked();

    let shown = rig.cx.read(|cx| view.read(cx).shown.clone()).expect("answered");
    assert_eq!(shown.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(), [CHAT]);
    assert_eq!(rig.core.asked(), ["conversations"]);
}

fn trace() -> (Arc<ViewTrace>, Written) {
    let written = Written::default();
    (Arc::new(ViewTrace::new(written.clone())), written)
}

#[test]
fn sign_out_shows_the_hint_and_stops_the_engine() {
    let mut rig = Rig::start();
    let (trace, written) = trace();
    let models = rig.models.clone();
    let window = rig.cx.add_window(|_, cx| Root::new(models, trace, cx));

    rig.cx.update(|cx| {
        rig.bridge
            .update(cx, |bridge, cx| bridge.sign_out("run `proto-lab auth a`".into(), cx))
            .detach();
    });
    rig.cx.run_until_parked();

    let status = rig.cx.read(|cx| rig.models.status.read(cx).clone());
    assert_eq!(
        status.account,
        Account::SignedOut {
            hint: "run `proto-lab auth a`".into()
        }
    );
    assert_eq!(status.engine, Engine::Stopped);
    assert_eq!(rig.shutdowns(), 1, "nothing more is sent: the engine is shut down");
    assert_eq!(status_text(&status).as_ref(), "Signed out: run `proto-lab auth a`");
    let shown = window
        .read_with(&rig.cx, |root, _| root.shown())
        .expect("the window is open");
    assert_eq!(shown, Shown::SignedOut);
    let changes: Vec<String> = written
        .lines()
        .iter()
        .map(|line| format!("{} {}", line["view"], line["change"]))
        .collect();
    assert_eq!(changes, [r#""window" "opened""#, r#""status" "signed-out""#]);
}

#[test]
fn the_root_view_traces_its_opening_and_each_status_it_shows() {
    let mut rig = Rig::start();
    let (trace, written) = trace();
    let models = rig.models.clone();
    let _window = rig.cx.add_window(|_, cx| Root::new(models, trace, cx));

    rig.send(Event::Connected {
        endpoint: "00000000-0000-4000-8000-0000000000e1".to_owned(),
    });
    // a status change that shows nothing new traces nothing
    rig.send(Event::Registration(teams_trouter::RegistrationReport::Registered));
    rig.send(Event::Disconnected(Down::Closed));

    let lines = written.lines();
    let changes: Vec<(&str, &str)> = lines
        .iter()
        .map(|line| {
            (
                line["view"].as_str().unwrap_or(""),
                line["change"].as_str().unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        changes,
        [("window", "opened"), ("status", "connected"), ("status", "offline")]
    );
    assert_eq!(lines[0]["kinds"], serde_json::json!(["connecting"]));
    assert!(lines.iter().all(|line| line["at_ms"].as_u64().is_some_and(|at| at > 0)));
}

#[test]
fn no_message_text_or_name_reaches_the_trace() {
    let mut rig = Rig::start();
    let (trace, written) = trace();
    let models = rig.models.clone();
    let shared = Arc::clone(&trace);
    let _window = rig.cx.add_window(|_, cx| Root::new(models, shared, cx));
    let text = "the quarterly numbers are confidential";
    let name = "Lab Person";

    rig.send(Event::Message {
        conversation: CHAT.to_owned(),
        id: "1767225600001".to_owned(),
        change: MessageChange::New,
        applied: Applied::Inserted,
        message: Some(Box::new(message("1767225600001", text, name))),
        source: Source::CatchUp,
    });
    rig.send(Event::Typing {
        conversation: CHAT.to_owned(),
        from: Some(LAB_B.to_owned()),
        typing: true,
    });
    // a view that tried to trace text or a name as an id gets it left out, and counted
    trace.record(
        &TraceLine::new("chat", "shown")
            .conversation(CHAT)
            .messages(["1767225600001", text, name, "lab-b"])
            .kind("message"),
    );
    rig.cx
        .update(|cx| rig.models.signed_out("run `proto-lab auth a`".into(), cx));
    rig.cx.run_until_parked();

    let all = written.text();
    for secret in [text, name, "quarterly", "Lab B", "lab-b", "proto-lab auth"] {
        assert!(!all.contains(secret), "{secret:?} reached the trace:\n{all}");
    }
    let lines = written.lines();
    let chat = lines.iter().find(|line| line["view"] == "chat").expect("traced");
    assert_eq!(chat["conversation"], CHAT);
    assert_eq!(chat["messages"], serde_json::json!(["1767225600001"]));
    assert_eq!(chat["refused"], 3);
}

#[test]
fn a_trace_id_has_a_service_id_s_shape_only() {
    for id in [CHAT, LAB_B, "48:notifications", "1767225600001", "7"] {
        assert_eq!(TraceId::new(id).map(|id| id.as_str()), Some(id), "{id}");
    }
    let long = format!("19:{}", "a".repeat(600));
    for refused in [
        "",
        "hello",
        "hello world",
        "lab-b",
        "19: spaced",
        "Привет:мир",
        long.as_str(),
    ] {
        assert_eq!(TraceId::new(refused), None, "{refused}");
    }
}

#[test]
fn a_trace_that_is_off_writes_nothing_and_the_view_goes_on() {
    let mut cx = TestAppContext::single();
    let models = cx.update(teams_ui::Models::new);
    let window = cx.add_window(|_, cx| Root::new(models, Arc::new(ViewTrace::off()), cx));
    assert_eq!(
        window.read_with(&cx, |root, _| root.shown()).expect("open"),
        Shown::Connecting
    );
}
