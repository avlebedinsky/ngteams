//! The Workflows app in the main window over synthetic data, rendered in the private UI sandbox in Teams Light and
//! Dark: Home, its next page and suggestions, Templates and its Filter, "Your workflows", a template opened from the
//! app, Build from scratch and its question, the Chat tab and the rail's More apps. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic view-test engine, no live requests"
)]
mod common;
#[path = "support/workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;
use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, Entity, Window, WindowingModes};
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use teams_proto::{WorkflowRequest, WorkflowResponse};
use teams_store::{Team, TeamChannel};
use teams_ui::{
    Bridge, Core, Models, ViewTrace,
    settings::{Settings, SettingsModel},
    shell::{Shell, ShellParts},
    theme::{Appearance, DARK_KEY, LIGHT_KEY},
    workflow_dialog::AppTab,
};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};
const CHILD: &str = "TEAMS_UI_WORKFLOWS_APP_WINDOW";
const PAGES: [&str; 12] = [
    "home",
    "home-next",
    "home-find",
    "templates",
    "templates-filter",
    "all",
    "webhook",
    "weekly",
    "build",
    "build-ask",
    "chat",
    "more-apps",
];
fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}
fn main() -> ExitCode {
    let result = match std::env::var(CHILD) {
        Ok(value) => child(&value),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("workflows app window: {error}");
            ExitCode::FAILURE
        }
    }
}
fn shots() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip(
        "workflows app",
        &SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            ..SandboxOptions::default()
        },
    )?
    else {
        return Ok(());
    };
    let only = std::env::var("WORKFLOWS_APP_PAGES").ok();
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in PAGES {
            if only
                .as_deref()
                .is_some_and(|only| !only.split(',').any(|wanted| wanted == page))
            {
                continue;
            }
            shot(&sandbox, theme, name, page).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    Ok(())
}
fn shot(sandbox: &Sandbox, theme: &str, name: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}:{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    let file = format!("workflows-app-{page}-{name}.png");
    let png = sandbox.screenshot(&file, true)?;
    let kept = shots()?.join(&file);
    fs::copy(png, &kept)?;
    say(&format!("shot {}", kept.display()));
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}
fn after_frames(window: &mut Window, n: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if n <= 1 {
            done(window, cx);
        } else {
            after_frames(window, n - 1, done);
            window.refresh();
        }
    });
    window.refresh();
}
fn team() -> Team {
    Team {
        id: "19:synthetic-team@thread.tacv2".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        channels: vec![TeamChannel {
            id: "synthetic-channel".into(),
            name: "General".into(),
            general: true,
            kind: teams_store::ChannelKind::Standard,
            shown: true,
            about: teams_store::ChannelAbout::default(),
        }],
        ..Team::default()
    }
}
/// The gallery and two workflows, one of them off; the weekly card for "weekly".
fn answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::List { .. } => Ok(serde_json::json!({"value": [
            fixtures::webhook_flow("Started"),
            fixtures::stopped_flow()
        ]})),
        WorkflowRequest::Get { flow, .. } if flow == "synthetic-off" => Ok(fixtures::stopped_flow()),
        WorkflowRequest::Template { id } if id == "synthetic-template" => fixtures::cards_answer(request),
        WorkflowRequest::Gallery { .. } => fixtures::answer(request),
        other => fixtures::webhook_answer(other),
    }
}
fn child(value: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, page) = value.split_once(':').ok_or("bad child mode")?;
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, page, cx) {
            eprintln!("workflows app open: {error}");
            cx.quit();
        }
    });
    Ok(())
}
fn open(theme: &'static str, page: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    *core.teams.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vec![team()]);
    *core
        .workflow_handler
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(answer);
    let (events, received) = mpsc::unbounded_channel();
    let models = Models::new(cx);
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, "ngteams-workflows-app-test", "Workflows".into(), parts)?;
    let root = teams_ui::window::content(window, cx).ok_or_else(|| std::io::Error::other("missing shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing shell"))?;
    window.update(cx, |_, window, _| {
        after_frames(window, 30, move |window, cx| {
            step(&shell, &page, window, cx);
            after_frames(window, 30, move |window, cx| {
                then(&shell, &page, window, cx);
                after_frames(window, 30, |_, _| say("ready"));
            });
        });
    })?;
    std::mem::forget((bridge, events));
    Ok(())
}
/// Opens the app (or the More apps flyout) at the page's tab.
fn step(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    if page == "more-apps" {
        shell.update(cx, Shell::toggle_more_apps);
        return;
    }
    shell.update(cx, |shell, cx| shell.open_workflows(page == "all", window, cx));
    let Some(app) = shell.read(cx).workflows().cloned() else {
        return;
    };
    match page {
        "templates" | "templates-filter" => app.update(cx, |app, cx| app.show_tab(AppTab::Templates, window, cx)),
        "chat" => app.update(cx, |app, cx| app.show_tab(AppTab::Chat, window, cx)),
        _ => {}
    }
}
/// The page's step once its tab shows.
fn then(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    let Some(app) = shell.read(cx).workflows().cloned() else {
        return;
    };
    let pages = app.read(cx).pages().clone();
    pages.update(cx, |pages, cx| match page {
        "home-next" => pages.next_templates(cx),
        "home-find" => pages.find_template("planner", window, cx),
        "templates-filter" => pages.open_filter(cx),
        "webhook" => pages.open_app_template("synthetic-webhook", window, cx),
        "weekly" => pages.open_app_template("synthetic-template", window, cx),
        "build" => pages.build_from_scratch(cx),
        "build-ask" => {
            pages.build_from_scratch(cx);
            pages.ask_build_in_power_automate(cx);
        }
        _ => {}
    });
}
