//! The native client's core: what the user asks for goes out through the [`outbox`], every attempt under a fresh
//! decision of the fence; what comes back is matched to the outbox by its `clientmessageid` and kept in the account's
//! `teams-store`.
//!
//! - [`outbox`]: commands as outbox rows with a `clientmessageid` of their own, each attempt a fresh `Fence::check`,
//!   the outbox's part of the plan's retry table, our own messages matched to their rows when they come back, and no
//!   resend across runs.
//!
//! Catch-up sync (M1 Task 17) fills the outbox's [`ConversationSync`] seam; the engine (Task 18) owns the store and
//! runs the outbox on its own runtime.
//!
//! M1 Task 13 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

pub mod outbox;

pub use outbox::{
    CHECK_RETRY_WAIT, Clock, Command, ConversationSync, MAX_ATTEMPTS, MAX_FAILED_CHECKS, Outbox, OutboxError, Received,
    SentDue, SyncError, Unsynced, enqueue, new_client_message_id,
};
