//! The run's apps as calls need them (the shared harness is `tests/common`): each sandbox with its private PipeWire (its
//! microphone's tone, its speaker recorded, its fake camera), the call keys, the switcher's commands, the in-call bar.

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use serde_json::Value;
use teams_media::tone::Tone;
use ui_sandbox::input::{Modifier, keys};
use ui_sandbox::pipewire::timeline::Timeline;
use ui_sandbox::pipewire::{Echo, PipeWireOptions, PrivatePipeWire};
use ui_sandbox::{NIRI_FULL_WINDOW, SandboxOptions};

use super::Lab;
use super::common::Launch;
pub use super::common::{App, STEP, at_ms, is, strings};

/// How long the run lets the switcher take in a query before it presses Enter (the switcher traces no keystroke).
const SETTLE: Duration = Duration::from_secs(3);

/// Whether `line` is an `in-call shown` whose kinds hold every one of `kinds`.
pub fn in_call_with(line: &Value, kinds: &[&str]) -> bool {
    is(line, "in-call", "shown") && {
        let shown = strings(line, "kinds");
        kinds.iter().all(|kind| shown.iter().any(|one| one == kind))
    }
}

impl App {
    /// The sandbox of `alias`, its microphone playing `tone`, its speaker looped back into it by `echo` when given;
    /// then the app started in it, ready once its push channel, its first catch-up and its calling registration came
    /// (`CALL_LIVE_APP_LOG` sets its diagnostic log's level: `debug` carries payloads, lab data kept with the lab).
    pub fn with_pipewire(lab: &Lab, alias: &'static str, tone: Tone, echo: Option<Echo>) -> Self {
        let options = SandboxOptions {
            niri_extra: NIRI_FULL_WINDOW,
            pipewire: Some(PipeWireOptions {
                tone,
                echo,
                ..PipeWireOptions::default()
            }),
            ..SandboxOptions::default()
        };
        let launch = Launch {
            binary: lab.app.clone(),
            out: lab.out.clone(),
            shots: lab.shots.clone(),
            env: std::env::var("CALL_LIVE_APP_LOG")
                .ok()
                .map(|level| (teams_log::ENV, level))
                .into_iter()
                .collect(),
            ready: vec![
                ("status", "connected"),
                ("status", "caught-up"),
                ("calling", "registered"),
            ],
        };
        let tools = lab.tools.as_ref().expect("the UI sandbox's tools");
        Self::new(tools, alias, &options, launch)
    }

    /// The sandbox's private PipeWire.
    pub fn pipewire(&self) -> &PrivatePipeWire {
        self.sandbox.pipewire().expect("the sandbox runs its PipeWire")
    }

    /// The speaker's recording and when its samples were heard.
    pub fn recording(&self) -> (PathBuf, Timeline) {
        let pipewire = self.pipewire();
        (pipewire.recording(), pipewire.recording_timeline())
    }

    /// Ctrl+Shift+`letter`: the call keys.
    pub fn ctrl_shift(&mut self, letter: char) {
        let (key, _) = self.input.keymap().key_for(letter).unwrap();
        self.input.key_with(&[Modifier::Ctrl, Modifier::Shift], key).unwrap();
    }

    /// Runs the switcher's command `query` names (Ctrl+K, the query, Enter).
    pub fn command(&mut self, query: &str) {
        let from = self.trace().mark();
        self.ctrl('k');
        self.trace()
            .wait_from(from, STEP, "switcher opened", |line| is(line, "switcher", "opened"));
        self.type_text(query);
        thread::sleep(SETTLE);
        self.key(keys::ENTER);
    }

    /// Waits for an `in-call shown` from `from` on whose kinds hold every one of `kinds`.
    pub fn in_call(&mut self, from: usize, limit: Duration, kinds: &[&str]) -> Value {
        let what = format!("in-call shown {kinds:?}");
        self.trace()
            .wait_from(from, limit, &what, |line| in_call_with(line, kinds))
    }
}
