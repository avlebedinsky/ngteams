//! A real compositor file drop between two application processes, not a synthetic handler call. All files and
//! accounts are made up; uploads are mocked. The source offers actual text/uri-list data over Wayland.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic UI-test core"
)]
mod common;

use gpui::StatefulInteractiveElement as _;
use gpui::{
    App, AppContext as _, Bounds, Context, Entity, ExternalDragPayload, ExternalPaths, FileDragPaths,
    InteractiveElement as _, IntoElement, KeyBinding, ParentElement as _, Render, Styled as _, Window, WindowBounds,
    WindowOptions, WindowingModes, actions, div, px, rgb, size,
};
use std::{
    cell::Cell,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
    sync::{Arc, PoisonError},
    thread,
    time::Duration,
};
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    chat_view::ChatView,
    compose::Compose,
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
};
use ui_sandbox::{Running, Sandbox, SandboxOptions, SandboxResult, input::keys, sandbox_or_skip};

const CHILD: &str = "TEAMS_ATTACHMENT_WINDOW";
const FILES: &str = "TEAMS_ATTACHMENT_FILES";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const WAIT: Duration = Duration::from_secs(30);
const LAYOUT: &str = "layout { gaps 0; default-column-width { proportion 0.75; }; focus-ring { off; }; border { off; }; }\nwindow-rule { match app-id=\"ngteams-file-source\"; default-column-width { fixed 360; }; }\n";

actions!(attachment_test, [SendFiles]);

fn say(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn main() -> ExitCode {
    let result = match std::env::var(CHILD) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("attachment window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn expect(running: &mut Running, wanted: &str) -> SandboxResult {
    running
        .line_where(WAIT, |line| line == wanted)
        .map(|_| ())
        .map_err(|error| running.fail(&format!("expected {wanted}: {error:?}")))
}

fn shot(sandbox: &Sandbox, name: &str) -> SandboxResult {
    let png = sandbox.screenshot(name, false)?;
    let target = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .ok_or("target")?
        .join("ui-shots");
    std::fs::create_dir_all(&target)?;
    std::fs::copy(png, target.join(name))?;
    say(&format!("shot {}", target.join(name).display()));
    Ok(())
}

fn parent() -> SandboxResult {
    let Some(sandbox) = sandbox_or_skip(
        "native attachment drop",
        &SandboxOptions {
            width: 1440,
            height: 900,
            niri_extra: LAYOUT,
            ..SandboxOptions::default()
        },
    )?
    else {
        return Ok(());
    };
    let paths = [sandbox.path("made-up notes.txt"), sandbox.path("made-up-report.pdf")];
    for path in &paths {
        std::fs::write(path, b"A synthetic attachment, not a real document.\n")?;
    }
    let files = paths
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        let mut source =
            Running::new(sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, "source"), (FILES, &files)])?)?;
        expect(&mut source, "ready")?;
        let mut target = Running::new(sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, theme)])?)?;
        expect(&mut target, "ready")?;
        let mut input = sandbox.input()?;
        input.move_to(100, 400)?;
        input.button_down(keys::BTN_LEFT)?;
        input.move_to(150, 400)?;
        input.move_to(390, 400)?;
        thread::sleep(Duration::from_millis(200));
        input.move_to(900, 400)?;
        expect(&mut target, "hover true")?;
        shot(&sandbox, &format!("attachments-hover-{name}.png"))?;
        // Leaving the destination must remove the highlight before anything is attached.
        input.move_to(100, 400)?;
        expect(&mut target, "hover false")?;
        input.move_to(900, 400)?;
        thread::sleep(Duration::from_millis(200));
        input.move_to(902, 400)?;
        expect(&mut target, "hover true")?;
        input.button_up(keys::BTN_LEFT)?;
        expect(&mut target, "files ready 2")?;
        thread::sleep(Duration::from_millis(200));
        shot(&sandbox, &format!("attachments-staged-{name}.png"))?;
        input.click(900, 300, keys::BTN_LEFT)?;
        input.key(keys::F1)?;
        expect(&mut target, "sent 2 draft preserved")?;
        sandbox.niri_msg(&["action", "close-window"])?;
        let (status, stderr) = target.finish(WAIT)?;
        if !status.success() {
            return Err(format!("target {status}: {stderr}").into());
        }
        sandbox.niri_msg(&["action", "close-window"])?;
        let (status, stderr) = source.finish(WAIT)?;
        if !status.success() {
            return Err(format!("source {status}: {stderr}").into());
        }
    }
    say("real Wayland file drop: ok");
    Ok(())
}

fn frames(window: &Window, left: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if left <= 1 {
            done(window, cx);
        } else {
            frames(window, left - 1, done);
        }
    });
}

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let source = mode == "source";
    let dark = mode == DARK_KEY;
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        teams_ui::window::init(cx);
        let opened = if source { open_source(cx) } else { open_target(dark, cx) };
        if let Err(error) = opened {
            eprintln!("open: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

struct FileSource(Vec<PathBuf>);
impl Render for FileSource {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("file-source")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(rgb(0x00dd_dddd))
            .text_color(rgb(0x0022_2222))
            .child("Drag these synthetic files")
            .child("made-up notes.txt")
            .child("made-up-report.pdf")
            .on_drag(self.0.clone(), |paths, _, _, cx| {
                cx.new(|_| ExternalPaths(paths.iter().cloned().collect()))
            })
            .external_drag_payload::<Vec<PathBuf>>(|paths, _, _| {
                Some(ExternalDragPayload::Files(FileDragPaths::new(
                    paths.iter().map(|path| (path.clone(), false)),
                )))
            })
    }
}

fn options(id: &str, cx: &mut App) -> WindowOptions {
    WindowOptions {
        app_id: Some(id.to_owned()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1080.0), px(900.0)),
            cx,
        ))),
        ..WindowOptions::default()
    }
}

fn open_source(cx: &mut App) -> gpui::Result<()> {
    let files = std::env::var(FILES)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .collect();
    let options = options("ngteams-file-source", cx);
    cx.open_window(options, |window, cx| {
        frames(window, 10, |_, _| say("ready"));
        cx.new(|_| FileSource(files))
    })?;
    Ok(())
}

struct Target {
    chat: Entity<ChatView>,
    compose: Entity<Compose>,
    core: Arc<common::FakeCore>,
}
impl Render for Target {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .key_context("AttachmentHarness")
            .size_full()
            .child(self.chat.clone())
            .on_action(cx.listener(|this, _: &SendFiles, window, cx| {
                this.compose.update(cx, |compose, cx| compose.send(window, cx));
                let actions = this.core.actions.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(teams_core::Action::Send { text, .. }) = actions.last()
                    && text.html() == "<p>Keep this draft</p>"
                {
                    say(&format!("sent {} draft preserved", text.files().len()));
                }
            }))
    }
}

fn open_target(dark: bool, cx: &mut App) -> gpui::Result<()> {
    cx.bind_keys([KeyBinding::new("f1", SendFiles, Some("AttachmentHarness"))]);
    let core = Arc::new(common::FakeCore::default());
    core.members.lock().unwrap_or_else(PoisonError::into_inner).insert(
        AB.to_owned(),
        vec![
            teams_store::Member::new(common::LAB_A, None),
            teams_store::Member::new(common::LAB_B, None),
        ],
    );
    let settings = cx.new(|_| {
        SettingsModel::in_memory(Settings {
            appearance: Appearance {
                follow_system: false,
                theme: if dark { DARK_KEY } else { LIGHT_KEY },
                ..Appearance::default()
            },
            ..Settings::default()
        })
    });
    let parts = ShellParts {
        core: core.clone() as Arc<dyn Core>,
        models: Models::new(cx),
        settings: settings.clone(),
        trace: Arc::new(ViewTrace::off()),
    };
    let follow = theme::follow_settings(&settings, cx);
    follow.detach();
    let options = options("ngteams-file-target", cx);
    cx.open_window(options, |window, cx| {
        let chat = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
        let compose = chat.read(cx).compose().clone();
        let hovering = Rc::new(Cell::new(false));
        cx.observe(&chat, move |chat, cx| {
            let now = chat.read(cx).file_drop_active();
            if hovering.replace(now) != now {
                say(&format!("hover {now}"));
            }
        })
        .detach();
        cx.observe(&compose, |compose, cx| {
            let files = compose.read(cx).attachments();
            if files.len() == 2
                && files
                    .iter()
                    .all(|file| matches!(file.state, teams_ui::compose::attachments::AttachmentState::Ready(_)))
            {
                say("files ready 2");
            }
        })
        .detach();
        let editor = compose.read(cx).editor().clone();
        editor.update(cx, |editor, cx| {
            editor.set_doc(
                teams_ui::editor::doc::Doc::new(vec![teams_ui::editor::doc::Block::new(
                    teams_ui::editor::doc::Kind::Paragraph,
                    0,
                    vec![teams_ui::editor::doc::Run::new(
                        "Keep this draft",
                        teams_ui::editor::doc::Attrs::default(),
                    )],
                )]),
                cx,
            );
        });
        let view = cx.new(|_| Target { chat, compose, core });
        frames(window, 10, |_, _| say("ready"));
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    Ok(())
}
