//! [`Host`]: the handle the consumer drives the engine with over its worker's socket. Every call is a command posted
//! to the engine and returns at once; what follows comes back as [`Event`]s.

use std::sync::atomic::{AtomicU64, Ordering};

use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

use crate::config::{ConfigError, HostConfig};
use crate::event::{EditingCommand, Event, MenuToken, PolicyToken, ReplyToken, SchemeResponse, SchemeToken};
use crate::frames::BufferId;
use crate::input::Input;
use crate::spec::{Geometry, Rgba, SessionId, SessionSpec, UserScript, ViewId, ViewSpec};
use crate::worker::Client;

/// What a page loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Load {
    Uri(String),
    /// A document from text, resolved against `base_uri` (`about:blank` when `None`).
    Html {
        html: String,
        base_uri: Option<String>,
    },
}

/// The result of [`Host::evaluate`]: the script's value as JSON (`None` for `undefined` or a value JSON cannot
/// express), or the exception's message.
pub type Evaluation = Result<Option<String>, String>;

/// A command for the engine thread.
#[derive(Debug, Serialize, Deserialize)]
pub enum Command {
    CreateSession(SessionId, SessionSpec),
    DropSession(SessionId),
    ClearSession(SessionId),
    CreateView(ViewId, Box<ViewSpec>),
    CloseView(ViewId),
    View(ViewId, ViewCommand),
    Policy(PolicyToken, bool),
    Reply(ReplyToken, Result<String, String>),
    Scheme(SchemeToken, SchemeResponse),
    Clipboard(String),
    OptionMenu(MenuToken, Option<u32>),
    Quit,
}

/// A command for one page.
#[derive(Debug, Serialize, Deserialize)]
pub enum ViewCommand {
    Load(Load),
    Reload,
    Stop,
    GoBack,
    GoForward,
    Zoom(f64),
    Scripts(Vec<UserScript>),
    Background(Option<Rgba>),
    Geometry(Geometry),
    Visible(bool),
    Focused(bool),
    Input(Input),
    Presented(BufferId),
    FallBackToShm,
    Editing(EditingCommand),
    ImePreedit {
        text: String,
        cursor: u32,
    },
    ImeCommit(String),
    /// Never sent to a worker as is: its reply channel stays in this process (crate `worker`).
    #[serde(skip)]
    Evaluate {
        script: String,
        world: Option<String>,
        reply: Sender<Evaluation>,
    },
}

/// Why the engine did not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("the WPE engine runs once per process: WebKit binds its main run loop to the first thread that uses it")]
    AlreadyStarted,
    #[error("the WPE engine thread did not start: {0}")]
    Thread(#[source] std::io::Error),
    #[error("the WPE display did not connect: {0}")]
    Display(String),
    #[error("the WPE engine thread ended while starting")]
    Ended,
    /// The worker process (crate `worker`) did not start, answer or keep to its protocol.
    #[error("the WPE worker failed: {0}")]
    Worker(String),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The engine, in the sibling `ngteams-web` process. Dropping it shuts the engine down, as [`Host::shutdown`] does.
///
/// `Send` and `Sync`: every method posts a command, so a renderer thread may report a frame presented.
#[derive(Debug)]
pub struct Host {
    engine: Client,
}

impl Host {
    /// Starts a worker on `source`.
    ///
    /// # Errors
    ///
    /// An invalid `config`, a worker that can't be started or doesn't answer, or an engine that doesn't start.
    pub fn start_worker(
        config: &HostConfig,
        source: &crate::EngineSource,
    ) -> Result<(Self, Receiver<Event>), StartError> {
        config.validate()?;
        let (engine, events) = Client::start(config, source)?;
        Ok((Self { engine }, events))
    }

    /// The worker's pid.
    #[must_use]
    pub fn worker_pid(&self) -> u32 {
        self.engine.pid()
    }

    fn send(&self, command: Command) {
        self.engine.send(command);
    }

    /// Runs `script` in the page's main frame, in `world` (`None`: the page's own); the result arrives on the
    /// returned channel (an error when the engine ended first).
    #[must_use]
    pub fn evaluate(&self, view: ViewId, script: String, world: Option<String>) -> Receiver<Evaluation> {
        self.engine.evaluate(view, script, world)
    }

    /// Closes every page, ends every web process and stops the engine, waiting (a bounded time) until it has; the
    /// worker is then reaped. Commands after it are dropped. Once is enough: a second call (or the drop after it)
    /// returns at once.
    pub fn shutdown(&self) {
        self.engine.shutdown();
    }
}

crate::host_commands!(Host, next_id);

impl Drop for Host {
    fn drop(&mut self) {
        self.shutdown();
    }
}
