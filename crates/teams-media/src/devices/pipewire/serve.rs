//! The loop thread: PipeWire's main loop over the one connection, the streams the devices asked for, the devices
//! the server holds ([`listing`](super::listing)), and the user's choice of each kind.
//!
//! **A device moved** ([`Command::Choose`]): every open stream of that kind gets a pending replacement asking for the
//! new choice; the loop swaps them ([`SWAP_EVERY`]) once the replacement streams (the session manager linked it), or
//! after [`SWAP_WAIT`] at the latest, retiring the old one; a replacement that ends before then is dropped and the old
//! one kept. So the device's queue stays open throughout and the media goes on (see [`streams`](super::streams)).

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use pipewire::context::ContextRc;
use pipewire::core::{CoreRc, PW_ID_CORE};
use pipewire::main_loop::{MainLoopRc, MainLoopWeak};
use pipewire::properties::PropertiesBox;

use super::listing::{self, Listing};
use super::streams::{self, Gate, Target};
use crate::devices::catalog::{Choices, DeviceKind};
use crate::devices::{AudioFrame, VideoFrame};
use crate::queue::Queue;

/// How often the loop looks for replacements to swap in.
const SWAP_EVERY: Duration = Duration::from_millis(20);
/// How long a replacement gets to be linked before it is swapped in anyway.
const SWAP_WAIT: Duration = Duration::from_secs(1);

/// The device side's way into the loop thread.
pub(super) enum Command {
    Microphone {
        id: u64,
        frames: Queue<AudioFrame>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    Speaker {
        id: u64,
        queue: Queue<f32>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    Camera {
        id: u64,
        frames: Queue<VideoFrame>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    /// A shared screen's stream, to the node `node` the screen-cast portal named: no device choice moves it.
    Screen {
        id: u64,
        node: u32,
        frames: Queue<VideoFrame>,
        ready: mpsc::Sender<Result<(), String>>,
    },
    /// Use `name` for `kind` from now on (`None`: the system's default), moving the open streams of that kind.
    Choose {
        kind: DeviceKind,
        name: Option<String>,
    },
    Close(u64),
    Quit,
}

/// Makes a device's stream again, asking for another target.
type Make = Box<dyn Fn(&CoreRc, Target<'_>, Rc<Gate>) -> Result<Box<dyn Any>, String>>;

/// A stream and its gate.
struct Held {
    gate: Rc<Gate>,
    _open: Box<dyn Any>,
}

impl Held {
    /// Drops the stream as replaced: it closes nothing on its way out.
    fn retire(self) {
        self.gate.retire();
    }
}

/// An open device's stream, and its replacement while one is being linked; a shared screen's is of no kind (no choice
/// moves it).
struct Entry {
    kind: Option<DeviceKind>,
    make: Make,
    live: Held,
    pending: Option<(Held, Instant)>,
}

impl Entry {
    /// Makes a replacement asking for `target` (a replacement already waiting is retired).
    fn replace(&mut self, core: &CoreRc, target: Target<'_>) {
        let gate = Gate::pending();
        // a replacement that can't be made leaves the device where it is
        if let Ok(open) = (self.make)(core, target, Rc::clone(&gate))
            && let Some((old, _)) = self.pending.replace((Held { gate, _open: open }, Instant::now()))
        {
            old.retire();
        }
    }

    /// Swaps the replacement in once it streamed or waited long enough; drops it if it ended first.
    fn swap(&mut self, now: Instant) {
        let Some((pending, since)) = self.pending.take() else {
            return;
        };
        if pending.gate.has_ended() {
            pending.retire();
        } else if pending.gate.has_streamed() || now.duration_since(since) >= SWAP_WAIT {
            pending.gate.go_live();
            std::mem::replace(&mut self.live, pending).retire();
        } else {
            self.pending = Some((pending, since));
        }
    }
}

/// Everything the loop thread's commands act on.
struct State {
    core: CoreRc,
    entries: HashMap<u64, Entry>,
    choices: Choices,
}

type Shared = Rc<RefCell<State>>;

/// What the loop thread starts with.
pub(super) struct Start<'a> {
    pub fd: OwnedFd,
    pub client_conf: Option<&'a Path>,
    pub commands: pipewire::channel::Receiver<Command>,
    pub listing: Arc<Listing>,
    pub choices: Choices,
}

/// Runs the loop until [`Command::Quit`] or the server's fatal error; reports on `started` once the devices are
/// listed (two round trips: the registry's nodes, then the defaults of the metadata bound in the first), or why
/// they weren't.
pub(super) fn run(start: Start<'_>, started: &mpsc::Sender<Result<(), String>>) {
    if let Err(error) = serve(start, started) {
        let _ = started.send(Err(error));
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn serve(start: Start<'_>, started: &mpsc::Sender<Result<(), String>>) -> Result<(), String> {
    // libwebrtc's stubs of libpipewire's functions stand in for its own in this binary: loaded first
    if !crate::rtc::load_pipewire() {
        log::warn!("libwebrtc couldn't load libpipewire into its stubs");
    }
    pipewire::init();
    let main_loop = MainLoopRc::new(None).map_err(text)?;
    let mut context_props = PropertiesBox::new();
    if let Some(conf) = start.client_conf {
        let conf = conf.to_str().ok_or("the client config's path isn't UTF-8")?;
        context_props.insert(*pipewire::keys::CONFIG_NAME, conf);
    }
    let context = ContextRc::new(&main_loop, Some(context_props)).map_err(text)?;
    let core = context.connect_fd_rc(start.fd, None).map_err(text)?;
    let registry = core.get_registry_rc().map_err(text)?;
    let _watching = listing::watch(&registry, start.listing);
    let _core_listener = listen_core(&core, &main_loop, started.clone())?;
    let state: Shared = Rc::new(RefCell::new(State {
        core: core.clone(),
        entries: HashMap::new(),
        choices: start.choices,
    }));
    let handler = {
        let state = Rc::clone(&state);
        let main_loop = main_loop.downgrade();
        move |command| handle(command, &state, &main_loop)
    };
    let _receiver = start.commands.attach(main_loop.loop_(), handler);
    let swapping = Rc::clone(&state);
    let timer = main_loop.loop_().add_timer(move |_| {
        let now = Instant::now();
        for entry in swapping.borrow_mut().entries.values_mut() {
            entry.swap(now);
        }
    });
    timer
        .update_timer(Some(SWAP_EVERY), Some(SWAP_EVERY))
        .into_sync_result()
        .map_err(text)?;
    main_loop.run();
    state.borrow_mut().entries.clear();
    Ok(())
}

/// Reports on `started` after the second round trip; quits the loop on the server's fatal error.
fn listen_core(
    core: &CoreRc,
    main_loop: &MainLoopRc,
    started: mpsc::Sender<Result<(), String>>,
) -> Result<pipewire::core::Listener, String> {
    let first = core.sync(0).map_err(text)?;
    let second = Rc::new(RefCell::new(None));
    let syncing = core.downgrade();
    let quit_loop = main_loop.downgrade();
    Ok(core
        .add_listener_local()
        .done(move |id, seq| {
            if id != PW_ID_CORE {
                return;
            }
            if seq == first {
                let next = syncing.upgrade().and_then(|core| core.sync(0).ok());
                *second.borrow_mut() = next;
            } else if Some(seq) == *second.borrow() {
                let _ = started.send(Ok(()));
            }
        })
        .error(move |id, _seq, _res, _message| {
            if id == PW_ID_CORE
                && let Some(main_loop) = quit_loop.upgrade()
            {
                main_loop.quit();
            }
        })
        .register())
}

fn handle(command: Command, state: &Shared, main_loop: &MainLoopWeak) {
    let (id, kind, make, ready): (u64, Option<DeviceKind>, Make, _) = match command {
        Command::Microphone { id, frames, ready } => (
            id,
            Some(DeviceKind::Microphone),
            Box::new(move |core: &CoreRc, target: Target<'_>, gate| {
                streams::microphone(core, frames.clone(), target, gate)
            }),
            ready,
        ),
        Command::Speaker { id, queue, ready } => (
            id,
            Some(DeviceKind::Speaker),
            Box::new(move |core: &CoreRc, target: Target<'_>, gate| {
                streams::speaker(core, queue.clone(), target, gate)
            }),
            ready,
        ),
        Command::Camera { id, frames, ready } => (
            id,
            Some(DeviceKind::Camera),
            Box::new(move |core: &CoreRc, target: Target<'_>, gate| {
                streams::camera(core, frames.clone(), target, gate)
            }),
            ready,
        ),
        Command::Screen {
            id,
            node,
            frames,
            ready,
        } => (
            id,
            None,
            Box::new(move |core: &CoreRc, _: Target<'_>, gate| streams::screen(core, frames.clone(), node, gate)),
            ready,
        ),
        Command::Choose { kind, name } => return choose(&mut state.borrow_mut(), kind, name.as_deref()),
        Command::Close(id) => {
            // the entry goes after the borrow: its streams' own callbacks may run as they go
            let removed = state.borrow_mut().entries.remove(&id);
            drop(removed);
            return;
        }
        Command::Quit => {
            if let Some(main_loop) = main_loop.upgrade() {
                main_loop.quit();
            }
            return;
        }
    };
    let (core, target) = {
        let state = state.borrow();
        (
            state.core.clone(),
            kind.and_then(|kind| state.choices.get(kind)).map(str::to_owned),
        )
    };
    let gate = Gate::live();
    let reply = make(&core, target.as_deref(), Rc::clone(&gate)).map(|open| {
        let entry = Entry {
            kind,
            make,
            live: Held { gate, _open: open },
            pending: None,
        };
        state.borrow_mut().entries.insert(id, entry);
    });
    let _ = ready.send(reply);
}

/// Takes the user's choice of `kind` and moves the open streams of that kind when it changed.
fn choose(state: &mut State, kind: DeviceKind, name: Option<&str>) {
    if !state.choices.set(kind, name) {
        return;
    }
    let State { core, entries, .. } = state;
    for entry in entries.values_mut().filter(|entry| entry.kind == Some(kind)) {
        entry.replace(core, name);
    }
}
