use serde_json::json;
use teams_wpe::{
    Button, EditingCommand, Event, Geometry, Input, Key, Modifiers, OptionKind, Pointer, PointerKind, Rgba, Scroll,
    ViewId,
};

use super::super::{Result, page, pump::Engine};

pub(super) fn run(engine: &mut Engine, view: ViewId) -> Result {
    engine.host.set_visible(view, true);
    engine.host.set_focused(view, true);
    engine.until(view, "document.hasFocus()", |value| value == true)?;
    engine.eval(view, "window.fieldPressed=false;document.getElementById('field').addEventListener('mouseup',()=>window.fieldPressed=true);true", None)?;
    click(engine, view, "#field", Button::Primary)?;
    engine.until(view, "window.fieldPressed", |value| value == true)?;
    key(engine, view, 0xff51, Some(113));
    engine.until(view, "window.events.includes('ArrowLeft')", |value| value == true)?;
    engine.eval(
        view,
        "let f=document.getElementById('field');f.value='';f.focus();f.setSelectionRange(0,0);true",
        None,
    )?;
    engine.wait(|event| matches!(event, Event::ImeCursorArea { view: id, area } if *id == view && area.height > 0))?;
    engine.host.ime_preedit(view, "仮".into(), 1);
    engine.host.ime_commit(view, "合成".into());
    engine.until(view, "document.getElementById('field').value", |value| value == "合成")?;
    clipboard(engine, view)?;
    keys(engine, view)?;
    options(engine, view)?;
    context(engine, view)?;
    geometry(engine, view)?;
    Ok(())
}

fn clipboard(engine: &mut Engine, view: ViewId) -> Result {
    engine.host.editing_command(view, EditingCommand::SelectAll);
    engine.host.set_clipboard("synthetic paste αβ".into());
    engine.host.editing_command(view, EditingCommand::PasteAsPlainText);
    engine.until(view, "document.getElementById('field').value", |value| {
        value == "synthetic paste αβ"
    })?;
    engine.host.editing_command(view, EditingCommand::SelectAll);
    engine.host.editing_command(view, EditingCommand::Copy);
    engine.wait(|event| matches!(event, Event::Clipboard { text, .. } if text == "synthetic paste αβ"))?;
    engine.host.editing_command(view, EditingCommand::Cut);
    engine.until(view, "document.getElementById('field').value", |value| value == "")?;
    engine.host.editing_command(view, EditingCommand::Paste);
    engine.until(view, "document.getElementById('field').value", |value| {
        value == "synthetic paste αβ"
    })?;
    engine.host.editing_command(view, EditingCommand::Undo);
    engine.until(view, "document.getElementById('field').value", |value| value == "")?;
    engine.host.editing_command(view, EditingCommand::Redo);
    engine.until(view, "document.getElementById('field').value", |value| {
        value == "synthetic paste αβ"
    })?;
    engine.host.editing_command(view, EditingCommand::SelectAll);
    engine.host.set_clipboard("x".repeat(65_536));
    engine.host.editing_command(view, EditingCommand::Paste);
    engine.until(view, "document.getElementById('field').value.length", |value| {
        value == 65_536
    })?;
    Ok(())
}

fn key(engine: &Engine, view: ViewId, keyval: u32, keycode: Option<u32>) {
    for pressed in [true, false] {
        engine.host.input(
            view,
            Input::Key(Key {
                pressed,
                keyval,
                keycode,
                modifiers: Modifiers::empty(),
            }),
        );
    }
}

fn keys(engine: &mut Engine, view: ViewId) -> Result {
    engine.host.editing_command(view, EditingCommand::SelectAll);
    key(engine, view, 0x61, Some(38));
    engine.until(view, "document.getElementById('field').value", |value| value == "a")?;
    key(engine, view, 0xff08, None);
    engine.until(view, "document.getElementById('field').value", |value| value == "")?;
    engine.eval(
        view,
        "window.wheels=0;window.wheelReady=false;document.addEventListener('wheel',e=>{window.wheels++;e.preventDefault()},{passive:false});requestAnimationFrame(()=>requestAnimationFrame(()=>window.wheelReady=true));true",
        None,
    )?;
    engine.until(view, "window.wheelReady", |value| value == true)?;
    pointer(engine, view, PointerKind::Move, 250.0, 210.0);
    engine.host.input(
        view,
        Input::Scroll(Scroll {
            x: 250.0,
            y: 210.0,
            delta_x: 0.0,
            delta_y: -1.0,
            precise: false,
            stop: false,
            modifiers: Modifiers::empty(),
        }),
    );
    engine.until(view, "window.wheels", |value| {
        value.as_u64().is_some_and(|count| count > 0)
    })?;
    engine.host.input(
        view,
        Input::Scroll(Scroll {
            x: 25.0,
            y: 25.0,
            delta_x: 0.5,
            delta_y: 1.5,
            precise: true,
            stop: true,
            modifiers: Modifiers::empty(),
        }),
    );
    Ok(())
}

fn pointer(engine: &Engine, view: ViewId, kind: PointerKind, x: f64, y: f64) {
    engine.host.input(
        view,
        Input::Pointer(Pointer {
            kind,
            x,
            y,
            modifiers: Modifiers::empty(),
        }),
    );
}

pub(super) fn click(engine: &mut Engine, view: ViewId, selector: &str, button: Button) -> Result {
    let script = format!(
        "(()=>{{const r=document.querySelector({}).getBoundingClientRect();return [r.left+r.width/2,r.top+r.height/2]}})()",
        serde_json::to_string(selector)?
    );
    let position = engine.eval(view, &script, None)?;
    let (x, y) = (
        position[0].as_f64().ok_or("no pointer x")?,
        position[1].as_f64().ok_or("no pointer y")?,
    );
    pointer(engine, view, PointerKind::Enter, x, y);
    pointer(engine, view, PointerKind::Move, x, y);
    pointer(engine, view, PointerKind::Down(button), x, y);
    pointer(engine, view, PointerKind::Up(button), x, y);
    Ok(())
}

fn options(engine: &mut Engine, view: ViewId) -> Result {
    click(engine, view, "#choices", Button::Primary)?;
    let Event::OptionMenu(menu) = engine.wait(|event| matches!(event, Event::OptionMenu(menu) if menu.view == view))?
    else {
        unreachable!()
    };
    assert!(menu.area.width > 0 && menu.area.height > 0);
    assert!(
        menu.items
            .iter()
            .any(|item| item.kind == OptionKind::GroupLabel && item.label == "group")
    );
    assert!(menu.items.iter().any(|item| item.label == "disabled" && !item.enabled));
    let row = menu
        .items
        .iter()
        .position(|item| item.label == "third")
        .ok_or("no third option")?;
    assert_eq!(menu.items[row].kind, OptionKind::GroupChild);
    assert_eq!(menu.items[row].tooltip.as_deref(), Some("synthetic tooltip"));
    engine.host.choose_option(menu.token, Some(u32::try_from(row)?));
    engine.until(view, "document.getElementById('choices').value", |value| {
        value == "third"
    })?;
    click(engine, view, "#choices", Button::Primary)?;
    let Event::OptionMenu(menu) = engine.wait(|event| matches!(event, Event::OptionMenu(menu) if menu.view == view))?
    else {
        unreachable!()
    };
    engine.host.choose_option(menu.token, None);
    assert_eq!(
        engine.eval(view, "document.getElementById('choices').value", None)?,
        json!("third")
    );
    Ok(())
}

fn context(engine: &mut Engine, view: ViewId) -> Result {
    click(engine, view, "#link", Button::Secondary)?;
    let Event::ContextMenu(menu) =
        engine.wait(|event| matches!(event, Event::ContextMenu(menu) if menu.view == view))?
    else {
        unreachable!()
    };
    assert_eq!(menu.link.as_deref(), Some(page::TWO));
    assert!(!menu.editable);
    assert!(menu.x >= 0 && menu.y >= 0);
    pointer(engine, view, PointerKind::Leave, 0.0, 0.0);
    Ok(())
}

fn geometry(engine: &mut Engine, view: ViewId) -> Result {
    engine.host.set_zoom(view, 2.0);
    engine.until(view, "window.innerWidth", |value| {
        value.as_u64().is_some_and(|width| width <= 160)
    })?;
    engine.host.set_zoom(view, 1.0);
    engine.host.set_geometry(
        view,
        Geometry {
            width: 320,
            height: 240,
            scale: 2.0,
        },
    );
    engine.frame(view, 640, 480)?;
    engine.until(view, "window.devicePixelRatio", |value| value == 2)?;
    engine.host.set_geometry(
        view,
        Geometry {
            width: 480,
            height: 300,
            scale: 1.0,
        },
    );
    engine.frame(view, 480, 300)?;
    engine.host.set_visible(view, false);
    engine.until(view, "document.visibilityState", |value| value == "hidden")?;
    engine.host.set_visible(view, true);
    engine.until(view, "document.visibilityState", |value| value == "visible")?;
    engine.host.set_focused(view, false);
    engine.until(view, "document.hasFocus()", |value| value == false)?;
    engine.host.set_focused(view, true);
    engine.until(view, "document.hasFocus()", |value| value == true)?;
    background(engine, view)?;
    engine.host.set_geometry(
        view,
        Geometry {
            width: 320,
            height: 240,
            scale: 1.0,
        },
    );
    engine.frame(view, 320, 240)?;
    Ok(())
}

fn background(engine: &mut Engine, view: ViewId) -> Result {
    engine.eval(
        view,
        "document.documentElement.style.background='transparent';document.body.style.background='transparent';true",
        None,
    )?;
    for (color, expected) in [(Some(Rgba::TRANSPARENT), [0, 0, 0, 0]), (None, [255, 255, 255, 255])] {
        engine.host.set_background(view, color);
        // Pixel bytes are directly observable for SHM hosts. DMA hosts exercise the
        // same operation and verify their imported buffers separately.
        if engine.frames.get(&view).is_some_and(|frame| frame.shm) {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while engine.frames.get(&view).and_then(|frame| frame.corner) != Some(expected) {
                if std::time::Instant::now() >= end {
                    return Err(format!("background expected {expected:?}, frames={:?}", engine.frames).into());
                }
                engine.pump()?;
            }
        }
    }
    Ok(())
}
