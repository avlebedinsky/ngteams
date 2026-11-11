//! Stores of every kind, and what a page leaves open when it closes.

use serde_json::json;
use teams_wpe::{Event, Load, PolicyKind, SessionId, SessionSpec};

use super::super::{Result, page, pump::Engine};

pub(super) fn run(engine: &mut Engine) -> Result {
    let data_dir = engine.root.join("store-data");
    let persistent = engine.host.create_session(SessionSpec::Persistent {
        data_dir: data_dir.clone(),
        cache_dir: engine.root.join("store-cache"),
    });
    one_page(engine, persistent)?;
    engine.host.clear_session(persistent);
    engine
        .wait(|event| matches!(event, Event::SessionCleared { session, result: Ok(()) } if *session == persistent))?;
    assert!(data_dir.is_dir(), "a persistent store keeps its data on disk");
    engine.host.drop_session(persistent);
    // nothing listens on the discard port: only loads past the proxy could fail, and the scheme's never go through it
    let guarded = engine.host.create_session(SessionSpec::Guarded {
        proxy: "http://127.0.0.1:9".into(),
        ignore_hosts: vec!["localhost".into()],
    });
    one_page(engine, guarded)?;
    engine.host.drop_session(guarded);
    let session = engine.host.create_session(SessionSpec::Ephemeral);
    denied_navigation(engine, session)?;
    closed_with_questions_open(engine, session)?;
    engine.host.drop_session(session);
    Ok(())
}

/// A page on `session` shows the first page, then closes.
fn one_page(engine: &mut Engine, session: SessionId) -> Result {
    let view = super::create(engine, super::spec(session))?;
    engine.load(view, Load::Uri(page::ONE.into()))?;
    assert_eq!(engine.eval(view, "document.title", None)?, json!("one"));
    engine.host.close_view(view);
    Ok(())
}

/// A navigation the consumer refuses leaves the page where it was.
fn denied_navigation(engine: &mut Engine, session: SessionId) -> Result {
    let view = super::create(engine, super::spec(session))?;
    engine.load(view, Load::Uri(page::ONE.into()))?;
    let denied = "synthetic://elsewhere/denied";
    engine.eval(view, &format!("location.href={denied:?};true"), None)?;
    engine.wait(|event| {
        matches!(event, Event::Policy(request) if matches!(&request.kind, PolicyKind::Navigation { uri, .. } if uri == denied))
    })?;
    assert_eq!(
        engine.eval(view, "[document.title,location.href]", None)?,
        json!(["one", page::ONE])
    );
    engine.host.close_view(view);
    Ok(())
}

/// A page closed while a navigation, a script message and a read of the scheme wait for the consumer: each is
/// answered for it, and the consumer's late answers change nothing.
fn closed_with_questions_open(engine: &mut Engine, session: SessionId) -> Result {
    let view = super::create(engine, super::spec(session))?;
    engine.load(view, Load::Uri(page::ONE.into()))?;
    engine.eval(
        view,
        "window.heldReply=null;window.webkit.messageHandlers.bridge.postMessage({action:'hold'}).then(v=>window.heldReply=v,e=>window.heldReply=String(e));new Image().src='synthetic://fixture/hold';true",
        None,
    )?;
    engine.wait(|event| matches!(event, Event::ScriptMessage(message) if message.view == view))?;
    engine.wait(|event| matches!(event, Event::SchemeRequest(request) if request.view == Some(view) && request.uri.ends_with("/hold")))?;
    engine.eval(view, "location.href='synthetic://fixture/hold-policy';true", None)?;
    engine.wait(|event| {
        matches!(event, Event::Policy(request) if request.view == view && matches!(&request.kind, PolicyKind::Navigation { uri, .. } if uri.ends_with("/hold-policy")))
    })?;
    assert_eq!(engine.held_policies.len(), 1);
    assert_eq!(engine.held_replies.len(), 1);
    engine.host.close_view(view);
    for token in engine.held_policies.drain(..) {
        engine.host.decide_policy(token, true);
    }
    for token in engine.held_replies.drain(..) {
        engine.host.reply_script_message(token, Ok("{}".into()));
    }
    for token in engine.held.drain(..) {
        engine.host.finish_scheme_request(
            token,
            teams_wpe::SchemeResponse::Bytes {
                data: b"synthetic late answer".to_vec(),
                mime: "text/plain".into(),
            },
        );
    }
    // the engine still serves pages after the late answers
    one_page(engine, session)
}
