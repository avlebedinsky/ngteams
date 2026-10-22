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
//! - [`presence`]: others' presence, read from UPS by polling for the people the views asked about; never stored.
//! - [`search`]: local search: each message's plain text handed to the store's index as it is stored, and the
//!   messages stored before indexed by a backfill in batches.
//! - [`teams`]: the account's teams and their channels, read from CSA in the background and kept in the store;
//!   read-only, as a channel's posts are.
//! - [`lab`]: a lab account's engine, assembled as every lab binary builds it (its session over the secret store the
//!   binary hands in).
//!
//! M1 Tasks 13, 17 and 18 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`); M2 Task 5
//! (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`).

pub mod engine;
pub mod lab;
pub mod outbox;
pub mod presence;
pub mod profiles;
pub mod search;
pub mod sync;
pub mod teams;

pub use engine::{
    Action, Answer, Cause, EngineError, EngineHandle, EngineOptions, EngineRequests, Event, MessageChange, Parts, Push,
    Reply, Source,
};
pub use outbox::{
    Arrival, CHECK_RETRY_WAIT, Clock, Command, ConversationSync, DueFailed, Lane, MAX_ATTEMPTS, MAX_FAILED_CHECKS,
    MAX_TRANSIENT_WAIT, Outbox, OutboxError, Received, Sent, SentDue, SyncError, Transient, TransientSent,
    UnreadableSeen, Unsynced, create_chat, enqueue, new_client_message_id,
};
pub use presence::{Availability, MAX_WATCHED, PRESENCE_MIN_GAP, PRESENCE_POLL};
pub use profiles::{MAX_LOOKUPS, NameFrom, PROFILE_MAX_AGE, PROFILE_RETRY, Person};
pub use search::{Backfill, INDEX_BATCH, search_text};
pub use sync::{
    CLOCK_SKEW_MS, CatchUp, CatchUpError, CatchUpReport, Changes, ConversationFailed, Gaps, MAX_PAGES, StoredMessage,
    Synced,
};
pub use teams::{READING_TEAMS, TEAMS_MAX_AGE, TEAMS_RETRY, TeamsReadError};
