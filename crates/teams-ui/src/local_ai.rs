//! The AI providers that the app asks, shared by everything that asks one: live captions
//! translated ([`crate::translate`]), a meeting's recap summarized ([`crate::recap_summary`]), a chat translated
//! ([`crate::chat_translate`]), a draft proofread ([`crate::message_correct`]) and a draft translated in its box
//! ([`crate::draft_translate`]). Each is asked with the model the user picked for that work ([`AiChoice`]: the one
//! recommended for the work, the provider's own default, or any model it takes, named). Claude Code and the Codex CLI
//! are started as a child process with nothing of the user's setup that could act on what it is given:
//!
//! - **Claude Code** runs as `claude -p --input-format stream-json --output-format stream-json`: one user message per
//!   question in one conversation, its `result` line the answer (a chat's translation also reads its text as it
//!   streams, `--include-partial-messages`). No tools, MCP servers, slash commands, settings files (so no hooks) or
//!   saved session.
//! - **Codex** runs as `codex app-server` (JSON-RPC over stdio): `initialize`, one ephemeral read-only thread whose
//!   base instructions are ours, then one `turn/start` per question, its last agent message the answer (its deltas
//!   the text so far). Hooks, apps,
//!   plugins, MCP servers, tools and project docs are turned off for it.
//! - **An API provider** with the user's key (Anthropic, OpenAI, an OpenAI-compatible server): a conversation over
//!   HTTP ([`api`]). The list of the providers, their order and their switches, and their keys in the keyring are
//!   [`providers`]; a [`Target`] is where a [`Launch`] goes, a tool's program or an API.
//!
//! The models each tool offers are listed from what it reports itself ([`catalog`]), never from a list of ours.
//!
//! **One switch** ([`AiSwitch`], "Use AI features" in Settings): every AI entry point asks [`enabled`] first. While it
//! is off, [`pick`] refuses all work ([`Failure::Off`]), and no tool is looked for ([`Tools::of`]), no model is listed
//! ([`Catalog::of`], [`providers::ApiModels::of`]) and no API is asked. The views hide each AI control while it is off.
//! Only this module builds a [`Target`]: `tests/ai_switch.rs` keeps it so.
//!
//! [`Tool`] is one such process or API conversation: started, asked and read back, each wait bounded and stopped as
//! soon as its caller drops the work. **Failures** ([`Failure`]) say what the user can do: the tool missing, signed out,
//! too slow, or stopped in its own words; for an API also no key, no credit, a rate limit, a model it does not know, no
//! model chosen, the provider turned off or removed. Nothing here logs what is asked or answered, the tool's output, or
//! a key.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use teams_platform::command::Tree;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod api;
pub mod catalog;
pub mod providers;
#[cfg(windows)]
mod windows_prompt;

pub use catalog::{Catalog, Listed};
pub use providers::{AiKeys, KeyVault, Picked, Provider, Providers, pick};

/// How long a wait on the tool polls before it looks at whether its work was dropped.
const POLL: Duration = Duration::from_millis(100);
/// The most of the tool's stderr kept, to tell a sign-in failure: its tail.
const STDERR_TAIL: usize = 4096;
/// What a tool's refusal says when it isn't signed in.
const SIGN_IN: [&str; 9] = [
    "login",
    "log in",
    "sign in",
    "signed in",
    "logged in",
    "authenticat",
    "api key",
    "unauthorized",
    "credential",
];

/// The kind of an AI provider: a command-line tool on this computer, or an API that the app asks with a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Claude Code (`claude`).
    Claude,
    /// The Codex CLI (`codex`).
    Codex,
    /// The Anthropic Messages API.
    Anthropic,
    /// The OpenAI Chat Completions API.
    OpenAi,
    /// An API that takes OpenAI's Chat Completions at its own base URL (OpenRouter, Ollama, LM Studio, Azure).
    Compatible,
}

/// What a tool is asked to do: each work has the model it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Work {
    /// Live captions, one at a time: the fastest model.
    Captions,
    /// A meeting's transcript summarized: one long job, a stronger model.
    Summary,
    /// A chat's messages translated, a batch at a time with the chat as context, and the account's own before they
    /// are sent: the fast model, as someone waits on a send.
    Chat,
    /// A draft proofread before it is sent, when the user asks: the stronger model, as the better word is the point.
    Correction,
    /// A draft translated in its box, when the user asks ([`crate::draft_translate`]): the stronger model, as the
    /// user sends what comes back, with little reasoning, as the user waits on it.
    Draft,
}

impl Backend {
    pub const ALL: [Self; 5] = [
        Self::Claude,
        Self::Codex,
        Self::Anthropic,
        Self::OpenAi,
        Self::Compatible,
    ];
    /// The command-line tools, in the order Automatic tries them when the user did not order the providers.
    pub const CLI: [Self; 2] = [Self::Claude, Self::Codex];

    /// Its name in the settings file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Compatible => "openai-compatible",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|backend| backend.key() == key)
    }

    /// Whether it is a command-line tool on this computer (else an API asked with a key).
    #[must_use]
    pub fn is_cli(self) -> bool {
        matches!(self, Self::Claude | Self::Codex)
    }

    /// The command it runs.
    #[must_use]
    pub fn program(self) -> &'static str {
        self.key()
    }

    /// The model recommended for `work` ([`Model::Recommended`]); `None`: this app knows none (an OpenAI-compatible
    /// API serves the models of its own server). Codex has no model named plain `luna`: it refuses it for a `ChatGPT`
    /// account, so the newest Luna it lists is asked for captions, and its mid-sized Terra for a summary.
    #[must_use]
    pub fn recommended(self, work: Work) -> Option<&'static str> {
        let fast = matches!(work, Work::Captions | Work::Chat);
        match self {
            Self::Claude => Some(if fast { "haiku" } else { "sonnet" }),
            Self::Codex | Self::OpenAi => Some(if fast { "gpt-6-luna" } else { "gpt-6.1-terra" }),
            Self::Anthropic => Some(if fast { "claude-haiku-5-5" } else { "claude-sonnet-5-5" }),
            Self::Compatible => None,
        }
    }

    /// How hard the model reasons for `work`: Codex's effort, Anthropic's `output_config.effort`, OpenAI's
    /// `reasoning_effort` (Claude Code and OpenAI-compatible servers take none).
    #[must_use]
    pub fn effort(self, work: Work) -> &'static str {
        match work {
            Work::Captions | Work::Chat | Work::Draft => "low",
            Work::Summary | Work::Correction => "medium",
        }
    }

    /// The model `model` asks it for doing `work`: `None` leaves it to the tool's own default.
    #[must_use]
    pub fn model_of(self, work: Work, model: &Model) -> Option<String> {
        match model {
            Model::Recommended => self.recommended(work).map(str::to_owned),
            Model::ToolDefault => None,
            Model::Named(name) => Some(name.clone()),
        }
    }

    /// What the views call it doing `work` with `model`: "Claude Haiku" (recommended), "Claude Code" (its default),
    /// "Claude Code (opus)" (named).
    #[must_use]
    pub fn label_of(self, work: Work, model: &Model) -> String {
        match model {
            Model::Recommended => self.label(work).to_owned(),
            Model::ToolDefault => self.name().to_owned(),
            Model::Named(name) => format!("{} ({name})", self.name()),
        }
    }

    /// Its name in a label: "Claude Code", "Codex", "Anthropic API".
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Anthropic => "Anthropic API",
            Self::OpenAi => "OpenAI API",
            Self::Compatible => "OpenAI-compatible API",
        }
    }

    /// What the views call it doing `work` with its recommended model.
    #[must_use]
    pub fn label(self, work: Work) -> &'static str {
        let fast = matches!(work, Work::Captions | Work::Chat);
        match self {
            Self::Claude | Self::Anthropic => {
                if fast {
                    "Claude Haiku"
                } else {
                    "Claude Sonnet"
                }
            }
            Self::Codex => {
                if fast {
                    "Codex GPT-6-Luna"
                } else {
                    "Codex GPT-6.1-Terra"
                }
            }
            Self::OpenAi => {
                if fast {
                    "GPT-6-Luna"
                } else {
                    "GPT-6.1-Terra"
                }
            }
            Self::Compatible => self.name(),
        }
    }

    /// The tool's name, as a notice says it.
    #[must_use]
    pub fn tool(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "The Codex CLI",
            Self::Anthropic => "The Anthropic API",
            Self::OpenAi => "The OpenAI API",
            Self::Compatible => "The OpenAI-compatible API",
        }
    }

    /// Who the tool sends what it is given to.
    #[must_use]
    pub fn provider(self) -> &'static str {
        match self {
            Self::Claude | Self::Anthropic => "Anthropic",
            Self::Codex | Self::OpenAi => "OpenAI",
            Self::Compatible => "the API's server",
        }
    }

    /// What signs the tool in.
    #[must_use]
    pub fn sign_in(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex login",
            Self::Anthropic | Self::OpenAi | Self::Compatible => "",
        }
    }

    /// Its command line for `work` with `model`, `instructions` being the system prompt (Codex takes the model and the
    /// instructions in its thread instead).
    #[must_use]
    pub fn args(self, work: Work, model: &Model, instructions: &str) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut args = vec!["-p".to_owned()];
                if let Some(model) = self.model_of(work, model) {
                    args.extend(["--model".to_owned(), model]);
                }
                // a chat's translations show as each message's comes, not once the whole batch is done
                if work == Work::Chat {
                    args.push("--include-partial-messages".to_owned());
                }
                args.extend(
                    [
                        "--input-format",
                        "stream-json",
                        "--output-format",
                        "stream-json",
                        "--verbose",
                        "--tools",
                        "",
                        "--no-session-persistence",
                        "--setting-sources",
                        "",
                        "--strict-mcp-config",
                        "--disable-slash-commands",
                        "--system-prompt",
                        instructions,
                    ]
                    .map(str::to_owned),
                );
                args
            }
            Self::Anthropic | Self::OpenAi | Self::Compatible => Vec::new(),
            Self::Codex => {
                let mut args = vec![
                    "app-server".to_owned(),
                    "-c".to_owned(),
                    format!("model_reasoning_effort=\"{}\"", self.effort(work)),
                    "-c".to_owned(),
                    "mcp_servers={}".to_owned(),
                    "-c".to_owned(),
                    "project_doc_max_bytes=0".to_owned(),
                ];
                for feature in CODEX_OFF {
                    args.extend(["-c".to_owned(), format!("features.{feature}=false")]);
                }
                args
            }
        }
    }
}

/// Codex's features turned off: no hooks of the user's run on what it is given, and no tools.
const CODEX_OFF: [&str; 13] = [
    "hooks",
    "apps",
    "plugins",
    "multi_agent",
    "shell_tool",
    "unified_exec",
    "image_generation",
    "goals",
    "view_image",
    "browser_use",
    "computer_use",
    "tool_suggest",
    "skill_search",
];

/// Which provider does a work, as the settings keep it ([`providers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToolChoice {
    /// The first provider in the user's order that is on and can work ([`providers::pick`]).
    #[default]
    Automatic,
    /// The provider of this id ([`providers::Provider::id`]): the command-line tools keep their kind's key (`claude`,
    /// `codex`), so a choice of an earlier build names the same tool.
    Only(String),
}

impl ToolChoice {
    /// The command-line tool `backend`.
    #[must_use]
    pub fn cli(backend: Backend) -> Self {
        Self::Only(backend.key().to_owned())
    }

    /// Its name in the settings file.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Automatic => "automatic",
            Self::Only(id) => id,
        }
    }

    /// The choice `key` names: `automatic`, or a provider's id ([`providers::id_shaped`]).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "automatic" => Some(Self::Automatic),
            _ => providers::id_shaped(key).then(|| Self::Only(key.to_owned())),
        }
    }
}

/// The model a tool is asked for, as the settings keep it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Model {
    /// The one this app recommends for the work ([`Backend::model`]).
    #[default]
    Recommended,
    /// None asked for: the tool uses its own default.
    ToolDefault,
    /// Any model the tool takes, by the name its command line takes (an alias or a full name).
    Named(String),
}

/// The longest model name kept, in bytes.
pub const MAX_MODEL_NAME: usize = 100;

impl Model {
    /// Its name in the settings file: `recommended`, `default`, or the model's name.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Recommended => "recommended",
            Self::ToolDefault => "default",
            Self::Named(name) => name,
        }
    }

    /// The model `key` names (as the settings file or the user types it, trimmed): `recommended`, `default` (Claude
    /// Code's alias for its default, so the same thing), or a model's name: visible characters without spaces, at
    /// most [`MAX_MODEL_NAME`] bytes, not starting with `-` (it goes on a command line).
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        let key = key.trim();
        match key {
            "recommended" => Some(Self::Recommended),
            "default" => Some(Self::ToolDefault),
            _ => {
                let shaped = !key.is_empty()
                    && key.len() <= MAX_MODEL_NAME
                    && !key.starts_with('-')
                    && key.chars().all(|c| !c.is_whitespace() && !c.is_control());
                shaped.then(|| Self::Named(key.to_owned()))
            }
        }
    }
}

/// A local AI choice of the settings: which tool, then which model. Every setting that picks a local AI keeps one,
/// and the settings show each with the same control ([`crate::settings_view`]'s AI picker).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct AiChoice {
    pub tool: ToolChoice,
    pub model: Model,
}

impl AiChoice {
    /// `tool` with its recommended model.
    #[must_use]
    pub fn of(tool: ToolChoice) -> Self {
        Self {
            tool,
            model: Model::Recommended,
        }
    }
}

/// "Use AI features" as the settings last published it ([`crate::settings::SettingsModel::publish_ai`]): an app
/// global that each AI entry point asks through [`enabled`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiSwitch(pub bool);

impl Global for AiSwitch {}

impl AiSwitch {
    /// Makes `on` the app's switch, when it changed. Off, the work that runs stops: each view that runs one follows
    /// the switch.
    pub fn publish(on: bool, cx: &mut App) {
        if cx.try_global::<Self>() != Some(&Self(on)) {
            cx.set_global(Self(on));
        }
    }
}

/// Whether the AI features may run now. The app publishes its switch at start, before any view; a view test that
/// publishes none runs them.
#[must_use]
pub fn enabled(cx: &App) -> bool {
    cx.try_global::<AiSwitch>().is_none_or(|switch| switch.0)
}

/// The tools found on this computer: an app global, looked for on the `PATH` once (and again on Try again:
/// [`Tools::look_again`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tools {
    pub(crate) found: Vec<(Backend, PathBuf)>,
}

impl Global for Tools {}

impl Tools {
    /// Looks for each tool on the `PATH`.
    #[must_use]
    pub fn detect() -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let found = Backend::CLI
            .into_iter()
            .filter_map(|backend| {
                std::env::split_paths(&path)
                    .flat_map(|dir| program_paths(&dir, backend.program()))
                    .find(|candidate| executable(candidate))
                    .map(|program| (backend, program))
            })
            .collect();
        Self { found }
    }

    /// These tools, at these paths.
    #[must_use]
    pub fn with(found: Vec<(Backend, PathBuf)>) -> Self {
        Self { found }
    }

    /// The app's, looked for on first use; none while the AI features are off, and nothing is looked for.
    pub fn of(cx: &mut App) -> Self {
        if !enabled(cx) {
            return Self::default();
        }
        if !cx.has_global::<Self>() {
            cx.set_global(Self::detect());
        }
        cx.global::<Self>().clone()
    }

    /// Looks on the `PATH` again (Try again after a tool was missing); when what is found changed, the models the
    /// tools offer are listed again too.
    pub fn look_again(cx: &mut App) {
        if enabled(cx) {
            Self::found_again(Self::detect(), cx);
        }
    }

    /// The tools a look on the `PATH` found: the app's from now on, and the models listed again when they changed.
    pub fn found_again(found: Self, cx: &mut App) {
        if cx.try_global::<Self>() == Some(&found) {
            return;
        }
        cx.set_global(found);
        if cx.has_global::<Catalog>() {
            Catalog::relist(cx);
        }
    }

    /// The tools installed, Claude Code first.
    pub fn installed(&self) -> impl Iterator<Item = Backend> + '_ {
        Backend::CLI
            .into_iter()
            .filter(|backend| self.found.iter().any(|(found, _)| found == backend))
    }

    /// Where `backend` is, when it is installed.
    #[must_use]
    pub fn program(&self, backend: Backend) -> Option<PathBuf> {
        self.found
            .iter()
            .find(|(found, _)| *found == backend)
            .map(|(_, program)| program.clone())
    }
}

#[cfg(unix)]
pub(crate) fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
pub(crate) fn executable(path: &Path) -> bool {
    path.is_file()
        && path.extension().is_some_and(|extension| {
            ["exe", "com", "cmd", "bat"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn program_paths(dir: &Path, program: &str) -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        vec![dir.join(program)]
    }
    #[cfg(windows)]
    {
        ["exe", "com", "cmd", "bat"]
            .map(|extension| dir.join(format!("{program}.{extension}")))
            .to_vec()
    }
}

/// Why a tool's work ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The tool asked for isn't on the `PATH`; `None`: no provider can do the work.
    NotInstalled(Option<Backend>),
    /// The tool is not signed in, or the API refused its key.
    SignedOut(Backend),
    TimedOut(Backend),
    /// The tool stopped or refused, in its own words when it gave some.
    Stopped(Backend, Option<String>),
    /// The API provider has no key in the keyring.
    NoKey(Backend),
    /// The API account has no credit left.
    NoCredit(Backend),
    /// The API refused more requests for now.
    RateLimited(Backend),
    /// The API does not know this model.
    UnknownModel(Backend, String),
    /// No model is chosen, and the provider has no default one.
    NoModel(Backend),
    /// The provider chosen is turned off in the settings.
    TurnedOff(Backend),
    /// The provider chosen is not in the settings any more.
    NoProvider,
    /// The user turned the AI features off ("Use AI features").
    Off,
    /// A failure of the API provider that the user named: its words name that provider, so that two providers of one
    /// kind are told apart ([`Failure::of`]).
    Named(String, Box<Failure>),
}

/// Where the settings of the AI providers are.
const PROVIDERS_PLACE: &str = "Settings under General, AI providers";

impl Failure {
    /// What a draft's failure line adds when the tool isn't installed: install the one named; with no provider, install
    /// or turn on a tool, or an API provider.
    #[must_use]
    pub fn install_hint(&self) -> &'static str {
        match self {
            Self::NotInstalled(None) => {
                " Install or turn on Claude Code or the Codex CLI, or turn on or add an API provider with a key in \
                 Settings under General, AI providers."
            }
            Self::NotInstalled(Some(_)) => " Install it, or choose another tool in Settings under General.",
            Self::Named(_, failure) => failure.install_hint(),
            _ => "",
        }
    }

    /// This failure said of the API provider named `name`. A command-line tool is one of its kind and keeps its own
    /// words, as do the failures of no provider.
    #[must_use]
    pub fn of(self, name: &str) -> Self {
        match self.backend() {
            Some(backend) if !backend.is_cli() && !matches!(self, Self::Named(..)) => {
                Self::Named(name.to_owned(), Box::new(self))
            }
            _ => self,
        }
    }

    /// The kind of the provider that failed, if one did.
    fn backend(&self) -> Option<Backend> {
        match self {
            Self::NotInstalled(backend) => *backend,
            Self::SignedOut(backend)
            | Self::TimedOut(backend)
            | Self::Stopped(backend, _)
            | Self::NoKey(backend)
            | Self::NoCredit(backend)
            | Self::RateLimited(backend)
            | Self::UnknownModel(backend, _)
            | Self::NoModel(backend)
            | Self::TurnedOff(backend) => Some(*backend),
            Self::NoProvider | Self::Off => None,
            Self::Named(_, failure) => failure.backend(),
        }
    }

    /// What a view says of it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Named(name, failure) => failure.said_of(name, name),
            failure => {
                let backend = failure.backend();
                failure.said_of(backend.map_or("", Backend::tool), backend.map_or("", Backend::name))
            }
        }
    }

    /// Its words, with `tool` the subject of a sentence and `name` the provider in a list.
    fn said_of(&self, tool: &str, name: &str) -> String {
        match self {
            Self::NotInstalled(None) => "No AI provider can do this: no Claude Code (claude) or Codex CLI (codex) is \
                                         installed and on, and no API provider is on with a key."
                .to_owned(),
            Self::NotInstalled(Some(backend)) => format!("{tool} ({}) isn't installed.", backend.program()),
            Self::SignedOut(backend) if !backend.is_cli() => {
                format!("{tool} refused the API key. Check the key in {PROVIDERS_PLACE}.")
            }
            Self::SignedOut(backend) => format!(
                "{tool} isn't signed in. Run {} in a terminal, then try again.",
                backend.sign_in()
            ),
            Self::TimedOut(_) => format!("{tool} took too long to answer."),
            Self::Stopped(_, Some(why)) => format!("{tool} stopped: {why}"),
            Self::Stopped(_, None) => format!("{tool} stopped unexpectedly."),
            Self::NoKey(_) => format!("{tool} has no API key. Add one in {PROVIDERS_PLACE}."),
            Self::NoCredit(_) => {
                format!("{tool} says that the account has no credit left. Add credit to it, then try again.")
            }
            Self::RateLimited(_) => {
                format!("{tool} refused more requests for now (rate limit). Wait a moment, then try again.")
            }
            Self::UnknownModel(_, model) => {
                format!("{tool} does not know the model {model}. Choose another model in Settings under General.")
            }
            Self::NoModel(_) => format!("{tool} has no model chosen. Choose one in Settings under General."),
            Self::TurnedOff(_) => format!("{name} is turned off in {PROVIDERS_PLACE}."),
            Self::NoProvider => {
                "The AI provider chosen for this is not in the settings any more. Choose another in Settings \
                 under General."
                    .to_owned()
            }
            Self::Off => "The AI features are off. Turn on \"Use AI features\" in Settings under General.".to_owned(),
            Self::Named(_, failure) => failure.said_of(tool, name),
        }
    }
}

/// Where a [`Tool`] goes: a program on this computer, or an API.
#[derive(Clone, Debug)]
pub enum Target {
    Program(PathBuf),
    Api(providers::ApiTarget),
}

impl Target {
    /// `failure` said of this target's API provider, by its name ([`Failure::of`]); a program's stays as it is.
    #[must_use]
    pub fn named(&self, failure: Failure) -> Failure {
        match self {
            Self::Program(_) => failure,
            Self::Api(api) => failure.of(&api.name),
        }
    }
}

/// What a [`Tool`] runs: the tool, where it is, its work, model and instructions, and how long it may take to start.
#[derive(Clone, Debug)]
pub struct Launch {
    pub backend: Backend,
    pub target: Target,
    pub work: Work,
    pub model: Model,
    pub instructions: String,
    /// How long the process may take to start (Codex answers its thread's start).
    pub first: Duration,
}

/// `text` cut to at most `max` bytes, on a character's boundary.
#[must_use]
pub fn cap(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// A running tool and where its conversation stands: a process, or a conversation with an API.
pub struct Tool {
    link: Link,
    /// Questions asked, and their characters.
    turns: usize,
    chars: usize,
    /// It answered once: later answers may get a shorter wait.
    answered: bool,
}

enum Link {
    Process(Box<Process>),
    /// The conversation, and the name of its provider for the words of its failures.
    Api(Box<api::Conversation>, String),
}

impl Tool {
    /// Starts `launch`'s tool (Codex's thread started too; an API's key read), giving up once `stop` is set.
    ///
    /// # Errors
    /// The tool couldn't start, refused, or took longer than [`Launch::first`].
    pub fn start(launch: &Launch, stop: &AtomicBool) -> Result<Self, Failure> {
        let link = match &launch.target {
            Target::Program(program) => Link::Process(Box::new(Process::start(launch, program, stop)?)),
            Target::Api(api) => Link::Api(
                Box::new(api::Conversation::start(launch, api).map_err(|failure| failure.of(&api.name))?),
                api.name.clone(),
            ),
        };
        Ok(Self {
            link,
            turns: 0,
            chars: 0,
            answered: false,
        })
    }

    /// Questions asked so far.
    #[must_use]
    pub fn turns(&self) -> usize {
        self.turns
    }

    /// Characters asked so far.
    #[must_use]
    pub fn chars(&self) -> usize {
        self.chars
    }

    /// Whether it answered once.
    #[must_use]
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Sends `message` and waits up to `wait` for the answer, trimmed.
    ///
    /// # Errors
    /// The tool refused, stopped, took longer than `wait`, or `stop` was set.
    pub fn ask(&mut self, message: &str, wait: Duration, stop: &AtomicBool) -> Result<String, Failure> {
        self.ask_streaming(message, wait, stop, &mut |_| {})
    }

    /// [`Self::ask`], telling `partial` the answer's text so far each time more of it comes (when the tool streams it:
    /// Claude Code started for [`Work::Chat`], Codex and the APIs), before the whole answer is given back.
    ///
    /// # Errors
    /// As [`Self::ask`].
    pub fn ask_streaming(
        &mut self,
        message: &str,
        wait: Duration,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        let deadline = Instant::now() + wait;
        self.turns += 1;
        self.chars += message.len();
        let text = match &mut self.link {
            Link::Process(process) => process.ask(message, deadline, stop, partial),
            Link::Api(conversation, name) => conversation
                .ask(message, deadline, stop, partial)
                .map_err(|failure| failure.of(name)),
        }?;
        self.answered = true;
        Ok(text.trim().to_owned())
    }

    /// Ends it: a process's stdin closed (both tools leave then), and after a moment its whole group killed. An API
    /// conversation has no request open between two questions (a wait that ends drops its request at once), so nothing
    /// is left to end.
    pub fn close(self) {
        if let Link::Process(process) = self.link {
            process.close();
        }
    }
}

/// A running command-line tool.
struct Process {
    backend: Backend,
    work: Work,
    /// The model asked for, `None`: the tool's default.
    model: Option<String>,
    child: Tree,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    /// Codex's thread.
    thread: Option<String>,
    next_id: u64,
    // Dropped after the process tree: a tool may read its prompt after startup.
    #[cfg(windows)]
    _prompt: Option<tempfile::TempDir>,
}

impl Process {
    fn start(launch: &Launch, program: &Path, stop: &AtomicBool) -> Result<Self, Failure> {
        let backend = launch.backend;
        let args = backend.args(launch.work, &launch.model, &launch.instructions);
        #[cfg(windows)]
        let (args, prompt) = windows_prompt::prepare(launch, program, args)
            .map_err(|error| Failure::Stopped(backend, Some(error.to_string())))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Tree::spawn(&mut command).map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => Failure::NotInstalled(Some(backend)),
            _ => Failure::Stopped(backend, Some(error.to_string())),
        })?;
        let stdin = child.stdin.take();
        let lines = child.stdout.take().map_or_else(
            || mpsc::channel().1,
            |stdout| {
                let (sender, lines) = mpsc::channel();
                thread::spawn(move || read_lines(stdout, &sender));
                lines
            },
        );
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            let tail = Arc::clone(&stderr);
            thread::spawn(move || keep_tail(pipe, &tail));
        }
        let mut tool = Self {
            backend,
            work: launch.work,
            model: backend.model_of(launch.work, &launch.model),
            child,
            stdin,
            lines,
            stderr,
            thread: None,
            next_id: 1,
            #[cfg(windows)]
            _prompt: prompt,
        };
        if backend == Backend::Codex
            && let Err(failure) = tool.codex_thread(&launch.instructions, launch.first, stop)
        {
            tool.close();
            return Err(failure);
        }
        Ok(tool)
    }

    fn ask(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        if self.backend == Backend::Codex {
            self.ask_codex(message, deadline, stop, partial)
        } else {
            self.ask_claude(message, deadline, stop, partial)
        }
    }

    /// Codex: initializes the app server and starts the thread the questions go to.
    fn codex_thread(&mut self, instructions: &str, wait: Duration, stop: &AtomicBool) -> Result<(), Failure> {
        let deadline = Instant::now() + wait;
        let initialize = self.request(
            "initialize",
            &json!({"clientInfo": {"name": "ngteams", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        self.answer_of(initialize, deadline, stop)?;
        self.write(&json!({"jsonrpc": "2.0", "method": "initialized"}))?;
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let mut params = json!({
            "cwd": cwd,
            "approvalPolicy": "never",
            "sandbox": "read-only",
            "ephemeral": true,
            "baseInstructions": instructions,
        });
        // none named: the model the user's Codex is set to
        if let (Some(model), Some(params)) = (&self.model, params.as_object_mut()) {
            params.insert("model".to_owned(), Value::String(model.clone()));
        }
        let start = self.request("thread/start", &params)?;
        let started = self.answer_of(start, deadline, stop)?;
        let thread = started
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or(Failure::Stopped(self.backend, None))?;
        self.thread = Some(thread.to_owned());
        Ok(())
    }

    /// Sends JSON-RPC request `method`; its id.
    fn request(&mut self, method: &str, params: &Value) -> Result<u64, Failure> {
        let id = self.next_id;
        self.next_id += 1;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    /// The result of JSON-RPC request `id`, answering any request the server makes meanwhile.
    fn answer_of(&mut self, id: u64, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            let message = self.next_message(deadline, stop)?;
            self.refuse_request(&message)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(self.refusal(error.get("message").and_then(Value::as_str)));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// The server asking something of the client (an approval): refused, as nothing here may be approved.
    fn refuse_request(&mut self, message: &Value) -> Result<(), Failure> {
        if let (Some(id), Some(_)) = (message.get("id"), message.get("method")) {
            self.write(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported"}}))?;
        }
        Ok(())
    }

    fn ask_claude(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        self.write(&json!({"type": "user", "message": {"role": "user", "content": message}}))?;
        let mut so_far = String::new();
        loop {
            let line = self.next_message(deadline, stop)?;
            match line.get("type").and_then(Value::as_str) {
                Some("stream_event") => {
                    stream_event(&line, &mut so_far, partial);
                    continue;
                }
                Some("result") => {}
                _ => continue,
            }
            let said = line.get("result").and_then(Value::as_str);
            if line.get("is_error").and_then(Value::as_bool) == Some(true) {
                return Err(self.refusal(said));
            }
            return Ok(said.unwrap_or_default().to_owned());
        }
    }

    fn ask_codex(
        &mut self,
        message: &str,
        deadline: Instant,
        stop: &AtomicBool,
        partial: &mut dyn FnMut(&str),
    ) -> Result<String, Failure> {
        let thread = self.thread.clone().unwrap_or_default();
        let id = self.request(
            "turn/start",
            &json!({
                "threadId": thread,
                "input": [{"type": "text", "text": message}],
                "effort": Backend::Codex.effort(self.work),
            }),
        )?;
        let (mut text, mut error) = (None::<String>, None::<String>);
        // the agent message being written: its item and its text so far
        let (mut item, mut so_far) = (String::new(), String::new());
        loop {
            let line = self.next_message(deadline, stop)?;
            self.refuse_request(&line)?;
            if line.get("id").and_then(Value::as_u64) == Some(id)
                && let Some(refused) = line.get("error")
            {
                return Err(self.refusal(refused.get("message").and_then(Value::as_str)));
            }
            let params = line.get("params").unwrap_or(&Value::Null);
            match line.get("method").and_then(Value::as_str) {
                Some("item/agentMessage/delta") => {
                    let of = params.get("itemId").and_then(Value::as_str).unwrap_or_default();
                    if of != item {
                        of.clone_into(&mut item);
                        so_far.clear();
                    }
                    so_far.push_str(params.get("delta").and_then(Value::as_str).unwrap_or_default());
                    partial(&so_far);
                }
                Some("item/completed") if params.pointer("/item/type") == Some(&json!("agentMessage")) => {
                    text = params.pointer("/item/text").and_then(Value::as_str).map(str::to_owned);
                }
                Some("error") if params.get("willRetry") != Some(&json!(true)) => {
                    error = params
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                Some("turn/completed") => {
                    let failed = params.pointer("/turn/error/message").and_then(Value::as_str);
                    return match (error.or_else(|| failed.map(str::to_owned)), text) {
                        (Some(why), _) => Err(self.refusal(Some(&why))),
                        (None, Some(text)) => Ok(text),
                        (None, None) => Err(Failure::Stopped(self.backend, Some("it gave no answer.".to_owned()))),
                    };
                }
                _ => {}
            }
        }
    }

    fn write(&mut self, message: &Value) -> Result<(), Failure> {
        let backend = self.backend;
        let stdin = self.stdin.as_mut().ok_or(Failure::Stopped(backend, None))?;
        let mut line = message.to_string();
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|_| self.exited())
    }

    /// The tool's next JSON line (other lines skipped), by `deadline`.
    fn next_message(&mut self, deadline: Instant, stop: &AtomicBool) -> Result<Value, Failure> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Failure::Stopped(self.backend, None));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Failure::TimedOut(self.backend));
            }
            match self.lines.recv_timeout(left.min(POLL)) {
                Ok(line) => {
                    if let Ok(value) = serde_json::from_str::<Value>(&line) {
                        return Ok(value);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(self.exited()),
            }
        }
    }

    /// Why the process ended: signed out when its last words say so, else in those words.
    fn exited(&mut self) -> Failure {
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        // the stderr reader may still be taking its last bytes
        thread::sleep(Duration::from_millis(50));
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let last = tail.lines().rev().find(|line| !line.trim().is_empty());
        self.refusal(last)
    }

    /// A refusal in the tool's words: a sign-in one as such.
    fn refusal(&self, said: Option<&str>) -> Failure {
        let Some(said) = said.map(str::trim).filter(|said| !said.is_empty()) else {
            return Failure::Stopped(self.backend, None);
        };
        let lower = said.to_lowercase();
        if SIGN_IN.iter().any(|word| lower.contains(word)) {
            return Failure::SignedOut(self.backend);
        }
        let line = said.lines().next().unwrap_or(said);
        let mut why = cap(line, 160);
        if why.len() < line.len() {
            why.push('…');
        }
        Failure::Stopped(self.backend, Some(why))
    }

    /// Ends the process: its stdin closed (both tools leave then), and after a moment its whole group killed.
    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(1);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        self.child.stop();
    }
}

/// Claude Code's streamed event `line` (`--include-partial-messages`): a text delta adds to `so_far`, told to `partial`;
/// a new message starts it over.
fn stream_event(line: &Value, so_far: &mut String, partial: &mut dyn FnMut(&str)) {
    let event = line.get("event").unwrap_or(&Value::Null);
    match event.get("type").and_then(Value::as_str) {
        Some("message_start") => so_far.clear(),
        Some("content_block_delta") if event.pointer("/delta/type") == Some(&json!("text_delta")) => {
            so_far.push_str(event.pointer("/delta/text").and_then(Value::as_str).unwrap_or_default());
            partial(so_far);
        }
        _ => {}
    }
}

/// Hands each line the tool writes to its reader, until it ends.
fn read_lines(stdout: impl std::io::Read, sender: &mpsc::Sender<String>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if sender.send(line).is_err() {
            break;
        }
    }
}

/// Keeps the last [`STDERR_TAIL`] bytes the tool writes to stderr.
fn keep_tail(mut pipe: impl std::io::Read, tail: &Mutex<String>) {
    let mut chunk = [0_u8; 1024];
    while let Ok(read @ 1..) = pipe.read(&mut chunk) {
        let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
        tail.push_str(&String::from_utf8_lossy(&chunk[..read]));
        if tail.len() > STDERR_TAIL {
            let cut = tail.len() - STDERR_TAIL;
            let cut = (cut..tail.len()).find(|at| tail.is_char_boundary(*at)).unwrap_or(0);
            tail.drain(..cut);
        }
    }
}
