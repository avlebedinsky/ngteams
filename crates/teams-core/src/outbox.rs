//! The outbox: every write the user asks for goes out from here, and from nowhere else.
//!
//! - **A command becomes a row** ([`enqueue`]): a [`Command`] is stored as the row's payload under a fresh
//!   `clientmessageid` ([`new_client_message_id`]), which the row keeps across every attempt.
//! - **Each attempt asks the fence again** ([`Outbox::send`]): the request is built from the row, and
//!   [`Fence::check`] reads the target's members fresh from the membership source and logs its decision before
//!   anything goes out. A denied attempt marks the row denied and sends nothing; a permit covers that one attempt.
//! - **The outbox's part of the retry table** (the plan's Technical Details): a 2xx is sent; a 401 is tried once more
//!   at once (the transport has had the session refresh the refused token); a 429 or 503 waits its `Retry-After`
//!   ([`DEFAULT_RETRY_WAIT`] without one) as the row's `not_before`; any other 4xx (and any other answer) fails the
//!   row, never retried. A network error or a timeout, and a 5xx other than 503, leave it unknown whether the message
//!   arrived: before the next attempt the conversation is synced ([`ConversationSync`]) and searched for the row's
//!   `clientmessageid`; found, the row is sent and nothing goes out again; not found, a new attempt goes out with the
//!   same `clientmessageid` (whether the service dedupes by it is unverified, so the check comes first). Every
//!   attempt after the first starts with that check, whatever ended the one before. A check that can't sync sends
//!   nothing: the row waits [`CHECK_RETRY_WAIT`], doubled for each check of its that failed before. A row is given up
//!   (failed) after [`MAX_ATTEMPTS`] attempts or [`MAX_FAILED_CHECKS`] checks that couldn't sync, both counted since
//!   it was made or last resent.
//! - **Writes on one thing go out in the order they were asked for** ([`Lane`]): a row waits, sending nothing, while
//!   an older pending row of this run acts on the same message (an edit, a delete, a reaction or taking it back) or,
//!   for a read receipt, on the same conversation's read horizon; so a take-back sent while its reaction waits out a
//!   `Retry-After` can't be undone by the reaction going out after it, nor a newer edit by an older one. Rows on
//!   anything else go out meanwhile; a message post and a new chat wait for nothing.
//! - **Our own message coming back** ([`Outbox::receive`], for a Trouter delivery and a sync alike) is stored once
//!   (by its id and version) and matched by its `clientmessageid` to its row, which becomes sent with the message's
//!   id; it may come back before the post's answer does, and the answer then changes nothing.
//! - **No resend across runs**: a run sends only its own rows ([`teams_store::Store::outbox_due`]); rows an earlier
//!   process left pending or in flight are listed by [`teams_store::Store::outbox_left_by_earlier_runs`] and go out
//!   again only after an explicit [`teams_store::Store::resend`], which keeps their `clientmessageid` and starts their
//!   budget over. An adopted row that had an attempt (in any run) begins with the sync check above, so a message an
//!   earlier attempt got through isn't sent twice.
//!
//! The commands are the chat writes of `teams-proto` (M1 Task 12): a message post, an edit, a soft delete, a reaction
//! and taking it back, a read receipt and a new chat; the engine (M1 Task 18) builds them from what the user asks
//! for. Only a message post can be looked for by its `clientmessageid`, so only its row is checked and tried again
//! after an attempt whose outcome is unknown; any other such row is failed rather than risk doing it twice. The sync
//! the check relies on is the [`ConversationSync`] seam, which M1 Task 17's catch-up sync fills.

use std::error::Error;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use teams_auth::token::TokenError;
use teams_fence::{Denied, Fence, MembershipSource, Mri};
use teams_proto::chat::MessageText;
use teams_proto::{
    Author, Authorized, CreateChat, DEFAULT_RETRY_WAIT, DeleteMessage, EditMessage, HttpError, Mutating, Original,
    ReadReceipt, RemoveReaction, Response, SendMessage, SetReaction, Transport,
};
use teams_store::{
    Applied, AttemptOutcome, CheckFailed, Message, NewOutboxEntry, OutboxEntry, OutboxKind, OutboxState, Reaction,
    Store, StoreError,
};

/// The most attempts a row gets since it was made or last resent ([`Store::resend`]), throttled ones included; after
/// the last it is failed.
pub const MAX_ATTEMPTS: u32 = 5;

/// The most checks before a new attempt that can fail to sync since a row was made or last resent; at the last the row
/// is failed, since whether its earlier attempt arrived can't be told.
pub const MAX_FAILED_CHECKS: u32 = 5;

/// How long a row waits after its first check that couldn't sync; each later one doubles it.
pub const CHECK_RETRY_WAIT: Duration = Duration::from_secs(30);

/// How many fresh `clientmessageid`s a command is tried with when one is taken already.
const CLIENT_MESSAGE_ID_TRIES: usize = 3;

/// What the user asked for, as the outbox stores it (the row's payload, JSON). Each attempt builds its sealed request
/// from it, with the row's `created_at` as the time the request carries (a post's compose time, an edit's time, a
/// reaction's or a read receipt's), so every attempt sends the same request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Posts a message to an existing chat: its text, from which the request builds its HTML, its mentions and its
    /// target.
    Send { conversation: String, text: MessageText },
    /// Edits one of the account's messages: its new text, and what the edit sends again of the message as it was
    /// sent.
    Edit {
        conversation: String,
        message_id: String,
        text: MessageText,
        original: Sent,
    },
    /// Soft-deletes one of the account's messages.
    Delete { conversation: String, message_id: String },
    /// Sets (`on`) or takes back the account's reaction `key` on a message.
    React {
        conversation: String,
        message_id: String,
        key: String,
        on: bool,
    },
    /// Moves the account's read horizon to a message (sent with `client_message_id`).
    ReadReceipt {
        conversation: String,
        message_id: String,
        client_message_id: String,
    },
    /// Creates a chat of the account and `others` (MRIs): their 1:1 chat for one, a new group chat for two or more.
    CreateChat { others: Vec<String> },
}

/// What commands go out in the order they were asked for on: a later one of this run waits while an earlier one on
/// the same lane is pending, since sent out of order the earlier would undo the later (a reaction coming back after
/// its take-back, an older edit over a newer one, the read horizon moving back).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane<'a> {
    /// A message's edits, its delete, and the account's reactions on it.
    Message { conversation: &'a str, message_id: &'a str },
    /// The account's read horizon in a conversation.
    ReadHorizon { conversation: &'a str },
}

/// What an edit sends again of the message it changes, as the store had it when the edit was asked for: its
/// `clientmessageid` and its times, in milliseconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    pub client_message_id: String,
    pub compose_time_ms: i64,
    pub arrival_time_ms: i64,
}

impl Command {
    /// The row's kind.
    #[must_use]
    pub fn kind(&self) -> OutboxKind {
        match self {
            Self::Send { .. } => OutboxKind::Send,
            Self::Edit { .. } => OutboxKind::Edit,
            Self::Delete { .. } => OutboxKind::Delete,
            Self::React { on: true, .. } => OutboxKind::AddReaction,
            Self::React { on: false, .. } => OutboxKind::RemoveReaction,
            Self::ReadReceipt { .. } => OutboxKind::ReadReceipt,
            Self::CreateChat { .. } => OutboxKind::CreateChat,
        }
    }

    /// The conversation it writes to; none for a chat that doesn't exist yet.
    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        match self {
            Self::Send { conversation, .. }
            | Self::Edit { conversation, .. }
            | Self::Delete { conversation, .. }
            | Self::React { conversation, .. }
            | Self::ReadReceipt { conversation, .. } => Some(conversation),
            Self::CreateChat { .. } => None,
        }
    }

    /// What it has to go out in order on with the other commands of this run ([`Lane`]); none for a message post and
    /// a new chat.
    #[must_use]
    pub fn lane(&self) -> Option<Lane<'_>> {
        match self {
            Self::Edit {
                conversation,
                message_id,
                ..
            }
            | Self::Delete {
                conversation,
                message_id,
            }
            | Self::React {
                conversation,
                message_id,
                ..
            } => Some(Lane::Message {
                conversation,
                message_id,
            }),
            Self::ReadReceipt { conversation, .. } => Some(Lane::ReadHorizon { conversation }),
            Self::Send { .. } | Self::CreateChat { .. } => None,
        }
    }

    /// The existing message it acts on: an edit's, a delete's, a reaction's, a read receipt's.
    #[must_use]
    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::Edit { message_id, .. }
            | Self::Delete { message_id, .. }
            | Self::React { message_id, .. }
            | Self::ReadReceipt { message_id, .. } => Some(message_id),
            Self::Send { .. } | Self::CreateChat { .. } => None,
        }
    }
}

/// The request a new chat of `me` and `others` is: their 1:1 chat for one other, a group chat for more; `None` when
/// `others` don't make a chat (none, `me` among them, one given twice).
#[must_use]
pub fn create_chat(me: &Mri, others: &[String]) -> Option<CreateChat> {
    match others {
        [] => None,
        [other] => CreateChat::one_to_one(me.clone(), Mri::new(other)),
        _ => CreateChat::group(me.clone(), others.iter().map(Mri::new)),
    }
}

/// Why a [`ConversationSync`] couldn't sync.
pub type SyncError = Box<dyn Error + Send + Sync>;

/// Syncs one conversation from the service, for the outbox's check after an attempt whose outcome is unknown. M1 Task
/// 17's catch-up sync implements it.
pub trait ConversationSync: Sync {
    /// Reads `conversation`'s messages from the service now, never from the store alone, and stores what changed
    /// (through [`Outbox::receive`], or [`Store::upsert_message`]), so that a message we sent that reached the service
    /// is in `store` once it returns.
    fn sync_conversation<'a>(
        &'a self,
        store: &'a mut Store,
        conversation: &'a str,
    ) -> impl Future<Output = Result<(), SyncError>> + Send + 'a;

    /// As [`ConversationSync::sync_conversation`], for a message we sent no earlier than `since` (milliseconds since
    /// the Unix epoch: the row's `created_at`); the outbox's check calls this one. A sync that reads the newest
    /// messages a page at a time reads back until it reaches one older than `since`, and fails when it can't, so a
    /// message we sent is never missed for being more than a page back. By default it is
    /// [`ConversationSync::sync_conversation`], for a sync that reads everything at once.
    fn sync_conversation_since<'a>(
        &'a self,
        store: &'a mut Store,
        conversation: &'a str,
        _since: i64,
    ) -> impl Future<Output = Result<(), SyncError>> + Send + 'a {
        self.sync_conversation(store, conversation)
    }
}

/// The time in milliseconds since the Unix epoch, as the outbox reads it for `not_before`.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// What the outbox can fail with.
#[derive(Debug, thiserror::Error)]
pub enum OutboxError {
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The conversation couldn't be synced before a new attempt, so whether the last one arrived is unknown: nothing was
    /// sent, and the row waits (or, at [`MAX_FAILED_CHECKS`], is failed).
    #[error("the conversation couldn't be synced, so outbox row {id} wasn't sent again: {source}")]
    Sync {
        id: i64,
        #[source]
        source: SyncError,
    },
    /// A pending row of another run: only an explicit [`Store::resend`] makes it this run's.
    #[error("outbox row {id} belongs to run {run_id}, not this one: only an explicit resend sends it")]
    OtherRun { id: i64, run_id: i64 },
    #[error("no random source for a clientmessageid: {0}")]
    Random(getrandom::Error),
    #[error("the command can't be stored: {0}")]
    Payload(#[source] serde_json::Error),
}

/// What one [`Outbox::send_due`] did.
#[derive(Debug, Default)]
pub struct SentDue {
    /// Every due row as it ended up, oldest first; one whose check couldn't sync is here too, waiting or failed.
    pub rows: Vec<OutboxEntry>,
    /// The rows whose check before a new attempt couldn't sync, so nothing was sent for them.
    pub unsynced: Vec<Unsynced>,
}

/// A row whose check before a new attempt couldn't sync its conversation.
#[derive(Debug)]
pub struct Unsynced {
    pub id: i64,
    pub source: SyncError,
}

/// A message stored by [`Outbox::receive`]: what the store did with it, and the outbox row it was matched to (ours).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub applied: Applied,
    pub outbox: Option<OutboxEntry>,
}

/// What a message that came in is matched to its outbox row by ([`Outbox::match_arrival`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival<'a> {
    /// The conversation it came in.
    pub conversation: &'a str,
    /// The service's id for it.
    pub id: &'a str,
    pub client_message_id: Option<&'a str>,
    /// Its sender's MRI.
    pub sender: Option<&'a str>,
}

/// A fresh `clientmessageid`: a random 19-digit number, as the web client makes them.
///
/// # Errors
///
/// [`OutboxError::Random`] when the system random source fails.
pub fn new_client_message_id() -> Result<String, OutboxError> {
    const LOWEST: u64 = 1_000_000_000_000_000_000;
    let random = getrandom::u64().map_err(OutboxError::Random)?;
    Ok((LOWEST + random % (9 * LOWEST)).to_string())
}

/// Puts `command` in the outbox as a pending row of this run, with a fresh `clientmessageid`.
///
/// # Errors
///
/// [`OutboxError::Payload`] when the command can't be serialized, [`OutboxError::Random`] without a random source,
/// [`OutboxError::Store`] when the row can't be written.
pub fn enqueue(store: &mut Store, command: &Command) -> Result<OutboxEntry, OutboxError> {
    let payload = serde_json::to_string(command).map_err(OutboxError::Payload)?;
    let mut tries = 1;
    loop {
        let new = NewOutboxEntry {
            conversation_id: command.conversation().map(str::to_owned),
            kind: command.kind(),
            client_message_id: new_client_message_id()?,
            payload: payload.clone(),
        };
        match store.enqueue(&new) {
            Err(StoreError::DuplicateClientMessageId(_)) if tries < CLIENT_MESSAGE_ID_TRIES => tries += 1,
            stored => return Ok(stored?),
        }
    }
}

/// How one attempt went.
enum Tried {
    /// The service answered.
    Answered(Response),
    Denied(Denied),
    /// Nothing went out, and nothing will: the row is failed.
    NotSent(String),
    /// Nothing went out yet: try again after `wait`.
    Throttled {
        wait: Duration,
        error: String,
    },
    /// The request may or may not have reached the service.
    Uncertain(String),
}

/// Sends the outbox's rows over a transport, each attempt under a fresh permit (see the module docs).
pub struct Outbox<T> {
    transport: Arc<T>,
    fence: Arc<Fence>,
    membership: Arc<dyn MembershipSource + Send>,
    /// The account: its messages go out as its, and only its messages are matched to outbox rows.
    me: Author,
    clock: Clock,
}

impl<T: Transport<Error = HttpError>> Outbox<T> {
    /// An outbox for the account `me`, sending over `transport` under `fence`'s permits, with `membership` as the
    /// source the fence reads each target's members from on every attempt.
    #[must_use]
    pub fn new(transport: Arc<T>, fence: Arc<Fence>, membership: Arc<dyn MembershipSource + Send>, me: Author) -> Self {
        Self {
            transport,
            fence,
            membership,
            me,
            clock: Arc::new(now_ms),
        }
    }

    /// Another clock (tests: a `Retry-After` without waiting for it).
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The account the outbox writes as.
    #[must_use]
    pub fn me(&self) -> &Author {
        &self.me
    }

    /// Sends every row of this run that is due, oldest first, as [`Outbox::send`] does; returns the rows as they
    /// end up. A row whose check can't sync doesn't hold up the rows after it: it is listed in
    /// [`SentDue::unsynced`] and waits (see the module docs). A row that waits behind an older one on its lane
    /// ([`Lane`]) is left out: nothing happened to it.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails; the rows after it wait for the next call.
    pub async fn send_due<S: ConversationSync>(&self, store: &mut Store, sync: &S) -> Result<SentDue, OutboxError> {
        let due = store.outbox_due((self.clock)())?;
        let mut done = SentDue {
            rows: Vec::with_capacity(due.len()),
            unsynced: Vec::new(),
        };
        for row in due {
            if waits_behind(store, &row)? {
                continue;
            }
            match self.send(store, sync, row.id).await {
                Ok(row) => done.rows.push(row),
                Err(OutboxError::Sync { id, source }) => {
                    done.rows.push(entry(store, id)?);
                    done.unsynced.push(Unsynced { id, source });
                }
                Err(error) => return Err(error),
            }
        }
        Ok(done)
    }

    /// Sends one row of this run, if it is pending and due: attempts until it is sent, denied or failed, or waits
    /// for a `Retry-After` (see the module docs). A row of this run that isn't due, or waits behind an older pending
    /// row on its lane ([`Lane`]), and a row of any run that isn't pending, comes back as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::OtherRun`] for a pending row of another run, due or not (nothing is sent),
    /// [`OutboxError::Sync`] when the check before a new attempt can't sync (nothing is sent, the row waits or, at
    /// [`MAX_FAILED_CHECKS`], is failed), [`OutboxError::Store`] when the store fails.
    pub async fn send<S: ConversationSync>(
        &self,
        store: &mut Store,
        sync: &S,
        id: i64,
    ) -> Result<OutboxEntry, OutboxError> {
        let mut refreshed = false;
        loop {
            let row = entry(store, id)?;
            if row.state == OutboxState::Pending && row.run_id != store.run_id() {
                return Err(OutboxError::OtherRun { id, run_id: row.run_id });
            }
            if !self.is_due(&row) || waits_behind(store, &row)? {
                return Ok(row);
            }
            if row.attempts > 0
                && let Some(arrived) = self.look_for_arrival(store, sync, &row).await?
            {
                return Ok(arrived);
            }
            let row = store.begin_attempt(id)?;
            let tried = self.attempt(&row).await;
            let (outcome, again) = self.settle(&row, tried, &mut refreshed);
            let row = store.finish_attempt(id, &outcome)?;
            if !again || row.state != OutboxState::Pending {
                return Ok(row);
            }
        }
    }

    /// Stores a message that came in (a Trouter delivery or a sync) and, when it is ours, matches it by its
    /// `clientmessageid` to its row, which becomes sent with the message's id. The same message coming in again
    /// (the other way, or a later version) is stored once, and its row is left as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails, its conversation unknown included.
    pub fn receive(
        &self,
        store: &mut Store,
        message: &Message,
        reactions: Option<&[Reaction]>,
    ) -> Result<Received, OutboxError> {
        let applied = store.upsert_message(message, reactions)?;
        let outbox = self.match_own(store, message)?;
        Ok(Received { applied, outbox })
    }

    fn is_mine(&self, message: &Message) -> bool {
        message.sender.as_deref() == Some(self.me.mri().as_str())
    }

    fn is_due(&self, row: &OutboxEntry) -> bool {
        row.state == OutboxState::Pending && row.not_before.is_none_or(|not_before| not_before <= (self.clock)())
    }

    /// Matches a message that came in to its row without storing it, for one that can't be stored (its version or
    /// its arrival time can't be read); [`Outbox::receive`] matches a stored one the same way. When it is ours, the
    /// message post with its `clientmessageid` in its conversation becomes sent with its id; a row already sent with
    /// a message id stays as it is.
    ///
    /// # Errors
    ///
    /// [`OutboxError::Store`] when the store fails.
    pub fn match_arrival(&self, store: &mut Store, arrival: &Arrival<'_>) -> Result<Option<OutboxEntry>, OutboxError> {
        let mine = arrival.sender == Some(self.me.mri().as_str());
        let Some(client_message_id) = arrival.client_message_id.filter(|_| mine) else {
            return Ok(None);
        };
        let Some(row) = store.outbox_by_client_message_id(client_message_id)? else {
            return Ok(None);
        };
        if findable(&row) != Some(arrival.conversation) {
            return Ok(None);
        }
        if row.state == OutboxState::Sent && row.message_id.is_some() {
            return Ok(Some(row));
        }
        Ok(store.outbox_arrived(client_message_id, arrival.id)?)
    }

    /// The row a message of ours belongs to ([`Outbox::match_arrival`]).
    fn match_own(&self, store: &mut Store, message: &Message) -> Result<Option<OutboxEntry>, OutboxError> {
        let arrival = Arrival {
            conversation: &message.conversation_id,
            id: &message.id,
            client_message_id: message.client_message_id.as_deref(),
            sender: message.sender.as_deref(),
        };
        self.match_arrival(store, &arrival)
    }

    /// Before a new attempt: syncs the row's conversation and looks for its `clientmessageid` among our messages.
    /// Found, the row is sent; `None` when it wasn't found (or a row of its kind can't be looked for). A sync that fails
    /// leaves the row waiting its backoff, or fails it ([`Outbox::check_failed`]), and is returned as
    /// [`OutboxError::Sync`].
    async fn look_for_arrival<S: ConversationSync>(
        &self,
        store: &mut Store,
        sync: &S,
        row: &OutboxEntry,
    ) -> Result<Option<OutboxEntry>, OutboxError> {
        let Some(conversation) = findable(row) else {
            return Ok(None);
        };
        if let Err(source) = sync.sync_conversation_since(store, conversation, row.created_at).await {
            let after = self.check_failed(store, row, &source)?;
            if after.state == OutboxState::Sent {
                // the sync matched it before it failed
                return Ok(Some(after));
            }
            return Err(OutboxError::Sync { id: row.id, source });
        }
        let current = entry(store, row.id)?;
        if current.state != OutboxState::Pending {
            // the sync's own receive matched it
            return Ok(Some(current));
        }
        match store.message_by_client_id(conversation, &row.client_message_id)? {
            Some(message) if self.is_mine(&message) => Ok(store.outbox_arrived(&row.client_message_id, &message.id)?),
            _ => Ok(None),
        }
    }

    /// A check that couldn't sync: the row waits its backoff, or is failed at its last check.
    fn check_failed(
        &self,
        store: &mut Store,
        row: &OutboxEntry,
        source: &SyncError,
    ) -> Result<OutboxEntry, OutboxError> {
        let failed = row.failed_checks.saturating_add(1);
        let error =
            format!("the conversation couldn't be synced to look for it ({source}); whether it arrived is unknown");
        let outcome = if failed >= MAX_FAILED_CHECKS {
            CheckFailed::GiveUp {
                error: format!("{error}, so it isn't sent again; given up after {failed} checks"),
            }
        } else {
            let doubled = CHECK_RETRY_WAIT.saturating_mul(2_u32.saturating_pow(failed - 1));
            CheckFailed::Wait {
                not_before: (self.clock)().saturating_add(i64::try_from(doubled.as_millis()).unwrap_or(i64::MAX)),
                error,
            }
        };
        Ok(store.check_failed(row.id, &outcome)?)
    }

    /// One attempt: the row's request, a fresh permit for its target, and the transport.
    async fn attempt(&self, row: &OutboxEntry) -> Tried {
        let command = match serde_json::from_str::<Command>(&row.payload) {
            Ok(command) => command,
            Err(error) => return Tried::NotSent(format!("the stored command can't be read: {error}")),
        };
        let at = row.created_at;
        let me = self.me.clone();
        match command {
            Command::Send { conversation, text } => {
                let request = SendMessage::new(conversation, me, text)
                    .composed_at(at)
                    .with_client_message_id(&row.client_message_id);
                self.deliver(request).await
            }
            Command::Edit {
                conversation,
                message_id,
                text,
                original,
            } => {
                let original = Original {
                    client_message_id: original.client_message_id,
                    compose_time_ms: original.compose_time_ms,
                    arrival_time_ms: original.arrival_time_ms,
                };
                let request = EditMessage::new(conversation, message_id, me, text, original).edited_at(at);
                self.deliver(request).await
            }
            Command::Delete {
                conversation,
                message_id,
            } => self.deliver(DeleteMessage::new(conversation, message_id)).await,
            Command::React {
                conversation,
                message_id,
                key,
                on: true,
            } => {
                self.deliver(SetReaction::new(conversation, message_id, key).at(at))
                    .await
            }
            Command::React {
                conversation,
                message_id,
                key,
                on: false,
            } => self.deliver(RemoveReaction::new(conversation, message_id, key)).await,
            Command::ReadReceipt {
                conversation,
                message_id,
                client_message_id,
            } => {
                let request = ReadReceipt::new(conversation, message_id, client_message_id).at(at);
                self.deliver(request).await
            }
            Command::CreateChat { others } => match create_chat(me.mri(), &others) {
                Some(request) => self.deliver(request).await,
                None => Tried::NotSent("the chat's members don't make a chat".to_owned()),
            },
        }
    }

    async fn deliver<R: Mutating + Send>(&self, request: R) -> Tried {
        let permit = match self.fence.check(request.target(), &*self.membership).await {
            Ok(permit) => permit,
            Err(denied) => return Tried::Denied(denied),
        };
        let write = match Authorized::new(request, permit) {
            Ok(write) => write,
            Err(mismatch) => return Tried::NotSent(mismatch.to_string()),
        };
        match self.transport.deliver(write).await {
            Ok(answer) => Tried::Answered(answer),
            Err(error) => from_transport(error),
        }
    }

    /// What an attempt makes of the row, and whether another attempt follows at once.
    fn settle(&self, row: &OutboxEntry, tried: Tried, refreshed: &mut bool) -> (AttemptOutcome, bool) {
        match tried {
            Tried::Answered(answer) => self.settle_answer(row, &answer, refreshed),
            Tried::Denied(denied) => (
                AttemptOutcome::Denied {
                    reason: denied.to_string(),
                },
                false,
            ),
            Tried::NotSent(error) => (AttemptOutcome::Failed { error }, false),
            Tried::Throttled { wait, error } => (self.retry(row, wait, error), false),
            Tried::Uncertain(error) => self.again_if_findable(row, &error),
        }
    }

    fn settle_answer(&self, row: &OutboxEntry, answer: &Response, refreshed: &mut bool) -> (AttemptOutcome, bool) {
        let status = answer.status();
        match status {
            200..=299 => (
                AttemptOutcome::Sent {
                    message_id: answered_id(row, answer),
                },
                false,
            ),
            401 if !*refreshed => {
                *refreshed = true;
                let error = "HTTP 401, the token was refreshed".to_owned();
                (self.retry(row, Duration::ZERO, error), true)
            }
            429 | 503 => {
                let wait = answer.retry_after().unwrap_or(DEFAULT_RETRY_WAIT);
                (self.retry(row, wait, format!("HTTP {status}")), false)
            }
            500..=599 => self.again_if_findable(row, &format!("HTTP {status}")),
            _ => (
                AttemptOutcome::Failed {
                    error: format!("HTTP {status}"),
                },
                false,
            ),
        }
    }

    /// An attempt whose message may have arrived: another one (after the sync check) for a row that can be looked
    /// for; any other is failed rather than risk doing it twice.
    fn again_if_findable(&self, row: &OutboxEntry, error: &str) -> (AttemptOutcome, bool) {
        let error = format!("{error}; whether it arrived is unknown");
        if findable(row).is_some() {
            (self.retry(row, Duration::ZERO, error), true)
        } else {
            (
                AttemptOutcome::Failed {
                    error: format!("{error}, so it isn't sent again"),
                },
                false,
            )
        }
    }

    /// Another attempt after `wait`, unless the row has had all of its attempts since it was made or last resent.
    fn retry(&self, row: &OutboxEntry, wait: Duration, error: String) -> AttemptOutcome {
        let attempts = row.attempts_since_resend();
        if attempts >= MAX_ATTEMPTS {
            return AttemptOutcome::Failed {
                error: format!("{error}; given up after {attempts} attempts"),
            };
        }
        let wait = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
        AttemptOutcome::Retry {
            not_before: (self.clock)().saturating_add(wait),
            error,
        }
    }
}

/// The conversation a row's message can be looked for in: a message post's.
fn findable(row: &OutboxEntry) -> Option<&str> {
    if row.kind == OutboxKind::Send {
        row.conversation_id.as_deref()
    } else {
        None
    }
}

/// Whether an older pending row of this run is on `row`'s lane ([`Lane`]), so `row` waits for it to settle. A row
/// whose command can't be read is on no lane (its attempt fails it).
fn waits_behind(store: &Store, row: &OutboxEntry) -> Result<bool, OutboxError> {
    let Ok(command) = serde_json::from_str::<Command>(&row.payload) else {
        return Ok(false);
    };
    let Some(lane) = command.lane() else {
        return Ok(false);
    };
    // every pending row of this run, whenever it is due, oldest first
    let pending = store.outbox_due(i64::MAX)?;
    Ok(pending
        .iter()
        .take_while(|older| older.id < row.id)
        .any(|older| serde_json::from_str::<Command>(&older.payload).is_ok_and(|command| command.lane() == Some(lane))))
}

fn entry(store: &Store, id: i64) -> Result<OutboxEntry, OutboxError> {
    store
        .outbox_entry(id)?
        .ok_or(OutboxError::Store(StoreError::UnknownOutboxEntry(id)))
}

/// A transport error as an attempt's outcome: one that may have happened after the request went out is uncertain.
fn from_transport(error: HttpError) -> Tried {
    match error {
        HttpError::Failed(_) | HttpError::BodyTooLarge => Tried::Uncertain(error.to_string()),
        HttpError::Credentials(TokenError::Throttled { retry_after, .. }) => Tried::Throttled {
            wait: retry_after.unwrap_or(DEFAULT_RETRY_WAIT),
            error: "the token service is throttling".to_owned(),
        },
        other => Tried::NotSent(other.to_string()),
    }
}

/// The id a row that went through is kept with: a post's new message, a new chat's thread (from the answer), the
/// message an edit, a delete or a reaction acted on; none for a read receipt.
fn answered_id(row: &OutboxEntry, answer: &Response) -> Option<String> {
    match row.kind {
        OutboxKind::Send | OutboxKind::CreateChat => message_id(answer),
        OutboxKind::ReadReceipt => None,
        OutboxKind::Edit | OutboxKind::Delete | OutboxKind::AddReaction | OutboxKind::RemoveReaction => {
            let command = serde_json::from_str::<Command>(&row.payload).ok()?;
            command.message_id().map(str::to_owned)
        }
    }
}

/// The new message's id from a post's answer: the last segment of its `Location`, or else its `OriginalArrivalTime` (a
/// message's id is its arrival time). A new chat's answer gives its thread id the same way (`Location`
/// `…/v1/threads/<id>`).
fn message_id(answer: &Response) -> Option<String> {
    let located = answer
        .location()
        .and_then(|location| location.split(['?', '#']).next())
        .and_then(|path| path.rsplit('/').next())
        .filter(|id| !id.is_empty());
    if let Some(id) = located {
        return Some(id.to_owned());
    }
    let body: Value = serde_json::from_slice(answer.body()).ok()?;
    match body.get("OriginalArrivalTime")? {
        Value::Number(time) => Some(time.to_string()),
        Value::String(time) if !time.is_empty() => Some(time.clone()),
        _ => None,
    }
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}
