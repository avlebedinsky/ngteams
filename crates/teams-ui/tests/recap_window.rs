//! A meeting chat's tabs, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the process's
//! main thread), for their screenshots: the app's window over the fake engine (`common`), its store holding a made-up
//! meeting chat whose recording's card offers "View recap"; its Recap (two recordings: Part 1 with the AI summary's
//! upsell, its transcript searched, Part 2), the Recap of a meeting transcribed and not recorded, and the Shared tab.
//! The test runs itself again inside the sandbox once per page and theme; the parent screenshots each window into
//! `target/ui-shots/recap-*.png`. `RECAP_PAGES` (a comma list) picks pages.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the shared fake engine of the view tests (test code, which unwraps): its test-context rig and trace \
              capture serve them, not a window; this binary runs without the test harness, so not as `cfg(test)`"
)]
mod common;
#[path = "support/recap_fixtures.rs"]
mod fixtures;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use std::{fs, mem};

use common::{FakeCore, FakeEvents};
use gpui::{App, AppContext as _, Entity, Window, WindowingModes};
use teams_core::ChatListing;
use teams_ui::recap_view::Pane;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ChatTab, Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "TEAMS_UI_RECAP_WINDOW_TEST";

/// Each page and the themes it is shot in.
const PAGES: [(&str, &[&str]); 6] = [
    ("chat", &[LIGHT_KEY]),
    ("recap", &[LIGHT_KEY, DARK_KEY]),
    ("transcript", &[LIGHT_KEY, DARK_KEY]),
    ("part-2", &[LIGHT_KEY]),
    ("transcript-only", &[LIGHT_KEY]),
    ("shared", &[LIGHT_KEY, DARK_KEY]),
];

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn say(text: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

fn main() -> ExitCode {
    let result = match std::env::var(CHILD_ENV) {
        Ok(mode) => child(&mode),
        Err(_) => parent(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("recap window: {error}");
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
        "recap window",
        &SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            ..SandboxOptions::default()
        },
    )?
    else {
        return Ok(());
    };
    let only = std::env::var("RECAP_PAGES").ok();
    for (page, themes) in PAGES {
        if only
            .as_deref()
            .is_some_and(|only| !only.split(',').any(|wanted| wanted == page))
        {
            continue;
        }
        for theme in themes {
            shot(&sandbox, theme, page).map_err(|error| format!("{error}{}", sandbox.logs()))?;
        }
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, page: &str) -> SandboxResult {
    let mode = format!("{theme}:{page}");
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, &mode)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no ready: {error:?}")))?;
    let name = if theme == DARK_KEY { "dark" } else { "light" };
    let file = format!("recap-{page}-{name}.png");
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

/// The meeting chats, their messages, the people and the recaps MCPS would answer.
fn seed(core: &FakeCore) {
    let me = core.me().to_owned();
    for id in [fixtures::MEETING, fixtures::TRANSCRIPT_ONLY] {
        locked(&core.conversations).push(fixtures::conversation(id));
    }
    locked(&core.messages).insert(fixtures::MEETING.to_owned(), fixtures::messages(&me));
    let only: Vec<_> = fixtures::messages(&me)
        .into_iter()
        .filter(|message| message.message_type == "Event/Call")
        .map(|mut message| {
            fixtures::TRANSCRIPT_ONLY.clone_into(&mut message.conversation_id);
            message
        })
        .collect();
    locked(&core.messages).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), only);
    locked(&core.names).insert(format!("8:orgid:{}", fixtures::ORGANIZER), "Ada Lab".to_owned());
    locked(&core.names).insert(me, "Ada Lab".to_owned());
    locked(&core.meeting_contents).insert(fixtures::MEETING.to_owned(), fixtures::recorded());
    locked(&core.meeting_contents).insert(fixtures::TRANSCRIPT_ONLY.to_owned(), fixtures::transcribed());
    for item in ["ITEM1", "ITEM2", "ITEM9"] {
        locked(&core.transcripts).insert(fixtures::transcript_location(item), fixtures::VTT.to_owned());
        locked(&core.thumbnails).insert(fixtures::sharing_link(item), poster());
    }
    *locked(&core.listing) = ChatListing {
        favorites: None,
        empty: Vec::new(),
    };
}

/// A recording's poster: a made-up frame, as Graph's thumbnail of the recording would be.
fn poster() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    let frame = image::RgbaImage::from_fn(640, 360, |x, y| {
        let shade = u8::try_from((x / 4 + y / 4) % 200).unwrap_or(0);
        image::Rgba([20, 60 + shade / 2, 40 + shade / 3, 255])
    });
    if frame
        .write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .is_err()
    {
        bytes.clear();
    }
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

fn child(mode: &str) -> SandboxResult {
    ui_sandbox::window_allowed()?;
    let (theme, page) = mode.split_once(':').ok_or("bad child mode")?;
    let theme: &'static str = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
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
            eprintln!("failed to open the window: {error:#}");
            cx.quit();
        }
    });
    Ok(())
}

fn open(theme: &'static str, page: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    let (events, received) = mpsc::unbounded_channel();
    let source = FakeEvents {
        events: received,
        shutdowns: Arc::new(AtomicUsize::new(0)),
    };
    let models = Models::new(cx);
    let shared: Arc<dyn Core> = core;
    let bridge = cx.new(|cx| Bridge::start(source, shared, models.clone(), cx));
    let chat = if page == "transcript-only" {
        fixtures::TRANSCRIPT_ONLY
    } else {
        fixtures::MEETING
    };
    let settings = Settings {
        appearance: Appearance {
            follow_system: false,
            theme,
            ..Appearance::default()
        },
        selected_chat: Some(chat.to_owned()),
        ..Settings::default()
    };
    let parts = ShellParts {
        core: bridge.read(cx).core(),
        models,
        settings: cx.new(|_| SettingsModel::in_memory(settings)),
        trace: Arc::new(ViewTrace::off()),
    };
    let window = teams_ui::window::open(cx, "ngteams-recap-test", "Recap".into(), parts)?;
    let root = teams_ui::window::content(window, cx).ok_or_else(|| io::Error::other("missing shell"))?;
    let shell = root
        .read(cx)
        .shell_in(cx)
        .cloned()
        .ok_or_else(|| io::Error::other("missing shell"))?;
    window.update(cx, |_, window, _| {
        after_frames(window, 30, move |window, cx| {
            step(&shell, &page, window, cx);
            after_frames(window, 30, move |window, cx| {
                then(&shell, &page, window, cx);
                after_frames(window, 30, |_, _| say("ready"));
            });
        });
    })?;
    mem::forget((bridge, events));
    Ok(())
}

/// Opens the page's tab.
fn step(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    let tab = match page {
        "chat" => ChatTab::Chat,
        "shared" => ChatTab::Shared,
        _ => ChatTab::Recap,
    };
    shell.update(cx, |shell, cx| shell.show_chat_tab(tab, window, cx));
}

/// The page's step once its tab shows.
fn then(shell: &Entity<Shell>, page: &str, _: &mut Window, cx: &mut App) {
    let Some(recap) = shell.read(cx).recap_view().cloned() else {
        return;
    };
    recap.update(cx, |recap, cx| match page {
        "transcript" | "transcript-only" => {
            recap.show_pane(Pane::Transcript, cx);
            if page == "transcript" {
                recap.find("video");
                recap.step_match(false, cx);
            }
        }
        "part-2" => {
            recap.select_part(1, cx);
            recap.show_pane(Pane::Transcript, cx);
        }
        _ => {}
    });
}
