//! One call as the machine keeps it: who the account is in it, its callbacks, the links its services handed out, where
//! it is in its life, and the writes of it still waiting for their outcome.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use teams_fence::{Delivery, Mri, Observed};
use teams_proto::{CallContext, CallLink, CallParticipant, Links};

use crate::agent::{CallAgent, Callback, Kind};
use crate::read::{Attached, Conversation, Invitation};
use crate::{CallId, CallState, CallWrite, End, WriteId};

/// The conversation's callbacks a start, a join or an `attach` hands out (`conversationRequest.links`, as captured).
pub(crate) const CONVERSATION_LINKS: [&str; 5] = [
    "conversationEnd",
    "conversationUpdate",
    "localParticipantUpdate",
    "addParticipantSuccess",
    "addParticipantFailure",
];

/// The call's callbacks a start or a join hands out (`callInvitation.links`).
pub(crate) const START_CALL_LINKS: [&str; 5] = ["progress", "mediaAnswer", "acceptance", "redirection", "end"];

/// The callbacks for the media controller (`clientContentForMediaController`).
pub(crate) const MEDIA_CONTROLLER_LINKS: [&str; 2] = ["controlVideoStreaming", "csrcInfo"];

/// The call's callbacks an acceptance's acknowledgement and a callee's `accept` hand out.
pub(crate) const ANSWERED_CALL_LINKS: [&str; 7] = [
    "mediaRenegotiation",
    "transfer",
    "replacement",
    "balanceUpdate",
    "retargetCompletion",
    "controlVideoStreaming",
    "updateMediaDescriptions",
];

/// Which way the call goes.
#[derive(Clone, Debug)]
pub(crate) enum Side {
    /// The account placed it: a chat's call, or the test call of the echo service (`Some`), added once accepted.
    Placed { echo: Option<Mri> },
    /// The account joined the running call at `conversation` in the group chat `chat`.
    Joined { chat: String, conversation: CallLink },
    /// The account was invited.
    Invited(Box<Invitation>),
}

/// Where the call is in its life (see [`crate::Calls`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// A start or a join sent, its answer awaited.
    Placing,
    /// A start answered: the callees ring, the acceptance awaited.
    Ringing,
    /// A join answered: the acceptance awaited.
    Joining,
    /// An invitation's receipt and `attach` sent, the `attach`'s answer awaited (nothing shown yet).
    Attaching,
    /// An invitation offered: it rings here, the account's answer or decline awaited.
    Offered,
    /// The account's `accept` sent.
    Answering,
    /// The account's decline sent.
    Declining,
    /// The call is up (signaling: the acceptance applied, or the `accept` answered).
    Connected,
    /// The account leaves: its `leave` sent (`sent`), or waiting for the conversation's `leave` link.
    Leaving { sent: bool, end: Ending },
}

/// Why the account leaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    HungUp,
    Failed(Step),
}

impl Ending {
    pub(crate) fn end(self) -> End {
        match self {
            Self::HungUp => End::HungUp,
            Self::Failed(step) => End::Failed(step.name()),
        }
    }
}

impl Phase {
    /// The state a caller is told, if this phase is one.
    pub(crate) fn state(self) -> Option<CallState> {
        match self {
            Self::Placing => Some(CallState::Placing),
            Self::Ringing => Some(CallState::Ringing),
            Self::Joining => Some(CallState::Joining),
            Self::Offered => Some(CallState::Offered),
            Self::Answering => Some(CallState::Answering),
            Self::Declining => Some(CallState::Declining),
            Self::Connected => Some(CallState::Connected),
            Self::Leaving { .. } => Some(CallState::Leaving),
            Self::Attaching => None,
        }
    }
}

/// A write the machine waits for the outcome of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Start,
    Join,
    Attach,
    Ringing,
    EndpointState,
    Accept,
    Decline,
    Acknowledge,
    EndpointMetadata,
    Add,
    Leave,
}

impl Step {
    /// The write's name ([`crate::CallWrite::name`]).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Join => "join",
            Self::Attach => "attach",
            Self::Ringing => "progress",
            Self::EndpointState => "update_endpoint_state",
            Self::Accept => "accept",
            Self::Decline => "reject",
            Self::Acknowledge => "acknowledge",
            Self::EndpointMetadata => "update_endpoint_metadata",
            Self::Add => "add",
            Self::Leave => "leave",
        }
    }
}

/// The keep-alive interval the call's services asked for, and when it next falls due.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeepAlive {
    pub(crate) interval: Duration,
    pub(crate) next: Instant,
}

/// The acknowledgement of a placed or joined call's acceptance: its delivery response went (or couldn't be built),
/// and the request that takes its place if it fails.
#[derive(Clone, Debug)]
pub(crate) struct Acknowledgement {
    pub(crate) delivery: Option<crate::DeliveryKey>,
    pub(crate) link: Option<teams_proto::CallLink>,
    pub(crate) links: Links,
    pub(crate) by_request: bool,
}

/// One call.
#[derive(Debug)]
pub(crate) struct Call {
    pub(crate) id: CallId,
    pub(crate) side: Side,
    pub(crate) phase: Phase,
    pub(crate) context: CallContext,
    /// The account in this call (its participant id fresh for the call).
    pub(crate) me: CallParticipant,
    agent: CallAgent,
    callbacks: Vec<Callback>,
    pub(crate) conversation: Conversation,
    /// An invitation's leg, from its `attach`'s answer.
    pub(crate) leg: Option<Attached>,
    pub(crate) acknowledgement: Option<Acknowledgement>,
    pub(crate) keep_alive: Option<KeepAlive>,
    pub(crate) writes: BTreeMap<WriteId, Step>,
    /// Writes that go one after another, each once the write before it (`held_after`) was answered `2xx`.
    pub(crate) held: VecDeque<(Step, CallWrite)>,
    pub(crate) held_after: Option<WriteId>,
    /// An acceptance delivered before the start's or the join's answer, applied once that answer came.
    pub(crate) early_acceptance: Option<(Delivery, Observed)>,
    pub(crate) started: Instant,
}

impl Call {
    pub(crate) fn new(
        id: CallId,
        side: Side,
        context: CallContext,
        me: CallParticipant,
        agent: CallAgent,
        started: Instant,
    ) -> Self {
        let phase = match side {
            Side::Invited(_) => Phase::Attaching,
            Side::Placed { .. } | Side::Joined { .. } => Phase::Placing,
        };
        Self {
            id,
            side,
            phase,
            context,
            me,
            agent,
            callbacks: Vec::new(),
            conversation: Conversation::default(),
            leg: None,
            acknowledgement: None,
            keep_alive: None,
            writes: BTreeMap::new(),
            held: VecDeque::new(),
            held_after: None,
            early_acceptance: None,
            started,
        }
    }

    /// A new callback of this call.
    pub(crate) fn callback(&mut self, kind: Kind, name: &'static str) -> Result<String, getrandom::Error> {
        let callback = self.agent.callback(kind, name)?;
        let url = callback.url.clone();
        self.callbacks.push(callback);
        Ok(url)
    }

    /// New callbacks `kind`/`names` of this call, as a request's links.
    pub(crate) fn links(&mut self, kind: Kind, names: &[&'static str]) -> Result<Links, getrandom::Error> {
        names.iter().try_fold(Links::new(), |links, name| {
            Ok(links.with(*name, self.callback(kind, name)?))
        })
    }

    /// The callback a delivery at `path` came to, if it is one of this call's.
    pub(crate) fn callback_at(&self, path: &str) -> Option<(Kind, &'static str)> {
        self.callbacks
            .iter()
            .find(|callback| callback.path == path)
            .map(|callback| (callback.kind, callback.name))
    }

    /// The path of this call's callback `kind`/`name` (the first made).
    #[cfg(test)]
    pub(crate) fn path_of(&self, kind: Kind, name: &str) -> Option<&str> {
        self.callbacks
            .iter()
            .find(|callback| callback.kind == kind && callback.name == name)
            .map(|callback| callback.path.as_str())
    }

    /// The invitation this call answers, if it was invited.
    pub(crate) fn invitation(&self) -> Option<&Invitation> {
        match &self.side {
            Side::Invited(invitation) => Some(invitation),
            Side::Placed { .. } | Side::Joined { .. } => None,
        }
    }

    /// The running group call this call is in, as an invitation named it: its group chat and its conversation.
    pub(crate) fn running(&self) -> Option<(&str, &CallLink)> {
        match &self.side {
            Side::Joined { chat, conversation } => Some((chat, conversation)),
            Side::Invited(invitation) => invitation
                .group_chat
                .as_deref()
                .map(|chat| (chat, &invitation.conversation)),
            Side::Placed { .. } => None,
        }
    }

    /// Whether the call is in a group chat (its modalities are a group call's).
    pub(crate) fn in_group(&self, group_chat: Option<&str>) -> bool {
        group_chat.is_some()
            || self
                .invitation()
                .is_some_and(|invitation| invitation.group_chat.is_some())
    }

    /// Whether a `leave` may go for the call now: it has its conversation's `leave` link, and the account is in it
    /// (placed and answered by its services, or answered by the account).
    pub(crate) fn may_leave(&self) -> bool {
        let in_it = match self.phase {
            Phase::Ringing | Phase::Joining | Phase::Answering | Phase::Connected => true,
            Phase::Leaving { sent, .. } => !sent,
            Phase::Placing | Phase::Attaching | Phase::Offered | Phase::Declining => false,
        };
        in_it && self.conversation.leave.is_some()
    }

    /// How long it has rung, in whole seconds (a cancel names it).
    pub(crate) fn rang_seconds(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.started).as_secs()
    }
}
