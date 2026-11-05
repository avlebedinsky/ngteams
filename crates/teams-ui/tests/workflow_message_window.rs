//! Workflows run from a message, rendered in the private UI sandbox over synthetic data in Teams Light and Dark: the
//! instructions card, the Overview's "How to run", the Workflows dialog for a message, a message's menu and its "More
//! actions", and the run dialog's states. Screenshots go to `target/ui-shots/workflow-message-*.png`. Never reaches a
//! tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared offline test fake; this sandbox harness runs without cfg(test)"
)]
mod common;
#[path = "support/workflow_message_fixtures.rs"]
mod message_fixtures;

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window,
    WindowOptions, WindowingModes, div, rgb,
};
use teams_store::{Message, Team, TeamChannel};
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    chat_view::ChatView,
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
    workflow_dialog::{FlowRef, WorkflowDialog},
    workflow_run::{MessageRef, WorkflowRun},
};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_WORKFLOW_MESSAGE_WINDOW";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const PAGES: [&str; 10] = [
    "instructions",
    "overview",
    "message-dialog",
    "menu",
    "menu-more",
    "run-follow",
    "run-missing",
    "run-schedule",
    "run-task",
    "run-error",
];

fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |mode| child(&mode));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("workflow message window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("workflow messages", &options)? else {
        return Ok(());
    };
    for (theme, label) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for page in PAGES {
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
        .map_err(|error| running.fail(&format!("no page ready: {error:?}")))?;
    let name = format!("workflow-message-{label}-{page}.png");
    let screenshot = sandbox.screenshot(&name, true)?;
    fs::copy(screenshot, shots()?.join(&name))?;
    say(&format!("shot {name}"));
    sandbox.niri_msg(&["action", "close-window"])?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

/// The page over the theme's background, centred.
struct Stage(AnyView);

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

fn synthetic_place() -> (Team, TeamChannel) {
    let channel = TeamChannel {
        id: "synthetic-channel".into(),
        name: "General".into(),
        general: true,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
        about: teams_store::ChannelAbout::default(),
    };
    (
        Team {
            id: "synthetic-team".into(),
            group_id: Some("synthetic-group".into()),
            name: "Synthetic team".into(),
            channels: vec![channel.clone()],
            ..Team::default()
        },
        channel,
    )
}

fn stored() -> Message {
    Message {
        conversation_id: AB.to_owned(),
        id: "1".to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(common::LAB_B.to_owned()),
        sender_name: Some("Synthetic Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: "<p>Synthetic message to follow up on</p>".to_owned(),
        properties: None,
        arrival_time: 1_767_268_800_000,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

fn chat_message() -> MessageRef {
    MessageRef {
        conversation: AB.to_owned(),
        id: "1".to_owned(),
        root: None,
        in_channel: false,
        team: None,
    }
}

fn flow(id: &str, name: &str) -> FlowRef {
    FlowRef {
        environment: "Default-synthetic".into(),
        id: id.into(),
        name: name.into(),
    }
}

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, page) = mode.split_once('|').unwrap_or((LIGHT_KEY, "instructions"));
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let page = page.to_owned();
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
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
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(if page == "run-error" {
            message_fixtures::failing_answer
        } else {
            message_fixtures::message_answer
        });
        core.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(AB.to_owned(), vec![stored()]);
        core.names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(common::LAB_B.to_owned(), "Synthetic Bee".to_owned());
        *core.teams.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vec![synthetic_place().0]);
        let core: Arc<dyn Core> = core;
        let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
        let models = Models::new(cx);
        let options = WindowOptions {
            app_id: Some("ngteams-workflow-message-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let parts = ShellParts {
                core: Arc::clone(&core),
                models,
                settings,
                trace: Arc::new(ViewTrace::off()),
            };
            let view = page_view(&page, core, parts, window, cx);
            let stage = cx.new(|_| Stage(view));
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}

/// The view a page shows, with its step taken once it settled.
fn page_view(page: &str, core: Arc<dyn Core>, parts: ShellParts, window: &mut Window, cx: &mut App) -> AnyView {
    let (team, channel) = synthetic_place();
    match page {
        "instructions" | "overview" | "message-dialog" => {
            let dialog = cx.new(|cx| {
                if page == "message-dialog" {
                    WorkflowDialog::new_for_message(core, None, chat_message(), window, cx)
                } else {
                    WorkflowDialog::new(core, team, channel, window, cx)
                }
            });
            let shown = dialog.clone();
            let page = page.to_owned();
            after(window, 15, move |window, cx| {
                match page.as_str() {
                    "instructions" => {
                        shown.update(cx, |view, cx| {
                            view.select_template(message_fixtures::FOLLOW, window, cx);
                        });
                    }
                    "overview" => shown.update(cx, |view, cx| view.open_workflow(message_fixtures::FOLLOW_FLOW, cx)),
                    _ => shown.update(cx, WorkflowDialog::scroll_to_end),
                }
                after(window, 15, |_, _| say("ready"));
            });
            dialog.into()
        }
        "menu" | "menu-more" => {
            let chat: Entity<ChatView> = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
            let shown = chat.clone();
            let more = page == "menu-more";
            after(window, 15, move |window, cx| {
                shown.update(cx, |chat, cx| chat.toggle_options("1", cx));
                after(window, 15, move |window, cx| {
                    if more {
                        shown.update(cx, ChatView::open_more_actions);
                    }
                    after(window, 15, |_, _| say("ready"));
                });
            });
            div_view(chat, cx)
        }
        _ => {
            let (id, name) = match page {
                "run-schedule" => (
                    message_fixtures::SCHEDULE_FLOW,
                    "Schedule a message reply to send at the perfect time",
                ),
                "run-task" | "run-error" => (
                    message_fixtures::TASK_FLOW,
                    "Turn a Teams message into a task in Synthetic plan",
                ),
                "run-follow" | "run-missing" => (message_fixtures::FOLLOW_CARD_FLOW, "Follow up on a message"),
                _ => (message_fixtures::FOLLOW_FLOW, "Follow up on a message"),
            };
            let run = cx.new(|cx| WorkflowRun::new(core, flow(id, name), chat_message(), window, cx));
            let shown = run.clone();
            let step = page.to_owned();
            after(window, 15, move |window, cx| {
                if matches!(step.as_str(), "run-missing" | "run-error") {
                    shown.update(cx, WorkflowRun::submit);
                }
                after(window, 15, |_, _| say("ready"));
            });
            run.into()
        }
    }
}

/// A chat view in a window-sized frame, so its menu has room.
fn div_view(chat: Entity<ChatView>, cx: &mut App) -> AnyView {
    cx.new(|_| Framed(chat)).into()
}

struct Framed(Entity<ChatView>);

impl Render for Framed {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(gpui::px(900.0)).h(gpui::px(640.0)).child(self.0.clone())
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
