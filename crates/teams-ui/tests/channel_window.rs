//! Offline synthetic channel surfaces, rendered in the private UI sandbox in Teams Light and Dark.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared synthetic view-test engine, no live requests"
)]
mod common;
use common::{FakeCore, FakeEvents, LAB_A, LAB_B};
use gpui::{App, AppContext as _, Window, WindowingModes};
use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicUsize},
    time::Duration,
};
use teams_store::{ChannelKind, Team, TeamChannel};
use teams_ui::{
    Bridge, Core, Models, ViewTrace,
    channel_menu::ChannelChoice,
    settings::{Settings, SettingsModel},
    shell::ShellParts,
    theme::{Appearance, DARK_KEY, LIGHT_KEY},
};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};
const CHILD: &str = "TEAMS_UI_CHANNEL_WINDOW_TEST";
const TEAM: &str = "19:synthetic-team@thread.tacv2";
const CHANNEL: &str = "19:synthetic-channel@thread.tacv2";
const SURFACES: [&str; 9] = [
    "menu",
    "notifications",
    "edit",
    "manage",
    "members",
    "email",
    "archive",
    "delete",
    "new-section",
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
            eprintln!("channel window: {error}");
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
        "channel menu surfaces",
        &SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            ..SandboxOptions::default()
        },
    )?
    else {
        return Ok(());
    };
    for (theme, name) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        for surface in SURFACES {
            shot(&sandbox, theme, name, surface).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    Ok(())
}
fn shot(sandbox: &Sandbox, theme: &str, name: &str, surface: &str) -> SandboxResult {
    let mode = format!("{theme}:{surface}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    let file = format!("channel-{surface}-{name}.png");
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
fn after_frames(window: &Window, n: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if n <= 1 {
            done(window, cx);
        } else {
            after_frames(window, n - 1, done);
        }
    });
}
fn team(private: bool) -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "Product design".to_owned(),
        owner: true,
        group_id: Some("synthetic-group".to_owned()),
        about: teams_store::TeamAbout {
            tenant: "synthetic-tenant".to_owned(),
            ..teams_store::TeamAbout::default()
        },
        channels: vec![
            TeamChannel {
                id: TEAM.to_owned(),
                name: "General".to_owned(),
                general: true,
                kind: ChannelKind::Standard,
                shown: true,
                about: teams_store::ChannelAbout::default(),
            },
            TeamChannel {
                id: CHANNEL.to_owned(),
                name: "Planning".to_owned(),
                general: false,
                kind: if private {
                    ChannelKind::Private
                } else {
                    ChannelKind::Standard
                },
                shown: true,
                about: teams_store::ChannelAbout {
                    description: "Plan upcoming releases and share progress with the team.".to_owned(),
                    owner: true,
                    group: private.then(|| "synthetic-channel-group".to_owned()),
                    ..teams_store::ChannelAbout::default()
                },
            },
        ],
        ..Team::default()
    }
}
fn child(value: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, surface) = value.split_once(':').ok_or("bad child mode")?;
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
    let surface = surface.to_owned();
    gpui_platform::linux(WindowingModes::WAYLAND).run(move |cx: &mut App| {
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Err(error) = open(theme, surface, cx) {
            eprintln!("channel window open: {error}");
            cx.quit();
        }
    });
    Ok(())
}
fn open(theme: &'static str, surface: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    let team = team(surface == "members");
    *core.teams.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(vec![team.clone()]);
    *core
        .team_roster
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        vec![member(LAB_A, "Alex Morgan", true), member(LAB_B, "Sam Rivera", false)];
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
    let window = teams_ui::window::open(cx, "ngteams-channel-surface-test", "Channel management".into(), parts)?;
    let root =
        teams_ui::window::content(window, cx).ok_or_else(|| std::io::Error::other("missing channel test shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| std::io::Error::other("missing channel test shell"))?;
    window.update(cx, |_, window, _| {
        after_frames(window, 30, move |window, cx| {
            if surface == "menu" {
                let teams = shell.read(cx).teams().clone();
                teams.update(cx, |teams, cx| teams.open_channel_menu(CHANNEL, window, cx));
            } else {
                let choice = match surface.as_str() {
                    "notifications" => ChannelChoice::Notifications,
                    "edit" => ChannelChoice::Edit,
                    "manage" | "members" => ChannelChoice::Manage,
                    "email" => ChannelChoice::Email,
                    "archive" => ChannelChoice::Archive,
                    "delete" => ChannelChoice::Delete,
                    _ => ChannelChoice::NewSection,
                };
                let channel = team.channels[1].clone();
                shell.update(cx, |shell, cx| {
                    shell.channel_chosen(&team, &channel, choice, window, cx);
                });
            }
            after_frames(window, 30, |_, _| say("ready"));
        });
    })?;
    std::mem::forget((bridge, events));
    Ok(())
}
fn member(mri: &str, name: &str, owner: bool) -> teams_core::TeamMember {
    teams_core::TeamMember {
        mri: mri.to_owned(),
        name: name.to_owned(),
        email: None,
        title: None,
        location: None,
        owner,
        guest: false,
        moderator: false,
    }
}
