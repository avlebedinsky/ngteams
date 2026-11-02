//! `teams-headless`: one lab account's native client from the command line (see the library's docs).

use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use teams_core::{Action, EngineHandle, SearchScope};
use teams_headless::cli::{CallAudio, Cli, Command, WATCH_TONE_SECONDS};
use teams_headless::commands::{self, Answering, WatchEnd};
#[cfg(not(test))]
use teams_headless::lab::Lab;
use teams_headless::lab::{self, VideoCounts};
use teams_headless::print::{now_ms, visible};
use teams_headless::{availability, call, chat, echo};
use teams_proto::chat::MessageText;
use teams_proto::{join_url_chat, join_url_meeting};
use teams_store::OutboxState;
use teams_trouter::Stop;
#[cfg(not(test))]
use tokio::signal::unix::{Signal, SignalKind, signal};

#[cfg(test)]
#[path = "../tests/main/mod.rs"]
mod tests;
#[cfg(test)]
use tests::{Signals, TestLab as Lab};

/// How long `availability` and `call` wait for their socket's registration.
const REGISTER_WAIT: Duration = Duration::from_secs(90);

/// How long a call's hang-up may take to end it.
const CALL_END_WAIT: Duration = Duration::from_secs(15);

/// The exit code after SIGINT or SIGTERM cut a one-shot command short.
const INTERRUPTED: u8 = 130;

type Outcome = Result<ExitCode, Box<dyn Error>>;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if teams_fence::test_support_enabled() {
        // cargo unified a dev-dependency's feature into this build (`cargo test`, `--all-targets`, `--all-features`):
        // its fence API would take any roster and any member list, and its transports a mock origin
        eprintln!(
            "teams-headless: built with teams-fence's test-support feature; rebuild with `cargo build -p teams-headless`"
        );
        return ExitCode::FAILURE;
    }
    if teams_fence::personal_enabled() {
        // the personal build's fence came in with `ngteams` built in the same cargo invocation: a lab binary never
        // has it
        eprintln!(
            "teams-headless: built with teams-fence's personal feature (a build with `ngteams`); rebuild with \
             `cargo build -p teams-headless`"
        );
        return ExitCode::FAILURE;
    }
    teams_headless::debug_log::init_from_env();
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("teams-headless: can't start the async runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(error) => {
            // an error can carry what the service sent (a conversation id, its message)
            eprintln!("teams-headless: {}", visible(&error.to_string()));
            ExitCode::FAILURE
        }
    }
}

/// SIGINT and SIGTERM, taken over before anything is set up, so a signal ends a command between its steps.
#[cfg(not(test))]
struct Signals {
    interrupt: Signal,
    terminate: Signal,
}

#[cfg(not(test))]
impl Signals {
    fn new() -> io::Result<Self> {
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
        })
    }

    async fn next(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {}
            _ = self.terminate.recv() => {}
        }
    }
}

async fn run(cli: Cli) -> Outcome {
    let mut signals = Signals::new()?;
    let lab = Lab::load(cli.alias)?;
    let mut out = io::stdout().lock();
    dispatch(&lab, cli.command, &mut signals, &mut out).await
}

/// Runs the parsed command on the account already loaded above. Tests supply only a loopback account here; the
/// runnable binary always gets its concrete lab from `run`, after the build's refusal checks in `main`.
async fn dispatch(lab: &Lab, command: Command, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    match command {
        Command::List { limit } => {
            commands::list(&lab.open_store()?, limit, &lab.names, out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Show { chat, limit } => {
            let conversation = conversation(lab, &chat)?;
            commands::show(&lab.open_store()?, &conversation, limit, &lab.names, out)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Sync => sync(lab, signals, out).await,
        command @ (Command::Watch { .. }
        | Command::Call { .. }
        | Command::Join { .. }
        | Command::JoinMeeting { .. }) => calling_command(lab, command, signals, out).await,
        Command::Presence { wait, mris } => presence(lab, &mris, Duration::from_secs(wait), signals, out).await,
        Command::Profile { mris, wait } => profile(lab, &mris, Duration::from_secs(wait), signals, out).await,
        Command::Search { query } => search(lab, &query, out).await,
        Command::TeamReads { team, group } => team_reads(lab, &team, &group, out).await,
        Command::Series { keep, cleanup } => {
            let handle = lab.start_engine(false).await?;
            let probed = tokio::select! {
                probed = teams_headless::series::probe(&handle, now_ms(), keep, cleanup, out) => Some(probed),
                () = signals.next() => None,
            };
            let code = |fine: bool| if fine { ExitCode::SUCCESS } else { ExitCode::FAILURE };
            finish(handle, probed.map(|probed| probed.map(code)))
        }
        Command::Availability {
            manual,
            manual_after,
            reset_after,
            hold,
        } => {
            let plan = availability::Plan {
                manual: manual.map(|status| (status.into(), Duration::from_secs(manual_after))),
                reset_after: reset_after.map(Duration::from_secs),
                hold: Duration::from_secs(hold),
                register_wait: REGISTER_WAIT,
            };
            own_availability(lab, plan, signals, out).await
        }
        command => {
            let (action, wait) = write_action(lab, command)?;
            one_write(lab, action, wait, signals, out).await
        }
    }
}

/// `watch` (with or without its calling), `call` and `join`.
async fn calling_command(lab: &Lab, command: Command, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    match command {
        Command::Watch {
            json,
            answer_lab_calls,
            calls,
            audio,
        } => {
            if answer_lab_calls || calls {
                let answering = match (answer_lab_calls, audio.camera_after()) {
                    (true, Some(after)) => Answering::LabCallsWithCamera(Duration::from_secs(after)),
                    (true, None) => Answering::LabCalls,
                    (false, _) => Answering::Nothing,
                };
                let watching = (json, answering);
                watch_calls(lab, watching, audio, signals, out).await
            } else {
                watch(lab, json, signals, out).await
            }
        }
        Command::Call {
            target,
            audio,
            seconds,
            wait,
        } => {
            let target = if target == "echo" {
                call::Target::Echo(lab.echo()?)
            } else {
                call::Target::Chat(conversation(lab, &target)?)
            };
            let plan = call_plan(seconds, wait, 0, audio.camera_after());
            let asked = CallAsked { target, audio, plan };
            place_call(lab, asked, signals, out).await
        }
        Command::Join {
            chat,
            audio,
            seconds,
            wait,
            ring_wait,
        } => {
            let plan = call_plan(seconds, wait, ring_wait, audio.camera_after());
            let asked = CallAsked {
                target: call::Target::Join(conversation(lab, &chat)?),
                audio,
                plan,
            };
            place_call(lab, asked, signals, out).await
        }
        Command::JoinMeeting {
            link,
            audio,
            seconds,
            wait,
        } => {
            let (Some(chat), Some(meeting)) = (join_url_chat(&link), join_url_meeting(&link)) else {
                return Err("that isn't a meeting's join link naming its chat, organizer and tenant".into());
            };
            let plan = call_plan(seconds, wait, 0, audio.camera_after());
            let asked = CallAsked {
                target: call::Target::Meeting(chat, meeting),
                audio,
                plan,
            };
            place_call(lab, asked, signals, out).await
        }
        _ => Err("not a calling command".into()),
    }
}

/// A catch-up through the engine ([`commands::sync`]).
async fn sync(lab: &Lab, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let handle = lab.start_engine(false).await?;
    let synced = tokio::select! {
        synced = commands::sync(&handle, out) => Some(synced),
        () = signals.next() => None,
    };
    finish(handle, synced.map(|synced| synced.map(|_| ExitCode::SUCCESS)))
}

/// One write through the engine ([`write`]).
async fn one_write(lab: &Lab, action: Action, wait: Duration, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let mut handle = lab.start_engine(false).await?;
    let written = tokio::select! {
        written = write(&mut handle, action, wait, out) => Some(written),
        () = signals.next() => None,
    };
    finish(handle, written)
}

/// The other lab accounts' presence and `others`', read once through the engine ([`commands::presence`]).
async fn presence(
    lab: &Lab,
    others: &[String],
    wait: Duration,
    signals: &mut Signals,
    out: &mut impl Write,
) -> Outcome {
    let mut whom: Vec<String> = teams_fence::lab::Alias::ALL
        .into_iter()
        .filter(|alias| *alias != lab.alias)
        .filter_map(|alias| lab.names.mri(alias).map(str::to_owned))
        .collect();
    for mri in others {
        if !whom.contains(mri) {
            whom.push(mri.clone());
        }
    }
    let mut handle = lab.start_engine(false).await?;
    let read = tokio::select! {
        read = commands::presence(&mut handle, &whom, wait, &lab.names, out) => Some(read),
        () = signals.next() => None,
    };
    finish(handle, read.map(|read| read.map(|()| ExitCode::SUCCESS)))
}

/// `whom`'s names as the engine looks them up ([`commands::profile`]).
async fn profile(lab: &Lab, whom: &[String], wait: Duration, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let mut handle = lab.start_engine(false).await?;
    let looked_up = tokio::select! {
        looked_up = commands::profile(&mut handle, whom, wait, &lab.names, out) => Some(looked_up),
        () = signals.next() => None,
    };
    finish(handle, looked_up.map(|looked_up| looked_up.map(|()| ExitCode::SUCCESS)))
}

/// The service's search for `query` (`teams-headless search`): the suggestions, then the results of every kind, each
/// kind's count printed, and the chats the messages found are in by id.
async fn search(lab: &Lab, query: &str, out: &mut impl Write) -> Outcome {
    let handle = lab.start_engine(false).await?;
    let mut failed = false;
    for (name, scope) in [("suggestions", SearchScope::Suggestions), ("all", SearchScope::All)] {
        match handle.search_service(query, scope).await {
            Ok(found) => {
                for (kind, count) in [
                    ("messages", found.messages.len()),
                    ("people", found.people.len()),
                    ("files", found.files.len()),
                    ("chats", found.chats.len()),
                    ("channels", found.channels.len()),
                ] {
                    writeln!(out, "search {name} {kind} {count}")?;
                }
                for message in &found.messages {
                    writeln!(out, "search {name} message-in {}", message.conversation)?;
                }
            }
            Err(error) => {
                writeln!(out, "search {name} failed {}", error.kind())?;
                failed = true;
            }
        }
    }
    let code = if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS };
    finish(handle, Some(Ok::<_, io::Error>(code)))
}

/// The conversation a command's chat argument names for the lab account.
fn conversation(lab: &Lab, chat: &str) -> Result<String, chat::ChatError> {
    chat::resolve(chat, lab.alias, &lab.names, lab.abc.as_deref())
}

/// A write command's action, and how long to wait for it to settle.
fn write_action(lab: &Lab, command: Command) -> Result<(Action, Duration), Box<dyn Error>> {
    let (action, wait) = match command {
        Command::Send { chat, text, wait } => (
            Action::Send {
                conversation: conversation(lab, &chat)?,
                text: MessageText::plain(&text)?,
            },
            wait,
        ),
        Command::Edit {
            chat,
            message,
            text,
            wait,
        } => (
            Action::Edit {
                conversation: conversation(lab, &chat)?,
                message_id: message,
                text: MessageText::plain(&text)?,
            },
            wait,
        ),
        Command::Delete { chat, message, wait } => (
            Action::Delete {
                conversation: conversation(lab, &chat)?,
                message_id: message,
            },
            wait,
        ),
        Command::React {
            chat,
            message,
            key,
            remove,
            wait,
        } => {
            let conversation = conversation(lab, &chat)?;
            let action = if remove {
                Action::Unreact {
                    conversation,
                    message_id: message,
                    key,
                }
            } else {
                Action::React {
                    conversation,
                    message_id: message,
                    key,
                }
            };
            (action, wait)
        }
        Command::Read { chat, message, wait } => (
            Action::Read {
                conversation: conversation(lab, &chat)?,
                message_id: message,
            },
            wait,
        ),
        Command::Sync
        | Command::List { .. }
        | Command::Show { .. }
        | Command::Watch { .. }
        | Command::Presence { .. }
        | Command::Profile { .. }
        | Command::Search { .. }
        | Command::TeamReads { .. }
        | Command::Series { .. }
        | Command::Availability { .. }
        | Command::Call { .. }
        | Command::Join { .. }
        | Command::JoinMeeting { .. } => {
            return Err("not a write".into());
        }
    };
    Ok((action, Duration::from_secs(wait)))
}

/// What the team pages read of a team, each once.
async fn team_reads(lab: &Lab, team: &str, group: &str, out: &mut impl Write) -> Outcome {
    let handle = lab.start_engine(false).await?;
    let read = commands::team_reads(&handle, team, group, out).await;
    finish(handle, Some(read.map(|()| ExitCode::SUCCESS)))
}

/// A write after a catch-up (its summary on stderr, the write's lines on stdout): the store has the latest state its
/// checks read (an edit only of the account's own message, …). Exits non-zero unless it went through.
async fn write(handle: &mut EngineHandle, action: Action, wait: Duration, out: &mut impl Write) -> Outcome {
    commands::sync(handle, &mut io::stderr()).await?;
    let state = commands::write(handle, action, wait, out).await?;
    Ok(if state == OutboxState::Sent {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The push channel's events until a signal: exits 0 on the signal, non-zero when the channel stopped on its own.
async fn watch(lab: &Lab, json: bool, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let mut handle = lab.start_engine(true).await?;
    let end = commands::watch(&mut handle, json, Answering::Nothing, &lab.names, out, signals.next()).await;
    handle.shutdown();
    watch_outcome(end?)
}

/// How `watch` exits: 0 on a signal, non-zero when the channel or the engine stopped on its own.
fn watch_outcome(end: WatchEnd) -> Outcome {
    match end {
        WatchEnd::Stopped | WatchEnd::PushStopped(Stop::Requested) => Ok(ExitCode::SUCCESS),
        WatchEnd::PushStopped(why) => Err(format!("the push channel stopped: {why:?}").into()),
        WatchEnd::EngineStopped => Err("the engine stopped".into()),
    }
}

/// The account's own availability from its push channel's endpoint ([`availability::run`]): exits 0 when every step
/// went through or a signal ended it, non-zero otherwise.
async fn own_availability(lab: &Lab, plan: availability::Plan, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let mut handle = lab.start_engine(true).await?;
    let end = availability::run(&mut handle, plan, out, signals.next()).await;
    handle.shutdown();
    match end? {
        availability::End::Done { all_sent: true } | availability::End::Stopped => Ok(ExitCode::SUCCESS),
        availability::End::Done { all_sent: false } => Err("a step didn't go through".into()),
        availability::End::NotRegistered => Err("no registration went through: nothing was set".into()),
        availability::End::PushStopped(why) => Err(format!("the push channel stopped: {why:?}").into()),
        availability::End::EngineStopped => Err("the engine stopped".into()),
    }
}

/// A call's or a join's plan: held `seconds` once connected, connected within `wait` of its placing, a join's
/// invitation within `ring_wait`, the camera on `camera_after` seconds after it connected.
fn call_plan(seconds: u64, wait: u64, ring_wait: u64, camera_after: Option<u64>) -> call::Plan {
    call::Plan {
        register_within: REGISTER_WAIT,
        invited_within: Duration::from_secs(ring_wait),
        connect_within: Duration::from_secs(wait),
        hold: Duration::from_secs(seconds),
        end_within: CALL_END_WAIT,
        camera_after: camera_after.map(Duration::from_secs),
    }
}

/// A call as the command line asks for it.
struct CallAsked {
    target: call::Target,
    audio: CallAudio,
    plan: call::Plan,
}

/// The call's files: the tone written as `audio` asks, for `length`, beside the recording (`audio.record`, or
/// `<data_dir>/recordings/<alias>-<what>-<ms>.wav`); with `--media`, the movie as the microphone and the camera,
/// probed first and its `media` line printed.
fn call_files(
    lab: &Lab,
    audio: &CallAudio,
    (what, length): (&str, Duration),
    out: &mut impl Write,
) -> Result<call::CallFiles, Box<dyn Error>> {
    let recording = match &audio.record {
        Some(path) => path.clone(),
        None => lab
            .recordings_dir()?
            .join(format!("{}-{what}-{}.wav", lab.alias.as_str(), now_ms())),
    };
    let keying = if audio.bursts {
        call::Keying::Bursts
    } else {
        call::Keying::Keyed
    };
    let files = call::CallFiles::write_for(recording, audio.tone, keying, length)?;
    let Some(path) = &audio.media else {
        return Ok(files);
    };
    let movie = lab::call_movie(path.clone(), audio.media_from, audio.media_loop)?;
    writeln!(
        out,
        "{}",
        lab::media_line(path, audio.media_from, audio.media_loop, &movie)
    )?;
    Ok(files.with_movie(movie))
}

/// What the recording holds of each tone `audio` asks to hear (its own tone when it names none): the keyed tone's
/// blocks and round trips, and the tone's presence however it was keyed; and when the devices opened. What the first
/// one's keyed tone came back as.
fn heard_lines(
    files: &call::CallFiles,
    audio: &CallAudio,
    own_echo: bool,
    out: &mut impl Write,
) -> Result<Option<echo::Echo>, Box<dyn Error>> {
    writeln!(out, "recording {}", visible(&files.recording.display().to_string()))?;
    writeln!(out, "{}", call::devices_line(files.opened()))?;
    writeln!(out, "{}", call::samples_line(files.sample_starts()))?;
    if files.opened().speaker.is_none() {
        // the call ended before its media was set up: nothing was recorded
        writeln!(out, "echo heard=false")?;
        return Ok(None);
    }
    if files.plays_movie() {
        // the microphone played the movie, not the tone: there is no tone to look for
        writeln!(out, "echo not_applicable media=true")?;
        return Ok(None);
    }
    let hear = if audio.hear.is_empty() {
        vec![audio.tone]
    } else {
        audio.hear.clone()
    };
    let mut first = None;
    for (index, hz) in hear.into_iter().enumerate() {
        let heard = if own_echo {
            files.heard_own_echo(hz)?
        } else {
            files.heard(hz)?
        };
        writeln!(out, "{}", call::echo_line(heard.as_ref()))?;
        writeln!(out, "{}", call::tone_line(hz, &files.presence(hz)?))?;
        if index == 0 {
            first = heard;
        }
    }
    Ok(first)
}

/// `watch --calls` or `watch --answer-lab-calls`: the push channel's events and the calling stack's until a signal,
/// each call the engine offers answered with [`Answering::LabCalls`] (and none without), its media on files (`audio`,
/// the tone [`WATCH_TONE_SECONDS`] long unless it says otherwise); then what the last answered call's recording holds.
/// Exits as `watch` does.
async fn watch_calls(
    lab: &Lab,
    (json, answering): (bool, Answering),
    audio: CallAudio,
    signals: &mut Signals,
    out: &mut impl Write,
) -> Outcome {
    let length = Duration::from_secs(audio.tone_seconds.unwrap_or(WATCH_TONE_SECONDS));
    let files = call_files(lab, &audio, ("watch", length), out)?;
    let (video, counts) = video_of(&audio);
    let mut handle = lab.start_calling(files.devices(), true, video).await?;
    let end = commands::watch(&mut handle, json, answering, &lab.names, out, signals.next()).await;
    // the engine's stop ends a call still up and closes its media, and with it the recording
    handle.shutdown();
    let end = end?;
    if files.opened().speaker.is_some() {
        heard_lines(&files, &audio, false, out)?;
    }
    if let Some(counts) = counts {
        writeln!(out, "{}", counts.line())?;
    }
    watch_outcome(end)
}

/// One call ([`call::run`]), its media on files: the tone written first, the engine started with its calling, a
/// catch-up before a call from a chat (a group chat's members as the store holds them), the call, the engine stopped
/// (its recording closed), then what the recording holds of the tone at `hear`. Exits 0 only when the call went as
/// asked and, for the echo, the keyed tone came back.
async fn place_call(lab: &Lab, asked: CallAsked, signals: &mut Signals, out: &mut impl Write) -> Outcome {
    let CallAsked { target, audio, plan } = asked;
    let what = if matches!(target, call::Target::Join(_) | call::Target::Meeting(..)) {
        "join"
    } else {
        "call"
    };
    let length = audio
        .tone_seconds
        .map_or_else(|| call::CallFiles::length_for(&plan), Duration::from_secs);
    let files = call_files(lab, &audio, (what, length), out)?;
    let (video, counts) = video_of(&audio);
    let mut handle = lab.start_calling(files.devices(), false, video).await?;
    if !matches!(target, call::Target::Echo(_)) {
        let mut stderr = io::stderr();
        let synced = tokio::select! {
            synced = commands::sync(&handle, &mut stderr) => Some(synced),
            () = signals.next() => None,
        };
        match synced {
            Some(Ok(_)) => {}
            synced => return finish(handle, synced.map(|synced| synced.map(|_| ExitCode::SUCCESS))),
        }
    }
    let echo = matches!(target, call::Target::Echo(_));
    let showing = call::Showing {
        names: &lab.names,
        video: counts.as_ref(),
    };
    let report = call::run(&mut handle, target, &plan, showing, out, signals.next()).await;
    // the engine's stop closes the call's media, and with it the recording
    handle.shutdown();
    let report = report?;
    if report.call.is_none() {
        writeln!(out, "recording {}", visible(&files.recording.display().to_string()))?;
        // a signal before the call was placed, its media not set up within `--wait`, or no running call to join
        if !report.interrupted {
            return Ok(ExitCode::FAILURE);
        }
        eprintln!("teams-headless: interrupted");
        return Ok(ExitCode::from(INTERRUPTED));
    }
    let heard = heard_lines(&files, &audio, echo, out)?;
    if let Some(counts) = counts {
        writeln!(out, "{}", counts.line())?;
    }
    Ok(
        if report.completed() && (!echo || heard.is_some() || files.plays_movie()) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        },
    )
}

/// The call's video when `audio` asks for it (`--video`), and the counts of what it shows. Without the flag, none.
fn video_of(audio: &CallAudio) -> (Option<teams_core::CallVideo>, Option<VideoCounts>) {
    if !audio.takes_video() {
        return (None, None);
    }
    let counts = VideoCounts::default();
    let video = teams_core::lab::call_video(Arc::new(counts.clone()));
    (Some(video), Some(counts))
}

/// Stops the engine (after the request it is on) and gives the command's outcome; `None` when a signal cut it short.
fn finish<E: Into<Box<dyn Error>>>(handle: EngineHandle, outcome: Option<Result<ExitCode, E>>) -> Outcome {
    handle.shutdown();
    if let Some(outcome) = outcome {
        return outcome.map_err(Into::into);
    }
    eprintln!("teams-headless: interrupted");
    Ok(ExitCode::from(INTERRUPTED))
}
