//! Devices added to the sandbox's PipeWire while it runs, and removed again: a microphone playing a sound of its own
//! (an `Audio/Source`: a steady tone, or given samples over and over, each buffer of those marked on a [`Timeline`]),
//! or a speaker recorded to a file of its own (an `Audio/Sink`), each a stream of the helper's thread that the session
//! manager takes for a device. A node's `priority.session` above the sandbox's own devices'
//! ([`super::DEVICE_PRIORITY`]) makes it the system's default as it comes, as a headset plugged in does; below, it is
//! only there to be chosen. Dropping its [`AddedNode`] removes it, as a device unplugged.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use pipewire::core::CoreRc;
use pipewire::properties::properties;
use pipewire::spa::utils::Direction;
use pipewire::stream::{StreamFlags, StreamRc, StreamState};
use teams_media::devices::wav::WavWriter;
use teams_media::tone::{Tone, ToneGenerator};

use super::media::{self, Command, Open, as_pod, audio_format, text, to_usize};
use super::timeline::{Timeline, now_us};
use crate::SandboxResult;

/// How long the helper's thread gets to add or remove a node.
const NODE_WAIT: Duration = Duration::from_secs(10);

/// A device to add: its node's name, what a person reads, and its priority as a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeSpec {
    pub name: String,
    pub description: String,
    pub priority: u32,
}

/// What an added microphone plays.
pub enum Sound {
    /// A steady tone.
    Tone(Tone),
    /// These samples, over and over from the first; each buffer marked on the timeline (its first sample counted
    /// from the node's start).
    Looped(Arc<[f32]>, Timeline),
}

/// What a node added does.
pub(super) enum Added {
    /// Plays its sound.
    Microphone(NodeSpec, Sound),
    /// Records what it is given to `recording`.
    Speaker(NodeSpec, PathBuf),
}

impl Added {
    pub(super) fn name(&self) -> &str {
        match self {
            Self::Microphone(spec, _) | Self::Speaker(spec, _) => &spec.name,
        }
    }
}

/// A node added to the sandbox's PipeWire; removed when dropped (or by [`AddedNode::remove`]).
pub struct AddedNode {
    name: String,
    recording: Option<PathBuf>,
    timeline: Option<Timeline>,
    commands: pipewire::channel::Sender<Command>,
    removed: bool,
}

impl AddedNode {
    /// Asks the helper's thread to add `added` and waits until it is there.
    pub(super) fn add(commands: pipewire::channel::Sender<Command>, added: Added) -> SandboxResult<Self> {
        let name = added.name().to_owned();
        let recording = match &added {
            Added::Speaker(_, path) => Some(path.clone()),
            Added::Microphone(..) => None,
        };
        let timeline = match &added {
            Added::Microphone(_, Sound::Looped(_, timeline)) => Some(timeline.clone()),
            Added::Microphone(..) | Added::Speaker(..) => None,
        };
        let (done, answer) = mpsc::channel();
        commands
            .send(Command::Add(added, done))
            .map_err(|_| "the sandbox's media thread has stopped")?;
        answer
            .recv_timeout(NODE_WAIT)
            .map_err(|_| format!("{name} wasn't added in time"))??;
        Ok(Self {
            name,
            recording,
            timeline,
            commands,
            removed: false,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A speaker's recording: 16-bit mono WAV at 48 kHz, brought up to date every second and when it is removed.
    #[must_use]
    pub fn recording(&self) -> Option<&Path> {
        self.recording.as_deref()
    }

    /// When a looped microphone's samples were played ([`Sound::Looped`]); `None` for any other node.
    #[must_use]
    pub fn timeline(&self) -> Option<&Timeline> {
        self.timeline.as_ref()
    }

    /// Removes the node, as a device unplugged, and waits until it is gone.
    ///
    /// # Errors
    /// The helper's thread has stopped or didn't answer in time.
    pub fn remove(mut self) -> SandboxResult {
        self.removed = true;
        let (done, gone) = mpsc::channel();
        self.commands
            .send(Command::Remove(self.name.clone(), done))
            .map_err(|_| "the sandbox's media thread has stopped")?;
        gone.recv_timeout(NODE_WAIT)
            .map_err(|_| format!("{} wasn't removed in time", self.name))?;
        Ok(())
    }
}

impl Drop for AddedNode {
    fn drop(&mut self) {
        if !self.removed {
            let (done, gone) = mpsc::channel();
            if self.commands.send(Command::Remove(self.name.clone(), done)).is_ok() {
                let _ = gone.recv_timeout(NODE_WAIT);
            }
        }
    }
}

/// Makes the node on the helper's thread.
pub(super) fn add(core: &CoreRc, added: Added) -> Result<Box<dyn Any>, String> {
    match added {
        Added::Microphone(spec, sound) => microphone(core, &spec, sound),
        Added::Speaker(spec, recording) => speaker(core, &spec, &recording),
    }
}

/// A stream the session manager takes for a device of `class`.
fn device_stream(core: &CoreRc, spec: &NodeSpec, class: &str) -> Result<StreamRc, String> {
    let priority = spec.priority.to_string();
    let props = properties! {
        *pipewire::keys::MEDIA_TYPE => "Audio",
        *pipewire::keys::MEDIA_CLASS => class,
        *pipewire::keys::NODE_NAME => spec.name.as_str(),
        *pipewire::keys::NODE_DESCRIPTION => spec.description.as_str(),
        *pipewire::keys::PRIORITY_SESSION => priority.as_str(),
    };
    StreamRc::new(core.clone(), &spec.name, props).map_err(text)
}

fn connect(stream: &StreamRc, direction: Direction) -> Result<(), String> {
    let format = audio_format()?;
    stream
        .connect(direction, None, StreamFlags::MAP_BUFFERS, &mut [as_pod(&format)?])
        .map_err(text)
}

/// What an added microphone's stream plays, and how many samples it played so far.
struct Playing {
    source: Source,
    played: u64,
}

enum Source {
    Tone(ToneGenerator),
    Looped {
        samples: Arc<[f32]>,
        at: usize,
        timeline: Timeline,
    },
}

impl Playing {
    fn new(sound: Sound) -> Self {
        let source = match sound {
            Sound::Tone(tone) => Source::Tone(ToneGenerator::new(tone)),
            Sound::Looped(samples, timeline) => Source::Looped {
                samples,
                at: 0,
                timeline,
            },
        };
        Self { source, played: 0 }
    }

    /// The next `out.len()` samples, marked on a looped sound's timeline as played now.
    fn fill(&mut self, out: &mut [f32]) {
        match &mut self.source {
            Source::Tone(tone) => tone.fill(out),
            Source::Looped { samples, at, timeline } => {
                timeline.mark(self.played, now_us());
                for sample in out.iter_mut() {
                    *sample = samples.get(*at).copied().unwrap_or(0.0);
                    *at = if *at + 1 >= samples.len() { 0 } else { *at + 1 };
                }
            }
        }
        self.played += u64::try_from(out.len()).unwrap_or(0);
    }
}

fn microphone(core: &CoreRc, spec: &NodeSpec, sound: Sound) -> Result<Box<dyn Any>, String> {
    let stream = device_stream(core, spec, "Audio/Source")?;
    let listener = stream
        .add_local_listener_with_user_data(Playing::new(sound))
        .process(|stream, tone| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = usize::try_from(buffer.requested()).unwrap_or(usize::MAX);
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(bytes) = data.data() else {
                return;
            };
            let count = if requested == 0 {
                bytes.len() / 4
            } else {
                requested.min(bytes.len() / 4)
            };
            let mut samples = vec![0.0; count];
            tone.fill(&mut samples);
            for (slot, sample) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(&samples) {
                slot.copy_from_slice(&sample.to_le_bytes());
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = 4;
            *chunk.size_mut() = u32::try_from(count * 4).unwrap_or(0);
        })
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Output)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

fn speaker(core: &CoreRc, spec: &NodeSpec, recording: &Path) -> Result<Box<dyn Any>, String> {
    let stream = device_stream(core, spec, "Audio/Sink")?;
    let writer = WavWriter::create(recording).map_err(text)?;
    let listener = stream
        .add_local_listener_with_user_data(Some(writer))
        .process(|stream, writer| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (to_usize(data.chunk().offset()), to_usize(data.chunk().size()));
            let bytes = data.data().unwrap_or_default();
            let end = offset.saturating_add(size).min(bytes.len());
            let (samples, _) = bytes.get(offset..end).unwrap_or_default().as_chunks::<4>();
            let samples: Vec<f32> = samples.iter().map(|sample| f32::from_le_bytes(*sample)).collect();
            if let Some(file) = writer
                && file.write(&samples).is_err()
            {
                *writer = None;
            }
        })
        .state_changed(|_, _, _, new| {
            if let StreamState::Error(error) = new {
                log::warn!("an added speaker failed: {error}");
            }
        })
        .register()
        .map_err(text)?;
    connect(&stream, Direction::Input)?;
    Ok(Box::new(Open {
        _listener: listener,
        _stream: stream,
    }))
}

/// The helper's way in, for [`super::PrivatePipeWire`]'s `add_*`.
pub(super) fn commands_of(helper: Option<&media::MediaHelper>) -> SandboxResult<pipewire::channel::Sender<Command>> {
    Ok(helper.ok_or("the sandbox's PipeWire has no media thread")?.commands())
}
