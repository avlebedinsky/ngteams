//! A YouTube link's app card (`properties.cards`), as a user sees and clicks it in the app's window in the UI sandbox
//! (`harness = false`: GPUI runs on the process's main thread). It is the QA test of task 405.
//!
//! The parent runs the mock (`teams-mock`) with synthetic messages of the captured shape
//! (`tests/fixtures/chatsvc/app-card.json`): the link in the text, the `InputExtension` span, `properties.links` with
//! `previewenabled: false`, and `properties.cards` as a JSON string of a list. Every video id, title, channel and link
//! is made up. It runs a's app as a child in the sandbox with a control socket. It reads the app as a user sees it:
//! the rows the child describes on request, the screen, the mock's picture reads and the sandbox's URI opener (it
//! records each link and opens nothing).
//!
//! - **A picture that fails.** The gateway has no picture yet: the card shows a grey 16:9 box with the play mark,
//!   and a click on it still opens the video.
//! - **A 1:1 chat.** A `youtu.be` link with `?si=` and `t=`, a card without a poster: the gateway is asked for
//!   YouTube's `hqdefault.jpg` of the video. The card is in the bubble under the link, 16:9, cropped from the middle,
//!   with the play mark. A click on the picture, on "Open" and on the title each opens the watch link with its `t=`.
//! - **A group chat.** A `youtube.com/watch` link with a poster and a very long title: the gateway is asked for the
//!   poster. A Shorts link with a title in another script. Two cards in one message: each click opens its own video.
//! - **A quote.** a's own message quotes a card message and has a card of its own: the quote shows the text only.
//! - **A channel.** A post with a card in the channel, and a reply with a card in the post's thread.
//! - **Another card type.** An entry that this build does not draw: the message says "can't be shown".
//! - **Window sizes.** A narrow window keeps the card in the window at 16:9. A wide window keeps the card's width.
//!
//! Screenshots are kept under `target/ui-shots/qa-app-card-*.png`, in Teams Light and Dark. Without weston 15, niri
//! or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, RawFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use ngteams::descriptors::adopt_trace;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_ctl::protocol::{OpenTarget, Request};
use teams_ctl::{Client, Timeouts};
use teams_mock::{
    AC_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, TEAM_CHANNEL, TrouterMock,
};
use teams_proto::chat::card::{ActionKind, Card, Kind};
use teams_proto::{Author, Client as ChatClient, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::{Body, ChatView, MessageItem};
use teams_ui::rich_text::Block;
use teams_ui::shell::Shell;
use teams_ui::tray::{MENU_PATH, MENU_QUIT};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::VirtualInput;
use ui_sandbox::input::keys;
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip, wait_until};

const CHILD_ENV: &str = "NGTEAMS_QA_APP_CARD_TEST";
const TRACE_FD_ENV: &str = "NGTEAMS_QA_APP_CARD_TRACE_FD";
const ORIGIN_ENV: &str = "NGTEAMS_QA_APP_CARD_ORIGIN";
const TROUTER_ENV: &str = "NGTEAMS_QA_APP_CARD_TROUTER";
const TOKEN_ENV: &str = "NGTEAMS_QA_APP_CARD_TOKEN";
const DIR_ENV: &str = "NGTEAMS_QA_APP_CARD_DIR";
/// The file in the app's directory the parent asks the child about its views through.
const ASK: &str = "qa-app-card-ask";
/// The file in the app's directory the child reads the theme to show from (`light` or `dark`).
const THEME_FILE: &str = "theme";
const STARTUP: Duration = Duration::from_secs(90);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);
/// How long the screen gets to settle before a shot.
const SETTLE: Duration = Duration::from_millis(900);

/// The wide screen, in output pixels.
const WIDE: (u32, u32) = (1920, 1080);
/// The narrow window's width, in output pixels.
const NARROW: u32 = 760;

/// The gateway's thumbnail path.
const THUMBNAIL: &str = "/urlp/v1/url/image/Thumbnail";
/// The made-up video picture the gateway gives: YouTube's 4:3 `hqdefault` shape, a 16:9 middle in [`PICTURE`] with
/// bars in [`BAR`] above and below. A correct 16:9 crop shows no bar.
const PICTURE: [u8; 3] = [46, 139, 87];
const BAR: [u8; 3] = [255, 0, 255];
/// The card's text box: [`teams_ui::card_view::CARD_WIDTH`] less the padding on both sides.
const PICTURE_WIDTH: u32 = 416;

const APP_ID: &str = "com.microsoft.teamspace.tab.youtube";
const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
/// The app's icon, as the delivered copy names it: the gateway's copy of a made-up icon path.
const ICON: &str = "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/content?url=https%3a%2f%2fres.cdn.office.net%2fteamsappdata%2fmade-up%2fcom.microsoft.teamspace.tab.youtube_largeimage.png";
/// The icon of the card whose picture fails: another made-up path, so that its failure is not kept for the others.
const FAIL_ICON: &str = "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/content?url=https%3a%2f%2fres.cdn.office.net%2fteamsappdata%2fmade-up%2ffail_largeimage.png";

/// A made-up video and its link's card.
#[derive(Clone, Debug)]
struct Video {
    id: &'static str,
    /// The link in the message's text.
    link: String,
    /// The card's `Media` source and the "Open" button's `websiteUrl`: what a click opens.
    watch: String,
    poster: Option<String>,
    title: &'static str,
    channel: &'static str,
    counts: &'static str,
    icon: &'static str,
}

impl Video {
    fn new(id: &'static str, link: String, watch: String, title: &'static str) -> Self {
        Self {
            id,
            link,
            watch,
            poster: None,
            title,
            channel: "Made-up Channel",
            counts: "1.2K views · 3 days ago",
            icon: ICON,
        }
    }

    /// The picture the gateway is asked for: the poster, else YouTube's `hqdefault.jpg` of the video.
    fn picture(&self) -> String {
        self.poster
            .clone()
            .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.id))
    }
}

const LONG_TITLE: &str = "A made-up video with a very long title that goes on and on: part one of twelve, with notes, \
    chapters, a made-up guest, a made-up studio, a made-up city, and more words than any card has room for in four \
    lines of large bold text";
const OTHER_SCRIPT_TITLE: &str = "Выдуманное видео — 架空の動画のタイトル";

/// The videos of the walk.
struct Videos {
    fail: Video,
    direct: Video,
    long: Video,
    shorts: Video,
    first: Video,
    second: Video,
    quoted: Video,
    quoting: Video,
    post: Video,
    reply: Video,
}

fn videos() -> Videos {
    let watch = |id: &str| format!("https://www.youtube.com/watch?v={id}");
    let mut fail = Video::new(
        "Fa1_lEd-Pi9",
        "https://youtu.be/Fa1_lEd-Pi9".to_owned(),
        watch("Fa1_lEd-Pi9"),
        "A made-up video whose picture fails",
    );
    fail.icon = FAIL_ICON;
    let mut long = Video::new("Rb2-cDe_Fg3", watch("Rb2-cDe_Fg3"), watch("Rb2-cDe_Fg3"), LONG_TITLE);
    long.poster = Some("https://i.ytimg.com/vi/Rb2-cDe_Fg3/maxresdefault.jpg".to_owned());
    Videos {
        fail,
        direct: Video::new(
            "Qa1_bCd-Ef2",
            "https://youtu.be/Qa1_bCd-Ef2?si=MadeUpShare1&t=42".to_owned(),
            "https://www.youtube.com/watch?v=Qa1_bCd-Ef2&t=42s".to_owned(),
            "A made-up video for the 1:1 chat",
        ),
        long,
        shorts: Video::new(
            "Sc3_dEf-Gh4",
            "https://youtube.com/shorts/Sc3_dEf-Gh4?si=MadeUpShare3".to_owned(),
            "https://www.youtube.com/shorts/Sc3_dEf-Gh4".to_owned(),
            OTHER_SCRIPT_TITLE,
        ),
        first: Video::new(
            "Td4-eFg_Hi5",
            "https://youtu.be/Td4-eFg_Hi5".to_owned(),
            watch("Td4-eFg_Hi5"),
            "The first of two made-up videos",
        ),
        second: Video::new(
            "Ue5_fGh-Ij6",
            "https://www.youtube.com/watch?v=Ue5_fGh-Ij6&t=1m5s".to_owned(),
            "https://www.youtube.com/watch?v=Ue5_fGh-Ij6&t=65s".to_owned(),
            "The second of two made-up videos",
        ),
        quoted: Video::new(
            "Vf6-gHi_Jk7",
            "https://youtu.be/Vf6-gHi_Jk7".to_owned(),
            watch("Vf6-gHi_Jk7"),
            "A made-up video that a quote names",
        ),
        quoting: Video::new(
            "Wg7_hIj-Kl8",
            "https://youtu.be/Wg7_hIj-Kl8".to_owned(),
            watch("Wg7_hIj-Kl8"),
            "A made-up video in a message with a quote",
        ),
        post: Video::new(
            "Xh8-iJk_Lm9",
            "https://youtu.be/Xh8-iJk_Lm9".to_owned(),
            watch("Xh8-iJk_Lm9"),
            "A made-up video in a channel post",
        ),
        reply: Video::new(
            "Yi9_jKl-Mn0",
            "https://youtu.be/Yi9_jKl-Mn0".to_owned(),
            watch("Yi9_jKl-Mn0"),
            "A made-up video in a channel reply",
        ),
    }
}

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_app_card_window: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Notes a step on stderr with the time since the process started.
fn step(what: &str) {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let start = START.get_or_init(Instant::now);
    eprintln!("[{:>7.2}s] {what}", start.elapsed().as_secs_f64());
}

/// What the walk saw wrong that does not stop it: each is said at the end, and the test fails.
static FINDINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn note(finding: String) {
    step(&format!("FINDING: {finding}"));
    FINDINGS.lock().unwrap_or_else(PoisonError::into_inner).push(finding);
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent ---

fn parent() -> SandboxResult {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams app cards", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    let dir = tempfile::tempdir()?;
    if let Err(error) = walk(&sandbox, &runtime, &service, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    drop(sandbox);
    let options = SandboxOptions {
        width: WIDE.0,
        height: WIDE.1,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams app cards, wide", &options)? else {
        return Ok(());
    };
    let service = Service::start(&runtime)?;
    runtime.block_on(service.mock.serve_bytes(THUMBNAIL, "image/png", hqdefault_png()?));
    let dir = tempfile::tempdir()?;
    if let Err(error) = wide(&sandbox, &service, dir.path()) {
        return Err(format!("{error}{}", sandbox.logs()).into());
    }
    let findings = std::mem::take(&mut *FINDINGS.lock().unwrap_or_else(PoisonError::into_inner));
    if !findings.is_empty() {
        return Err(format!("the walk ended with findings:\n- {}", findings.join("\n- ")).into());
    }
    say("ngteams app cards: ok");
    Ok(())
}

/// The walk on the 1280×800 screen.
fn walk(sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let outcome = (|| -> SandboxResult {
        picture_fails(&mut run, sandbox, runtime, service)?;
        runtime.block_on(service.mock.serve_bytes(THUMBNAIL, "image/png", hqdefault_png()?));
        one_to_one(&mut run, sandbox, runtime, service)?;
        long_title(&mut run, sandbox, runtime, service)?;
        shorts(&mut run, sandbox, runtime, service)?;
        two_cards(&mut run, sandbox, service)?;
        quote(&mut run, sandbox, service)?;
        channel(&mut run, sandbox, service)?;
        other_card(&mut run, sandbox, service)?;
        narrow(&mut run, sandbox, service)
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// The card whose picture the gateway does not have: a grey 16:9 box with the play mark; a click still opens.
fn picture_fails(
    run: &mut AppRun,
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
) -> SandboxResult {
    let video = &service.videos.fail;
    run.open(&service.chats.fail)?;
    let state = run.until("the card whose picture fails", |state| {
        state.item(&service.ids.fail).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, &service.ids.fail, 0, video)?;
    asked_for(runtime, service, &video.picture())?;
    thread::sleep(SETTLE);
    let shot = run.shot(sandbox, "qa-app-card-fail-light.png")?;
    if !boxes(&shot, |pixel| near(pixel, PICTURE, 12), 100)?.is_empty() {
        return Err(format!("a picture is drawn though the gateway has none: {}", shot.display()).into());
    }
    let grey = boxes(&shot, |pixel| near(pixel, [238, 238, 238], 3), 150)?;
    let media = grey
        .iter()
        .find(|found| is_16_9(found))
        .copied()
        .ok_or_else(|| format!("no grey 16:9 box: {grey:?} ({})", shot.display()))?;
    let centre = pixel_at(&shot, media.centre())?;
    if !near(centre, [0, 0, 0], 40) {
        return Err(format!("no play mark in the grey box's middle: {centre:?} ({})", shot.display()).into());
    }
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(&opened, &video.watch, "a click on the grey box")?;
    run.theme(true)?;
    run.shot(sandbox, "qa-app-card-fail-dark.png")?;
    run.theme(false)?;
    step("a picture that fails: a grey 16:9 box with the play mark, and a click opens the video");
    Ok(())
}

/// The 1:1 chat: a `youtu.be` link with `?si=` and `t=`, no poster.
fn one_to_one(
    run: &mut AppRun,
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
) -> SandboxResult {
    let video = &service.videos.direct;
    let id = &service.ids.direct;
    run.open(AC_CHAT)?;
    let state = run.until("the 1:1 chat's card", |state| {
        state.item(id).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, id, 0, video)?;
    asked_for(runtime, service, &video.picture())?;
    let (shot, media) = run.picture(sandbox, "qa-app-card-1to1-light.png", 1)?;
    expect_full_card(&shot, media)?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(&opened, &video.watch, "a click on the picture")?;
    let (open_x, open_y) = open_button(&shot, media)?;
    let opened = run.click_opens(sandbox, open_x, open_y)?;
    same_link(&opened, &video.watch, "a click on Open")?;
    let opened = run.click_opens(sandbox, media.x0 + 30, media.y1 + 22)?;
    same_link(&opened, &video.watch, "a click on the title")?;
    run.theme(true)?;
    let (dark_shot, dark) = run.picture(sandbox, "qa-app-card-1to1-dark.png", 1)?;
    no_bars(&dark_shot, dark, "the picture in Teams Dark")?;
    if !is_16_9(&dark) || dark.width().abs_diff(media.width()) > 1 {
        return Err(format!("the card in Teams Dark: {dark:?}, in Teams Light: {media:?}").into());
    }
    run.theme(false)?;
    step("1:1: a youtu.be link's card, 16:9 under the link; the picture, Open and the title open the watch link");
    Ok(())
}

/// A group chat: a `youtube.com/watch` link with a poster and a very long title.
fn long_title(
    run: &mut AppRun,
    sandbox: &Sandbox,
    runtime: &tokio::runtime::Runtime,
    service: &Service,
) -> SandboxResult {
    let video = &service.videos.long;
    let id = &service.ids.long;
    run.open(&service.chats.group)?;
    let state = run.until("the long title's card", |state| {
        state.item(id).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, id, 0, video)?;
    asked_for(runtime, service, &video.picture())?;
    let (shot, media) = run.picture(sandbox, "qa-app-card-group-long-light.png", 1)?;
    expect_full_card(&shot, media)?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(&opened, &video.watch, "a click on the picture")?;
    let (open_x, open_y) = open_button(&shot, media)?;
    // four lines of large text at most, then the channel, the counts and Open
    if open_y > media.y1 + 4 * 30 + 90 {
        return Err(format!("the long title takes more than four lines: Open at {open_y}, picture {media:?}").into());
    }
    let opened = run.click_opens(sandbox, open_x, open_y)?;
    same_link(&opened, &video.watch, "a click on Open")?;
    run.theme(true)?;
    run.picture(sandbox, "qa-app-card-group-long-dark.png", 1)?;
    run.theme(false)?;
    step("group: a youtube.com link with a poster and a long title");
    Ok(())
}

/// A group chat: a Shorts link with a title in another script.
fn shorts(run: &mut AppRun, sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, service: &Service) -> SandboxResult {
    let video = &service.videos.shorts;
    let id = &service.ids.shorts;
    run.open(&service.chats.shorts)?;
    let state = run.until("the Shorts card", |state| {
        state.item(id).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, id, 0, video)?;
    asked_for(runtime, service, &video.picture())?;
    let (shot, media) = run.picture(sandbox, "qa-app-card-shorts-light.png", 1)?;
    expect_full_card(&shot, media)?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(&opened, &video.watch, "a click on the Shorts picture")?;
    run.theme(true)?;
    run.picture(sandbox, "qa-app-card-shorts-dark.png", 1)?;
    run.theme(false)?;
    step("group: a Shorts link with a title in another script");
    Ok(())
}

/// Two cards in one message: each picture opens its own video.
fn two_cards(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    let id = &service.ids.pair;
    run.open(&service.chats.pair)?;
    let state = run.until("the two cards", |state| {
        state.item(id).is_some_and(|item| item.cards.len() == 2)
    })?;
    expect_card(&state, id, 0, &service.videos.first)?;
    expect_card(&state, id, 1, &service.videos.second)?;
    thread::sleep(SETTLE);
    let shot = run.shot(sandbox, "qa-app-card-two-light.png")?;
    let pictures = boxes(&shot, |pixel| near(pixel, PICTURE, 12), 100)?;
    let lower = pictures
        .iter()
        .max_by_key(|found| found.y0)
        .copied()
        .ok_or_else(|| format!("no picture ({})", shot.display()))?;
    if !is_16_9(&lower) {
        return Err(format!("the second card's picture is not 16:9: {lower:?}").into());
    }
    no_bars(&shot, lower, "the second card's picture")?;
    let opened = run.click_opens(sandbox, lower.x0 + lower.width() / 4, lower.y0 + lower.height() / 4)?;
    same_link(&opened, &service.videos.second.watch, "a click on the second picture")?;
    // the first card above it
    let (x, y) = (lower.x0 + lower.width() / 2, lower.y0 + lower.height() / 2);
    run.input.scroll(x, y, -12)?;
    thread::sleep(SETTLE);
    let shot = run.shot(sandbox, "qa-app-card-two-top-light.png")?;
    let pictures = boxes(&shot, |pixel| near(pixel, PICTURE, 12), 100)?;
    let upper = pictures
        .iter()
        .filter(|found| found.height() > 40)
        .min_by_key(|found| found.y0)
        .copied()
        .ok_or_else(|| format!("no first picture after a scroll ({})", shot.display()))?;
    let opened = run.click_opens(sandbox, upper.x0 + upper.width() / 4, upper.y1.saturating_sub(20))?;
    same_link(&opened, &service.videos.first.watch, "a click on the first picture")?;
    run.theme(true)?;
    run.shot(sandbox, "qa-app-card-two-top-dark.png")?;
    run.theme(false)?;
    step("two cards in one message: each picture opens its own video");
    Ok(())
}

/// a's message that quotes a card message and has its own card.
fn quote(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    run.open(&service.chats.quotes)?;
    let state = run.until("the quoting message", |state| {
        state
            .item(&service.ids.quoting)
            .is_some_and(|item| item.cards.len() == 1)
            && state
                .item(&service.ids.quoted)
                .is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, &service.ids.quoted, 0, &service.videos.quoted)?;
    expect_card(&state, &service.ids.quoting, 0, &service.videos.quoting)?;
    let quoting = state.item(&service.ids.quoting).ok_or("no quoting message")?;
    if !quoting.own || !quoting.quote.contains(&service.videos.quoted.link) {
        return Err(format!("the quoting message: {quoting:?}").into());
    }
    let (shot, media) = run.picture(sandbox, "qa-app-card-quote-light.png", 1)?;
    no_bars(&shot, media, "the quoting message's picture")?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(
        &opened,
        &service.videos.quoting.watch,
        "a click on the quoting message's picture",
    )?;
    run.theme(true)?;
    run.picture(sandbox, "qa-app-card-quote-dark.png", 1)?;
    run.theme(false)?;
    step("a quote: the quoting message shows its own card under the quote");
    Ok(())
}

/// The channel: a post with a card, and a reply with a card in the post's thread.
fn channel(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    run.verb("open-channel")?;
    let state = run.until_in("posts", "the channel post's card", |state| {
        state.item(&service.ids.post).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, &service.ids.post, 0, &service.videos.post)?;
    let (shot, media) = run.picture(sandbox, "qa-app-card-channel-light.png", 1)?;
    if !is_16_9(&media) {
        return Err(format!("the post's picture is not 16:9: {media:?} ({})", shot.display()).into());
    }
    no_bars(&shot, media, "the post's picture")?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(&opened, &service.videos.post.watch, "a click on the post's picture")?;
    run.theme(true)?;
    run.picture(sandbox, "qa-app-card-channel-dark.png", 1)?;
    run.theme(false)?;
    run.verb(&format!("open-thread {}", service.ids.post))?;
    let state = run.until_in("thread", "the reply's card", |state| {
        state.item(&service.ids.reply).is_some_and(|item| item.cards.len() == 1)
    })?;
    expect_card(&state, &service.ids.reply, 0, &service.videos.reply)?;
    thread::sleep(SETTLE);
    let shot = run.shot(sandbox, "qa-app-card-thread-light.png")?;
    let pictures = boxes(&shot, |pixel| near(pixel, PICTURE, 12), 100)?;
    let right = pictures
        .iter()
        .max_by_key(|found| (found.x0, found.y0))
        .copied()
        .ok_or_else(|| format!("no picture in the thread ({})", shot.display()))?;
    if !is_16_9(&right) {
        return Err(format!("the reply's picture is not 16:9: {right:?} ({})", shot.display()).into());
    }
    no_bars(&shot, right, "the reply's picture in the thread")?;
    for beside in pictures.iter().filter(|found| found.x0 < right.x0) {
        no_bars(&shot, *beside, "the post's picture beside the thread")?;
    }
    let opened = run.click_opens(sandbox, right.x0 + right.width() / 4, right.y0 + right.height() / 4)?;
    same_link(&opened, &service.videos.reply.watch, "a click on the reply's picture")?;
    run.theme(true)?;
    run.shot(sandbox, "qa-app-card-thread-dark.png")?;
    run.theme(false)?;
    run.input.key(keys::ESC)?;
    step("a channel: the post's card, and the reply's card in its thread");
    Ok(())
}

/// A card of a type this build does not draw: "can't be shown", no app card.
fn other_card(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    run.open(&service.chats.other)?;
    let state = run.until("the other card type", |state| {
        state.item(&service.ids.other).is_some_and(|item| item.unshown_cards)
    })?;
    let item = state.item(&service.ids.other).ok_or("no message with the other card")?;
    if !item.cards.is_empty() {
        return Err(format!("a card this build cannot draw got drawn: {item:?}").into());
    }
    run.shot(sandbox, "qa-app-card-other-light.png")?;
    step("another card type still says it cannot be shown");
    Ok(())
}

/// The 1:1 chat in a narrow window: the card stays in the window and keeps 16:9.
fn narrow(run: &mut AppRun, sandbox: &Sandbox, service: &Service) -> SandboxResult {
    run.open(AC_CHAT)?;
    run.until("the 1:1 chat's card", |state| {
        state
            .item(&service.ids.direct)
            .is_some_and(|item| item.cards.len() == 1)
    })?;
    sandbox.niri_msg(&["action", "set-column-width", &NARROW.to_string()])?;
    thread::sleep(SETTLE);
    let (shot, media) = run.picture(sandbox, "qa-app-card-narrow-light.png", 1)?;
    if media.x1 > NARROW || !is_16_9(&media) {
        return Err(format!("the card in the narrow window: {media:?} ({})", shot.display()).into());
    }
    no_bars(&shot, media, "the picture in a narrow window")?;
    let opened = run.click_opens(sandbox, media.x0 + media.width() / 4, media.y0 + media.height() / 4)?;
    same_link(
        &opened,
        &service.videos.direct.watch,
        "a click on the picture in a narrow window",
    )?;
    run.theme(true)?;
    run.picture(sandbox, "qa-app-card-narrow-dark.png", 1)?;
    run.theme(false)?;
    sandbox.niri_msg(&["action", "set-column-width", "100%"])?;
    thread::sleep(SETTLE);
    step("a narrow window: the card stays in it at 16:9");
    Ok(())
}

/// The 1920×1080 screen: the card keeps its width.
fn wide(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult {
    let mut run = AppRun::launch(sandbox, service, dir)?;
    let outcome = (|| -> SandboxResult {
        run.open(AC_CHAT)?;
        run.until("the 1:1 chat's card", |state| {
            state
                .item(&service.ids.direct)
                .is_some_and(|item| item.cards.len() == 1)
        })?;
        let (shot, media) = run.picture(sandbox, "qa-app-card-wide-light.png", 1)?;
        expect_full_card(&shot, media)?;
        run.theme(true)?;
        run.picture(sandbox, "qa-app-card-wide-dark.png", 1)?;
        run.theme(false)?;
        run.open(&service.chats.group)?;
        run.until("the long title's card", |state| {
            state.item(&service.ids.long).is_some_and(|item| item.cards.len() == 1)
        })?;
        let (shot, media) = run.picture(sandbox, "qa-app-card-wide-long-light.png", 1)?;
        expect_full_card(&shot, media)?;
        step("a wide window: the card keeps its width");
        Ok(())
    })();
    if let Err(error) = outcome {
        return Err(run.fail(sandbox, &*error));
    }
    run.quit(sandbox)
}

/// The row `id` holds card `n` of `video`: the app, the media's link and picture, the title, Open's and the card's
/// link; no link preview, nothing it cannot show.
fn expect_card(state: &State, id: &str, n: usize, video: &Video) -> SandboxResult {
    let item = state.item(id).ok_or_else(|| format!("no message {id}"))?;
    let card = item
        .cards
        .get(n)
        .ok_or_else(|| format!("message {id} has no card {n}: {item:?}"))?;
    let wanted = CardRow {
        app: Some("YouTube".to_owned()),
        icon: true,
        media: Some(video.watch.clone()),
        picture: Some(video.picture()),
        texts: vec![
            video.title.to_owned(),
            video.channel.to_owned(),
            video.counts.to_owned(),
        ],
        open: Some(video.watch.clone()),
        select: Some(video.watch.clone()),
    };
    if *card != wanted {
        return Err(format!("message {id}'s card {n}: {card:?}, wanted {wanted:?}").into());
    }
    if item.unshown_cards || item.links != 0 {
        return Err(format!("message {id} also says it cannot show a part, or shows a link preview: {item:?}").into());
    }
    Ok(())
}

/// The gateway was asked for `picture`'s thumbnail.
fn asked_for(runtime: &tokio::runtime::Runtime, service: &Service, picture: &str) -> SandboxResult {
    let found = wait_until(WAIT, || {
        service.thumbnails(runtime).into_iter().find(|asked| asked == picture)
    });
    if found.is_none() {
        return Err(format!(
            "the gateway was never asked for {picture}; it was asked for {:?}",
            service.thumbnails(runtime)
        )
        .into());
    }
    Ok(())
}

fn same_link(opened: &str, wanted: &str, what: &str) -> SandboxResult {
    step(&format!("{what} opened {opened}"));
    if opened == wanted {
        Ok(())
    } else {
        Err(format!("{what} opened {opened}, wanted {wanted}").into())
    }
}

/// The card's full picture at the card's full width: 16:9, no bar, the play mark in its middle.
fn expect_full_card(shot: &Path, media: Rect) -> SandboxResult {
    if !is_16_9(&media) {
        return Err(format!("the picture is not 16:9: {media:?} ({})", shot.display()).into());
    }
    if media.width().abs_diff(PICTURE_WIDTH) > 3 {
        return Err(format!(
            "the picture is {} px wide, wanted about {PICTURE_WIDTH}: {media:?}",
            media.width()
        )
        .into());
    }
    no_bars(shot, media, "the picture")?;
    let centre = pixel_at(shot, media.centre())?;
    if !near(centre, [0, 0, 0], 40) {
        return Err(format!("no play mark in the picture's middle: {centre:?} ({})", shot.display()).into());
    }
    let white = pixel_at(shot, (media.centre().0 + 20, media.centre().1))?;
    if !near(white, [255, 255, 255], 20) {
        return Err(format!("the play mark's circle is not white: {white:?} ({})", shot.display()).into());
    }
    Ok(())
}

/// Notes a finding when a part of the picture's bars ([`BAR`], or a blend of it at an edge) shows in or next to the
/// picture `media`: the 16:9 crop then shows a line of what is outside the video. The app's icon is the same made-up
/// picture, drawn whole (20 px), so only lines as wide as the picture count.
fn no_bars(shot: &Path, media: Rect, what: &str) -> SandboxResult {
    let bars: Vec<Rect> = boxes(shot, |[red, green, blue]| red > 100 && blue > 100 && green < 100, 60)?
        .into_iter()
        .filter(|bar| bar.x0 < media.x1 && media.x0 < bar.x1)
        .filter(|bar| bar.y1 + 3 >= media.y0 && bar.y0 <= media.y1 + 3)
        .collect();
    if !bars.is_empty() {
        note(format!(
            "{what} {media:?} shows a line of the picture's bar at its edge: {bars:?} ({})",
            shot.display()
        ));
    }
    Ok(())
}

/// The middle of the "Open" button under `media`: a run of 20 or more rows a little to the right of the card's text
/// edge whose colour differs from the card's and is not text.
fn open_button(shot: &Path, media: Rect) -> SandboxResult<(u32, u32)> {
    let image = image::open(shot)?.to_rgb8();
    let x = media.x0 + 5;
    let surface = image.get_pixel(x, media.y1 + 3).0;
    let mut run = 0;
    for y in media.y1 + 3..image.height().min(media.y1 + 320) {
        let pixel = image.get_pixel(x, y).0;
        let light = pixel.iter().all(|&channel| channel > 150);
        if light && !near(pixel, surface, 6) {
            run += 1;
        } else {
            if run >= 20 {
                return Ok((media.x0 + 25, y - run / 2));
            }
            run = 0;
        }
    }
    Err(format!("no Open button under the picture {media:?} ({})", shot.display()).into())
}

// --- the screen ---

/// A box on the screen, in output pixels: `x1` and `y1` are one past its last column and row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn width(&self) -> u32 {
        self.x1 - self.x0
    }

    fn height(&self) -> u32 {
        self.y1 - self.y0
    }

    fn centre(&self) -> (u32, u32) {
        (self.x0.midpoint(self.x1), self.y0.midpoint(self.y1))
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a box's sides are far below f32's exact range"
)]
fn is_16_9(found: &Rect) -> bool {
    found.height() > 0 && ((found.width() as f32 / found.height() as f32) - 16.0 / 9.0).abs() < 0.04
}

fn near(pixel: [u8; 3], wanted: [u8; 3], within: u8) -> bool {
    pixel.iter().zip(wanted).all(|(got, want)| got.abs_diff(want) <= within)
}

fn pixel_at(shot: &Path, (x, y): (u32, u32)) -> SandboxResult<[u8; 3]> {
    let image = image::open(shot)?.to_rgb8();
    if x >= image.width() || y >= image.height() {
        return Err(format!("({x}, {y}) is off the screen").into());
    }
    Ok(image.get_pixel(x, y).0)
}

/// The boxes of pixels `taken` takes: on each row the runs of `min` or more such pixels, runs less than 100 px apart
/// (the play mark splits a picture's row) taken as one span, rows joined while their spans overlap.
fn boxes(shot: &Path, taken: impl Fn([u8; 3]) -> bool, min: u32) -> SandboxResult<Vec<Rect>> {
    let image = image::open(shot)?.to_rgb8();
    let mut found: Vec<Rect> = Vec::new();
    for y in 0..image.height() {
        let mut spans: Vec<(u32, u32)> = Vec::new();
        let mut x = 0;
        while x < image.width() {
            if !taken(image.get_pixel(x, y).0) {
                x += 1;
                continue;
            }
            let start = x;
            while x < image.width() && taken(image.get_pixel(x, y).0) {
                x += 1;
            }
            if x - start >= min {
                match spans.last_mut() {
                    Some(last) if start - last.1 < 100 => last.1 = x,
                    _ => spans.push((start, x)),
                }
            }
        }
        for (x0, x1) in spans {
            let joined = found
                .iter_mut()
                .find(|open| open.y1 == y && open.x0 < x1 && x0 < open.x1);
            match joined {
                Some(open) => {
                    open.x0 = open.x0.min(x0);
                    open.x1 = open.x1.max(x1);
                    open.y1 = y + 1;
                }
                None => found.push(Rect {
                    x0,
                    y0: y,
                    x1,
                    y1: y + 1,
                }),
            }
        }
    }
    Ok(found)
}

/// YouTube's 4:3 `hqdefault` shape: 480×360, the 16:9 middle in [`PICTURE`], 45-row bars in [`BAR`].
fn hqdefault_png() -> SandboxResult<Vec<u8>> {
    let picture = image::RgbImage::from_fn(480, 360, |_, y| {
        if (45..315).contains(&y) {
            image::Rgb(PICTURE)
        } else {
            image::Rgb(BAR)
        }
    });
    let mut bytes = Vec::new();
    picture.write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

// --- the service ---

/// The chats the walk opens.
struct Chats {
    fail: String,
    group: String,
    shorts: String,
    pair: String,
    quotes: String,
    other: String,
}

/// The ids of the messages the walk reads.
struct Ids {
    fail: String,
    direct: String,
    long: String,
    shorts: String,
    pair: String,
    quoted: String,
    quoting: String,
    post: String,
    reply: String,
    other: String,
}

struct Service {
    mock: ChatServiceMock,
    trouter: TrouterMock,
    videos: Videos,
    chats: Chats,
    ids: Ids,
}

impl Service {
    fn start(runtime: &tokio::runtime::Runtime) -> SandboxResult<Self> {
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let videos = videos();
        let group = |topic: &str, members: &[&str]| mock.add_group(topic, members).ok_or("no group made");
        let chats = Chats {
            fail: group("Made-up broken picture", &[LAB_A, LAB_B])?,
            group: group("Made-up videos", &[LAB_A, LAB_B, LAB_C])?,
            shorts: group("Made-up shorts", &[LAB_A, LAB_B, LAB_C])?,
            pair: group("Made-up pair", &[LAB_A, LAB_B])?,
            quotes: group("Made-up quotes", &[LAB_A, LAB_B])?,
            other: group("Made-up other card", &[LAB_A, LAB_B])?,
        };
        let fail = post_cards(&mock, LAB_B, &chats.fail, "", &[&videos.fail])?;
        let direct = post_cards(&mock, LAB_C, AC_CHAT, "", &[&videos.direct])?;
        let long = post_cards(&mock, LAB_B, &chats.group, "", &[&videos.long])?;
        let shorts = post_cards(&mock, LAB_C, &chats.shorts, "", &[&videos.shorts])?;
        let pair = post_cards(&mock, LAB_B, &chats.pair, "", &[&videos.first, &videos.second])?;
        let quoted = post_cards(&mock, LAB_B, &chats.quotes, "", &[&videos.quoted])?;
        let quote = format!(
            "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"{quoted}\"><strong \
             itemprop=\"mri\" itemid=\"{LAB_B}\">Lab B</strong><span itemprop=\"time\" itemid=\"{quoted}\"></span>\
             <p itemprop=\"preview\">{}</p></blockquote>",
            videos.quoted.link
        );
        let quoting = post_cards(&mock, LAB_A, &chats.quotes, &quote, &[&videos.quoting])?;
        let other = post_other_card(&mock, &chats.other)?;
        let (post, reply) = channel_cards(&mock, &videos)?;
        // the deliveries queued before the cards were added: the reads give the messages with their cards
        let _ = mock.take_events(LAB_A);
        Ok(Self {
            mock,
            trouter,
            videos,
            chats,
            ids: Ids {
                fail,
                direct,
                long,
                shorts,
                pair,
                quoted,
                quoting,
                post,
                reply,
                other,
            },
        })
    }

    /// The pictures the gateway's thumbnail was asked for, in order.
    fn thumbnails(&self, runtime: &tokio::runtime::Runtime) -> Vec<String> {
        runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.url.path() == THUMBNAIL)
            .filter_map(|request| {
                request
                    .url
                    .query_pairs()
                    .find(|(name, _)| name == "url")
                    .map(|(_, url)| url.into_owned())
            })
            .collect()
    }
}

/// `from` posts `before` and the links of `videos` to `chat`, each with its card, as Teams' compose box sends them; its
/// id.
fn post_cards(
    mock: &ChatServiceMock,
    from: &str,
    chat: &str,
    before: &str,
    videos: &[&Video],
) -> SandboxResult<String> {
    let html = message_html(before, videos);
    let id = mock.post_as(from, chat, &html).ok_or("a post refused")?;
    add_cards(mock, chat, &id, videos)?;
    Ok(id)
}

/// The message's content: `before`, a paragraph with each link, an `InputExtension` span for each card.
fn message_html(before: &str, videos: &[&Video]) -> String {
    let links: Vec<String> = videos
        .iter()
        .map(|video| {
            format!(
                "<a href=\"{0}\" rel=\"noreferrer noopener\" title=\"{0}\" target=\"_blank\">{0}</a>",
                video.link
            )
        })
        .collect();
    let spans: Vec<String> = videos
        .iter()
        .map(|video| {
            format!(
                "\n<span itemid=\"{}\" itemscope=\"\" itemtype=\"http://schema.skype.com/InputExtension\"><span \
                 itemprop=\"cardId\"></span></span>",
                client_id(video)
            )
        })
        .collect();
    let spans = spans.concat();
    format!("{before}<p>{} </p>{spans}", links.join(" "))
}

fn client_id(video: &Video) -> String {
    format!("app-preview-card-made-up-{}", video.id)
}

/// Gives message `id` of `chat` the cards of `videos` and their links, as the compose box sends them.
fn add_cards(mock: &ChatServiceMock, chat: &str, id: &str, videos: &[&Video]) -> SandboxResult {
    let cmid = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let cards: Vec<Value> = videos.iter().map(|video| card_entry(video)).collect();
    let links: Vec<Value> = videos
        .iter()
        .enumerate()
        .map(|(n, video)| {
            json!({
                "@type": "http://schema.skype.com/HyperLink", "itemid": n.to_string(), "url": video.link,
                "previewenabled": false,
                "preview": {"previewurl": format!("https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/content?url=https%3a%2f%2fi.ytimg.com%2fvi%2f{}%2fmqdefault.jpg", video.id), "title": video.title, "description": video.counts},
            })
        })
        .collect();
    let changed = mock.change_message(chat, &cmid, |message| {
        message["properties"]["cards"] = json!(Value::from(cards).to_string());
        message["properties"]["links"] = json!(Value::from(links).to_string());
    });
    if changed {
        Ok(())
    } else {
        Err(format!("message {id} is not in {chat}").into())
    }
}

/// One entry of `properties.cards`: the YouTube app's Adaptive Card 1.6 of `video`, as captured.
fn card_entry(video: &Video) -> Value {
    let open = json!({
        "data": {"msteams": {"type": "invoke", "value": {"type": "tab/tabInfoAction", "tabInfo": {
            "contentUrl": format!("https://tabs.teams.microsoft.com/youtubeContentStage?videoId%3d{}&chatId={{chat.id}}&theme={{app.theme}}&userId={{user.id}}", video.id),
            "websiteUrl": video.watch, "entityId": "00000000-0000-4000-8000-0000000004e5",
        }}}},
        "type": "Action.Submit",
    });
    let mut media = json!({"sources": [{"mimeType": "video/mp4", "url": video.watch}], "type": "Media"});
    if let Some(poster) = &video.poster {
        media["poster"] = json!(poster);
    }
    let mut button = open.clone();
    button["title"] = json!("Open");
    json!({
        "appId": APP_ID, "cardClientId": client_id(video), "appName": "YouTube", "appIcon": video.icon,
        "contentType": ADAPTIVE,
        "content": {
            "type": "AdaptiveCard",
            "body": [
                media,
                {"color": null, "horizontalAlignment": null, "isSubtle": false, "maxLines": 4, "size": "large",
                 "text": video.title, "weight": "bolder", "wrap": true, "type": "TextBlock"},
                {"color": null, "horizontalAlignment": null, "isSubtle": true, "maxLines": 1, "size": null,
                 "text": video.channel, "weight": null, "wrap": true, "spacing": "Small", "type": "TextBlock"},
                {"color": null, "horizontalAlignment": null, "isSubtle": true, "maxLines": 1, "size": null,
                 "text": video.counts, "weight": null, "wrap": true, "spacing": "None", "type": "TextBlock"},
                {"actions": [button], "spacing": "Medium", "type": "ActionSet"},
            ],
            "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
            "version": "1.6",
            "fallbackText": "This card requires CaptionSource to be viewed.",
            "selectAction": open,
        },
    })
}

/// b's message with a card entry of a type this build does not draw (an Office 365 connector card).
fn post_other_card(mock: &ChatServiceMock, chat: &str) -> SandboxResult<String> {
    let id = mock
        .post_as(LAB_B, chat, "<p>A made-up connector update</p>")
        .ok_or("a post refused")?;
    let cmid = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    let cards = json!([{
        "appId": "made-up.connector", "cardClientId": "app-preview-card-made-up-other", "appName": "Made-up Connector",
        "contentType": "application/vnd.microsoft.teams.card.o365connector",
        "content": {"title": "A made-up connector card", "sections": []},
    }]);
    if mock.change_message(chat, &cmid, |message| {
        message["properties"]["cards"] = json!(cards.to_string());
    }) {
        Ok(id)
    } else {
        Err("the other card's message is not in the mock".into())
    }
}

/// b's post with a card in the team's channel, and c's reply with a card in its thread; their ids.
fn channel_cards(mock: &ChatServiceMock, videos: &Videos) -> SandboxResult<(String, String)> {
    let post = mock
        .add_message(
            TEAM_CHANNEL,
            (LAB_B, "Lab B"),
            "RichText/Html",
            &message_html("", &[&videos.post]),
        )
        .ok_or("no channel post")?;
    let cmid = (9_000_000_000_000_000_000_u64 + post.parse::<u64>()?).to_string();
    if !mock.change_message(TEAM_CHANNEL, &cmid, |message| {
        message["conversationLink"] = json!(format!(
            "{CHAT_BASE}/v1/users/ME/conversations/{TEAM_CHANNEL};messageid={post}"
        ));
    }) {
        return Err("the channel post is not in the mock".into());
    }
    add_cards(mock, TEAM_CHANNEL, &post, &[&videos.post])?;
    let reply = mock
        .add_message(
            &format!("{TEAM_CHANNEL};messageid={post}"),
            (LAB_C, "Lab C"),
            "RichText/Html",
            &message_html("", &[&videos.reply]),
        )
        .ok_or("no channel reply")?;
    add_cards(mock, TEAM_CHANNEL, &reply, &[&videos.reply])?;
    Ok((post, reply))
}

// --- what the app shows ---

/// A card as the child tells it.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct CardRow {
    app: Option<String>,
    icon: bool,
    media: Option<String>,
    picture: Option<String>,
    texts: Vec<String>,
    open: Option<String>,
    select: Option<String>,
}

/// A message's row as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct Item {
    id: String,
    own: bool,
    quote: String,
    links: usize,
    unshown_cards: bool,
    cards: Vec<CardRow>,
}

/// The open view as the child tells it.
#[derive(Debug, serde::Deserialize)]
struct State {
    items: Vec<Item>,
}

impl State {
    fn item(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// The child's view trace, line by line as it comes.
struct TraceReader {
    lines: mpsc::Receiver<Value>,
    seen: Vec<Value>,
}

impl TraceReader {
    fn start(reader: io::PipeReader) -> Self {
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(reader).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
                let _ = sender.send(value);
            }
        });
        Self {
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a line, not seen before, that `found` takes, named `what` in the error.
    fn next_where(&mut self, what: &str, found: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .map_err(|_| format!("no trace line {what}; the last seen {:?}", self.seen.last()))?;
            self.seen.push(line.clone());
            if found(&line) {
                return Ok(line);
            }
        }
    }
}

/// The app running in the sandbox: its process, its view trace, its control socket and the user's pointer.
struct AppRun {
    running: Running,
    trace: TraceReader,
    ctl: PathBuf,
    dir: PathBuf,
    input: VirtualInput,
    /// Where the pointer rests between clicks: on the empty title bar, away from every card and button.
    rest: (u32, u32),
    asked: u32,
}

impl AppRun {
    /// Starts the app on `service` with its store and settings in `dir`; once its window is drawn.
    fn launch(sandbox: &Sandbox, service: &Service, dir: &Path) -> SandboxResult<Self> {
        let (reader, writer) = io::pipe()?;
        let fd = writer.as_raw_fd().to_string();
        let token = service.mock.token(LAB_A).ok_or("the mock's token for a")?;
        let origin = service.mock.origin();
        let dir_text = dir.to_string_lossy().into_owned();
        let _ = fs::remove_file(dir.join(THEME_FILE));
        let child = sandbox.spawn_with_fds(
            &std::env::current_exe()?,
            &[],
            &[
                (CHILD_ENV, "app"),
                (TRACE_FD_ENV, &fd),
                (ORIGIN_ENV, &origin),
                (TROUTER_ENV, service.trouter.origin()),
                (TOKEN_ENV, &token),
                (DIR_ENV, &dir_text),
            ],
            &[writer.as_fd()],
        )?;
        drop(writer);
        let mut running = Running::new(child)?;
        let trace = TraceReader::start(reader);
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| running.fail(&format!("no ready line: {error:?}")))?;
        let ctl = dir.join("ctl.sock");
        wait_until(WAIT, || Client::connect(&ctl, Timeouts::default()).ok()).ok_or("no control socket")?;
        let mut run = Self {
            running,
            trace,
            ctl,
            dir: dir.to_owned(),
            input: sandbox.input()?,
            rest: (200, 24),
            asked: 0,
        };
        run.input.move_to(run.rest.0, run.rest.1)?;
        Ok(run)
    }

    /// `ngteamsctl open chat <chat>`, then the trace's `chat opened`.
    fn open(&mut self, chat: &str) -> SandboxResult {
        let mut client = Client::connect(&self.ctl, Timeouts::default())?;
        client.request(
            None,
            Request::Open {
                target: OpenTarget::Chat { chat: chat.to_owned() },
            },
        )?;
        self.trace.next_where(&format!("chat {chat} opened"), |line| {
            line["view"] == "chat" && line["change"] == "opened" && line["conversation"] == chat
        })?;
        Ok(())
    }

    /// Asks the child `verb`; its answer.
    fn question(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.dir.join(format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.dir.join(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    /// Asks the child to do `verb` on the shell; an error unless it answers `ok`.
    fn verb(&mut self, verb: &str) -> SandboxResult {
        let answer = self.question(verb)?;
        if answer == "ok" {
            Ok(())
        } else {
            Err(format!("{verb}: {answer}").into())
        }
    }

    fn state_in(&mut self, view: &str) -> SandboxResult<State> {
        let answer = self.question(&format!("state {view}"))?;
        serde_json::from_str(&answer).map_err(|error| format!("the state {answer}: {error}").into())
    }

    /// Waits up to [`WAIT`] for a state of the chat that `wanted` takes.
    fn until(&mut self, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        self.until_in("chat", what, wanted)
    }

    /// Waits up to [`WAIT`] for a state of `view` (`chat`, `posts` or `thread`) that `wanted` takes, named `what` in
    /// the error.
    fn until_in(&mut self, view: &str, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state_in(view)?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:#?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Shows the app in Teams Light or Dark (`dark`), as the user picks it in Settings; once it is applied.
    fn theme(&mut self, dark: bool) -> SandboxResult {
        let key = if dark { "dark" } else { "light" };
        fs::write(self.dir.join(THEME_FILE), key)?;
        let applied = format!("theme {key}");
        self.running
            .line_where(WAIT, |line| line == applied)
            .map_err(|error| format!("{applied} never applied: {error:?}"))?;
        Ok(())
    }

    /// A screenshot of the screen, kept as `target/ui-shots/<name>`; the sandbox's copy.
    fn shot(&mut self, sandbox: &Sandbox, name: &str) -> SandboxResult<PathBuf> {
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(600));
        let png = sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }

    /// Waits until the screen shows `count` or more video pictures; the shot and the lowest picture.
    fn picture(&mut self, sandbox: &Sandbox, name: &str, count: usize) -> SandboxResult<(PathBuf, Rect)> {
        let start = Instant::now();
        loop {
            thread::sleep(SETTLE);
            let shot = self.shot(sandbox, name)?;
            let pictures = boxes(&shot, |pixel| near(pixel, PICTURE, 12), 100)?;
            if pictures.len() >= count
                && let Some(lowest) = pictures.iter().max_by_key(|found| found.y0)
                && lowest.height() > 40
            {
                return Ok((shot, *lowest));
            }
            if start.elapsed() > WAIT {
                return Err(format!("no video picture on the screen: {pictures:?} ({})", shot.display()).into());
            }
        }
    }

    /// Clicks (`x`, `y`) as a user does; the link the app asked the opener for.
    fn click_opens(&mut self, sandbox: &Sandbox, x: u32, y: u32) -> SandboxResult<String> {
        let before = sandbox.opened()?.len();
        self.input.click(x, y, keys::BTN_LEFT)?;
        let opened = wait_until(WAIT, || sandbox.opened().ok()?.get(before).cloned())
            .ok_or_else(|| format!("a click at ({x}, {y}) opened nothing"))?;
        self.input.move_to(self.rest.0, self.rest.1)?;
        thread::sleep(Duration::from_millis(300));
        let after = sandbox.opened()?.len();
        if after != before + 1 {
            return Err(format!("a click at ({x}, {y}) opened {} links", after - before).into());
        }
        opened
            .args
            .last()
            .cloned()
            .ok_or_else(|| format!("the opener got no address: {opened:?}").into())
    }

    /// The error, with the last screen and the app's stderr.
    fn fail(&mut self, sandbox: &Sandbox, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let shot = self
            .shot(sandbox, "qa-app-card-failed.png")
            .map_or_else(|_| String::new(), |path| path.display().to_string());
        self.running.fail(&format!("{error} (last screen: {shot})"))
    }

    /// Quits through the tray menu, as a user does; once the app ended and released its engine.
    fn quit(mut self, sandbox: &Sandbox) -> SandboxResult {
        let name = format!("org.kde.StatusNotifierItem-{}-1", self.running.pid());
        let host = sandbox.bus().connect()?;
        let menu = zbus::blocking::Proxy::new(&host, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")?;
        menu.call_method("Event", &(MENU_QUIT, "clicked", zbus::zvariant::Value::from(""), 0_u32))?;
        self.running
            .line_where(WAIT, |line| line == "engine-released")
            .map_err(|error| self.running.fail(&format!("the engine wasn't released: {error:?}")))?;
        let (status, stderr) = self.running.finish(WAIT)?;
        if !status.success() {
            return Err(format!("the app exited {status}: {stderr}").into());
        }
        Ok(())
    }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

// --- the child ---

/// One account's mock token, for the transport and the push channel alike.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// a's engine on the parent's mock, its store in `dir`.
fn engine_on_the_mock(dir: &Path) -> SandboxResult<EngineHandle> {
    let origin = std::env::var(ORIGIN_ENV)?;
    let tokens = Arc::new(Tokens {
        token: std::env::var(TOKEN_ENV)?,
        renewals: broadcast::channel(4).0,
    });
    let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
    let transport = Arc::new(http()?);
    let trouter = Trouter::with_mock_origin(
        Arc::clone(&transport),
        Arc::clone(&tokens),
        &std::env::var(TROUTER_ENV)?,
    )?
    .with_options(Options {
        backoff_base: Duration::from_millis(50),
        backoff_cap: Duration::from_millis(50),
        backoff_jitter_percent: 0,
        ..Options::default()
    });
    let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
    Ok(engine::start(Parts {
        calls: None,
        store: Store::open(&support::store_file(dir, Alias::A))?,
        client: Arc::new(ChatClient::new(http()?.with_ams(AMS_BASE)?.with_csa(CSA_BASE)?)),
        outbox: Outbox::new(transport, author),
        push: Some(Push::new(trouter)),
        options: EngineOptions::default(),
        known_names: Vec::new(),
    })?)
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// The settings as the Appearance page leaves them once theme `key` is picked.
fn show_theme(settings: &mut teams_ui::Settings, key: &'static str) {
    settings.appearance.follow_system = false;
    settings.appearance.theme = key;
}

/// Shows the theme `file` names (`light` or `dark`) once it changes, as the user picks one in Settings, and says
/// `theme <name>` once it is applied.
fn follow_theme_file(settings: &Entity<teams_ui::SettingsModel>, file: PathBuf, cx: &mut App) {
    let settings = settings.downgrade();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let mut applied = String::new();
        loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            let Ok(wanted) = fs::read_to_string(&file) else {
                continue;
            };
            if wanted == applied {
                continue;
            }
            let key = if wanted == "dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            let changed =
                cx.update(|cx| settings.update(cx, |model, cx| model.change(|chosen| show_theme(chosen, key), cx)));
            if changed.is_err() {
                return;
            }
            // the frames in the new theme
            cx.background_executor().timer(Duration::from_millis(300)).await;
            say(&format!("theme {wanted}"));
            applied = wanted;
        }
    })
    .detach();
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let fd: RawFd = std::env::var(TRACE_FD_ENV)?.parse()?;
    let trace = teams_ui::ViewTrace::new(adopt_trace(fd)?);
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let engine = engine_on_the_mock(&dir)?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(dir.join("ui").join("a.toml"));
    let runner = support::runner(&dir)?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let ctl = dir.join("ctl.sock");
    let theme_file = dir.join(THEME_FILE);
    let ask = dir.join(ASK);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(trace),
                ctl: Some(ctl),
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let settings = launched.desktop.read(cx).settings().clone();
                follow_theme_file(&settings, theme_file.clone(), cx);
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, ask.clone(), cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    let store = Store::open(&support::store_file(&dir, Alias::A))?;
    if store.engine_lock().is_ok() {
        say("engine-released");
    }
    Ok(())
}

/// Answers the parent's questions as they come.
fn answer(window: WindowHandle<KitRoot>, ask: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = window
                .update(cx, |kit, window, cx| {
                    let shell = kit
                        .view()
                        .clone()
                        .downcast::<Root>()
                        .ok()?
                        .read(cx)
                        .shell_in(cx)
                        .cloned()?;
                    Some(on_shell(&shell, verb, window, cx))
                })
                .ok()
                .flatten()
                .unwrap_or_else(|| "no shell".to_owned());
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let (verb, argument) = verb.split_once(' ').unwrap_or((verb, ""));
    match verb {
        "state" => {
            let view = match argument {
                "posts" => shell.read(cx).channel().map(|channel| channel.read(cx).posts().clone()),
                "thread" => shell
                    .read(cx)
                    .channel()
                    .and_then(|channel| channel.read(cx).thread().map(|(_, view)| view.clone())),
                _ => shell.read(cx).chat().cloned(),
            };
            view.map_or_else(|| json!({"items": []}), |view| state_of(view.read(cx)))
                .to_string()
        }
        "open-channel" => {
            let list = shell.read(cx).list().clone();
            list.update(cx, |list, cx| list.open(TEAM_CHANNEL.to_owned(), cx));
            "ok".to_owned()
        }
        "open-thread" => {
            let Some(channel) = shell.read(cx).channel().cloned() else {
                return "no channel".to_owned();
            };
            channel.update(cx, |channel, cx| channel.open_thread(argument.to_owned(), window, cx));
            "ok".to_owned()
        }
        other => format!("unknown {other}"),
    }
}

/// The view's rows with their app cards, as [`State`] reads them.
fn state_of(view: &ChatView) -> Value {
    let items: Vec<Value> = view.message_items().map(item_of).collect();
    json!({ "items": items })
}

fn item_of(item: &MessageItem) -> Value {
    let quote = match &item.body {
        Body::Text(rendered) => rendered
            .0
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Quote(quote) => Some(quote.blocks.iter()),
                _ => None,
            })
            .flatten()
            .flat_map(Block::plain_lines)
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let cards: Vec<Value> = item
        .app_cards
        .as_ref()
        .map(|cards| cards.0.cards.iter().map(card_of).collect())
        .unwrap_or_default();
    json!({
        "id": item.id, "own": item.own, "quote": quote, "links": item.links.len(),
        "unshown_cards": item.unshown.cards, "cards": cards,
    })
}

fn card_of(card: &Card) -> Value {
    let link = |kind: &ActionKind| match kind {
        ActionKind::OpenTab(url) | ActionKind::OpenUrl(url) => Some(url.clone()),
        _ => None,
    };
    let (mut media, mut picture, mut texts, mut open) = (None, None, Vec::new(), None);
    for element in &card.body {
        match &element.kind {
            Kind::Media(found) => {
                media = found.link().map(str::to_owned);
                picture = found.picture();
            }
            Kind::Text(text) => texts.push(text.text.clone()),
            Kind::Actions(actions) => {
                open = actions
                    .iter()
                    .find(|action| action.title == "Open")
                    .and_then(|action| link(&action.kind));
            }
            _ => {}
        }
    }
    let app = card.app.as_ref();
    json!({
        "app": app.and_then(|app| app.name.clone()),
        "icon": app.and_then(teams_proto::chat::card::CardApp::icon_picture).is_some(),
        "media": media, "picture": picture, "texts": texts, "open": open,
        "select": card.select_action.as_ref().and_then(|action| link(&action.kind)),
    })
}
