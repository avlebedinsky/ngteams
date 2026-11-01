use serde_json::json;
use teams_wpe::{Event, InjectionTime, Load, LoadErrorKind, UserScript, ViewId};

use super::super::{Result, page, pump::Engine};

pub(super) fn run(engine: &mut Engine, view: ViewId) -> Result {
    let scripts = vec![
        UserScript {
            source: "window.injectedStart='synthetic-start'".into(),
            time: InjectionTime::Start,
            world: None,
        },
        UserScript {
            source: "window.injectedEnd='synthetic-end'".into(),
            time: InjectionTime::End,
            world: None,
        },
        UserScript {
            source: "window.privateValue=17".into(),
            time: InjectionTime::Start,
            world: Some("isolated".into()),
        },
    ];
    engine.host.set_scripts(view, scripts);
    engine.load(view, Load::Uri(page::ONE.into()))?;
    engine.frame(view, 320, 240)?;
    assert_eq!(
        engine.eval(view, "[document.title,pageSawStart,window.injectedEnd]", None)?,
        json!(["one", "synthetic-start", "synthetic-end"])
    );
    assert_eq!(
        engine.eval(view, "typeof window.privateValue", None)?,
        json!("undefined")
    );
    assert_eq!(engine.eval(view, "window.privateValue", Some("isolated"))?, json!(17));
    assert!(engine.eval(view, "throw new Error('synthetic failure')", None).is_err());
    assert!(engine.eval(view, "undefined", None)?.is_null());
    eprintln!("native stage: script replies");
    scripts_and_replies(engine, view)?;
    assert_eq!(engine.eval(view, "[confirm('synthetic confirm'),prompt('synthetic prompt','seed'),(()=>{alert('synthetic alert');return true})()]", None)?, json!([false, null, true]));
    assert_eq!(
        engine.eval(view, "window.open('synthetic://fixture/two')===null", None)?,
        true
    );
    eprintln!("native stage: history");
    history(engine, view)?;
    eprintln!("native stage: scheme errors");
    scheme_errors(engine, view)?;
    eprintln!("native stage: html load");
    engine.load(
        view,
        Load::Html {
            html: page::HTML.into(),
            base_uri: Some(page::ONE.into()),
        },
    )?;
    assert_eq!(engine.eval(view, "document.title", None)?, json!("one"));
    Ok(())
}

fn scripts_and_replies(engine: &mut Engine, view: ViewId) -> Result {
    engine.eval(view, "window.bridgeResult=null;window.webkit.messageHandlers.bridge.postMessage({action:'ok',n:7}).then(v=>window.bridgeResult=v);true", None)?;
    assert_eq!(
        engine.until(view, "window.bridgeResult", serde_json::Value::is_object)?,
        json!({"echo": {"action":"ok","n":7}})
    );
    engine.eval(view, "window.bridgeError=null;window.webkit.messageHandlers.bridge.postMessage({action:'reject'}).catch(e=>window.bridgeError=String(e));true", None)?;
    let rejected = engine.until(view, "window.bridgeError", serde_json::Value::is_string)?;
    assert!(rejected.as_str().unwrap().contains("synthetic rejection"));
    engine.eval(view, "window.invalidReply=null;window.webkit.messageHandlers.bridge.postMessage({action:'invalid-json'}).catch(e=>window.invalidReply=String(e));true", None)?;
    let invalid = engine.until(view, "window.invalidReply", serde_json::Value::is_string)?;
    assert!(invalid.as_str().unwrap().contains("reply is not JSON"));
    engine.eval(view, "window.primitiveReply=false;window.webkit.messageHandlers.bridge.postMessage(17).then(v=>window.primitiveReply=v.echo===null);true", None)?;
    engine.until(view, "window.primitiveReply", |value| value == true)?;
    engine.eval(view, "window.privateResult=null;window.webkit.messageHandlers.private.postMessage({isolated:true}).then(v=>window.privateResult=v);true", Some("isolated"))?;
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let value = engine.eval(view, "window.privateResult", Some("isolated"))?;
        if value.is_object() {
            assert_eq!(value, json!({"echo":{"isolated":true}}));
            break;
        }
        assert!(std::time::Instant::now() < end);
    }
    let before = engine.replies;
    engine.eval(view, "window.largeReply=0;window.webkit.messageHandlers.bridge.postMessage({data:'x'.repeat(65536)}).then(v=>window.largeReply=v.echo.data.length);true", None)?;
    engine.until(view, "window.largeReply", |value| value == 65_536)?;
    assert!(engine.replies > before);
    Ok(())
}

fn history(engine: &mut Engine, view: ViewId) -> Result {
    engine.load(view, Load::Uri(page::TWO.into()))?;
    engine.wait(|event| matches!(event, Event::Page { view: id, page } if *id == view && page.can_go_back && page.title.as_deref() == Some("two")))?;
    engine.host.go_back(view);
    engine.loaded(view)?;
    engine.until(view, "document.title", |value| value == "one")?;
    engine.host.go_forward(view);
    engine.loaded(view)?;
    engine.until(view, "document.title", |value| value == "two")?;
    engine.host.reload(view);
    engine.loaded(view)?;
    assert_eq!(engine.eval(view, "document.title", None)?, json!("two"));
    engine.load(view, Load::Uri("synthetic://fixture/file".into()))?;
    assert_eq!(engine.eval(view, "document.title", None)?, json!("file"));
    Ok(())
}

fn scheme_errors(engine: &mut Engine, view: ViewId) -> Result {
    for suffix in [
        "not-found",
        "permission",
        "is-directory",
        "failed",
        "missing-file",
        "directory",
    ] {
        let uri = format!("synthetic://fixture/{suffix}");
        engine.host.load(view, Load::Uri(uri.clone()));
        let failed = engine.wait(
            |event| matches!(event, Event::LoadFailed { view: id, uri: actual, .. } if *id == view && actual == &uri),
        )?;
        assert!(matches!(failed, Event::LoadFailed { error, .. } if error.kind == LoadErrorKind::Failed));
        engine.loaded(view)?;
    }
    engine.host.load(view, Load::Uri("synthetic://fixture/hold".into()));
    engine.wait(|event| matches!(event, Event::SchemeRequest(request) if request.uri.ends_with("/hold")))?;
    engine.host.stop(view);
    engine.wait(|event| matches!(event, Event::LoadFailed { view: id, error, .. } if *id == view && error.kind == LoadErrorKind::Cancelled))?;
    engine.loaded(view)?;
    for token in engine.held.drain(..) {
        engine.host.finish_scheme_request(
            token,
            teams_wpe::SchemeResponse::Error {
                kind: teams_wpe::SchemeError::Failed,
                message: "synthetic cancelled request completed".into(),
            },
        );
    }
    Ok(())
}
