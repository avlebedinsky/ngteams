//! A YouTube link's app card in a chat, rendered in the private UI sandbox over synthetic data in Teams Light and Dark:
//! someone's message and the account's own, each a link with its card under it. Screenshots go to
//! `target/ui-shots/app-card-*.png`. Never reaches a tenant.
#[expect(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "shared offline test fake; this sandbox harness runs without cfg(test)"
)]
#[path = "../../common/mod.rs"]
mod common;

use std::{
    fs,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _, Window, WindowOptions,
    div, rgb,
};
use teams_proto::LinkPreviewPicture;
use teams_store::Message;
use teams_ui::{
    Core, Models, ShellParts, ViewTrace,
    chat_view::ChatView,
    settings::{Settings, SettingsModel},
    theme::{self, Appearance, DARK_KEY, LIGHT_KEY},
    window::KitRoot,
};
#[path = "../window_fixture.rs"]
#[expect(dead_code, reason = "this scene does not request fixture diagnostics")]
mod fixture;
use fixture::{Running, Sandbox, SandboxResult, sandbox_or_skip};

const CHILD: &str = "TEAMS_UI_APP_CARD_WINDOW";
const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
const FIXTURE: &str = include_str!("../../../../../tests/fixtures/chatsvc/app-card.json");
const VIDEO_PICTURE: &str = "https://i.ytimg.com/vi/Ab3_dEf-Gh9/hqdefault.jpg";
const ICON: &str = "https://res.cdn.office.net/teamsappdata/made-up/com.microsoft.teamspace.tab.youtube_largeimage.png";

pub(super) fn main() -> ExitCode {
    let result = std::env::var(CHILD).map_or_else(|_| parent(), |theme| child(&theme));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("app card window: {error}");
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
    let Some(sandbox) = sandbox_or_skip("app cards")? else {
        return Ok(());
    };
    for (theme, label) in [(LIGHT_KEY, "light"), (DARK_KEY, "dark")] {
        shot(&sandbox, theme, label)?;
    }
    Ok(())
}

fn shot(sandbox: &Sandbox, theme: &str, label: &str) -> SandboxResult {
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD, theme)])?;
    let mut running = Running::new(child)?;
    running
        .line_where(Duration::from_secs(90), |line| line == "ready")
        .map_err(|error| running.fail(&format!("no chat ready: {error:?}")))?;
    let name = format!("app-card-{label}.png");
    let screenshot = sandbox.screenshot(&name, true)?;
    fs::copy(screenshot, shots()?.join(&name))?;
    say(&format!("shot {name}"));
    sandbox.close_window()?;
    let (status, stderr) = running.finish(Duration::from_secs(20))?;
    if !status.success() {
        return Err(format!("child {status}: {stderr}").into());
    }
    Ok(())
}

/// The fixture's message as `id`, from `sender` at `at`.
fn stored(id: &str, sender: &str, at: i64) -> Message {
    let fixture: serde_json::Value = serde_json::from_str(FIXTURE).unwrap_or_default();
    let read_back = &fixture["read_back"];
    Message {
        conversation_id: AB.to_owned(),
        id: id.to_owned(),
        version: 1,
        client_message_id: None,
        sender: Some(sender.to_owned()),
        sender_name: Some("Synthetic Bee".to_owned()),
        message_type: "RichText/Html".to_owned(),
        content: read_back["content"].as_str().unwrap_or_default().to_owned(),
        properties: Some(read_back["properties"].to_string()),
        arrival_time: at,
        compose_time: None,
        edit_time: None,
        delete_time: None,
    }
}

/// A made-up picture of a video: 4:3 with black bars above and below a 16:9 middle, as YouTube's `hqdefault` is.
fn video_picture() -> teams_core::PictureBytes {
    let picture = image::RgbaImage::from_fn(480, 360, |x, y| {
        if !(45..315).contains(&y) {
            return image::Rgba([0, 0, 0, 255]);
        }
        let across = u8::try_from(x * 255 / 480).unwrap_or(255);
        let down = u8::try_from((y - 45) * 255 / 270).unwrap_or(255);
        image::Rgba([90 + across / 2, 60 + down / 3, 160 - across / 3, 255])
    });
    png(&picture)
}

/// A made-up app icon: a red rounded square.
fn icon() -> teams_core::PictureBytes {
    let picture = image::RgbaImage::from_fn(40, 40, |x, y| {
        let corner = |at: u32| at.min(39 - at);
        if corner(x) + corner(y) < 6 {
            image::Rgba([0, 0, 0, 0])
        } else {
            image::Rgba([230, 30, 30, 255])
        }
    });
    png(&picture)
}

fn png(picture: &image::RgbaImage) -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    let _ = picture.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png);
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn child(theme: &str) -> SandboxResult {
    fixture::window_allowed()?;
    let theme = if theme == DARK_KEY { DARK_KEY } else { LIGHT_KEY };
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
        core.messages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                AB.to_owned(),
                vec![
                    stored("1", common::LAB_B, 1_767_268_800_000),
                    stored("2", common::LAB_A, 1_767_268_860_000),
                ],
            );
        core.names
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(common::LAB_B.to_owned(), "Synthetic Bee".to_owned());
        {
            let mut pictures = core.pictures.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(src) = LinkPreviewPicture::card_picture_src(VIDEO_PICTURE) {
                pictures.insert(src, video_picture());
            }
            if let Some(src) = LinkPreviewPicture::card_picture_src(ICON) {
                pictures.insert(src, icon());
            }
        }
        let core: Arc<dyn Core> = core;
        let settings = cx.new(|_| SettingsModel::in_memory(Settings::default()));
        let models = Models::new(cx);
        let options = WindowOptions {
            window_bounds: Some(teams_ui::window::initial_bounds(
                gpui::size(gpui::px(1100.0), gpui::px(900.0)),
                cx,
            )),
            app_id: Some("ngteams-app-card-test".to_owned()),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, move |window, cx| {
            let parts = ShellParts {
                core,
                models,
                settings,
                trace: Arc::new(ViewTrace::off()),
            };
            let chat: Entity<ChatView> = cx.new(|cx| ChatView::new(parts, AB.to_owned(), window, cx));
            after(window, 30, |_, _| say("ready"));
            let stage = cx.new(|_| Stage(chat));
            cx.new(|cx| KitRoot::new(stage, window, cx))
        });
        if opened.is_err() {
            cx.quit();
        }
    });
    Ok(())
}

/// The chat over the theme's background.
struct Stage(Entity<ChatView>);

impl Render for Stage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(theme::current(cx).background))
            .child(self.0.clone())
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
