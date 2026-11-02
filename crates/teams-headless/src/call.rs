//! `call` and `join`: one call through the engine (M3 Tasks 15 and 16). Once the calling socket is registered, the call
//! goes out (the test call of the echo service, a call from a chat, or a join of a group chat's running call once its
//! invitation rang out here unanswered), its events are printed as they come ([`event_line`]), and once its media
//! connected it is held for the time asked and hung up; a call that doesn't connect in time is hung up too. Every write
//! of it goes through the engine's outbox under a fresh `Fence::check`; nothing here answers or declines anything (the
//! one answering there is, `watch --answer-lab-calls`, is [`crate::commands::Answering`]).
//!
//! The call's media runs on the file backend the binary gives the engine (`teams-headless` opens no audio server): the
//! microphone plays [`crate::echo::keyed`]'s tone (or [`crate::echo::bursts`]), the speaker records what comes back,
//! and [`heard`] looks for the keyed tone in the recording afterwards ([`CallFiles::presence`] for any tone); the
//! devices note when their first samples start ([`crate::lab::TimedDevices`]), so two processes' files line up.

use std::borrow::Cow;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use teams_core::{CallAction, CallId, CallStatus, End, EngineHandle, Event, MeetingInfo, RegistrationReport};
use teams_fence::Mri;
use teams_media::devices::{SAMPLE_RATE, wav};
use teams_media::tone::Tone;

use crate::chat::LabNames;
use crate::commands::CommandError;
use crate::echo::{self, Echo, Presence};
use crate::lab::{Opened, SampleStarts, TimedDevices, VideoCounts};
use crate::print::{end_word, event_line, json_line, now_ms};

/// Whom a call goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The test call of the echo service with this MRI (lab.toml's `echo`).
    Echo(Mri),
    /// A call from this chat: its other member, or a group chat's other members as the store holds them.
    Chat(String),
    /// A join of the running call in this group chat, late: once an invitation to it rang here and its ring ended
    /// unanswered (the machine joins only a running call an invitation named).
    Join(String),
    /// The call of the meeting whose chat this is, started or not, as the calendar's Join goes into it.
    Meeting(String, MeetingInfo),
}

/// How long each part of a call may take or lasts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// How long the calling socket may take to register.
    pub register_within: Duration,
    /// For a join: how long the running call's invitation may take to ring here, and then its ring to end.
    pub invited_within: Duration,
    /// How long the call may take, from its placing, to connect its media.
    pub connect_within: Duration,
    /// How long the call is held once its media connected.
    pub hold: Duration,
    /// How long the hang-up may take to end the call.
    pub end_within: Duration,
    /// When the camera goes on, after the media connected (`None`: it stays off).
    pub camera_after: Option<Duration>,
}

/// How often a call's video counts are printed while it runs (`video_lines`).
pub const VIDEO_EVERY: Duration = Duration::from_secs(2);

/// What a call's output shows beside its events: the lab's names for the MRIs in them, and the call's video counts
/// (printed every [`VIDEO_EVERY`] while the call runs: `<ms> video_lines received=<line>:<frames>,…`, each line that
/// brought frames, counted since the call's media started) when it takes video.
#[derive(Clone, Copy)]
pub struct Showing<'a> {
    pub names: &'a LabNames,
    pub video: Option<&'a VideoCounts>,
}

impl<'a> Showing<'a> {
    /// The names alone: no video counts printed.
    #[must_use]
    pub const fn names(names: &'a LabNames) -> Self {
        Self { names, video: None }
    }
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
/// hangs it up and waits for its end, printing each event as it comes (each MRI by `showing`'s names, the video counts
/// [`Showing`] says) and a summary line at the end: `<ms> call_summary call=<id> setup_ms=<n> held=<bool>
/// end="<how>"`. `stop` (a signal) hangs the call up early.
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
    showing: Showing<'_>,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> Result<Report, CommandError> {
    let names = showing.names;
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
        Target::Join(chat) => {
            let rung = tokio::select! {
                rung = rung_out(handle, &chat, plan.invited_within, names, out) => rung?,
                () = &mut stop => {
                    report.interrupted = true;
                    return Ok(report);
                }
            };
            if !rung {
                writeln!(out, "{} no_running_call", now_ms())?;
                return Ok(report);
            }
            CallAction::Join { chat }
        }
        Target::Meeting(chat, meeting) => CallAction::JoinMeeting { chat, meeting },
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
        camera_at: None,
        video: showing.video,
        video_at: showing.video.map(|_| placed + VIDEO_EVERY),
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

/// Waits for an invitation to a running call in the group chat `chat` to ring here, and then for its ring to end,
/// answering nothing, each within `within`, printing the events until then: `true` once the ring ended (the call can
/// be joined), `false` when either didn't come in time.
async fn rung_out(
    handle: &mut EngineHandle,
    chat: &str,
    within: Duration,
    names: &LabNames,
    out: &mut impl Write,
) -> Result<bool, CommandError> {
    let mut ringing = None;
    let mut deadline = tokio::time::Instant::now() + within;
    loop {
        let Ok(event) = tokio::time::timeout_at(deadline, handle.next_event()).await else {
            return Ok(false);
        };
        let event = event.ok_or(CommandError::Stopped)?;
        print(out, &event, names)?;
        match event {
            Event::IncomingCall(incoming) if ringing.is_none() && incoming.group_chat.as_deref() == Some(chat) => {
                writeln!(out, "{} ring_let_go call={}", now_ms(), incoming.call.get())?;
                ringing = Some(incoming.call);
                deadline = tokio::time::Instant::now() + within;
            }
            Event::RingEnded { call, .. } if ringing == Some(call) => return Ok(true),
            _ => {}
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
    /// When the camera goes on, once the media connected.
    camera_at: Option<tokio::time::Instant>,
    /// The call's video counts, printed every [`VIDEO_EVERY`].
    video: Option<&'a VideoCounts>,
    /// When they are printed next.
    video_at: Option<tokio::time::Instant>,
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
                () = sleep_until(self.camera_at) => {
                    self.camera_at = None;
                    camera_on(self.handle.requests(), self.call, self.out).await?;
                    continue;
                }
                () = sleep_until(self.video_at) => {
                    self.video_lines()?;
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
                && self.status(status, report)
            {
                return Ok(());
            }
        }
    }

    /// The call is `status` now: held once its media connected, its end reported; `true` once it ended.
    fn status(&mut self, status: CallStatus, report: &mut Report) -> bool {
        match status {
            CallStatus::Connected if matches!(self.phase, Phase::Connecting(_)) => {
                let now = tokio::time::Instant::now();
                report.setup = Some(now - self.placed);
                self.phase = Phase::Held(now + self.plan.hold);
                self.camera_at = self.plan.camera_after.map(|after| now + after);
                false
            }
            CallStatus::Ended(end) => {
                report.end = Some(end);
                true
            }
            _ => false,
        }
    }

    /// Prints the video counts so far (when a line brought frames) and when they are printed next.
    fn video_lines(&mut self) -> Result<(), CommandError> {
        self.video_at = Some(tokio::time::Instant::now() + VIDEO_EVERY);
        if let Some(progress) = self.video.and_then(VideoCounts::progress) {
            writeln!(self.out, "{} video_lines {progress}", now_ms())?;
            self.out.flush()?;
        }
        Ok(())
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

/// Turns the camera of `call` on, printing what became of it: `<ms> camera_on call=<id>` or `camera_refused` with why.
///
/// # Errors
/// When the output can't be written.
pub async fn camera_on(
    requests: teams_core::EngineRequests,
    call: CallId,
    out: &mut impl Write,
) -> Result<(), CommandError> {
    let turned = requests.call(CallAction::Camera { call, on: true }).await;
    let at = now_ms();
    match turned {
        Ok(_) => writeln!(out, "{at} camera_on call={}", call.get())?,
        Err(error) => writeln!(
            out,
            "{at} camera_refused call={} why={}",
            call.get(),
            json_line(&Value::String(error.to_string()))
        )?,
    }
    out.flush()?;
    Ok(())
}

/// Ready at `at`; never without one.
pub async fn sleep_until(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

fn print(out: &mut impl Write, event: &Event, names: &LabNames) -> Result<(), CommandError> {
    writeln!(out, "{}", event_line(event, now_ms(), names))?;
    out.flush()?;
    Ok(())
}

/// The level the microphone's keyed tone peaks at.
pub const TONE_AMPLITUDE: f32 = 0.5;

/// How the microphone's tone is keyed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keying {
    /// On and off in 200 ms slots of [`echo::pattern`]: found again and timed by [`echo::find`].
    #[default]
    Keyed,
    /// In bursts of 1.6 s, 0.9 s apart ([`echo::bursts`]): what a lab browser's page hears as the tone.
    Bursts,
}

/// A call's audio files: the tone the microphone plays (written before the call) and the far end's recording, and the
/// devices on them, which note when they open.
#[derive(Clone, Debug)]
pub struct CallFiles {
    /// The tone, `<recording's stem>-sent.wav` beside the recording.
    pub sent: PathBuf,
    pub recording: PathBuf,
    /// How long the tone lasts (silence after it).
    pub length: Duration,
    pub keying: Keying,
    devices: TimedDevices,
}

impl CallFiles {
    /// Writes the tone at `hz`, keyed ([`echo::keyed`]), long enough for `plan`'s call, beside `recording`.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when it can't be written.
    pub fn write(recording: PathBuf, hz: f32, plan: &Plan) -> Result<Self, CommandError> {
        Self::write_for(recording, hz, Keying::Keyed, Self::length_for(plan))
    }

    /// How long a call's tone lasts under `plan`: the microphone opens before the media connects, so the whole call,
    /// and a little more.
    #[must_use]
    pub fn length_for(plan: &Plan) -> Duration {
        plan.connect_within + plan.hold + plan.end_within + Duration::from_secs(10)
    }

    /// Writes the tone at `hz`, keyed as `keying` says, for `length`, beside `recording`.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when it can't be written.
    pub fn write_for(recording: PathBuf, hz: f32, keying: Keying, length: Duration) -> Result<Self, CommandError> {
        let failed = |error: &dyn std::fmt::Display| CommandError::Recording(error.to_string());
        let stem = recording
            .file_stem()
            .ok_or_else(|| CommandError::Recording(format!("{} names no file", recording.display())))?
            .to_string_lossy()
            .into_owned();
        let sent = recording.with_file_name(format!("{stem}-sent.wav"));
        let tone = Tone {
            hz,
            amplitude: TONE_AMPLITUDE,
        };
        let samples = match keying {
            Keying::Keyed => echo::keyed(tone, length),
            Keying::Bursts => echo::bursts(tone, length),
        };
        let mut writer = wav::WavWriter::create(&sent).map_err(|error| failed(&error))?;
        writer.write(&samples).map_err(|error| failed(&error))?;
        writer.finish().map_err(|error| failed(&error))?;
        let devices = TimedDevices::new(crate::lab::call_devices(sent.clone(), recording.clone()));
        Ok(Self {
            sent,
            recording,
            length,
            keying,
            devices,
        })
    }

    /// The same files, `movie` the microphone and the camera instead of the tone and the test pattern: nothing is
    /// looked for in the recording then ([`CallFiles::plays_movie`]).
    #[must_use]
    pub fn with_movie(self, movie: teams_media::devices::movie::Movie) -> Self {
        Self {
            devices: self.devices.with_movie(movie),
            ..self
        }
    }

    /// Whether a movie is the microphone (the tone isn't played).
    #[must_use]
    pub fn plays_movie(&self) -> bool {
        self.devices.movie.is_some()
    }

    /// The call's devices on these files ([`crate::lab::call_devices`]), noting when they open.
    #[must_use]
    pub fn devices(&self) -> TimedDevices {
        self.devices.clone()
    }

    /// When the devices last opened.
    #[must_use]
    pub fn opened(&self) -> Opened {
        self.devices.opened()
    }

    /// When the microphone first returned audio and the speaker first accepted audio.
    #[must_use]
    pub fn sample_starts(&self) -> SampleStarts {
        self.devices.sample_starts()
    }

    /// How much of the tone at `hz` the recording holds, however it was keyed ([`echo::presence`]).
    ///
    /// # Errors
    /// [`CommandError::Recording`] when the recording can't be read.
    pub fn presence(&self, hz: f32) -> Result<Presence, CommandError> {
        let samples = wav::read(&self.recording).map_err(|error| CommandError::Recording(error.to_string()))?;
        Ok(echo::presence(hz, &samples))
    }

    /// The keyed tone at `hz` from a far end whose microphone may have started earlier ([`heard`]).
    /// Frequency does not identify a source: another headless peer may use the same tone.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when the recording can't be read.
    pub fn heard(&self, hz: f32) -> Result<Option<Echo>, CommandError> {
        heard(&self.recording, hz, self.length, self.length)
    }

    /// Our own keyed tone returned by the echo service, using the microphone's sample origin and causal offsets.
    /// The caller must know the far end is echoing this source; matching frequencies do not establish that.
    ///
    /// # Errors
    /// [`CommandError::Recording`] when the recording can't be read.
    pub fn heard_own_echo(&self, hz: f32) -> Result<Option<Echo>, CommandError> {
        let samples = wav::read(&self.recording).map_err(|error| CommandError::Recording(error.to_string()))?;
        let Some(aligned) = microphone_timeline(&samples, self.sample_starts(), self.length.saturating_mul(2)) else {
            return Ok(None);
        };
        // A short capture can accidentally match an unplayed later block of the long source file. Unlike another
        // process's recording, our own echo cannot precede the microphone: never grant it the generic 60 s lead.
        Ok(echo::find(hz, self.length, &aligned, self.length))
    }
}

/// The speaker's samples on the microphone's clock. A later speaker leaves its unrecorded prefix empty; an earlier
/// speaker's leading samples precede anything the microphone could have played. No valid arrival can start after
/// the last source sample plus the maximum delay, so a larger clock difference supplies no evidence.
fn microphone_timeline(samples: &[f32], started: SampleStarts, latest: Duration) -> Option<Cow<'_, [f32]>> {
    let microphone = started.microphone?.instant;
    let speaker = started.speaker?.instant;
    let count =
        |duration: Duration| usize::try_from(duration.as_nanos() * u128::from(SAMPLE_RATE) / 1_000_000_000).ok();
    match speaker.checked_duration_since(microphone) {
        Some(later) if later > latest => None,
        Some(later) if later.is_zero() => Some(Cow::Borrowed(samples)),
        Some(later) => {
            let mut aligned = vec![0.0; count(later)?];
            aligned.extend_from_slice(samples);
            Some(Cow::Owned(aligned))
        }
        None => Some(Cow::Borrowed(
            samples.get(count(microphone.duration_since(speaker))?..)?,
        )),
    }
}

/// How much earlier than the recording's own start a block of the far end's keyed tone may be found ([`heard`]): a
/// far end in another process opened its microphone before this side's speaker opened (a caller's opens at its start,
/// a callee's speaker at its answer), so what it sent first comes back "before" the recording.
pub const LEAD: Duration = Duration::from_mins(1);

/// The keyed tone at `hz` (the first `sent` of it) in the recording at `recording`, found from [`LEAD`] before its
/// start to at most `max_offset` after ([`echo::find_with_lead`]); `None` when it isn't there.
///
/// # Errors
/// When the recording can't be read.
pub fn heard(recording: &Path, hz: f32, sent: Duration, max_offset: Duration) -> Result<Option<Echo>, CommandError> {
    let samples = wav::read(recording).map_err(|error| CommandError::Recording(error.to_string()))?;
    Ok(echo::find_with_lead(hz, sent, &samples, max_offset, LEAD))
}

/// The tone's presence as printed: `tone hz=<n> heard=<bool> seconds=<n> first_ms=<n|-> peak=<n>`.
#[must_use]
pub fn tone_line(hz: f32, presence: &Presence) -> String {
    format!(
        "tone hz={hz} heard={} seconds={:.1} first_ms={} peak={:.3}",
        presence.heard(),
        presence.length().as_secs_f32(),
        presence
            .first
            .map_or_else(|| "-".to_owned(), |first| first.as_millis().to_string()),
        presence.peak
    )
}

/// When device construction began, in Unix ms. Sample origins for aligning recordings are printed separately by
/// [`samples_line`]; constructing a device can block before any audio runs.
#[must_use]
pub fn devices_line(opened: Opened) -> String {
    format!(
        "devices microphone_opened_ms={} speaker_opened_ms={}",
        wall_ms(opened.microphone),
        wall_ms(opened.speaker),
    )
}

/// The first source frame and sink write, in Unix ms. A block found at signed `offset` in another process's recording
/// arrived at `speaker_started + sent_at + offset`; its source sent it at `microphone_started + sent_at`.
#[must_use]
pub fn samples_line(started: SampleStarts) -> String {
    format!(
        "samples microphone_started_ms={} speaker_started_ms={}",
        wall_ms(started.microphone.map(|start| start.wall)),
        wall_ms(started.speaker.map(|start| start.wall)),
    )
}

fn wall_ms(at: Option<SystemTime>) -> String {
    at.and_then(|at| at.duration_since(UNIX_EPOCH).ok())
        .map_or_else(|| "-".to_owned(), |since| since.as_millis().to_string())
}

/// What was heard as printed: `echo heard=false`, or `echo heard=true level=<n> blocks=<n> sent_from_ms=<n>
/// sent_to_ms=<n> offsets_ms=<a,b,…> round_trips=<n> p50_ms=<n> p95_ms=<n> max_ms=<n>`.
#[must_use]
pub fn echo_line(echo: Option<&Echo>) -> String {
    let Some(echo) = echo else {
        return "echo heard=false".to_owned();
    };
    let ms = |duration: Option<Duration>| duration.map_or_else(|| "-".to_owned(), |d| d.as_millis().to_string());
    let signed =
        |duration: Option<Duration>| duration.map_or_else(|| "-".to_owned(), |d| echo.signed_ms(d).to_string());
    let (from, to) = echo.sent_span().unzip();
    let offsets: Vec<String> = echo
        .blocks
        .iter()
        .map(|block| echo.signed_ms(block.offset).to_string())
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
        signed(echo.raw_percentile(50)),
        signed(echo.raw_percentile(95)),
        signed(echo.delays.last().copied()),
    )
}

#[cfg(test)]
#[path = "../tests/support/call_timeline.rs"]
mod timeline_tests;
