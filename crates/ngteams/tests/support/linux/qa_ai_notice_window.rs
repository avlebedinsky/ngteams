//! QA of task 579 (round 1), in the UI sandbox: the AI notice and the "Use AI features" switch. The real app runs as a
//! child in the sandbox, as lab account a on `teams-mock`, with its settings as the installed app makes them (the
//! first run's screen asked for). The parent drives it with the sandbox's virtual pointer and keyboard, and asks the
//! child through a file in the sandbox dir to do what a click on a view does (the same view methods) and what the
//! window holds (`harness = false`: GPUI runs on the process's main thread).
//!
//! - **A made-up `claude`** ([`FAKE_CLAUDE`]) is first on the app's `PATH`, which holds no real `claude` or `codex`.
//!   It logs each start (with its arguments) and each question, corrects "teh", translates a chat's messages, and
//!   holds its answer while a `hold` file exists.
//! - **Four runs of the app**: a new profile (the first run's screen, then the notice, answered "off" with the
//!   keyboard); a restart over that file (no notice; the switch on with the keyboard; on and off while a correction and
//!   a chat's translation run); an upgrade from a file without `ai_features` at the largest font (the notice, answered
//!   "on" with the keyboard); a file with a bad value (the notice, answered "on" with the pointer).
//!
//! `QA579_ONLY` (a comma list of check names) runs only those. `QA579_EVIDENCE` names a directory that gets the
//! screenshots, the settings files, the tool's logs and the results. Screenshots also go to
//! `target/ui-shots/qa579-*.png`. Every name and text is made up.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use serde_json::{Value, json};
use support::{Alias, display_name};
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PRESENCE_BASE,
    PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::{Author, Client, Credentials, HttpTransport, Mri};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_translate::Mark;
use teams_ui::chat_view::ChatView;
use teams_ui::local_ai::Backend;
use teams_ui::settings_view::Page;
use teams_ui::shell::{Main, Shell};
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

const CHILD_ENV: &str = "QA579_CHILD";
const ONLY_ENV: &str = "QA579_ONLY";
const EVIDENCE_ENV: &str = "QA579_EVIDENCE";
const SETTINGS_ENV: &str = "QA579_SETTINGS";
const TOOLS_ENV: &str = "QA579_TOOLS";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "qa579-ask";
const STARTUP: Duration = Duration::from_secs(120);
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(80);
const SETTLE: Duration = Duration::from_millis(900);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_secs(3);
/// After a key, before the next one.
const STEP: Duration = Duration::from_millis(250);

/// b's messages in the a–b chat, for the chat's translation.
const B_SAYS: [&str; 2] = [
    "<p>good morning, the QA notes are ready</p>",
    "<p>please read them before noon</p>",
];
/// What b says while a chat's translation is held.
const B_LATER: &str = "<p>one more note about the release</p>";

/// A made-up Claude Code (`__STATE__` its state directory): it logs each start with its arguments, `--help` names two
/// models; it logs each question, waits while `hold` exists, translates a chat's messages ("DE " before each) and
/// corrects anything else ("teh" to "the").
const FAKE_CLAUDE: &str = r#"#!/bin/bash
state="__STATE__"
printf 'start %s\n' "$*" >> "$state/calls.log"
case "$1" in
  --help)
    printf '%s\n' "Usage: claude [options]" "  --model <model>  Model for the current session: an alias such as 'sonnet' or 'opus'"
    exit 0;;
esac
printf '%s' "$$" > "$state/pid"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$state/received.log"
  while [ -e "$state/hold" ]; do sleep 0.02; done
  echo '{"type":"system","subtype":"init"}'
  ans=""
  case "$line" in
    *'<translate>'*)
      while IFS= read -r m; do
        n=$(printf '%s' "$m" | sed 's/^<m id=\\"\([0-9]*\)\\".*/\1/')
        body=$(printf '%s' "$m" | sed 's/^<m [^>]*>\(.*\)<\/m>$/\1/; s/<x0>/<x0>DE /')
        ans="$ans<m id=\\\"$n\\\" lang=\\\"en\\\">$body</m>"
      done < <(printf '%s' "$line" | sed 's/\\n/\n/g' | grep '^<m id=')
      ;;
    *)
      t=$(printf '%s' "$line" | sed 's/.*<message>\\n\(.*\)\\n<\/message>.*/\1/')
      ans="<corrected>$(printf '%s' "$t" | sed 's/teh/the/g')</corrected>";;
  esac
  printf '{"type":"result","subtype":"success","is_error":false,"result":"%s"}\n' "$ans"
done
"#;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV) {
        Ok(_) => child(),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("qa_ai_notice_window: {error}");
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

fn shots_dir() -> io::Result<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/ui-shots")
        .canonicalize()
        .or_else(|_| {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-shots");
            fs::create_dir_all(&dir).map(|()| dir)
        })?;
    Ok(dir)
}

fn evidence_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os(EVIDENCE_ENV)?);
    fs::create_dir_all(&dir).ok().map(|()| dir)
}

fn wanted(name: &str) -> bool {
    std::env::var(ONLY_ENV).map_or(true, |only| only.split(',').any(|one| one.trim() == name))
}

// --- the parent ---

type Check = (&'static str, fn(&mut User<'_>) -> SandboxResult);

/// A new profile: the first run's screen, then the notice; answered "off" with the keyboard.
const NEW_CHECKS: [Check; 7] = [
    ("first_run_screen_comes_first", first_run_screen_comes_first),
    ("notice_after_the_first_run_screen", notice_after_the_first_run_screen),
    ("notice_in_both_themes", notice_in_both_themes),
    ("escape_on_the_notice", escape_on_the_notice),
    ("keyboard_turns_ai_off", keyboard_turns_ai_off),
    ("off_hides_and_blocks_ai", off_hides_and_blocks_ai),
    ("no_tool_ran_while_off", no_tool_ran_while_off),
];

/// A restart over the new profile's file.
const RESTART_CHECKS: [Check; 6] = [
    ("no_notice_after_restart", no_notice_after_restart),
    ("keyboard_turns_the_switch_on", keyboard_turns_the_switch_on),
    ("on_works_with_the_tool", on_works_with_the_tool),
    ("off_stops_a_running_correction", off_stops_a_running_correction),
    (
        "off_stops_a_running_chat_translation",
        off_stops_a_running_chat_translation,
    ),
    ("on_again_shows_stored_translations", on_again_shows_stored_translations),
];

/// An upgrade from a build without the setting, at the largest font.
const UPGRADE_CHECKS: [Check; 3] = [
    ("upgrade_shows_the_notice", upgrade_shows_the_notice),
    ("notice_large_font_and_narrow", notice_large_font_and_narrow),
    ("keyboard_keeps_ai_on", keyboard_keeps_ai_on),
];

/// A bad value in the file.
const BAD_CHECKS: [Check; 1] = [("bad_value_asks_again", bad_value_asks_again)];

/// What an earlier build kept: the first run confirmed, the largest font, no `ai_features`.
const UPGRADE_FILE: &str = "first_run = \"confirmed\"\nmessage_translate_to = \"German\"\nfont_size = 24\n";
/// A file whose `ai_features` is not true or false.
const BAD_FILE: &str = "first_run = \"confirmed\"\nai_features = \"maybe\"\n";

#[derive(Default)]
struct Shared {
    passed: Vec<String>,
    failed: Vec<(String, String)>,
}

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("qa_ai_notice_window", &options)? else {
        return Ok(());
    };
    let tool = FakeTool::install(&sandbox)?;
    let mut shared = Shared::default();
    let fresh = sandbox.path("settings.toml");
    let upgrade = sandbox.path("upgrade-settings.toml");
    fs::write(&upgrade, UPGRADE_FILE)?;
    let bad = sandbox.path("bad-settings.toml");
    fs::write(&bad, BAD_FILE)?;
    let runs: [(&'static str, &Path, &[Check]); 4] = [
        ("new", &fresh, &NEW_CHECKS),
        ("restart", &fresh, &RESTART_CHECKS),
        ("upgrade", &upgrade, &UPGRADE_CHECKS),
        ("bad", &bad, &BAD_CHECKS),
    ];
    let mut quits = Vec::new();
    for (name, file, checks) in runs {
        if !checks.iter().any(|(check, _)| wanted(check)) {
            continue;
        }
        quits.push(run(&sandbox, &tool, (name, file, checks), &mut shared)?);
    }
    summarize(&tool, &shared);
    for quit in quits {
        quit?;
    }
    ensure(shared.failed.is_empty(), || {
        format!("{} checks failed", shared.failed.len())
    })
}

/// One run of the app over `file`, its `checks` in order; how its app ended.
fn run(
    sandbox: &Sandbox,
    tool: &FakeTool,
    (name, file, checks): (&'static str, &Path, &[Check]),
    shared: &mut Shared,
) -> SandboxResult<SandboxResult> {
    tool.mark_run(name)?;
    let child = sandbox.spawn(
        &std::env::current_exe()?,
        &[],
        &[
            (CHILD_ENV, "app"),
            (SETTINGS_ENV, &file.display().to_string()),
            (TOOLS_ENV, &tool.bin.display().to_string()),
            ("PATH", &tool.path),
            ("CLAUDE_CONFIG_DIR", &tool.home.display().to_string()),
            ("CODEX_HOME", &tool.home.display().to_string()),
        ],
    )?;
    let running = Running::new(child)?;
    let mut user = User::start(sandbox, running, tool, name, file.to_owned())?;
    for (check, walk) in checks {
        if wanted(check) {
            user.run_check(check, *walk, shared);
        }
    }
    let quit = user.ask("quit").and_then(|_| {
        let (status, stderr) = user.running.finish(WAIT)?;
        ensure(status.success(), || {
            format!("the {name} run's app exited {status}: {stderr}")
        })
    });
    if let Some(dir) = evidence_dir() {
        let _ = fs::copy(file, dir.join(format!("settings-after-{name}.toml")));
    }
    Ok(quit)
}

fn summarize(tool: &FakeTool, shared: &Shared) {
    if let Some(dir) = evidence_dir() {
        for log in ["received.log", "calls.log"] {
            let _ = fs::copy(tool.state.join(log), dir.join(format!("cli-{log}")));
        }
    }
    let mut summary = String::new();
    for name in &shared.passed {
        let _ = writeln!(summary, "pass {name}");
    }
    for (name, why) in &shared.failed {
        let _ = writeln!(summary, "FAIL {name}: {why}");
    }
    say(&summary);
    if let Some(dir) = evidence_dir() {
        let _ = fs::write(dir.join("results.txt"), &summary);
    }
    say(&format!(
        "qa_ai_notice_window: {} passed, {} failed",
        shared.passed.len(),
        shared.failed.len()
    ));
}

/// The made-up tool as the parent sees it.
struct FakeTool {
    bin: PathBuf,
    state: PathBuf,
    home: PathBuf,
    /// The child's `PATH`: `bin`, the commands the tool uses, then the sandbox's own without a real `claude` or `codex`.
    path: String,
}

impl FakeTool {
    fn install(sandbox: &Sandbox) -> SandboxResult<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let (bin, state, home, commands) = (
            sandbox.path("ai-bin"),
            sandbox.path("ai-state"),
            sandbox.path("ai-home"),
            sandbox.path("ai-commands"),
        );
        for dir in [&bin, &state, &home, &commands] {
            fs::create_dir_all(dir)?;
        }
        let inherited = sandbox
            .client_env()
            .into_iter()
            .find(|(name, _)| *name == "PATH")
            .map(|(_, path)| path)
            .unwrap_or_default();
        let kept: Vec<PathBuf> = std::env::split_paths(&inherited)
            .filter(|dir| !["claude", "codex"].iter().any(|tool| dir.join(tool).exists()))
            .collect();
        for command in ["sed", "grep", "sleep"] {
            let found = std::env::split_paths(&inherited)
                .map(|dir| dir.join(command))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| format!("no {command} on the PATH"))?;
            std::os::unix::fs::symlink(found, commands.join(command))?;
        }
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE.replace("__STATE__", &state.display().to_string()))?;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        let path = std::env::join_paths([bin.clone(), commands].into_iter().chain(kept))?
            .into_string()
            .map_err(|_| "a PATH that isn't UTF-8")?;
        Ok(Self { bin, state, home, path })
    }

    fn lines(&self, log: &str) -> Vec<String> {
        fs::read_to_string(self.state.join(log))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The tool's starts in the current run (after its `run` line).
    fn starts(&self) -> Vec<String> {
        let calls = self.lines("calls.log");
        let from = calls
            .iter()
            .rposition(|line| line.starts_with("run "))
            .map_or(0, |at| at + 1);
        calls[from..].to_vec()
    }

    fn received(&self) -> usize {
        self.lines("received.log").len()
    }

    fn mark_run(&self, name: &str) -> SandboxResult {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.state.join("calls.log"))?;
        writeln!(log, "run {name}")?;
        Ok(())
    }

    fn hold(&self, on: bool) -> SandboxResult {
        let file = self.state.join("hold");
        if on {
            fs::write(file, "")?;
        } else if file.exists() {
            fs::remove_file(file)?;
        }
        Ok(())
    }

    /// Whether the tool's last process still runs (a zombie counts as ended).
    fn alive(&self) -> bool {
        let Ok(pid) = fs::read_to_string(self.state.join("pid")) else {
            return false;
        };
        fs::read_to_string(format!("/proc/{}/stat", pid.trim())).is_ok_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
        })
    }
}

/// A rectangle on the screen, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Rect {
    fn center(self) -> (u32, u32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// The box of the pixels in `shot` that `taken` takes, in `region`.
fn pixels_box(shot: &image::RgbImage, region: Rect, taken: impl Fn([u8; 3]) -> bool) -> Option<Rect> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in region.y..(region.y + region.h).min(shot.height()) {
        for x in region.x..(region.x + region.w).min(shot.width()) {
            if taken(shot.get_pixel(x, y).0) {
                found = Some(found.map_or((x, y, x, y), |(x0, y0, x1, y1)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                }));
            }
        }
    }
    found.map(|(x0, y0, x1, y1)| Rect {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
    })
}

/// The box of the pixels that differ between two shots of the same size.
fn changed_box(before: &image::RgbImage, after: &image::RgbImage) -> Option<Rect> {
    let whole = Rect {
        x: 0,
        y: 0,
        w: before.width().min(after.width()),
        h: before.height().min(after.height()),
    };
    changed_in(before, after, whole)
}

/// The box of the pixels in `region` that differ between two shots.
fn changed_in(before: &image::RgbImage, after: &image::RgbImage, region: Rect) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    let (x_end, y_end) = (
        (region.x + region.w).min(before.width()).min(after.width()),
        (region.y + region.h).min(before.height()).min(after.height()),
    );
    for y in region.y..y_end {
        for x in region.x..x_end {
            if before.get_pixel(x, y) != after.get_pixel(x, y) {
                found = Some(grow(found, x, y));
            }
        }
    }
    found
}

fn grow(bounds: Option<Rect>, x: u32, y: u32) -> Rect {
    match bounds {
        None => Rect { x, y, w: 1, h: 1 },
        Some(rect) => {
            let (x0, y0) = (rect.x.min(x), rect.y.min(y));
            let (x1, y1) = ((rect.x + rect.w).max(x + 1), (rect.y + rect.h).max(y + 1));
            Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
            }
        }
    }
}

/// The notice's dialog in Teams Light: the only bright surface over the dimmed window.
fn dialog_of(shot: &image::RgbImage) -> Option<Rect> {
    let whole = Rect {
        x: 0,
        y: 0,
        w: shot.width(),
        h: shot.height(),
    };
    pixels_box(shot, whole, |[r, g, b]| r > 235 && g > 235 && b > 235)
}

/// The primary button's fill (Teams' brand blue-violet) in the lowest quarter of `dialog`.
fn primary_button(shot: &image::RgbImage, dialog: Rect) -> Option<Rect> {
    let band = Rect {
        x: dialog.x,
        y: dialog.y + dialog.h * 3 / 4,
        w: dialog.w,
        h: dialog.h / 4,
    };
    let filled = |x: u32, y: u32| {
        x < shot.width() && {
            let [r, g, b] = shot.get_pixel(x, y).0;
            u16::from(b) > u16::from(r) + 60 && u16::from(b) > u16::from(g) + 60
        }
    };
    // a fill, not a focus ring or a link: brand on both sides too
    let mut found: Option<Rect> = None;
    for y in band.y..(band.y + band.h).min(shot.height()) {
        for x in band.x.max(10)..(band.x + band.w).min(shot.width()) {
            if filled(x, y) && filled(x - 10, y) && filled(x + 10, y) {
                found = Some(grow(found, x, y));
            }
        }
    }
    found
}

/// The user at the sandbox: its pointer and keyboard, its screenshots, the child it asks and the made-up tool.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    tool: &'a FakeTool,
    run: &'static str,
    settings_file: PathBuf,
    asked: u32,
    looks: u32,
}

impl<'a> User<'a> {
    fn start(
        sandbox: &'a Sandbox,
        running: Running,
        tool: &'a FakeTool,
        run: &'static str,
        settings_file: PathBuf,
    ) -> SandboxResult<Self> {
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut user = Self {
            sandbox,
            running,
            input: sandbox.input()?,
            tool,
            run,
            settings_file,
            asked: 0,
            looks: 0,
        };
        user.park()?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    fn run_check(&mut self, name: &str, check: fn(&mut User<'_>) -> SandboxResult, shared: &mut Shared) {
        say(&format!("check {name} ({} run)", self.run));
        match check(self) {
            Ok(()) => {
                say(&format!("  pass {name}"));
                shared.passed.push(name.to_owned());
            }
            Err(error) => {
                say(&format!("  FAIL {name}: {error}"));
                let _ = self.shot(&format!("failed-{name}"));
                if let Ok(state) = self.state() {
                    say(&format!("  state: {state}"));
                }
                shared.failed.push((name.to_owned(), error.to_string()));
                let _ = self.tool.hold(false);
                let _ = self.ask("theme-light");
            }
        }
    }

    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn ok(&mut self, verb: &str) -> SandboxResult {
        let answer = self.ask(verb)?;
        ensure(answer == "ok", || format!("{verb}: {answer}"))
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

    /// Watches for [`QUIET`] that the state stays as `kept` wants.
    fn stays(&mut self, what: &str, kept: impl Fn(&Value) -> bool) -> SandboxResult<Value> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            ensure(kept(&state), || {
                format!("{what}: changed after {:?}: {state}", start.elapsed())
            })?;
            if start.elapsed() > QUIET {
                return Ok(state);
            }
            thread::sleep(POLL);
        }
    }

    fn press(&mut self, spec: &str) -> SandboxResult {
        let (modifiers, key) =
            ui_sandbox::input::parse_key(spec, self.input.keymap()).ok_or_else(|| format!("no key {spec}"))?;
        self.input.key_with(&modifiers, key)?;
        thread::sleep(STEP);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> SandboxResult {
        self.input.type_text(text)?;
        let typed = u64::try_from(text.chars().count()).unwrap_or(u64::MAX);
        thread::sleep(STEP + Duration::from_millis(400 + 70_u64.saturating_mul(typed)));
        Ok(())
    }

    fn click(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.click(x, y, keys::BTN_LEFT)?;
        thread::sleep(STEP);
        Ok(())
    }

    /// The pointer on the title bar, away from every control that reacts to it.
    fn park(&mut self) -> SandboxResult {
        self.input.move_to(300, 12)
    }

    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        let file = format!("qa579-{name}.png");
        let png = self.sandbox.screenshot(&file, false)?;
        fs::copy(&png, shots_dir()?.join(&file))?;
        if let Some(dir) = evidence_dir() {
            fs::copy(&png, dir.join(&file))?;
        }
        Ok(png)
    }

    /// The screen as it is now, kept nowhere.
    fn look(&mut self) -> SandboxResult<image::RgbImage> {
        self.looks += 1;
        let png = self.sandbox.screenshot(&format!("look-{}.png", self.looks), false)?;
        let shot = image::open(&png)?.to_rgb8();
        fs::remove_file(png)?;
        Ok(shot)
    }

    /// The same screen in Teams Light and Teams Dark; back in Light after.
    fn shots(&mut self, name: &str) -> SandboxResult {
        self.park()?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-light"))?;
        self.ok("theme-dark")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-dark"))?;
        self.ok("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }

    fn settings_text(&self) -> String {
        fs::read_to_string(&self.settings_file).unwrap_or_default()
    }

    /// Presses `key` and says where the screen changed: the focus ring that moved.
    fn key_and_change(&mut self, key: &str) -> SandboxResult<Option<Rect>> {
        let before = self.look()?;
        self.press(key)?;
        thread::sleep(SETTLE);
        let after = self.look()?;
        Ok(changed_box(&before, &after))
    }

    /// The compositor sizes the window to `width`; waits until the app draws at that width.
    fn resize(&mut self, width: u32) -> SandboxResult {
        self.sandbox
            .niri_msg(&["action", "set-column-width", &width.to_string()])?;
        self.until(&format!("the window {width} px wide"), |state| {
            state["width"]
                .as_f64()
                .is_some_and(|drawn| (drawn - f64::from(width)).abs() < 2.0)
        })?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

fn notice_shown(state: &Value) -> bool {
    state["notice"] == true && state["first_run"] == false
}

/// No AI ran: the switch off, no tool looked for, no model listed, no tool started in this run.
fn nothing_ai(user: &mut User<'_>, what: &str) -> SandboxResult {
    let state = user.state()?;
    ensure(state["ai"] == false, || format!("{what}: AI is on: {state}"))?;
    ensure(state["tools_looked_for"] == false, || {
        format!("{what}: the app looked for the tools: {state}")
    })?;
    ensure(state["claude_models"].as_array().is_none_or(Vec::is_empty), || {
        format!("{what}: models listed: {state}")
    })?;
    let starts = user.tool.starts();
    ensure(starts.is_empty(), || format!("{what}: the tool started: {starts:?}"))
}

// --- the new profile ---

fn first_run_screen_comes_first(user: &mut User<'_>) -> SandboxResult {
    let state = user.state()?;
    ensure(state["first_run"] == true && state["notice"] == false, || {
        format!("the first run's screen, not the notice, first: {state}")
    })?;
    nothing_ai(user, "on the first run's screen")?;
    user.shot("first-run-screen")?;
    Ok(())
}

fn notice_after_the_first_run_screen(user: &mut User<'_>) -> SandboxResult {
    user.ok("confirm-first-run")?;
    user.until("the notice", notice_shown)?;
    user.stays("the notice stays", notice_shown)?;
    nothing_ai(user, "while the notice waits")?;
    let file = user.settings_text();
    ensure(!file.contains("ai_features"), || {
        format!("an answer in the file before one was given: {file}")
    })
}

fn notice_in_both_themes(user: &mut User<'_>) -> SandboxResult {
    user.shots("notice")?;
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog on the screen")?;
    say(&format!("  the dialog: {dialog:?}"));
    ensure(dialog.w >= 400 && dialog.w <= 520, || {
        format!("the dialog's width: {dialog:?}")
    })
}

/// Escape on the notice: what it does is noted; the notice must not go without an answer it keeps.
fn escape_on_the_notice(user: &mut User<'_>) -> SandboxResult {
    user.press("Escape")?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    say(&format!(
        "  after Escape: notice {} ai_features {}",
        state["notice"], state["ai_features"]
    ));
    user.shot("notice-after-escape")?;
    ensure(notice_shown(&state), || {
        format!("Escape closed the notice with no answer: {state}")
    })
}

/// Tab until the focus shows on one of the notice's buttons (at most `tabs`); which one (`true`: "Keep AI features
/// on", `false`: "Turn AI features off") after how many Tabs, or `None`. Each Tab is said; the first four are shots.
fn tab_to_the_notice(user: &mut User<'_>, tabs: u32, name: &str) -> SandboxResult<Option<(bool, u32)>> {
    let baseline = user.look()?;
    let dialog = dialog_of(&baseline).ok_or("no dialog on the screen")?;
    let keep = primary_button(&baseline, dialog).ok_or("no primary button")?;
    let buttons = Rect {
        x: dialog.x,
        y: keep.y.saturating_sub(8),
        w: dialog.w,
        h: keep.h + 16,
    };
    for tab in 1..=tabs {
        let changed = user.key_and_change("Tab")?;
        let after = user.look()?;
        let ring = changed_in(&baseline, &after, buttons);
        say(&format!(
            "  Tab {tab}: changed {changed:?}; ring on the notice's buttons {ring:?}"
        ));
        if tab <= 4 {
            user.shot(&format!("{name}-tab-{tab}"))?;
        }
        if let Some(ring) = ring {
            user.shot(&format!("{name}-tab-reached"))?;
            return Ok(Some((ring.center().0 >= keep.x, tab)));
        }
    }
    Ok(None)
}

/// Enter on the focused button, then Space if Enter did nothing within [`QUIET`]; the state after, each said.
fn activate(user: &mut User<'_>, name: &str) -> SandboxResult<Value> {
    for key in ["Return", "space"] {
        user.press(key)?;
        let start = Instant::now();
        let mut state = user.state()?;
        while state["notice"] == true && start.elapsed() < QUIET {
            thread::sleep(POLL);
            state = user.state()?;
        }
        user.shot(&format!("{name}-after-{key}"))?;
        say(&format!(
            "  {key}: notice {} ai_features {}",
            state["notice"], state["ai_features"]
        ));
        if state["notice"] == false {
            ensure(key == "Return", || {
                format!("Enter did nothing; Space answered: {}", state["ai_features"])
            })?;
            return Ok(state);
        }
    }
    Err("neither Enter nor Space answered the notice".into())
}

/// The notice answered with the pointer, as the fallback when the keyboard cannot reach it.
fn click_answer(user: &mut User<'_>, keep: bool) -> SandboxResult {
    user.press("Escape")?;
    user.press("Escape")?;
    user.park()?;
    thread::sleep(SETTLE);
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog")?;
    let primary = primary_button(&shot, dialog).ok_or("no primary button")?;
    let at = if keep {
        primary.center()
    } else {
        (primary.x.saturating_sub(70), primary.center().1)
    };
    user.click(at)?;
    user.until("an answer by the pointer", |state| state["notice"] == false)?;
    Ok(())
}

/// The notice is a dialog: the first Tab puts the focus on one of its buttons, never on a control behind it.
fn tabs_stay_in_the_notice(tabs: u32) -> SandboxResult {
    ensure(tabs <= 2, || {
        format!(
            "{} Tabs went to controls behind the notice before its first button",
            tabs - 1
        )
    })
}

/// Tab moves the keyboard to the notice's buttons, Enter on "Turn AI features off" answers it.
fn keyboard_turns_ai_off(user: &mut User<'_>) -> SandboxResult {
    let reached = tab_to_the_notice(user, 30, "notice")?;
    let Some((on_keep, tabs)) = reached else {
        click_answer(user, false)?;
        return Err("30 Tabs never put the focus on the notice's buttons (answered with the pointer)".into());
    };
    if on_keep {
        user.press("shift+Tab")?;
    }
    let state = match activate(user, "notice-off") {
        Ok(state) => state,
        Err(error) => {
            click_answer(user, false)?;
            return Err(format!("{error} (answered with the pointer)").into());
        }
    };
    ensure(state["ai_features"] == false && state["notice"] == false, || {
        format!("Enter on \"Turn AI features off\": {state}")
    })?;
    ensure(user.settings_text().contains("ai_features = false"), || {
        format!("the file: {}", user.settings_text())
    })?;
    tabs_stay_in_the_notice(tabs)
}

fn off_hides_and_blocks_ai(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-ab")?;
    thread::sleep(SETTLE);
    user.ok("focus-box")?;
    user.type_text("see teh notes")?;
    user.press("ctrl+shift+g")?;
    user.press("ctrl+shift+y")?;
    user.stays("no correction or translation starts", |state| {
        state["box"]["correction"] == "Idle"
            && state["box"]["status"] == "idle"
            && state["box"]["text"] == "see teh notes"
    })?;
    user.shots("chat-ai-off")?;
    let last = user.state()?["rows"]
        .as_array()
        .and_then(|rows| rows.iter().rev().find(|row| row["sender"] == LAB_B).cloned())
        .ok_or("no message of b")?;
    let id = last["id"].as_str().unwrap_or_default().to_owned();
    user.ok(&format!("select {id}"))?;
    user.press("t")?;
    user.stays("T translates nothing", |state| {
        state["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["mark"] == "none"))
    })?;
    user.ok("open-settings general")?;
    user.shots("settings-general-ai-off")?;
    user.ok("open-settings calls")?;
    user.shots("settings-calls-ai-off")?;
    user.ok("open-ab")?;
    nothing_ai(user, "AI off")
}

fn no_tool_ran_while_off(user: &mut User<'_>) -> SandboxResult {
    thread::sleep(QUIET);
    nothing_ai(user, "the whole run")?;
    ensure(user.tool.received() == 0, || "the tool got text".to_owned())
}

// --- the restart ---

fn no_notice_after_restart(user: &mut User<'_>) -> SandboxResult {
    user.stays("no notice and no first run's screen", |state| {
        state["notice"] == false && state["first_run"] == false
    })?;
    nothing_ai(user, "after the restart")?;
    user.shot("restart-no-notice")?;
    Ok(())
}

/// Where "Use AI features" is drawn on the General page at 1280 × 800 while AI is off (the last box).
const AI_SWITCH: (u32, u32) = (1134, 610);

/// Settings > General with the keyboard: Tab from the pages until the focus ring is on the switch, then Space.
fn keyboard_turns_the_switch_on(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-settings general")?;
    thread::sleep(SETTLE);
    user.ok("focus-pages")?;
    user.shot("settings-general-before-tab")?;
    let mut reached = None;
    for tab in 1..=15 {
        let changed = user.key_and_change("Tab")?;
        say(&format!("  Tab {tab}: changed {changed:?}"));
        let on_switch = changed.is_some_and(|rect| {
            (rect.x..rect.x + rect.w).contains(&AI_SWITCH.0) && (rect.y..rect.y + rect.h).contains(&AI_SWITCH.1)
        });
        if on_switch {
            reached = Some(tab);
            break;
        }
    }
    user.shot("settings-switch-focused")?;
    let Some(tabs) = reached else {
        user.ok("ai on")?;
        return Err("15 Tabs never put the focus on the switch (turned on as a click does)".into());
    };
    let before = user.settings_text();
    user.press("space")?;
    let state = user.until("a change", |state| {
        state["ai"] == true || state["file"] != before.as_str()
    })?;
    if state["ai"] != true {
        user.press("space")?;
        user.ok("ai on")?;
        return Err(format!("Space after {tabs} Tabs changed something else: {}", state["file"]).into());
    }
    say(&format!("  the switch on after {tabs} Tabs and Space"));
    user.shots("settings-general-ai-on")?;
    ensure(user.settings_text().contains("ai_features = true"), || {
        user.settings_text()
    })
}

fn on_works_with_the_tool(user: &mut User<'_>) -> SandboxResult {
    user.ok("open-ab")?;
    thread::sleep(SETTLE);
    user.ok("focus-box")?;
    user.press("ctrl+a")?;
    user.press("BackSpace")?;
    user.type_text("see teh notes")?;
    user.press("ctrl+shift+g")?;
    user.until("the correction", |state| state["box"]["correction"] == "Proposed")?;
    user.shots("chat-ai-on-correction")?;
    user.press("Escape")?;
    user.ok("chat-lang de")?;
    user.until("the chat translated", |state| {
        state["rows"].as_array().is_some_and(|rows| {
            rows.iter()
                .filter(|row| row["sender"] == LAB_B)
                .all(|row| row["mark"] == "translated")
        })
    })?;
    user.shots("chat-ai-on-translated")?;
    Ok(())
}

fn off_stops_a_running_correction(user: &mut User<'_>) -> SandboxResult {
    user.ok("focus-box")?;
    user.press("ctrl+a")?;
    user.press("BackSpace")?;
    user.type_text("teh draft")?;
    user.tool.hold(true)?;
    let asked = user.tool.received();
    user.press("ctrl+shift+g")?;
    user.until("the correction runs", |state| state["box"]["correction"] == "Running")?;
    wait_for("the tool got the draft", || user.tool.received() > asked)?;
    user.ok("ai off")?;
    user.until("the correction stops", |state| state["box"]["correction"] != "Running")?;
    wait_for("the tool's process ends", || !user.tool.alive())?;
    user.tool.hold(false)?;
    let after = user.tool.received();
    user.press("ctrl+shift+g")?;
    user.stays("nothing new runs", |state| state["box"]["correction"] != "Running")?;
    ensure(user.tool.received() == after, || {
        "the tool got more text while off".to_owned()
    })?;
    let state = user.state()?;
    ensure(state["box"]["text"] == "teh draft", || {
        format!("the draft changed: {state}")
    })
}

fn off_stops_a_running_chat_translation(user: &mut User<'_>) -> SandboxResult {
    user.ok("ai on")?;
    user.tool.hold(true)?;
    let asked = user.tool.received();
    user.ok("b-says")?;
    wait_for("the tool got b's new message", || user.tool.received() > asked)?;
    let running = user.state()?;
    say(&format!("  while held: {}", running["chat_translation"]));
    user.ok("ai off")?;
    wait_for("the tool's process ends", || !user.tool.alive())?;
    user.tool.hold(false)?;
    let after = user.tool.received();
    thread::sleep(QUIET);
    ensure(user.tool.received() == after, || {
        "the tool got more text while off".to_owned()
    })?;
    let state = user.state()?;
    ensure(
        state["rows"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|row| row["mark"] == "none")),
        || format!("a translation shows while off: {state}"),
    )?;
    user.shots("chat-ai-off-after-on")
}

fn on_again_shows_stored_translations(user: &mut User<'_>) -> SandboxResult {
    let asked = user.tool.received();
    user.ok("ai on")?;
    let state = user.until("the stored translations show", |state| {
        state["rows"].as_array().is_some_and(|rows| {
            rows.iter()
                .filter(|row| {
                    row["sender"] == LAB_B && row["text"].as_str().is_some_and(|text| !text.contains("release"))
                })
                .all(|row| row["mark"] == "translated")
        })
    })?;
    say(&format!("  questions after on: {}", user.tool.received() - asked));
    user.shots("chat-ai-on-again")?;
    let _ = state;
    Ok(())
}

fn wait_for(what: &str, done: impl Fn() -> bool) -> SandboxResult {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > WAIT {
            return Err(format!("{what}: never came").into());
        }
        thread::sleep(POLL);
    }
    Ok(())
}

// --- the upgrade ---

fn upgrade_shows_the_notice(user: &mut User<'_>) -> SandboxResult {
    user.until("the notice", notice_shown)?;
    nothing_ai(user, "the upgrade's notice")
}

/// At the largest font, and in a narrow window, the whole notice shows: its dialog inside the window, its buttons
/// inside the dialog.
fn notice_large_font_and_narrow(user: &mut User<'_>) -> SandboxResult {
    let mut clipped = Vec::new();
    for width in [1280, 720, 520] {
        if width != 1280 {
            user.resize(width)?;
        }
        user.shots(&format!("notice-font-24-{width}"))?;
        let shot = user.look()?;
        let dialog = dialog_of(&shot).ok_or("no dialog")?;
        let keep = primary_button(&shot, dialog);
        say(&format!("  {width} px: the dialog {dialog:?}, Keep {keep:?}"));
        let inside = dialog.y > 0
            && dialog.y + dialog.h < shot.height()
            && keep.is_some_and(|keep| keep.y + keep.h + 8 < dialog.y + dialog.h);
        if !inside {
            clipped.push(format!("{width} px: dialog {dialog:?}, Keep {keep:?}"));
        }
    }
    user.resize(1280)?;
    ensure(clipped.is_empty(), || {
        format!(
            "the notice does not fit the 800 px high window at font 24: {}",
            clipped.join("; ")
        )
    })
}

/// Tab to "Keep AI features on" and Enter.
fn keyboard_keeps_ai_on(user: &mut User<'_>) -> SandboxResult {
    let reached = tab_to_the_notice(user, 30, "notice-upgrade")?;
    let Some((on_keep, tabs)) = reached else {
        click_answer(user, true)?;
        return Err("30 Tabs never put the focus on the notice's buttons (answered with the pointer)".into());
    };
    if !on_keep {
        let changed = user.key_and_change("Tab")?;
        say(&format!("  Tab to Keep: changed {changed:?}"));
        user.shot("notice-upgrade-tab-keep")?;
    }
    let state = match activate(user, "notice-keep") {
        Ok(state) => state,
        Err(error) => {
            click_answer(user, true)?;
            return Err(format!("{error} (answered with the pointer)").into());
        }
    };
    ensure(state["ai_features"] == true && state["ai"] == true, || {
        format!("Enter on Keep: {state}")
    })?;
    ensure(user.settings_text().contains("ai_features = true"), || {
        user.settings_text()
    })?;
    tabs_stay_in_the_notice(tabs)
}

// --- the bad value ---

/// The rail's Settings at 1280 × 800.
const RAIL_SETTINGS: (u32, u32) = (37, 765);

fn bad_value_asks_again(user: &mut User<'_>) -> SandboxResult {
    user.until("the notice", notice_shown)?;
    nothing_ai(user, "a bad value")?;
    // the rail stays lit beside the notice: its Settings, as a user clicks it
    user.click(RAIL_SETTINGS)?;
    thread::sleep(SETTLE);
    let state = user.state()?;
    say(&format!(
        "  the rail's Settings while the notice waits: main {} notice {}",
        state["main"], state["notice"]
    ));
    user.shot("notice-rail-settings")?;
    user.park()?;
    let shot = user.look()?;
    let dialog = dialog_of(&shot).ok_or("no dialog")?;
    let keep = primary_button(&shot, dialog).ok_or("no primary button")?;
    user.click(keep.center())?;
    let state = user.until("an answer", |state| state["notice"] == false)?;
    ensure(state["ai_features"] == true && state["ai"] == true, || {
        format!("a click on Keep: {state}")
    })?;
    let file = user.settings_text();
    ensure(file.contains("ai_features = true"), || file)
}

// --- the child: the real app on the mock ---

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

/// The mock and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        for html in B_SAYS {
            mock.post_as(LAB_B, AB_CHAT, html).ok_or("b's post refused")?;
        }
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            known_names: vec![
                (LAB_B.to_owned(), "Lab B".to_owned()),
                (LAB_C.to_owned(), "Lab C".to_owned()),
            ],
        })?;
        let lab = Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            _trouter: trouter,
            dir,
        };
        Ok((lab, engine))
    }
}

/// The child's `PATH` holds no tool but the made-up one (a look on the `PATH` only: it starts nothing).
fn only_the_made_up_tool() -> SandboxResult {
    let made_up = PathBuf::from(std::env::var_os(TOOLS_ENV).ok_or("no made-up tool's directory")?);
    let found = teams_ui::local_ai::Tools::detect();
    for backend in [Backend::Claude, Backend::Codex] {
        if let Some(program) = found.program(backend)
            && !program.starts_with(&made_up)
        {
            return Err(format!("a real tool is on the PATH: {}", program.display()).into());
        }
    }
    Ok(())
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    only_the_made_up_tool()?;
    let settings_path = PathBuf::from(std::env::var_os(SETTINGS_ENV).ok_or("no settings path")?);
    let (lab, engine) = MockLab::start()?;
    let first = support::started(Alias::A, engine)?;
    // as the installed app makes them: the first run's screen asked for
    let settings = app::settings(teams_ui::SettingsFile::new(settings_path), None);
    let runner = support::runner(lab.dir.path())?;
    let mock = Arc::clone(&lab.mock);
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings,
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, mock, cx);
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
    drop(lab);
    Ok(())
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

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, mock: Arc<ChatServiceMock>, cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = match verb {
                "quit" => {
                    say(&format!("re {seq} ok"));
                    cx.update(|cx| cx.quit());
                    return;
                }
                "b-says" => mock
                    .post_as(LAB_B, AB_CHAT, B_LATER)
                    .map_or_else(|| "refused".to_owned(), |_| "ok".to_owned()),
                _ => window
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
                    .unwrap_or_else(|| "no shell".to_owned()),
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn chat_of(shell: &Entity<Shell>, cx: &App) -> Option<Entity<ChatView>> {
    let shell = shell.read(cx);
    shell.chat().filter(|_| matches!(shell.main(), Main::Chat(_))).cloned()
}

fn on_shell(shell: &Entity<Shell>, verb: &str, window: &mut Window, cx: &mut App) -> String {
    let words: Vec<&str> = verb.split_whitespace().collect();
    let model = shell.read(cx).settings().clone();
    match words.as_slice() {
        ["state"] => return state_of(shell, window, cx).to_string(),
        // what the first run's screen's button does
        ["confirm-first-run"] => model.update(cx, teams_ui::SettingsModel::confirm_first_run),
        // what a click on the switch does
        ["ai", on] => model.update(cx, |model, cx| {
            model.change(|settings| settings.ai_features = Some(*on == "on"), cx)
        }),
        ["open-ab"] => shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx)),
        ["open-settings", page] => {
            let page = if *page == "calls" { Page::Calls } else { Page::General };
            shell.update(cx, |shell, cx| shell.open_settings_page(page, window, cx));
        }
        ["focus-pages"] => {
            let view = shell.read(cx).settings_view().clone();
            view.update(cx, |view, cx| view.focus_pages(window, cx));
        }
        ["focus-box"] => {
            let Some(chat) = chat_of(shell, cx) else {
                return "no chat".to_owned();
            };
            let compose = chat.read(cx).compose().clone();
            compose.update(cx, |compose, cx| compose.focus(window, cx));
        }
        // the message selected and the list's focus, as Escape in the box and Up give them
        ["select", id] => {
            let Some(chat) = chat_of(shell, cx) else {
                return "no chat".to_owned();
            };
            chat.update(cx, |chat, cx| {
                chat.select(Some((*id).to_owned()), cx);
                chat.focus_handle().clone().focus(window, cx);
            });
        }
        // the whole chat read in a language, as the header's picker sets it
        ["chat-lang", language] => {
            let Some(chat) = chat_of(shell, cx) else {
                return "no chat".to_owned();
            };
            let language = if *language == "de" { Some("de") } else { None };
            let translation = chat.read(cx).translation().clone();
            translation.update(cx, |translation, cx| translation.set_language(language, cx));
        }
        [theme @ ("theme-dark" | "theme-light")] => {
            let key = if *theme == "theme-dark" {
                teams_ui::theme::DARK_KEY
            } else {
                teams_ui::theme::LIGHT_KEY
            };
            model.update(cx, |model, cx| {
                model.change(
                    |settings| {
                        settings.appearance.follow_system = false;
                        settings.appearance.theme = key;
                    },
                    cx,
                );
            });
        }
        ["focus"] => return focus_of(shell, window, cx),
        _ => return format!("unknown {verb}"),
    }
    "ok".to_owned()
}

/// Where the keys go: the chat's list or box, another element, or none.
fn focus_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> String {
    if let Some(chat) = chat_of(shell, cx) {
        let chat = chat.read(cx);
        if chat.focus_handle().is_focused(window) {
            return "list".to_owned();
        }
        if chat.compose().read(cx).focused(window, cx) {
            return "box".to_owned();
        }
    }
    if window.focused(cx).is_some() { "other" } else { "none" }.to_owned()
}

/// What the parent reads: the notice and the switch, the tools, the chat's box, its rows and its translation.
fn state_of(shell: &Entity<Shell>, window: &Window, cx: &App) -> Value {
    let model = shell.read(cx).settings().read(cx);
    let (compose, translation, rows) = chat_of(shell, cx).map_or((Value::Null, Value::Null, json!([])), |chat| {
        chat_state(&chat, window, cx)
    });
    json!({
        "first_run": model.awaiting_first_run(),
        "notice": model.awaiting_ai_notice(),
        "ai_features": model.settings().ai_features,
        "ai": teams_ui::local_ai::enabled(cx),
        "file": teams_ui::settings::render(model.settings()),
        "tools_looked_for": cx.has_global::<teams_ui::local_ai::Tools>(),
        "claude_models": cx.try_global::<teams_ui::local_ai::Catalog>().map_or(Vec::new(), |catalog| {
            catalog.models(Backend::Claude).iter().map(|listed| listed.id.clone()).collect()
        }),
        "main": format!("{:?}", shell.read(cx).main()),
        "width": f64::from(window.viewport_size().width),
        "box": compose,
        "chat_translation": translation,
        "rows": rows,
    })
}

fn chat_state(chat: &Entity<ChatView>, window: &Window, cx: &App) -> (Value, Value, Value) {
    let chat = chat.read(cx);
    let held = chat.compose().read(cx);
    let status = match held.draft_translation_status() {
        teams_ui::compose::DraftTranslationStatus::Idle => "idle".to_owned(),
        teams_ui::compose::DraftTranslationStatus::Running => "running".to_owned(),
        other => format!("{other:?}"),
    };
    let correction = match held.correction_status() {
        teams_ui::compose::CorrectionStatus::Failed(why) => format!("Failed: {why}"),
        other => format!("{other:?}"),
    };
    let compose = json!({
        "text": held.editor().read(cx).doc().plain_text(),
        "status": status,
        "correction": correction,
        "focused": held.focused(window, cx),
    });
    let model = chat.translation().read(cx);
    let progress = model.progress().map_or(
        Value::Null,
        |progress| json!({"done": progress.done, "total": progress.total}),
    );
    let translation = json!({"status": format!("{:?}", model.status()), "progress": progress});
    let rows: Vec<Value> = chat
        .message_items()
        .map(|item| {
            let mark = match &item.translation {
                None => "none",
                Some(Mark::Translated { .. }) => "translated",
                Some(Mark::Original { .. }) => "original",
                Some(Mark::Waiting) => "waiting",
                Some(Mark::Failed) => "failed",
                Some(Mark::Stopped { .. }) => "stopped",
            };
            let text = match &item.body {
                teams_ui::chat_view::Body::Text(rendered) => rendered
                    .0
                    .blocks
                    .iter()
                    .flat_map(teams_ui::rich_text::Block::plain_lines)
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            json!({"id": item.id, "sender": item.sender, "mark": mark, "text": text})
        })
        .collect();
    (compose, translation, json!(rows))
}
