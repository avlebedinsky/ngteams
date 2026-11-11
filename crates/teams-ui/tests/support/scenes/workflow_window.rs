//! Synthetic workflow manager rendered in the private UI sandbox, in Teams Light and Dark. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared offline test fake; this sandbox harness runs without cfg(test)"
)]
#[path = "../../common/mod.rs"]
mod common;
#[path = "../workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, WindowOptions,
    div, rgb,
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
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "this scene does not request fixture diagnostics")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};
const CHILD: &str = "TEAMS_UI_WORKFLOW_WINDOW";
pub(super) fn main() -> ExitCode {
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
    let Some(sandbox) = sandbox_or_skip("workflows")? else {
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
            "webhook-team-open",
            "webhook-team-changed",
            "webhook-rename",
            "webhook-off",
            "webhook-owners",
            "webhook-owner-search",
            "weekly",
            "weekly-bottom",
            "planner",
            "email",
            "list",
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
    sandbox.close_window()?;
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
    fixture::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "browse"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let run_inputs = page == "run-inputs";
    let webhook = page.starts_with("webhook");
    let page = page.to_owned();
    let application = fixture::application().with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
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
        } else if page == "webhook-off" {
            fixtures::stopped_webhook_answer
        } else if page == "list" {
            fixtures::saved_webhook_answer
        } else if page.starts_with("weekly") || page == "email" {
            fixtures::cards_answer
        } else if page == "planner" {
            fixtures::planner_answer
        } else if webhook {
            fixtures::webhook_answer
        } else {
            fixtures::answer
        });
        people(&core);
        let team = synthetic_team();
        *core.teams.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vec![team.clone(), other_team()]);
        let core: Arc<dyn Core> = core;
        let channel = team.channels[0].clone();
        let options = WindowOptions {
            window_bounds: Some(teams_ui::window::initial_bounds(
                gpui::size(gpui::px(1100.0), gpui::px(720.0)),
                cx,
            )),
            #[cfg(windows)]
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("NGTEAMS SYNTHETIC WORKFLOW".into()),
                ..Default::default()
            }),
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
                    then_page(&dialog, &page, window, cx);
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
/// The owners' names and who the co-owner search finds, all synthetic.
fn people(core: &common::FakeCore) {
    let mut names = core.names.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    names.insert(format!("8:orgid:{}", fixtures::OWNER), "Synthetic Maker".to_owned());
    names.insert(format!("8:orgid:{}", fixtures::CO_OWNER), "Synthetic Helper".to_owned());
    *core.addable.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = vec![teams_proto::TeamPerson {
        mri: "8:orgid:00000000-0000-4000-8000-00000000000c".into(),
        name: "Synthetic Newcomer".into(),
        email: Some("newcomer@example.test".into()),
        job_title: None,
    }];
}
fn other_team() -> Team {
    Team {
        id: "synthetic-other-team".into(),
        group_id: Some("synthetic-other-group".into()),
        name: "Other synthetic team".into(),
        channels: vec![TeamChannel {
            id: "synthetic-planning".into(),
            name: "Planning".into(),
            general: false,
            kind: teams_store::ChannelKind::Standard,
            shown: true,
            about: teams_store::ChannelAbout::default(),
        }],
        ..Team::default()
    }
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
        "webhook" | "webhook-collapsed" | "webhook-team-open" | "webhook-team-changed" => {
            dialog.update(cx, |view, cx| view.select_template("synthetic-webhook", window, cx));
        }
        "webhook-overview"
        | "webhook-delete"
        | "webhook-rename"
        | "webhook-off"
        | "webhook-owners"
        | "webhook-owner-search" => dialog.update(cx, |view, cx| view.open_workflow("synthetic-flow", cx)),
        "weekly" | "weekly-bottom" => {
            dialog.update(cx, |view, cx| view.select_template("synthetic-template", window, cx));
        }
        "planner" => dialog.update(cx, |view, cx| view.select_template("synthetic-planner", window, cx)),
        "email" => dialog.update(cx, |view, cx| view.select_template("synthetic-email", window, cx)),
        _ => {}
    }
}
/// The step a page takes once its screen is open: folding the card, asking to delete, renaming, opening or changing
/// the team, scrolling to the Owners and searching for a co-owner.
fn then_page(dialog: &Entity<WorkflowDialog>, page: &str, window: &mut Window, cx: &mut App) {
    match page {
        "webhook-collapsed" => dialog.update(cx, WorkflowDialog::collapse_all),
        "webhook-delete" => dialog.update(cx, WorkflowDialog::ask_delete),
        "webhook-rename" => dialog.update(cx, |view, cx| view.rename_open_workflow(window, cx)),
        "webhook-team-open" => dialog.update(cx, |view, cx| view.open_team_choices(window, cx)),
        "webhook-team-changed" => dialog.update(cx, |view, cx| view.choose_team("synthetic-other-group", window, cx)),
        "webhook-owners" | "weekly-bottom" | "list" => dialog.update(cx, WorkflowDialog::scroll_to_end),
        "webhook-owner-search" => dialog.update(cx, |view, cx| {
            view.scroll_to_end(cx);
            view.search_co_owner("Synth", window, cx);
        }),
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
