//! A meeting chat's tabs, drawn on the real platform in the UI sandbox (`harness = false`: GPUI runs on the process's
//! main thread), for their screenshots: the app's window over the fake engine (`common`), its store holding a made-up
//! meeting chat whose recording's card offers "View recap"; its Recap (two recordings: Part 1 with its transcript,
//! that transcript searched, Part 2, Part 1's recording playing as the engine streams it and paused; paused with its
//! captions on, its speed menu at 1.5x, its settings and its caption settings open; Part 1's recording downloading,
//! downloaded and failed), the Recap of a meeting transcribed and not recorded, and the Shared tab.
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
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{ChatTab, Shell, ShellParts};
use teams_ui::theme::{Appearance, DARK_KEY, LIGHT_KEY};
use teams_ui::video_player::{CaptionStyle, Menu, Page, players};
use teams_ui::{Bridge, Core, Models, ViewTrace};
use tokio::sync::mpsc;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "TEAMS_UI_RECAP_WINDOW_TEST";

/// Each page and the themes it is shot in.
const PAGES: [(&str, &[&str]); 15] = [
    ("downloading", &[LIGHT_KEY, DARK_KEY]),
    ("downloaded", &[LIGHT_KEY]),
    ("download-failed", &[LIGHT_KEY]),
    ("chat", &[LIGHT_KEY]),
    ("recap", &[LIGHT_KEY, DARK_KEY]),
    ("playing", &[LIGHT_KEY, DARK_KEY]),
    ("paused", &[LIGHT_KEY]),
    ("captions", &[LIGHT_KEY, DARK_KEY]),
    ("speed", &[LIGHT_KEY]),
    ("settings", &[LIGHT_KEY]),
    ("caption-settings", &[LIGHT_KEY]),
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
    locked(&core.recordings).insert("ITEM1".to_owned());
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

/// Part 1's transcript with a cue spoken in the made-up recording's 3 s, for its captions.
const CAPTIONED: &str = "WEBVTT\n\n\
    u1/1-0\n00:00:00.300 --> 00:00:03.500\n<v Ada Lab>Hello everyone, this is the weekly sync.\n\
    We look at the video player first.</v>\n\n";

/// The made-up recording's file, as Part 1's item names it.
const RECORDING_NAME: &str = "Weekly sync-20260102_100000-Meeting Recording.mp4";

/// The download pages' recording: Part 1's may be downloaded; downloading, its first 5 MiB of 12 MiB come and then
/// nothing more; downloaded, all of its 4.4 MB; failed, it is refused.
fn seed_download(core: &FakeCore, page: &str) {
    locked(&core.recording_files).insert(
        "ITEM1".to_owned(),
        teams_proto::RecordingAccess {
            can_download: true,
            name: Some(RECORDING_NAME.to_owned()),
            size: None,
        },
    );
    let size = match page {
        "downloading" => 12 * 1024 * 1024,
        "downloaded" => 4_659_019,
        _ => return,
    };
    locked(&core.file_bytes).insert(fixtures::sharing_link("ITEM1"), vec![0x5a; size]);
    if page == "downloading" {
        *locked(&core.stream_limit) = Some(5 * 1024 * 1024);
        *locked(&core.hold_streams_from) = Some(5 * 1024 * 1024);
    }
}

/// The download pages' file, in a folder of the run's own.
fn download_target() -> PathBuf {
    std::env::temp_dir()
        .join(format!("ngteams-recap-download-{}", std::process::id()))
        .join(RECORDING_NAME)
}

/// The pages that play Part 1's recording, then pause it.
const PLAYED: [&str; 6] = ["playing", "paused", "captions", "speed", "settings", "caption-settings"];

fn open(theme: &'static str, page: String, cx: &mut App) -> gpui::Result<()> {
    let core = Arc::new(FakeCore::default());
    seed(&core);
    if page == "captions" {
        let vtt = format!("{CAPTIONED}{}", fixtures::VTT.trim_start_matches("WEBVTT\n\n"));
        locked(&core.transcripts).insert(fixtures::transcript_location("ITEM1"), vtt);
        cx.set_global(CaptionStyle {
            on: true,
            ..CaptionStyle::default()
        });
    }
    seed_download(&core, &page);
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
                // the pages with a menu or captions are paused sooner, in its 3 s
                let frames = match page.as_str() {
                    "playing" | "paused" => 60,
                    page if PLAYED.contains(&page) => 15,
                    _ => 30,
                };
                then(&shell, &page, window, cx);
                // a playing recording is shot a second on, its frames drawn as they come; a paused one with its
                // controls, which show while it is paused
                after_frames(window, frames, move |window, cx| {
                    last(&shell, &page, window, cx);
                });
            });
        });
    })?;
    mem::forget((bridge, events));
    Ok(())
}

/// The page's last step: ready to be shot, a recording paused first on the pages that show it paused, with the menu
/// they show open.
fn last(shell: &Entity<Shell>, page: &str, window: &mut Window, cx: &mut App) {
    if page.starts_with("download") {
        // the row stays as it is drawn; the made-up file goes
        if let Some(folder) = download_target().parent() {
            let _ = fs::remove_dir_all(folder);
        }
    }
    if !PLAYED.contains(&page) || page == "playing" {
        say("ready");
        return;
    }
    if let Some(recap) = shell.read(cx).recap_view().cloned() {
        recap.update(cx, teams_ui::recap_view::RecapView::pause);
    }
    let menu = match page {
        "speed" => Some(Menu::Speed),
        "settings" => Some(Menu::Settings(Page::Main)),
        "caption-settings" => Some(Menu::Settings(Page::Captions)),
        _ => None,
    };
    if let Some(player) = players(cx).into_iter().find(|player| player.read(cx).id() == "recap-0") {
        player.update(cx, |player, cx| {
            if page == "speed" {
                player.set_rate(1.5, cx);
            }
            player.show_menu(menu, cx);
        });
    }
    after_frames(window, 20, |_, _| say("ready"));
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
        "transcript" => {
            recap.find("video");
            recap.step_match(false, cx);
        }
        "part-2" => recap.select_part(1, cx),
        "downloading" | "downloaded" | "download-failed" => {
            recap.save_recording(fixtures::sharing_link("ITEM1"), download_target(), cx);
        }
        // the recording streamed and played from half a second in, shot as it plays
        page if PLAYED.contains(&page) => recap.play_from(Duration::from_millis(500), cx),
        _ => {}
    });
}
