//! QA of task 670 (round 1), in the UI sandbox: a picture of the user's own for each account. The real app runs with
//! mock accounts that have the SAME profile picture ([`portal::PROFILE`]); the user tells them apart by the pictures
//! chosen. The user works with the sandbox's pointer and keyboard, and picks files in a fake file chooser
//! ([`portal::Chooser`]). Synthetic pictures only, made here.
//!
//! Scenes ([`SCENES`]; `QA_AP_SCENES` names the ones to run):
//! - `menu`: three accounts. Cancel the chooser; PNG, JPEG, GIF (its first frame), WebP, a PNG named `.gif`, a wide
//!   and a very large picture (the centre, not stretched); an empty, a broken, a text and a 65 MB file (a reason,
//!   nothing changes); "Use profile picture". Only that account changes on the rail, the title bar and the menu. A
//!   message and a notification from b, and b's face in the chat, keep b's profile picture. Light and Dark.
//! - `restart`: the next run on the same files, the source files deleted: the pictures stay. "Sign out and remove"
//!   deletes b's file. The run after it adds b again: its profile picture shows.
//! - `initials`: c has no profile picture: after "Use profile picture" it shows its initials.
//! - `settings`: Settings > Accounts, its Tab order and its buttons, in Light, Dark and a font of 20 px.
//! - `narrow`: a window 480 px wide: the rail, the menu and Settings > Accounts in Light and Dark. The title bar's
//!   face and its presence dot show whole, with the same space at their right as at 1280 px.
//!
//! Each file chooser of "Change picture" has a title that names a picture, shows the picture files by default and
//! offers all files too.
//!
//! `QA_AP_EVIDENCE` names a directory that gets the screenshots and notes too; they also go to
//! `target/ui-shots/qa670-*.png`. Without weston 15, niri or `dbus-daemon` it says so and passes, unless
//! `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
use std::{fs, thread};

use image::RgbImage;
use serde_json::Value;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

#[path = "qa_account_picture_child.rs"]
mod child;
#[path = "qa_account_picture_portal.rs"]
mod portal;

use portal::{Answer, Chooser, Files, Rgb};

const EVIDENCE_ENV: &str = "QA_AP_EVIDENCE";
const STARTUP: Duration = Duration::from_secs(180);
const WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_millis(700);
const STEP: Duration = Duration::from_millis(250);
/// The rail's width, px.
const RAIL: u32 = 76;
/// The title bar's height, px.
const TITLE: u32 = 48;
/// The fewest pixels of a face: more than a presence dot, a border or a focus line has.
const MIN_FACE: usize = 250;
/// How far a pixel may be from a synthetic colour.
const TOLERANCE: i32 = 26;
/// The space right of the title bar's face, px: as at 1280 px wide.
const TITLE_GAP: u32 = 14;
/// [`TITLE_GAP`] at a font of 20 px, as at 1280 px wide: the title bar's padding grows with the font.
const TITLE_GAP_FONT_20: u32 = 19;
/// Available's green: the presence dot.
const AVAILABLE: Rgb = [0x6b, 0xb7, 0x00];
/// The title of the picture's file chooser.
const PICTURE_TITLE: &str = "Choose a picture";
/// The picture files the chooser shows by default.
const PICTURE_GLOBS: [&str; 5] = ["*.png", "*.jpg", "*.jpeg", "*.gif", "*.webp"];

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(child::CHILD_ENV) {
        Ok(mode) => child::child(&mode),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_account_picture_window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn ensure(holds: bool, what: impl FnOnce() -> String) -> SandboxResult {
    if holds { Ok(()) } else { Err(what().into()) }
}

fn evidence() -> Option<PathBuf> {
    std::env::var_os(EVIDENCE_ENV).map(PathBuf::from)
}

/// Adds `line` to the evidence notes and says it.
fn note(line: &str) {
    say(&format!("note {line}"));
    if let Some(dir) = evidence() {
        let _ = fs::create_dir_all(&dir);
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("notes.txt"))
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

type Scene = (&'static str, fn() -> SandboxResult);

const SCENES: [Scene; 4] = [
    ("menu", menu_restart_remove),
    ("initials", initials),
    ("settings", settings_page),
    ("narrow", narrow),
];

fn parent() -> SandboxResult {
    let only = std::env::var("QA_AP_SCENES").unwrap_or_default();
    let mut failed = Vec::new();
    let mut passed = 0_usize;
    for (name, scene) in SCENES {
        if !only.is_empty() && !only.split(',').any(|wanted| wanted.trim() == name) {
            continue;
        }
        match scene() {
            Ok(()) => {
                passed += 1;
                say(&format!("ok {name}"));
            }
            Err(error) => {
                say(&format!("FAILED {name}: {error}"));
                failed.push(format!("{name}: {error}"));
            }
        }
    }
    say(&format!("scenes: {passed} passed, {} failed", failed.len()));
    if failed.is_empty() {
        say("qa_account_picture_window: ok");
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

fn sandbox(width: u32) -> SandboxResult<Option<Sandbox>> {
    let options = SandboxOptions {
        width,
        height: 800,
        niri_extra: NIRI_FULL_WINDOW,
        fill_output: true,
        ..SandboxOptions::default()
    };
    sandbox_or_skip("qa_account_picture_window", &options)
}

// --- the scenes ---

/// Three accounts in one window: the account menu's flows, then the next runs (see the module docs).
fn menu_restart_remove() -> SandboxResult {
    let Some(sandbox) = sandbox(1280)? else {
        return Ok(());
    };
    let chooser = Chooser::start(sandbox.bus().address())?;
    let files = Files::make()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, &chooser, root.path(), "abc:abc")?;
    let outcome = menu_flows(&mut user, &files).and_then(|()| others_keep_profile(&mut user));
    let quit = user.finish(outcome);
    quit?;
    // the source files go: the copies stay
    drop(files);
    let mut user = User::start(&sandbox, &chooser, root.path(), "abc:abc")?;
    let outcome = restart_and_remove(&mut user, root.path());
    user.finish(outcome)?;
    let mut user = User::start(&sandbox, &chooser, root.path(), "abc:abc")?;
    let outcome = added_again(&mut user);
    user.finish(outcome)?;
    let asked = chooser.asked();
    note(&format!("file chooser calls: {}", asked.len()));
    for call in &asked {
        note(&format!("file chooser answered: {}", call.answered));
    }
    if let Some(first) = asked.first() {
        note(&format!(
            "file chooser: title {:?}, accept label {:?}, filters {:?}, current filter {:?}",
            first.title, first.accept_label, first.filters, first.current_filter
        ));
    }
    expect_picture_chooser(&asked)
}

/// Each file chooser of "Change picture": its title names a picture, it shows the picture files when it opens, and it
/// offers all files too.
fn expect_picture_chooser(asked: &[portal::Asked]) -> SandboxResult {
    ensure(!asked.is_empty(), || "no file chooser opened".to_owned())?;
    for call in asked {
        let pictures = call.filters.first();
        let shows_pictures = pictures.is_some_and(|filter| {
            PICTURE_GLOBS
                .iter()
                .all(|glob| filter.rules.iter().any(|rule| rule == glob))
        });
        let all_files = call
            .filters
            .iter()
            .any(|filter| filter.name == "All files" && filter.rules.iter().any(|rule| rule == "*"));
        ensure(
            call.title == PICTURE_TITLE && shows_pictures && call.current_filter.as_ref() == pictures && all_files,
            || format!("the picture's file chooser: {call:?}"),
        )?;
    }
    Ok(())
}

fn menu_flows(user: &mut User<'_>, files: &Files) -> SandboxResult {
    user.until("each account has its profile picture", |state| {
        tiles(state).iter().all(|tile| tile["profile"] == true)
    })?;
    user.expect_rail(&["profile", "profile", "profile"])?;
    user.shot("three-same-profile")?;
    // b shown: its rail face opens its menu
    user.click_rail(1)?;
    user.until("b shown", |state| state["shown"] == "b")?;
    user.open_menu_on_rail(1)?;
    user.shot("menu-b-profile")?;
    ensure(user.menu_face()? == Some("profile"), || {
        "b's menu shows no profile picture".to_owned()
    })?;
    // no "Use profile picture" yet: Change picture is right above Sign out and remove; the chooser is cancelled
    user.chooser.queue(Answer::Cancel);
    let asked = user.chooser.asked().len();
    user.keys(&["end", "up", "enter"])?;
    user.wait_asked(asked + 1)?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    ensure(
        tile(&state, "b")["chosen"] == false && tile(&state, "b")["problem"].is_null(),
        || format!("a cancel changed b: {state}"),
    )?;
    user.expect_rail(&["profile", "profile", "profile"])?;
    for (index, (name, colour)) in portal::GOOD.iter().enumerate() {
        user.pick_by_menu(&files.path(name), index > 0, index % 2 == 1)?;
        user.until(&format!("b shows {name}"), |state| tile(state, "b")["chosen"] == true)?;
        user.wait_rail(1, colour_name(*colour))?;
        user.expect_rail(&["profile", colour_name(*colour), "profile"])?;
        user.expect_title(colour_name(*colour))?;
        user.expect_round(*colour)?;
        user.shot(&format!("b-chose-{}", name.replace('.', "-")))?;
    }
    bad_files(user, files)?;
    user.open_menu_on_rail(1)?;
    ensure(user.menu_face()? == Some(colour_name(portal::LARGE)), || {
        "b's menu doesn't show its chosen picture".to_owned()
    })?;
    user.shot("menu-b-chosen")?;
    user.both_themes("menu-b-chosen")?;
    user.keys(&["end", "up", "enter"])?;
    user.until("b back to its profile picture", |state| {
        tile(state, "b")["chosen"] == false
    })?;
    user.expect_rail(&["profile", "profile", "profile"])?;
    user.shot("b-profile-again")?;
    // a and b each a picture of their own: c keeps its profile picture
    user.pick_by_menu(&files.path(portal::GOOD[0].0), false, false)?;
    user.click_rail(0)?;
    user.until("a shown", |state| state["shown"] == "a")?;
    user.pick_by_menu_at(0, &files.path(portal::GOOD[1].0), false, true)?;
    user.until("a and b chose", |state| {
        tile(state, "a")["chosen"] == true && tile(state, "b")["chosen"] == true
    })?;
    user.wait_rail(0, "jpeg")?;
    user.expect_rail(&["jpeg", "png", "profile"])?;
    user.expect_title("jpeg")?;
    user.shot("a-jpeg-b-png-c-profile")?;
    user.both_themes("a-jpeg-b-png-c-profile")
}

/// The files the app refuses: a reason, and b keeps the picture it shows.
fn bad_files(user: &mut User<'_>, files: &Files) -> SandboxResult {
    let shown = colour_name(portal::LARGE);
    for (name, words) in portal::BAD {
        user.pick_by_menu(&files.path(name), true, false)?;
        let state = user.until(&format!("{name} refused"), |state| {
            tile(state, "b")["problem"].is_string() && state["toast"].is_string()
        })?;
        let problem = tile(&state, "b")["problem"].as_str().unwrap_or_default().to_owned();
        let toast = state["toast"].as_str().unwrap_or_default().to_owned();
        note(&format!("{name}: toast {toast:?}; Settings reason {problem:?}"));
        ensure(toast.contains(words) && problem.contains(words), || {
            format!("{name}: the reason doesn't say {words:?}: {toast:?}")
        })?;
        ensure(tile(&state, "b")["chosen"] == true, || {
            format!("{name} took b's picture away")
        })?;
        user.expect_rail(&["profile", shown, "profile"])?;
        user.shot(&format!("b-refused-{}", name.replace('.', "-")))?;
        user.until("the toast gone", |state| state["toast"].is_null())?;
    }
    Ok(())
}

/// b posts in its chat with a: a's notification and a's chat show b's profile picture, not b's chosen one.
fn others_keep_profile(user: &mut User<'_>) -> SandboxResult {
    user.click_rail(2)?;
    user.until("c shown", |state| state["shown"] == "c")?;
    let before = user.sandbox.bus().notifications().len();
    let posted = user.ask("b-posts A made-up hello from b")?;
    ensure(posted.starts_with("posted"), || format!("b's post: {posted}"))?;
    let start = Instant::now();
    let banner = loop {
        let notes = user.sandbox.bus().notifications();
        if let Some(banner) = notes
            .get(before..)
            .and_then(|new| new.iter().find(|note| note.image.is_some()))
        {
            break banner.clone();
        }
        if start.elapsed() > WAIT {
            note(&format!(
                "no banner with a picture for b's post: {:?}",
                notes.get(before..)
            ));
            return Err("no notification with a picture for b's post".into());
        }
        thread::sleep(POLL);
    };
    let (profile, chosen) = banner_colours(&banner);
    note(&format!(
        "b's banner: {profile} profile pixels, {chosen} pixels of b's chosen picture"
    ));
    ensure(profile > 100 && chosen == 0, || {
        "b's banner doesn't show b's profile picture".to_owned()
    })?;
    user.click_rail(0)?;
    user.until("a shown", |state| state["shown"] == "a")?;
    user.ask(&format!("open-chat {}", teams_mock::AB_CHAT))?;
    user.until("a's chat with b open", |state| state["chat"] == teams_mock::AB_CHAT)?;
    thread::sleep(SETTLE);
    let shot = user.shot("a-chat-with-b")?;
    let image = image::open(&shot)?.to_rgb8();
    let outside = Rect::outside_rail_and_title(&image);
    let faces = blobs(&image, outside, portal::PROFILE);
    let leaked = blobs(&image, outside, portal::PNG).len() + blobs(&image, outside, portal::JPEG).len();
    note(&format!(
        "a's chat with b: {} faces of b's profile picture, {leaked} chosen",
        faces.len()
    ));
    ensure(!faces.is_empty() && leaked == 0, || {
        "a's chat doesn't show b's profile picture".to_owned()
    })?;
    if let Some(face) = faces.first() {
        user.click(face.centre())?;
        thread::sleep(SETTLE);
        let shot = user.shot("b-card-from-a")?;
        let image = image::open(&shot)?.to_rgb8();
        let leaked = blobs(&image, Rect::outside_rail_and_title(&image), portal::PNG).len();
        ensure(leaked == 0, || "b's card shows b's chosen picture".to_owned())?;
        user.keys(&["escape"])?;
    }
    Ok(())
}

fn banner_colours(banner: &ui_sandbox::bus::Notification) -> (usize, usize) {
    let Some((_, _, pixels)) = &banner.image else {
        return (0, 0);
    };
    let count = |colour: Rgb| {
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|[r, g, b, _]| near([*r, *g, *b], colour))
            .count()
    };
    (count(portal::PROFILE), count(portal::PNG))
}

fn restart_and_remove(user: &mut User<'_>, root: &Path) -> SandboxResult {
    user.until("a and b show the pictures kept", |state| {
        tile(state, "a")["chosen"] == true && tile(state, "b")["chosen"] == true && tile(state, "c")["chosen"] == false
    })?;
    user.wait_rail(1, "png")?;
    user.expect_rail(&["jpeg", "png", "profile"])?;
    user.shot("restart-kept")?;
    let kept = pictures(root)?;
    note(&format!("picture files after the restart: {kept:?}"));
    ensure(kept.len() == 2, || format!("picture files: {kept:?}"))?;
    user.click_rail(1)?;
    user.until("b shown", |state| state["shown"] == "b")?;
    user.open_menu_on_rail(1)?;
    user.shot("menu-b-before-remove")?;
    user.keys(&["end", "enter"])?;
    user.until("b removed", |state| tiles(state).len() == 2)?;
    thread::sleep(SETTLE);
    let left = pictures(root)?;
    note(&format!("picture files after b's removal: {left:?}"));
    ensure(
        left.len() == 1 && !left.iter().any(|file| file.contains("000000000b")),
        || format!("b's picture file stays: {left:?}"),
    )?;
    user.expect_rail(&["jpeg", "profile"])?;
    user.shot("b-removed").map(drop)
}

fn added_again(user: &mut User<'_>) -> SandboxResult {
    user.until("b back with its profile picture", |state| {
        tiles(state).len() == 3 && tile(state, "b")["profile"] == true && tile(state, "b")["chosen"] == false
    })?;
    user.wait_rail(0, "jpeg")?;
    user.expect_rail(&["jpeg", "profile", "profile"])?;
    user.shot("b-added-again").map(drop)
}

/// The files in the pictures' directory beside the settings file.
fn pictures(root: &Path) -> SandboxResult<Vec<String>> {
    let dir = child::settings_file(root)
        .parent()
        .ok_or("no settings dir")?
        .join("account-pictures");
    let Ok(read) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

/// c has no profile picture: "Use profile picture" shows its initials.
fn initials() -> SandboxResult {
    let Some(sandbox) = sandbox(1280)? else {
        return Ok(());
    };
    let chooser = Chooser::start(sandbox.bus().address())?;
    let files = Files::make()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, &chooser, root.path(), "ac:a")?;
    let outcome = (|| {
        user.until("a's profile picture", |state| tile(state, "a")["profile"] == true)?;
        let (_, second) = user.rail_slots()?;
        user.click((RAIL / 2, second))?;
        user.until("c shown", |state| state["shown"] == "c")?;
        user.shot("c-initials")?;
        // c shows its initials: its rail face has no colour of its own to find, so the click goes to its slot
        user.click((RAIL / 2, second))?;
        user.until("c's menu open", |state| state["menu"] == true)?;
        user.shot("menu-c-initials")?;
        user.pick_in_open_menu(&files.path(portal::GOOD[3].0), false)?;
        user.until("c chose", |state| tile(state, "c")["chosen"] == true)?;
        user.wait_rail(1, "webp")?;
        user.shot("c-chose-webp")?;
        user.open_menu_on_rail(1)?;
        user.keys(&["end", "up", "enter"])?;
        user.until("c back to its initials", |state| tile(state, "c")["chosen"] == false)?;
        thread::sleep(SETTLE);
        let shot = user.shot("c-initials-again")?;
        let image = image::open(&shot)?.to_rgb8();
        let left = blobs(&image, Rect::rail(&image), portal::WEBP).len();
        ensure(left == 0, || {
            "c still shows the WebP after Use profile picture".to_owned()
        })?;
        user.ask("settings-accounts")?;
        thread::sleep(SETTLE);
        user.shot("c-initials-settings")?;
        Ok(())
    })();
    user.finish(outcome)
}

/// Settings > Accounts: its rows, its Tab order and its buttons; Light, Dark and a font of 20 px.
fn settings_page() -> SandboxResult {
    let Some(sandbox) = sandbox(1280)? else {
        return Ok(());
    };
    let chooser = Chooser::start(sandbox.bus().address())?;
    let files = Files::make()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, &chooser, root.path(), "abc:abc")?;
    let outcome = (|| {
        user.until("each account has its profile picture", |state| {
            tiles(state).iter().all(|tile| tile["profile"] == true)
        })?;
        user.click_rail(1)?;
        user.until("b shown", |state| state["shown"] == "b")?;
        user.pick_by_menu_at(1, &files.path(portal::GOOD[0].0), false, false)?;
        user.until("b chose", |state| tile(state, "b")["chosen"] == true)?;
        user.ask("settings-accounts")?;
        user.until("Settings > Accounts", |state| state["page"] == "Accounts")?;
        user.shot("settings-accounts")?;
        tab_order(&mut user, &files)?;
        user.ask("settings-accounts")?;
        user.chooser.queue(Answer::Pick(files.path(portal::BAD[2].0)));
        // a shows a chosen picture: its Change picture is the second stop
        user.tab_to_change(2)?;
        let last = user.chooser.asked().last().map(|asked| asked.answered.clone());
        note(&format!(
            "Settings > Accounts, a's Change picture: the chooser gave {last:?}"
        ));
        user.until("a's reason under its row", |state| {
            tile(state, "a")["problem"].is_string()
        })?;
        user.shot("settings-reason-a")?;
        user.both_themes("settings-reason-a")?;
        user.ask("font 20")?;
        user.shot("settings-font-20")?;
        user.ask("theme dark")?;
        user.shot("settings-font-20-dark")?;
        user.ask("font 14")?;
        user.ask("theme light")?;
        expect_picture_chooser(&user.chooser.asked())
    })();
    user.finish(outcome)
}

/// Tab from the page list, then Enter, a press more each time. The stops follow the rows as they show, top to bottom,
/// left to right: a row that shows a chosen picture has "Use profile picture" before "Change picture". Each Enter
/// changes the rows, so the stops of the next round move: from a (profile), b (chosen), c (profile) they are a's
/// Change, a's Change (a has a Use profile picture now), b's Use profile picture, then c's Change.
fn tab_order(user: &mut User<'_>, files: &Files) -> SandboxResult {
    let mut stops = Vec::new();
    for presses in 1..=6_usize {
        user.ask("settings-accounts")?;
        thread::sleep(SETTLE);
        let before = (user.state()?, user.rail_names()?);
        let asked = user.chooser.asked().len();
        user.chooser.clear();
        let (name, _) = portal::GOOD[presses % portal::GOOD.len()];
        user.chooser.queue(Answer::Pick(files.path(name)));
        for _ in 0..presses {
            user.keys(&["tab"])?;
        }
        user.shot(&format!("settings-tab-{presses}"))?;
        user.keys(&["enter"])?;
        thread::sleep(Duration::from_secs(2));
        let after = (user.state()?, user.rail_names()?);
        let opened = user.chooser.asked().len() > asked;
        let effect = effect_of(&before, &after, opened);
        note(&format!("Settings > Accounts, Tab x{presses} then Enter: {effect}"));
        stops.push(effect.clone());
        // a file not given is not for a later press
        user.chooser.clear();
        if effect == "change c" {
            break;
        }
    }
    ensure(stops == ["change a", "change a", "profile b", "change c"], || {
        format!("Tab stops: {stops:?}")
    })
}

/// What an Enter did: an account back to its profile picture, an account's rail face changed, or nothing.
fn effect_of(before: &(Value, Vec<&str>), after: &(Value, Vec<&str>), opened: bool) -> String {
    let accounts = ["a", "b", "c"];
    for account in accounts {
        if tile(&before.0, account)["chosen"] == true && tile(&after.0, account)["chosen"] == false {
            return format!("profile {account}");
        }
    }
    if opened {
        for (index, account) in accounts.iter().enumerate() {
            if before.1.get(index) != after.1.get(index) {
                return format!("change {account}");
            }
        }
        return "chooser, no face changed".to_owned();
    }
    format!("nothing (main {})", after.0["main"])
}

/// A window 480 px wide: the rail, the menu and Settings > Accounts.
fn narrow() -> SandboxResult {
    let Some(sandbox) = sandbox(480)? else {
        return Ok(());
    };
    let chooser = Chooser::start(sandbox.bus().address())?;
    let files = Files::make()?;
    let root = tempfile::tempdir()?;
    let mut user = User::start(&sandbox, &chooser, root.path(), "ab:ab")?;
    let outcome = (|| {
        user.until("each account has its profile picture", |state| {
            tiles(state).iter().all(|tile| tile["profile"] == true)
        })?;
        user.click_rail(1)?;
        user.until("b shown", |state| state["shown"] == "b")?;
        user.pick_by_menu_at(1, &files.path(portal::GOOD[0].0), false, false)?;
        user.until("b chose", |state| tile(state, "b")["chosen"] == true)?;
        user.wait_rail(1, "png")?;
        user.both_themes("narrow-rail")?;
        user.expect_title_whole("png", TITLE_GAP)?;
        user.ask("theme dark")?;
        user.expect_title_whole("png", TITLE_GAP)?;
        user.ask("theme light")?;
        user.open_menu_on_rail(1)?;
        user.both_themes("narrow-menu")?;
        user.keys(&["escape"])?;
        user.ask("settings-accounts")?;
        user.both_themes("narrow-settings")?;
        user.ask("font 20")?;
        user.shot("narrow-settings-font-20")?;
        user.expect_title_whole("png", TITLE_GAP_FONT_20)?;
        user.ask("font 14")?;
        Ok(())
    })();
    user.finish(outcome)
}

// --- the state ---

fn tiles(state: &Value) -> Vec<Value> {
    state["tiles"].as_array().cloned().unwrap_or_default()
}

fn tile(state: &Value, account: &str) -> Value {
    tiles(state)
        .into_iter()
        .find(|tile| tile["account"] == account)
        .unwrap_or(Value::Null)
}

// --- the pixels ---

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    fn rail(image: &RgbImage) -> Self {
        Self {
            x: 0,
            y: TITLE,
            w: RAIL,
            h: image.height() - TITLE,
        }
    }

    fn title(image: &RgbImage) -> Self {
        Self {
            x: image.width() - 90,
            y: 0,
            w: 90,
            h: TITLE,
        }
    }

    fn outside_rail_and_title(image: &RgbImage) -> Self {
        Self {
            x: RAIL,
            y: TITLE,
            w: image.width() - RAIL,
            h: image.height() - TITLE,
        }
    }
}

/// A face found: its box and its pixels.
#[derive(Clone, Copy, Debug)]
struct Blob {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    count: usize,
}

impl Blob {
    fn centre(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    fn width(self) -> u32 {
        self.x1 - self.x0 + 1
    }

    fn height(self) -> u32 {
        self.y1 - self.y0 + 1
    }

    /// Round and full: a face, not a line, a border, a dot or a band.
    fn is_face(self) -> bool {
        let (w, h) = (self.width(), self.height());
        let count = u32::try_from(self.count).unwrap_or(u32::MAX);
        let filled = f64::from(count) / f64::from(w * h);
        self.count >= MIN_FACE && w >= 16 && w.abs_diff(h) <= 4 && (0.62..=0.92).contains(&filled)
    }
}

fn near(pixel: [u8; 3], colour: Rgb) -> bool {
    pixel
        .iter()
        .zip(colour)
        .all(|(have, want)| (i32::from(*have) - i32::from(want)).abs() <= TOLERANCE)
}

/// The faces of `colour` in `area`, top to bottom.
fn blobs(image: &RgbImage, area: Rect, colour: Rgb) -> Vec<Blob> {
    let (w, h) = (area.w as usize, area.h as usize);
    let mut seen = vec![false; w * h];
    let at = |x: usize, y: usize| {
        (
            area.x + u32::try_from(x).unwrap_or(u32::MAX),
            area.y + u32::try_from(y).unwrap_or(u32::MAX),
        )
    };
    let matches = |x: usize, y: usize| {
        let (x, y) = at(x, y);
        near(image.get_pixel(x, y).0, colour)
    };
    let mut found = Vec::new();
    for start in 0..w * h {
        if seen[start] || !matches(start % w, start / w) {
            continue;
        }
        seen[start] = true;
        let mut blob = Blob {
            x0: u32::MAX,
            y0: u32::MAX,
            x1: 0,
            y1: 0,
            count: 0,
        };
        let mut stack = vec![start];
        while let Some(cell) = stack.pop() {
            let (x, y) = (cell % w, cell / w);
            let (px, py) = at(x, y);
            grow(&mut blob, px, py);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h && !seen[ny * w + nx] && matches(nx, ny) {
                    seen[ny * w + nx] = true;
                    stack.push(ny * w + nx);
                }
            }
        }
        if blob.is_face() {
            found.push(blob);
        }
    }
    found.sort_by_key(|blob| blob.y0);
    found
}

/// The box of every pixel of `colour` in `area`: a small mark such as a presence dot.
fn spot(image: &RgbImage, area: Rect, colour: Rgb) -> Option<Blob> {
    let mut blob = Blob {
        x0: u32::MAX,
        y0: u32::MAX,
        x1: 0,
        y1: 0,
        count: 0,
    };
    for y in area.y..area.y + area.h {
        for x in area.x..area.x + area.w {
            if near(image.get_pixel(x, y).0, colour) {
                grow(&mut blob, x, y);
            }
        }
    }
    (blob.count > 0).then_some(blob)
}

fn grow(blob: &mut Blob, x: u32, y: u32) {
    blob.x0 = blob.x0.min(x);
    blob.y0 = blob.y0.min(y);
    blob.x1 = blob.x1.max(x);
    blob.y1 = blob.y1.max(y);
    blob.count += 1;
}

/// The synthetic colours of a face, by name.
const COLOURS: [(&str, Rgb); 9] = [
    ("profile", portal::PROFILE),
    ("png", portal::PNG),
    ("jpeg", portal::JPEG),
    ("gif", portal::GIF),
    ("gif-later", portal::GIF_LATER),
    ("webp", portal::WEBP),
    ("renamed", portal::RENAMED),
    ("wide", portal::WIDE),
    ("large", portal::LARGE),
];

fn colour_name(colour: Rgb) -> &'static str {
    COLOURS
        .iter()
        .find(|(_, held)| *held == colour)
        .map_or("?", |(name, _)| name)
}

/// Each face in `area`, top to bottom: its colour's name and its box.
fn faces(image: &RgbImage, area: Rect) -> Vec<(&'static str, Blob)> {
    let mut all: Vec<(&'static str, Blob)> = COLOURS
        .iter()
        .flat_map(|(name, colour)| blobs(image, area, *colour).into_iter().map(move |blob| (*name, blob)))
        .collect();
    all.sort_by_key(|(_, blob)| blob.y0);
    all
}

// --- the user ---

struct User<'a> {
    sandbox: &'a Sandbox,
    chooser: &'a Chooser,
    running: Option<Running>,
    input: VirtualInput,
    asked: u32,
    shots: u32,
    /// The faces outside the rail and the title bar before the menu opened ([`User::menu_face`]).
    before_menu: Vec<(u32, u32)>,
}

impl<'a> User<'a> {
    /// The app started in the sandbox with `mode` ([`child::CHILD_ENV`]) on the files in `root`.
    fn start(sandbox: &'a Sandbox, chooser: &'a Chooser, root: &Path, mode: &str) -> SandboxResult<Self> {
        let root = root.to_string_lossy().into_owned();
        let spawned = sandbox.spawn(
            &std::env::current_exe()?,
            &[],
            &[(child::CHILD_ENV, mode), (child::ROOT_ENV, &root)],
        )?;
        let mut running = Running::new(spawned)?;
        if let Err(error) = running.line_where(STARTUP, |line| line == "ready") {
            return Err(running.fail(&format!("no ready line: {error:?}{}", sandbox.logs())));
        }
        let mut user = Self {
            sandbox,
            chooser,
            running: Some(running),
            input: sandbox.input()?,
            asked: 0,
            shots: 0,
            before_menu: Vec::new(),
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    /// Ends the run: a screenshot when `outcome` failed, then the app quits.
    fn finish(&mut self, outcome: SandboxResult) -> SandboxResult {
        if outcome.is_err() {
            let _ = self.shot("failed");
        }
        let quit = self.quit();
        outcome?;
        quit
    }

    fn running(&self) -> SandboxResult<&Running> {
        self.running.as_ref().ok_or_else(|| "the app has quit".into())
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{}.staged", child::ASK));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(child::ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running()?
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<Value> {
        Ok(serde_json::from_str(&self.ask("state")?)?)
    }

    fn until(&mut self, what: &str, wanted: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state}").into());
            }
            thread::sleep(POLL);
        }
    }

    fn wait_asked(&self, count: usize) -> SandboxResult {
        let start = Instant::now();
        while self.chooser.asked().len() < count {
            if start.elapsed() > WAIT {
                return Err(format!("the file chooser never opened ({:?})", self.chooser.asked()).into());
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    fn quit(&mut self) -> SandboxResult {
        if self.running.is_none() {
            return Ok(());
        }
        self.ask("quit")?;
        let running = self.running.take().ok_or("the app has quit")?;
        let (status, stderr) = running.finish(WAIT)?;
        ensure(status.success(), || format!("the app exited {status}: {stderr}"))
    }

    fn keys(&mut self, specs: &[&str]) -> SandboxResult {
        for spec in specs {
            let (modifiers, key) =
                ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
            self.input.key_with(&modifiers, key)?;
            thread::sleep(STEP);
        }
        Ok(())
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn park(&mut self) -> SandboxResult {
        self.input.move_to(RAIL + 300, TITLE + 400)
    }

    /// A screenshot as `qa670-<n>-<name>.png`, under `target/ui-shots/` and the evidence dir.
    fn shot(&mut self, name: &str) -> SandboxResult<PathBuf> {
        self.park()?;
        thread::sleep(SETTLE);
        self.shots += 1;
        let file = format!("qa670-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .parent()
            .map_or_else(|| PathBuf::from("target"), Path::to_owned)
            .join("ui-shots");
        fs::create_dir_all(&dir)?;
        let kept = dir.join(&file);
        fs::copy(&png, &kept)?;
        if let Some(dir) = evidence() {
            let dir = dir.join("shots");
            fs::create_dir_all(&dir)?;
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(kept)
    }

    /// The screen in Teams Dark (`<name>-dark`), then Teams Light again (`<name>-light`).
    fn both_themes(&mut self, name: &str) -> SandboxResult {
        self.ask("theme dark")?;
        self.shot(&format!("{name}-dark"))?;
        self.ask("theme light")?;
        self.shot(&format!("{name}-light"))?;
        Ok(())
    }

    fn image(&mut self) -> SandboxResult<RgbImage> {
        let png = self.sandbox.screenshot("qa670-probe.png", false)?;
        Ok(image::open(&png)?.to_rgb8())
    }

    /// The faces on the rail, top to bottom.
    fn rail(&mut self) -> SandboxResult<Vec<(&'static str, Blob)>> {
        let image = self.image()?;
        Ok(faces(&image, Rect::rail(&image)))
    }

    /// The rail's second face's centre, when only the first one has a synthetic colour.
    fn rail_slots(&mut self) -> SandboxResult<(u32, u32)> {
        let rail = self.rail()?;
        let first = rail.first().ok_or("no face on the rail")?.1;
        Ok((first.centre().1, first.y1 + 8 + first.height() / 2 + 4))
    }

    fn rail_names(&mut self) -> SandboxResult<Vec<&'static str>> {
        Ok(self.rail()?.iter().map(|(name, _)| *name).collect())
    }

    fn expect_rail(&mut self, wanted: &[&str]) -> SandboxResult {
        let rail = self.rail()?;
        let names: Vec<&str> = rail.iter().map(|(name, _)| *name).collect();
        ensure(names == wanted, || format!("the rail shows {names:?}, not {wanted:?}"))
    }

    /// Waits until the rail's `index`-th face is `name` (a picture is read in the background).
    fn wait_rail(&mut self, index: usize, name: &str) -> SandboxResult {
        let start = Instant::now();
        loop {
            let rail = self.rail()?;
            if rail.get(index).is_some_and(|(held, _)| *held == name) {
                return Ok(());
            }
            if start.elapsed() > WAIT {
                return Err(format!("the rail's face {index} never became {name}: {rail:?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// The title bar shows one face, `name`, whole and with [`TITLE_GAP`] at its right.
    fn expect_title(&mut self, name: &str) -> SandboxResult {
        self.expect_title_gap(name, TITLE_GAP)
    }

    /// The title bar shows one face, `name`, whole and with `wanted` px at its right.
    fn expect_title_gap(&mut self, name: &str, wanted: u32) -> SandboxResult {
        let image = self.image()?;
        let title = faces(&image, Rect::title(&image));
        let names: Vec<&str> = title.iter().map(|(name, _)| *name).collect();
        ensure(names == [name], || format!("the title bar shows {names:?}, not {name}"))?;
        let gap = title.first().map(|(_, face)| image.width() - 1 - face.x1);
        ensure(gap.is_some_and(|gap| gap.abs_diff(wanted) <= 1), || {
            format!("the title bar's face has {gap:?} px at its right, not {wanted}: {title:?}")
        })
    }

    /// [`Self::expect_title_gap`], and the face's presence dot shows whole at its lower right, inside the window.
    fn expect_title_whole(&mut self, name: &str, gap: u32) -> SandboxResult {
        self.park()?;
        thread::sleep(SETTLE);
        self.expect_title_gap(name, gap)?;
        let image = self.image()?;
        let area = Rect::title(&image);
        let face = faces(&image, area).first().map(|(_, face)| *face);
        let dot = spot(&image, area, AVAILABLE);
        let whole = face.zip(dot).is_some_and(|(face, dot)| {
            let (cx, cy) = face.centre();
            dot.width() >= 6
                && dot.width().abs_diff(dot.height()) <= 1
                && dot.x0 > cx
                && dot.y0 > cy
                && dot.x1 + 1 < image.width()
        });
        ensure(whole, || {
            format!("the title bar's presence dot isn't whole: face {face:?}, dot {dot:?}")
        })
    }

    /// The shown account's rail face of `colour` is a full disc: the centre, not the whole picture squeezed.
    fn expect_round(&mut self, colour: Rgb) -> SandboxResult {
        let image = self.image()?;
        let rail = blobs(&image, Rect::rail(&image), colour);
        let face = rail.first().ok_or_else(|| format!("no {} face", colour_name(colour)))?;
        let whites = (face.x0..=face.x1)
            .flat_map(|x| (face.y0..=face.y1).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let (cx, cy) = face.centre();
                let r = f64::from(face.width()) * 0.35;
                let (dx, dy) = (f64::from(x) - f64::from(cx), f64::from(y) - f64::from(cy));
                dx.hypot(dy) < r && near(image.get_pixel(x, y).0, portal::WHITE)
            })
            .count();
        ensure(whites == 0 && face.width().abs_diff(face.height()) <= 2, || {
            format!(
                "the {} face isn't the centre square: {face:?}, {whites} white",
                colour_name(colour)
            )
        })
    }

    fn click_rail(&mut self, index: usize) -> SandboxResult {
        let rail = self.rail()?;
        let (_, face) = rail
            .get(index)
            .ok_or_else(|| format!("no face {index} on the rail: {rail:?}"))?;
        let centre = face.centre();
        self.click(centre)?;
        self.park()?;
        thread::sleep(SETTLE);
        Ok(())
    }

    /// A click on the shown account's rail face: its account menu opens.
    fn open_menu_on_rail(&mut self, index: usize) -> SandboxResult {
        self.faces_before_menu()?;
        self.click_rail(index)?;
        self.until("the account menu open", |state| state["menu"] == true)?;
        Ok(())
    }

    /// A click on the title bar's face: the account menu opens.
    fn open_menu_on_title(&mut self) -> SandboxResult {
        let image = self.image()?;
        let title = faces(&image, Rect::title(&image));
        let (_, face) = title.first().ok_or("no face in the title bar")?;
        let centre = face.centre();
        self.faces_before_menu()?;
        self.click(centre)?;
        self.until("the account menu open", |state| state["menu"] == true)?;
        Ok(())
    }

    /// Keeps where the faces outside the rail and the title bar are before the menu opens.
    fn faces_before_menu(&mut self) -> SandboxResult {
        let image = self.image()?;
        self.before_menu = faces(&image, Rect::outside_rail_and_title(&image))
            .into_iter()
            .map(|(_, blob)| blob.centre())
            .collect();
        Ok(())
    }

    /// The colour of the face that came with the open menu (its header's).
    fn menu_face(&mut self) -> SandboxResult<Option<&'static str>> {
        let image = self.image()?;
        let found = faces(&image, Rect::outside_rail_and_title(&image));
        let before = &self.before_menu;
        Ok(found
            .iter()
            .find(|(_, blob)| {
                let (x, y) = blob.centre();
                !before
                    .iter()
                    .any(|&(bx, by)| x.abs_diff(bx) <= 6 && y.abs_diff(by) <= 6)
            })
            .map(|(name, _)| *name))
    }

    /// The shown account (the rail's face 1) opens its menu (on the title bar when `title`), and the user picks
    /// `file` in "Change picture" (two rows above the end when it shows a chosen picture).
    fn pick_by_menu(&mut self, file: &Path, chosen: bool, title: bool) -> SandboxResult {
        if title {
            self.open_menu_on_title()?;
        } else {
            self.open_menu_on_rail(1)?;
        }
        self.pick_in_open_menu(file, chosen)
    }

    fn pick_by_menu_at(&mut self, index: usize, file: &Path, chosen: bool, title: bool) -> SandboxResult {
        if title {
            self.open_menu_on_title()?;
        } else {
            self.open_menu_on_rail(index)?;
        }
        self.pick_in_open_menu(file, chosen)
    }

    fn pick_in_open_menu(&mut self, file: &Path, chosen: bool) -> SandboxResult {
        self.chooser.queue(Answer::Pick(file.to_owned()));
        let asked = self.chooser.asked().len();
        if chosen {
            self.keys(&["end", "up", "up", "enter"])?;
        } else {
            self.keys(&["end", "up", "enter"])?;
        }
        self.wait_asked(asked + 1)
    }

    /// Tab from the page list `presses` times to a "Change picture", then Enter.
    fn tab_to_change(&mut self, presses: usize) -> SandboxResult {
        thread::sleep(SETTLE);
        for _ in 0..presses {
            self.keys(&["tab"])?;
        }
        let asked = self.chooser.asked().len();
        self.keys(&["enter"])?;
        self.wait_asked(asked + 1)
    }
}

impl Drop for User<'_> {
    fn drop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.fail(&"dropped");
        }
        let mut summary = String::new();
        let _ = write!(summary, "{} screenshots", self.shots);
        say(&summary);
    }
}
