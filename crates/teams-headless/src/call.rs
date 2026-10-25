//! `call`: one call through the engine (M3 Task 15). Once the calling socket is registered, the call goes out (the test
//! call of the echo service, or a call from a chat), its events are printed as they come ([`event_line`]), and once its
//! media connected it is held for the time asked and hung up; a call that doesn't connect in time is hung up too.
//! Every write of it goes through the engine's outbox under a fresh `Fence::check`; nothing here answers or declines
//! anything.
//!
//! The call's media runs on the file backend the binary gives the engine (`teams-headless` opens no audio server): the
//! microphone plays [`crate::echo::keyed`]'s tone, the speaker records what comes back, and [`heard`] looks for that
//! tone in the recording afterwards.

use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use teams_core::{CallAction, CallId, CallStatus, End, EngineHandle, Event, RegistrationReport};
use teams_fence::Mri;
use teams_media::devices::file::FileDevices;
use teams_media::devices::wav;
use teams_media::tone::Tone;

use crate::chat::LabNames;
use crate::commands::CommandError;
use crate::echo::{self, Echo};
use crate::print::{end_word, event_line, json_line, now_ms};

/// Whom a call goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The test call of the echo service with this MRI (lab.toml's `echo`).
    Echo(Mri),
    /// A call from this chat: its other member, or a group chat's other members as the store holds them.
    Chat(String),
}

/// How long each part of a call may take or lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// How long the calling socket may take to register.
    pub register_within: Duration,
    /// How long the call may take, from its placing, to connect its media.
    pub connect_within: Duration,
    /// How long the call is held once its media connected.
    pub hold: Duration,
    /// How long the hang-up may take to end the call.
    pub end_within: Duration,
}

/// How a call went.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The call, once the engine took it.
    pub call: Option<CallId>,
    /// From the action to the media connected.
    pub setup: Option<Duration>,
    /// It was held the whole time asked, then hung up.
    pub held: bool,
    /// How it ended, if it ended in time.
    pub end: Option<End>,
    /// A signal cut it short.
    pub interrupted: bool,
}

impl Report {
    /// Whether the call went as asked: connected, held, hung up by us and ended so.
    #[must_use]
    pub fn completed(&self) -> bool {
        self.held && self.end == Some(End::HungUp) && !self.interrupted
    }
}

/// Places the call to `target` once the calling socket is registered, holds it `plan.hold` once its media connected,
/// hangs it up and waits for its end, printing each event as it comes and a summary line at the end: `<ms>
/// call_summary call=<id> setup_ms=<n> held=<bool> end="<how>"`. `stop` (a signal) hangs the call up early.
///
/// A signal, or `plan.connect_within` running out, while the engine still sets the call's media up ends the wait with
/// no call in the report (`interrupted`, or `call_not_connected_in_time` printed); the engine's stop then answers the
/// placing without placing it.
///
/// # Errors
/// [`CommandError::NotRegistered`] when the calling socket didn't register in time, the engine's refusal of the call,
/// or when the output can't be written.
pub async fn run(
    handle: &mut EngineHandle,
    target: Target,
    plan: &Plan,
    names: &LabNames,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> Result<Report, CommandError> {
    let mut report = Report::default();
    tokio::pin!(stop);
    tokio::select! {
        registered = registered(handle, plan.register_within, names, out) => registered?,
        () = &mut stop => {
            report.interrupted = true;
            return Ok(report);
        }
    }
    let action = match target {
        Target::Echo(echo) => CallAction::TestCall { echo },
        Target::Chat(chat) => CallAction::Call { chat },
    };
    let placed = tokio::time::Instant::now();
    writeln!(out, "{} call_placing", now_ms())?;
    // the engine answers once the call's media is set up (the relay's credentials, an allocation): a signal or the
    // time to connect running out meanwhile stops waiting, and the engine's stop answers the placing `Stopped`
    // without placing it
    let call = tokio::select! {
        call = handle.requests().call(action) => call?,
        () = &mut stop => {
            report.interrupted = true;
            return Ok(report);
        }
        () = tokio::time::sleep_until(placed + plan.connect_within) => {
            writeln!(out, "{} call_not_connected_in_time", now_ms())?;
            return Ok(report);
        }
    };
    report.call = Some(call);
    let following = Following {
        handle,
        call,
        placed,
        plan,
        names,
        out: &mut *out,
        phase: Phase::Connecting(placed + plan.connect_within),
    };
    following.run(stop, &mut report).await?;
    let setup = report
        .setup
        .map_or_else(|| "-".to_owned(), |setup| setup.as_millis().to_string());
    let end = report.end.as_ref().map_or_else(|| "-".to_owned(), end_word);
    writeln!(
        out,
        "{} call_summary call={} setup_ms={setup} held={} end={}",
        now_ms(),
        call.get(),
        report.held,
        json_line(&Value::String(end))
    )?;
    Ok(report)
}

/// Waits for the calling socket's registration, printing the events until then.
async fn registered(
    handle: &mut EngineHandle,
    within: Duration,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let event = tokio::time::timeout_at(deadline, handle.next_event())
            .await
            .map_err(|_| CommandError::NotRegistered)?
            .ok_or(CommandError::Stopped)?;
        print(out, &event, names)?;
        if matches!(event, Event::CallingRegistration(RegistrationReport::Registered)) {
            return Ok(());
        }
    }
}

/// Where a call is, as it is followed.
#[derive(Clone, Copy)]
enum Phase {
    /// Until its media connects, by this time.
    Connecting(tokio::time::Instant),
    /// Held until this time.
    Held(tokio::time::Instant),
    /// Hung up; its end comes by this time.
    Ending(tokio::time::Instant),
}

impl Phase {
    fn until(self) -> tokio::time::Instant {
        match self {
            Self::Connecting(until) | Self::Held(until) | Self::Ending(until) => until,
        }
    }
}

/// A call followed to its end (see [`run`]).
struct Following<'a, W> {
    handle: &'a mut EngineHandle,
    call: CallId,
    placed: tokio::time::Instant,
    plan: &'a Plan,
    names: &'a LabNames,
    out: &'a mut W,
    phase: Phase,
}

impl<W: Write> Following<'_, W> {
    /// Follows the call until it ended, or until its end didn't come in time after the hang-up.
    async fn run(mut self, stop: impl Future<Output = ()>, report: &mut Report) -> Result<(), CommandError> {
        tokio::pin!(stop);
        loop {
            let event = tokio::select! {
                event = self.handle.next_event() => event.ok_or(CommandError::Stopped)?,
                () = tokio::time::sleep_until(self.phase.until()) => {
                    if !self.deadline(report).await? {
                        return Ok(());
                    }
                    continue;
                }
                () = &mut stop, if !report.interrupted => {
                    report.interrupted = true;
                    if !matches!(self.phase, Phase::Ending(_)) {
                        self.hang_up().await;
                    }
                    continue;
                }
            };
            print(self.out, &event, self.names)?;
            if let Event::Call { call, status } = event
                && call == self.call
            {
                match status {
                    CallStatus::Connected if matches!(self.phase, Phase::Connecting(_)) => {
                        let now = tokio::time::Instant::now();
                        report.setup = Some(now - self.placed);
                        self.phase = Phase::Held(now + self.plan.hold);
                    }
                    CallStatus::Ended(end) => {
                        report.end = Some(end);
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }

    /// The phase's time is up: the call is hung up (held long enough, or not connected in time); `false` once its end
    /// didn't come in time after that.
    async fn deadline(&mut self, report: &mut Report) -> Result<bool, CommandError> {
        match self.phase {
            Phase::Held(_) => report.held = true,
            Phase::Connecting(_) => writeln!(self.out, "{} call_not_connected_in_time", now_ms())?,
            Phase::Ending(_) => {
                writeln!(self.out, "{} call_not_ended_in_time", now_ms())?;
                return Ok(false);
            }
        }
        self.hang_up().await;
        Ok(true)
    }

    /// Hangs the call up; its end is waited for from now on.
    async fn hang_up(&mut self) {
        let hang_up = CallAction::HangUp { call: self.call };
        if let Err(error) = self.handle.requests().call(hang_up).await {
            // a call that ended meanwhile has nothing to hang up: its end is on its way
            log::info!("the hang-up was refused: {error}");
        }
        self.phase = Phase::Ending(tokio::time::Instant::now() + self.plan.end_within);
    }
}

fn print(out: &mut impl Write, event: &Event, names: &LabNames) -> Result<(), CommandError> {
    writeln!(out, "{}", event_line(event, now_ms(), names))?;
    out.flush()?;
    Ok(())
}

/// The level the microphone's keyed tone peaks at.
pub const TONE_AMPLITUDE: f32 = 0.5;

/// A call's audio files: the keyed tone the microphone plays (written before the call) and the far end's recording.
#[derive(Clone, Debug, PartialEq)]
pub struct CallFiles {
    /// The keyed tone, `<recording's stem>-sent.wav` beside the recording.
    pub sent: PathBuf,
    pub recording: PathBuf,
    /// How long the keyed tone lasts (silence after it).
    pub length: Duration,
}

impl CallFiles {
    /// Writes the tone at `hz`, keyed ([`echo::keyed`]), long enough for `plan`'s call, beside `recording`.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when it can't be written.
    pub fn write(recording: PathBuf, hz: f32, plan: &Plan) -> Result<Self, CommandError> {
        let failed = |error: &dyn std::fmt::Display| CommandError::Recording(error.to_string());
        let stem = recording
            .file_stem()
            .ok_or_else(|| CommandError::Recording(format!("{} names no file", recording.display())))?
            .to_string_lossy()
            .into_owned();
        let sent = recording.with_file_name(format!("{stem}-sent.wav"));
        // the microphone opens before the media connects: the tone lasts the whole call, and a little more
        let length = plan.connect_within + plan.hold + plan.end_within + Duration::from_secs(10);
        let tone = Tone {
            hz,
            amplitude: TONE_AMPLITUDE,
        };
        let mut writer = wav::WavWriter::create(&sent).map_err(|error| failed(&error))?;
        writer
            .write(&echo::keyed(tone, length))
            .map_err(|error| failed(&error))?;
        writer.finish().map_err(|error| failed(&error))?;
        Ok(Self {
            sent,
            recording,
            length,
        })
    }

    /// The call's devices on these files ([`crate::lab::call_devices`]).
    #[must_use]
    pub fn devices(&self) -> FileDevices {
        crate::lab::call_devices(self.sent.clone(), self.recording.clone())
    }

    /// The keyed tone at `hz` in the recording ([`echo::find`]), at any offset the call's length allows; `None` when
    /// it isn't there.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when the recording can't be read.
    pub fn heard(&self, hz: f32) -> Result<Option<Echo>, CommandError> {
        heard(&self.recording, hz, self.length, self.length)
    }
}

/// The keyed tone at `hz` (the first `sent` of it) in the recording at `recording`, found at most `max_offset` later
/// ([`echo::find`]); `None` when it isn't there.
///
/// # Errors
/// When the recording can't be read.
pub fn heard(recording: &Path, hz: f32, sent: Duration, max_offset: Duration) -> Result<Option<Echo>, CommandError> {
    let samples = wav::read(recording).map_err(|error| CommandError::Recording(error.to_string()))?;
    Ok(echo::find(hz, sent, &samples, max_offset))
}

/// What was heard as printed: `echo heard=false`, or `echo heard=true level=<n> blocks=<n> sent_from_ms=<n>
/// sent_to_ms=<n> offsets_ms=<a,b,…> round_trips=<n> p50_ms=<n> p95_ms=<n> max_ms=<n>`.
#[must_use]
pub fn echo_line(echo: Option<&Echo>) -> String {
    let Some(echo) = echo else {
        return "echo heard=false".to_owned();
    };
    let ms = |duration: Option<Duration>| duration.map_or_else(|| "-".to_owned(), |d| d.as_millis().to_string());
    let (from, to) = echo.sent_span().unzip();
    let offsets: Vec<String> = echo
        .blocks
        .iter()
        .map(|block| block.offset.as_millis().to_string())
        .collect();
    format!(
        "echo heard=true level={:.3} blocks={} sent_from_ms={} sent_to_ms={} offsets_ms={} round_trips={} p50_ms={} \
         p95_ms={} max_ms={}",
        echo.level(),
        echo.blocks.len(),
        ms(from),
        ms(to),
        offsets.join(","),
        echo.delays.len(),
        ms(echo.delay_percentile(50)),
        ms(echo.delay_percentile(95)),
        ms(echo.delays.last().copied()),
    )
}
