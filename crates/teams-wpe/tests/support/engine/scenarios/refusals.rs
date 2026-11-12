//! What a page may not do, refused for it: its own closing is only asked for, permissions are denied, file choosers
//! cancelled, leaving is decided silently, and downloads never start.

use serde_json::json;
use teams_wpe::{Button, Event, Geometry, Load, Rgba, SessionId};

use super::super::{Result, page, pump::Engine};

const HTML: &str = r#"<!doctype html><title>refusals</title>
<style>input{display:block;margin:10px;width:180px;height:28px}</style>
<input id="file" type="file">"#;

pub(super) fn run(engine: &mut Engine, session: SessionId) -> Result {
    let mut spec = super::spec(session);
    spec.background = Some(Rgba::TRANSPARENT);
    let view = super::create(engine, spec)?;
    engine.load(
        view,
        Load::Html {
            html: HTML.into(),
            base_uri: Some(page::SECURE.into()),
        },
    )?;
    engine.host.set_visible(view, true);
    engine.host.set_focused(view, true);
    // a scale that is no scale counts as 1
    engine.host.set_geometry(
        view,
        Geometry {
            width: 300,
            height: 200,
            scale: 0.0,
        },
    );
    engine.frame(view, 300, 200)?;
    engine.eval(view, "window.close();true", None)?;
    engine.wait(|event| matches!(event, Event::CloseRequested { view: id } if *id == view))?;
    assert_eq!(engine.eval(view, "window.isSecureContext", None)?, json!(true));
    engine.eval(
        view,
        "window.permission=null;Notification.requestPermission().then(p=>window.permission=p);true",
        None,
    )?;
    engine.until(view, "window.permission", |value| value == "denied")?;
    engine.eval(view, "window.chosen=false;document.getElementById('file').addEventListener('click',()=>window.chosen=true);window.onbeforeunload=e=>{e.preventDefault();e.returnValue='stay'};true", None)?;
    super::input::click(engine, view, "#file", Button::Primary)?;
    engine.until(view, "window.chosen", |value| value == true)?;
    assert_eq!(
        engine.eval(view, "document.getElementById('file').files.length", None)?,
        json!(0)
    );
    // the page asks to stay on leaving, after its user interacted with it: it leaves anyway
    engine.load(view, Load::Uri(page::TWO.into()))?;
    engine.until(view, "document.title", |value| value == "two")?;
    engine.eval(
        view,
        "const a=document.createElement('a');a.href='synthetic://fixture/one';a.download='one.html';document.body.append(a);a.click();true",
        None,
    )?;
    let refused = engine.wait(|event| matches!(event, Event::DownloadRefused { session: id, .. } if *id == session))?;
    assert!(
        matches!(&refused, Event::DownloadRefused { view: Some(id), uri, .. } if *id == view && uri == page::ONE),
        "{refused:?}"
    );
    engine.host.close_view(view);
    Ok(())
}
