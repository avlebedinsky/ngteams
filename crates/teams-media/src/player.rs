//! A video file played inline in a chat, as Teams' card plays a video a message carries, its bytes read part by part
//! from a [`ByteSource`] (the engine's ranged reads, in the app).
//!
//! - **GStreamer plays it**: `playbin` (the system's GStreamer and plugins) demultiplexes, decodes, keeps sound and
//!   picture in step, seeks and mutes. It reads the file from an `appsrc` fed by [`source`]'s blocks (read ahead of
//!   what plays, so it rarely waits on the network), never from the network itself: the elements that would fetch
//!   anything (an adaptive demuxer a playlist's bytes would plug, a network source) are never plugged, and one that is
//!   made anyway fails the player before it starts. Its frames go to an appsink as BGRA and its sound to another as
//!   48 kHz mono, which plays them on the device seam's speaker ([`crate::devices`], the PipeWire the build allows),
//!   never on a sink of GStreamer's own. Hardware video decoders are left out too: making one probes the GPU's drivers,
//!   which can take seconds.
//! - **Its thread**: a [`Player`] runs on a thread of its own and is told what to do by [`Command`]s; it tells its
//!   [`PlayerOutput`] each frame to show and its [`Status`]. Dropped, it stops: its blocks' cache is closed at once, so
//!   a read waiting for the network gives up and the pipeline's threads end.
//! - A format no installed plugin plays, or a file that isn't a video, is [`Failure::Unsupported`] or
//!   [`Failure::NoCodec`]; a file that can't be had, [`Failure::Unavailable`]; one whose parts fail to arrive,
//!   [`Failure::Network`].

mod run;
pub mod source;

use std::sync::Arc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::devices::{Devices, VideoFrame};

pub use source::{ByteSource, Cache, SourceError};

/// What a player is made with: the devices whose speaker plays the sound (none: it plays silently).
#[derive(Clone, Default)]
pub struct PlayerSetup {
    pub speaker: Option<Arc<dyn Devices + Send + Sync>>,
}

impl std::fmt::Debug for PlayerSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayerSetup")
            .field("speaker", &self.speaker.is_some())
            .finish()
    }
}

/// Why a file can't be played.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The file can't be had (the account can't read it, or Graph gave no link).
    Unavailable,
    /// Its parts failed to arrive (the network).
    Network,
    /// Not a video the player plays (not a format GStreamer reads, or a file that doesn't add up).
    Unsupported,
    /// No decoder for its codec among the installed GStreamer plugins (or no GStreamer).
    NoCodec,
    /// The player couldn't start (its thread wasn't made).
    Internal,
}

/// What the player is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Opening the file, until its first frame.
    Loading,
    Paused,
    Playing,
    /// Playing, but waiting for a part of the file.
    Buffering,
    /// Played to its end.
    Ended,
    Failed(Failure),
}

/// The player's state as its output is told it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub position: Duration,
    /// The movie's length (zero until it was opened).
    pub duration: Duration,
    /// Whether its sound is played on a speaker.
    pub sound: bool,
}

/// What the player is told.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    Play,
    Pause,
    /// Goes to this time (kept within the movie).
    Seek(Duration),
    Muted(bool),
}

/// Where a player's frames and status go; called on GStreamer's and the player's threads.
pub trait PlayerOutput: Send + Sync {
    /// A frame to show now (BGRA, opaque).
    fn frame(&self, frame: VideoFrame);
    /// The player's status changed (or its position moved on).
    fn status(&self, status: Status);
}

/// A video file playing (see the module docs). Dropped, its thread stops its pipeline (not waited for).
pub struct Player {
    commands: mpsc::Sender<Command>,
    cache: Arc<Cache>,
    _thread: JoinHandle<()>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

impl Player {
    /// Opens the file `source` reads on a thread of its own, played once opened when `play`, else paused on its first
    /// frame; `None` when the thread can't be made (the output was told [`Failure::Internal`]).
    #[must_use]
    pub fn open(
        source: Arc<dyn ByteSource>,
        setup: PlayerSetup,
        output: Arc<dyn PlayerOutput>,
        play: bool,
    ) -> Option<Self> {
        let (commands, received) = mpsc::channel();
        let cache = Arc::new(Cache::new(source));
        let (theirs, told) = (Arc::clone(&cache), Arc::clone(&output));
        let spawned = thread::Builder::new()
            .name("video-player".into())
            .spawn(move || run::run(theirs, &setup, &output, &received, play));
        match spawned {
            Ok(thread) => Some(Self {
                commands,
                cache,
                _thread: thread,
            }),
            Err(_) => {
                cache.close();
                told.status(Status {
                    state: State::Failed(Failure::Internal),
                    position: Duration::ZERO,
                    duration: Duration::ZERO,
                    sound: false,
                });
                None
            }
        }
    }

    /// Tells the player `command`.
    pub fn send(&self, command: Command) {
        // a player whose thread ended takes nothing more
        let _ = self.commands.send(command);
    }
}

impl Drop for Player {
    /// Closes the cache (a read waiting for the network gives up), and the thread sees its commands end.
    fn drop(&mut self) {
        self.cache.close();
    }
}
