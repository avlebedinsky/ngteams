//! Synthetic workflow manager rendered in the private UI sandbox, in Teams Light and Dark. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared offline test fake; this sandbox harness runs without cfg(test)"
)]
mod common;
#[path = "support/workflow_fixtures.rs"]
mod fixtures;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, WindowOptions,
    WindowingModes, div, rgb,
};
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};
use teams_store::{Team, TeamChannel};
use teams_ui::{
    Core,
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
    workflow_dialog::WorkflowDialog,
};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};
const CHILD: &str = "TEAMS_UI_WORKFLOW_WINDOW";
fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |mode| child(&mode));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("workflow window: {error}");
            ExitCode::FAILURE
        }
    }
}
fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}
fn shots() -> io::Result<PathBuf> {
    let p = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .unwrap_or(Path::new("target"))
        .join("ui-shots");
    fs::create_dir_all(&p)?;
    Ok(p)
}
fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("workflows", &options)? else {
        return Ok(());
    };
    for (theme, label) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in [
            "browse",
            "configure",
            "run-inputs",
            "webhook",
            "webhook-collapsed",
            "webhook-overview",
            "webhook-delete",
        ] {
            shot(&sandbox, theme, label, page)?;
        }
    }
    Ok(())
}
fn shot(sandbox: &Sandbox, theme: &str, label: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}|{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, mode.as_str())])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no workflow ready: {error:?}")))?;
    let name = format!("workflow-{label}-{page}.png");
    let screenshot = sandbox.screenshot(&name, true)?;
    fs::copy(screenshot, shots()?.join(&name))?;
    say(&format!("shot {name}"));
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("workflow child {status}: {stderr}").into());
    }
    Ok(())
}
struct Stage(Entity<WorkflowDialog>);
impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(theme::current(cx).background))
            .child(self.0.clone())
    }
}
fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "browse"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let run_inputs = page == "run-inputs";
    let webhook = page.starts_with("webhook");
    let page = page.to_owned();
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        teams_ui::window::init(cx);
        theme::set_appearance(
            &Appearance {
                theme,
                follow_system: false,
                ..Appearance::default()
            },
            cx,
        );
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let core = Arc::new(common::FakeCore::default());
        *core
            .workflow_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(if run_inputs {
            fixtures::input_answer
        } else if webhook {
            fixtures::webhook_answer
        } else {
            fixtures::answer
        });
        let team = synthetic_team();
        *core.teams.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vec![team.clone()]);
        let core: Arc<dyn Core> = core;
        let channel = team.channels[0].clone();
        let options = WindowOptions {
            app_id: Some("ngteams-workflow-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let dialog = cx.new(|cx| WorkflowDialog::new(core, team, channel, window, cx));
            let stage = cx.new(|_| Stage(dialog.clone()));
            let root = cx.new(|cx| KitRoot::new(stage, window, cx));
            after(window, 15, move |window, cx| {
                open_page(&dialog, &page, window, cx);
                after(window, 15, move |window, cx| {
                    then_page(&dialog, &page, cx);
                    after(window, 15, |_, _| say("ready"));
                });
            });
            root
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}
fn synthetic_team() -> Team {
    let channel = |id: &str, name: &str| TeamChannel {
        id: id.into(),
        name: name.into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    Team {
        id: "synthetic-team".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        channels: vec![
            channel("synthetic-channel", "General"),
            channel("synthetic-alerts", "Alerts"),
        ],
        ..Team::default()
    }
}
/// Opens the screen `page` shows.
fn open_page(dialog: &Entity<WorkflowDialog>, page: &str, window: &mut Window, cx: &mut App) {
    match page {
        "configure" => dialog.update(cx, |view, cx| view.select_template("synthetic-template", window, cx)),
        "run-inputs" => dialog.update(cx, |view, cx| view.prepare_workflow_run("synthetic-flow", window, cx)),
        "webhook" | "webhook-collapsed" => {
            dialog.update(cx, |view, cx| view.select_template("synthetic-webhook", window, cx));
        }
        "webhook-overview" | "webhook-delete" => dialog.update(cx, |view, cx| view.open_workflow("synthetic-flow", cx)),
        _ => {}
    }
}
/// The step a page takes once its screen is open: folding the card, or asking to delete.
fn then_page(dialog: &Entity<WorkflowDialog>, page: &str, cx: &mut App) {
    match page {
        "webhook-collapsed" => dialog.update(cx, WorkflowDialog::collapse_all),
        "webhook-delete" => dialog.update(cx, WorkflowDialog::ask_delete),
        _ => {}
    }
}
fn after(window: &mut Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames == 0 {
            done(window, cx);
        } else {
            after(window, frames - 1, done);
            window.refresh();
        }
    });
    window.refresh();
}
