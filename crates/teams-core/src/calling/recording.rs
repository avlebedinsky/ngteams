//! A recording the account starts or stops (More → Record and transcribe, `docs/protocol/calling.md`, "More of a
//! running call"): Microsoft's recording bot is invited into the call when it isn't there yet (as live captions invite
//! it), then its command link is told to start recording and transcription, or to stop them. Each write is one
//! attempt, never resent: a start whose outcome is unknown is told as failed, and the roster shows whether it records
//! ([`crate::MeetingState::recording`]). Nothing here enters the persistent outbox.
use std::future::pending;
use std::time::{Duration, Instant};

use teams_proto::captions::{CaptionWrite, RecordStart};
use teams_proto::{Credentials, HttpError, Transport};

use super::{CallActionError, CallId, CallTried, Calling};
use crate::engine::Event;

/// How long the recording bot may take to join before a start is told failed.
const JOIN_WITHIN: Duration = Duration::from_secs(60);

/// What the account's own recording action is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecordingPhase {
    /// Nothing asked, or what was asked is done (the roster tells whether it records).
    #[default]
    Idle,
    /// Start recording asked: the bot being invited, or its start sent.
    Starting,
    /// Stop recording asked and sent.
    Stopping,
}

/// Why the account's start or stop didn't happen, in Teams' words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordingProblem {
    pub heading: &'static str,
    pub detail: &'static str,
}

/// The account's recording action in its call ([`Event::CallRecording`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallRecording {
    pub phase: RecordingPhase,
    /// The last start or stop that failed, until the next is asked.
    pub problem: Option<RecordingProblem>,
}

const START_FAILED: &str = "Couldn't start recording";
const NOT_FOR_THIS_CALL: RecordingProblem = RecordingProblem {
    heading: START_FAILED,
    detail: "Recording cannot be started for this call.",
};
const SOMETHING_WRONG: RecordingProblem = RecordingProblem {
    heading: START_FAILED,
    detail: "Something went wrong. Try to start recording again.",
};
const POLICY: RecordingProblem = RecordingProblem {
    heading: START_FAILED,
    detail: "Recording not started. Your IT admin has disabled Recording.",
};
const PENDING: RecordingProblem = RecordingProblem {
    heading: START_FAILED,
    detail: "Recording couldn't be started because another recording is pending.",
};
const STOP_FAILED: RecordingProblem = RecordingProblem {
    heading: "Couldn't stop recording and transcription.",
    detail: "Recording cannot be stopped for this call.",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    Invite,
    Start,
    Stop,
}

#[derive(Default)]
pub(super) struct Recordings {
    call: Option<CallId>,
    view: CallRecording,
    pub(super) writing: Option<(Operation, super::Attempt)>,
    /// The start asked, until it is sent.
    start: Option<RecordStart>,
    deadline: Option<Instant>,
}

pub(super) async fn next(writing: Option<&mut (Operation, super::Attempt)>) -> (Operation, CallTried) {
    match writing {
        Some((operation, attempt)) => (*operation, attempt.await),
        None => pending().await,
    }
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    fn record_emit(&self) {
        if let Some(call) = self.recording.call {
            self.emit(Event::CallRecording {
                call,
                recording: self.recording.view,
            });
        }
    }

    /// Starts recording and transcription as `start` names it, or stops them (`None`).
    pub(super) fn record(&mut self, call: CallId, start: Option<RecordStart>) -> Result<CallId, CallActionError> {
        if self.up != Some(call)
            || (self.recording.call == Some(call) && self.recording.view.phase != RecordingPhase::Idle)
        {
            return Err(teams_call::CallError::NotNow.into());
        }
        let Some(start) = start else {
            let write = self.calls.record_command(call, None)?;
            self.recording = Recordings {
                call: Some(call),
                view: CallRecording {
                    phase: RecordingPhase::Stopping,
                    problem: None,
                },
                ..Recordings::default()
            };
            self.record_send(Operation::Stop, write);
            self.record_emit();
            return Ok(call);
        };
        self.recording = Recordings {
            call: Some(call),
            view: CallRecording {
                phase: RecordingPhase::Starting,
                problem: None,
            },
            start: Some(start),
            deadline: Some(Instant::now() + JOIN_WITHIN),
            ..Recordings::default()
        };
        self.record_emit();
        if self.calls.caption_service(call).is_some() {
            self.record_service_changed(call);
        } else if self.captions.writing.is_none() {
            match self.calls.caption_invite(call) {
                Ok(write) => self.record_send(Operation::Invite, write),
                Err(_) => self.record_fail(NOT_FOR_THIS_CALL),
            }
        }
        // else live captions are inviting the bot already: its roster entry starts the recording
        Ok(call)
    }

    fn record_send(&mut self, operation: Operation, write: CaptionWrite) {
        let outbox = self.outbox.clone();
        self.recording.writing = Some((operation, Box::pin(async move { outbox.try_caption(write).await })));
    }

    /// The recording bot's roster entry changed: a start waiting for it is sent.
    pub(super) fn record_service_changed(&mut self, call: CallId) {
        if self.recording.call != Some(call)
            || self.recording.view.phase != RecordingPhase::Starting
            || self.recording.writing.is_some()
            || self.calls.caption_service(call).is_none()
        {
            return;
        }
        let Some(start) = self.recording.start.take() else {
            return;
        };
        match self.calls.record_command(call, Some(&start)) {
            Ok(write) => self.record_send(Operation::Start, write),
            Err(_) => self.record_fail(NOT_FOR_THIS_CALL),
        }
    }

    pub(super) fn record_written(&mut self, operation: Operation, tried: &CallTried) {
        self.recording.writing = None;
        let status = match tried {
            CallTried::Answered(answer) => Some(answer.status()),
            _ => None,
        };
        if !status.is_some_and(|status| (200..300).contains(&status)) {
            let problem = match (operation, status) {
                (Operation::Stop, _) => STOP_FAILED,
                (Operation::Invite, _) => NOT_FOR_THIS_CALL,
                (Operation::Start, Some(401 | 403)) => POLICY,
                (Operation::Start, Some(409)) => PENDING,
                (Operation::Start, _) => SOMETHING_WRONG,
            };
            self.record_fail(problem);
            return;
        }
        match operation {
            Operation::Invite => {
                // its roster entry may have come before the answer
                if let Some(call) = self.recording.call {
                    self.record_service_changed(call);
                }
            }
            Operation::Start | Operation::Stop => {
                self.recording.deadline = None;
                self.recording.view.phase = RecordingPhase::Idle;
                self.record_emit();
            }
        }
    }

    /// The recording bot couldn't join (its `addParticipantFailure`): a start waiting for it fails.
    pub(super) fn record_join_failed(&mut self, call: CallId) {
        if self.recording.call == Some(call) && self.recording.view.phase == RecordingPhase::Starting {
            self.record_fail(NOT_FOR_THIS_CALL);
        }
    }

    fn record_fail(&mut self, problem: RecordingProblem) {
        self.recording.writing = None;
        self.recording.start = None;
        self.recording.deadline = None;
        self.recording.view = CallRecording {
            phase: RecordingPhase::Idle,
            problem: Some(problem),
        };
        self.record_emit();
    }

    pub(super) fn record_check(&mut self, now: Instant) {
        if self.recording.writing.is_none() && self.recording.deadline.is_some_and(|deadline| deadline <= now) {
            self.record_fail(SOMETHING_WRONG);
        }
    }

    pub(super) fn record_ended(&mut self, call: CallId) {
        if self.recording.call == Some(call) {
            self.recording = Recordings::default();
        }
    }
}
