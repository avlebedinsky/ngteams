//! The native client's core: what the user asks for goes out through the [`outbox`], every attempt under a fresh
//! decision of the fence; what comes back is matched to the outbox by its `clientmessageid` and kept in the account's
//! `teams-store`.
//!
//! - [`outbox`]: commands as outbox rows with a `clientmessageid` of their own, each attempt a fresh `Fence::check`,
//!   the outbox's part of the plan's retry table, our own messages matched to their rows when they come back, and no
//!   resend across runs.
//! - [`sync`]: the catch-up sync, on start and after every reconnect of the push channel: the conversation list and
//!   each changed conversation's messages read through the service's own sync links and stored once, through the
//!   outbox's [`Outbox::receive`]; it fills the outbox's [`ConversationSync`] seam.
//!
//! - [`engine`]: one account's client on a tokio runtime and a thread of its own, reached through channels: actions
//!   go out through the outbox, the push channel's deliveries and the catch-up come into the store, and a catch-up
//!   runs after every reconnect. One engine per account (its lock); its request side is a cloneable handle of its own.
//! - [`profiles`]: people's names and pictures, read in the background (Graph, else the middle tier's profile lookup)
//!   and kept in the store.
//! - [`own_presence`]: the app's policy for the account's own presence: which own-availability write to send when
//!   (M2 Task 20); it sends nothing itself.
//! - [`presence`]: others' presence, read from UPS by polling for the people the views asked about; never stored.
//! - [`search`]: local search: each message's plain text handed to the store's index as it is stored, and the
//!   messages stored before indexed by a backfill in batches.
//! - [`service_search`]: the service's search (Substrate), read when the title bar's search asks: its suggestions and
//!   its results of every kind; nothing kept.
//! - [`emoticons`]: reactions' and messages' emoticons' animations, read from Teams' CDN when a view asks for one and
//!   kept for the run.
//! - [`pictures`]: pictures pasted into messages, read from the account's AMS base when a view shows one, kept for
//!   the run and cached on disk beside the store, so a later run shows them without reading them again.
//! - [`teams`]: the account's teams and their channels, read from CSA in the background and kept in the store;
//!   read-only, as a channel's posts are.
//! - [`calling`]: the account's calls (M3 Task 14): the call session joining the signaling machine (`teams-call`), the
//!   calling stack's socket, the outbox's call kind and the call's media (`teams-media`).
//! - [`account`]: one account's engine, assembled the one way every binary builds it (its session over the secret
//!   store the binary hands in, the fence and the member read it opened); [`lab`] gives it a lab account's parts, the
//!   personal build (`crates/ngteams`) its own.
//!
//! M1 Tasks 13, 17 and 18 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`); M2 Task 5
//! (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod account;
pub mod calendar;
pub mod calling;
pub mod channels;
pub mod emoticons;
pub mod engine;
pub mod file_content;
pub mod lab;
pub mod outbox;
pub mod own_presence;
pub mod pictures;
pub mod presence;
pub mod profiles;
pub mod search;
pub mod service_search;
pub mod sync;
pub mod teams;
pub mod thumbnails;

pub use calendar::{EventWrite, EventWritten, Invitable, MeetingAvailability};
pub use calling::{
    CallAction, CallActionError, CallMedia, CallParticipant, CallParts, CallStatus, CallVideo, CallView, GROUP_SLOTS,
    IncomingCall, Relay, VideoOutput,
};
pub use channels::{ChannelDraft, ChannelMade, ChannelProblem};
pub use emoticons::Strip;
pub use engine::{
    Action, Answer, Cause, EngineError, EngineHandle, EngineOptions, EngineRequests, Event, MessageChange, OwnStatus,
    Parts, Push, Reply, Source,
};
pub use file_content::FileStream;
pub use outbox::{
    Arrival, CHECK_RETRY_WAIT, CallRetries, CallTried, Clock, Command, ConversationSync, DueFailed, Lane, MAX_ATTEMPTS,
    MAX_FAILED_CHECKS, MAX_TRANSIENT_WAIT, Outbox, OutboxError, Received, Sent, SentDue, SyncError, Transient,
    TransientSent, UnreadableSeen, Unsynced, create_chat, enqueue, new_client_message_id,
};
pub use own_presence::{AVAILABLE_REFRESH, Choice, OwnPresencePolicy, Reporting};
pub use pictures::PictureBytes;
pub use presence::subscription::{SUBSCRIPTION_PRUNE, SUBSCRIPTION_RENEWAL, SUBSCRIPTION_RETRY};
pub use presence::{
    Availability, MAX_NAMED, MAX_WATCHED, PRESENCE_MIN_GAP, PRESENCE_POLL, PRESENCE_POLL_SUBSCRIBED, has_presence,
};
pub use profiles::{MAX_LOOKUPS, NameFrom, PROFILE_MAX_AGE, PROFILE_RETRY, Person};
pub use search::{Backfill, INDEX_BATCH, search_text};
pub use service_search::{SearchAnswer, SearchScope};
pub use sync::{
    CLOCK_SKEW_MS, CatchUp, CatchUpError, CatchUpReport, Changes, ConversationFailed, DRAFTS, Gaps, MAX_PAGES, NOTES,
    StoredMessage, Synced,
};
pub use teams::{ChatListing, READING_TEAMS, TEAMS_MAX_AGE, TEAMS_RETRY, TeamsReadError};
/// A call, how it ended and how its services ended it, as [`Event::Call`] and [`Event::RingEnded`] tell them.
pub use teams_call::{CallError, CallId, End, Identity, RemoteEnd};
pub use teams_proto::{
    Attendee, CalendarEvent, EventAttendee, EventDraft, EventKind, EventResponse, EventShowAs, EventTime, FloatingTime,
    FullEvent, MeetingInfo, Organizer, ShowAs, ZonedTime, join_url_chat,
};
/// How a registration of the push channel's endpoint went, as [`Event::Registration`] tells it.
pub use teams_trouter::RegistrationReport;
