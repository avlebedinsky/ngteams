//! Catch-up sync: what the account missed while its push channel was down (or before the client ran) is read from
//! the chat service and kept in the store, once, through the same path a Trouter delivery takes.
//!
//! - **The conversation list** ([`CatchUp::catch_up`]): the first catch-up reads the whole list, every later one
//!   follows the list's own sync link (`syncState`), which brings only the conversations that changed since. Each
//!   conversation is upserted by its `version`, its read horizon and (for a chat) its members are kept, and the
//!   messages of every one that is new or changed are synced. The list's sync link moves on only once every
//!   conversation it brought was synced: a conversation that failed is listed again by the next catch-up.
//! - **A conversation's messages** ([`CatchUp::sync_messages`]): the first read is the newest page, whose
//!   `backwardLink` is kept for older history ([`CatchUp::load_older`]); every later one follows the conversation's
//!   sync link, which brings what changed since. A sync answer that has a `backwardLink` of its own (never captured) is
//!   followed while its pages bring anything new or newer, so a gap longer than a page is read through to what the
//!   store had; past [`MAX_PAGES`] the rest is left as older history. A conversation the store doesn't know yet is
//!   first read as a thread.
//! - **One path for what comes in** ([`CatchUp::receive_message`]): a message as read (a sync page, or the resource of
//!   a Trouter delivery) becomes a store row and goes through [`Outbox::receive`], which stores it by its id and
//!   version and matches our own to its outbox row by `clientmessageid`. So a delivery and a sync that bring the same
//!   message, in either order, leave one row at the newest version.
//! - **Versions are never guessed**: a message whose `version` this build can't read ([`Field::Unknown`], or none at
//!   all) isn't stored, since it could be older or newer than the stored row; it is counted in [`Synced::skipped`],
//!   and so is a reaction set with a time it can't read (its reactions stay as they were). A conversation whose
//!   version can't be read keeps its stored row (only a later last activity is taken) and has its messages synced.
//! - **When**: on start and after every reconnect of the push channel ([`Gaps`], [`CatchUp::catch_up_if_due`]). The
//!   channel is connected first and the catch-up runs after, so nothing falls between the two: what arrives after the
//!   connect comes as a delivery, what came before by the catch-up, and what comes both ways is stored once.
//! - **The outbox's check** ([`ConversationSync`]): before a new attempt whose earlier one may have arrived, the outbox
//!   syncs the conversation through [`CatchUp::sync_messages`] and looks for the row's `clientmessageid`.
//!
//! Every read is a `GET` through the native transport (`teams_proto::chat::ChatReads`), and every link is the
//! service's own, followed only on the same chat base. Nothing here writes to the service.
//!
//! M1 Task 17 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

use std::sync::Arc;

use serde_json::Value;
use teams_fence::Mri;
use teams_proto::chat::{self, ChatReadError, ChatReads, Field, MAX_PAGE_SIZE, MessagePage};
use teams_proto::{Client, Credentials, HttpError, HttpTransport, Transport};
use teams_store::{
    Applied, Conversation, ConversationKind, Member, Message, Reaction, ReadHorizon, Store, StoreError, SyncState,
};

use crate::outbox::{Clock, ConversationSync, Outbox, OutboxError, Received, SyncError};

/// The most pages one read follows: the conversation list's `forwardLink`s, or a sync answer's `backwardLink`s.
pub const MAX_PAGES: usize = 50;

/// What a catch-up can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CatchUpError {
    #[error(transparent)]
    Read(#[from] ChatReadError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Outbox(#[from] OutboxError),
    /// The conversation list ran past [`MAX_PAGES`] pages: its sync link wasn't moved on.
    #[error("the conversation list ran past {MAX_PAGES} pages, so its sync link wasn't moved on")]
    TooManyPages,
}

/// What a sync did with the messages it read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Messages the store didn't have.
    pub inserted: usize,
    /// Messages the store had at an older version.
    pub updated: usize,
    /// Messages the store had at the same or a newer version: nothing changed.
    pub unchanged: usize,
    /// Entries not stored: not a message's shape, of another conversation, without a version or an arrival time this
    /// build can read. A message stored whose reaction set had a time it can't read counts here too.
    pub skipped: usize,
    /// Pages read.
    pub pages: usize,
}

impl Synced {
    /// Messages that were new or newer.
    #[must_use]
    pub fn changed(&self) -> usize {
        self.inserted + self.updated
    }

    fn count(&mut self, applied: Applied) {
        match applied {
            Applied::Inserted => self.inserted += 1,
            Applied::Updated => self.updated += 1,
            Applied::Ignored => self.unchanged += 1,
        }
    }

    fn add(&mut self, other: Self) {
        self.inserted += other.inserted;
        self.updated += other.updated;
        self.unchanged += other.unchanged;
        self.skipped += other.skipped;
        self.pages += other.pages;
    }
}

/// What one [`CatchUp::catch_up`] did.
#[derive(Debug, Default)]
pub struct CatchUpReport {
    /// Conversations the list brought: every one on the first catch-up, those changed since afterwards.
    pub conversations: usize,
    /// List entries that aren't a conversation's shape.
    pub skipped: usize,
    /// What the conversations' message syncs did, together.
    pub messages: Synced,
    /// Conversations that couldn't be synced. The list's sync link stayed where it was, so the next catch-up lists
    /// them again.
    pub failed: Vec<ConversationFailed>,
}

/// A conversation a catch-up couldn't sync.
#[derive(Debug)]
pub struct ConversationFailed {
    pub conversation: String,
    pub error: CatchUpError,
}

/// When a catch-up is due: before the first one, and whenever the push channel went down since the last one that ran
/// to the end while it was up.
#[derive(Debug)]
pub struct Gaps {
    due: bool,
    up: bool,
}

impl Default for Gaps {
    fn default() -> Self {
        Self::new()
    }
}

impl Gaps {
    /// A start: a catch-up is due, the channel isn't up yet.
    #[must_use]
    pub fn new() -> Self {
        Self { due: true, up: false }
    }

    /// The push channel is connected (the first time, or again).
    pub fn connected(&mut self) {
        self.up = true;
    }

    /// The push channel went down: what it would have brought is missed until the next catch-up.
    pub fn disconnected(&mut self) {
        self.up = false;
        self.due = true;
    }

    /// Whether a catch-up is due.
    #[must_use]
    pub fn is_due(&self) -> bool {
        self.due
    }

    /// A catch-up ran to the end with nothing failed: none is due until the channel drops again, unless it was down
    /// all along (then what it missed meanwhile keeps one due for when it connects).
    fn caught_up(&mut self) {
        self.due = !self.up;
    }
}

/// The catch-up sync of one account (see the module docs): reads through `client`, stores what comes in through
/// `outbox`'s [`Outbox::receive`].
pub struct CatchUp<C, T> {
    client: Arc<Client<HttpTransport<C>>>,
    outbox: Arc<Outbox<T>>,
    /// The account: its read horizon is the list's `consumptionhorizon`.
    me: Mri,
    page_size: u32,
    clock: Clock,
}

impl<C, T> std::fmt::Debug for CatchUp<C, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatchUp")
            .field("me", &self.me)
            .field("page_size", &self.page_size)
            .finish_non_exhaustive()
    }
}

impl<C: Credentials, T: Transport<Error = HttpError>> CatchUp<C, T> {
    /// The catch-up of the account `me`, reading through `client` and storing through `outbox`.
    #[must_use]
    pub fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>, me: Mri) -> Self {
        Self {
            client,
            outbox,
            me,
            page_size: MAX_PAGE_SIZE,
            clock: Arc::new(now_ms),
        }
    }

    /// Another size for a conversation's first page and its older pages ([`MAX_PAGE_SIZE`] at most).
    #[must_use]
    pub fn with_page_size(mut self, page_size: u32) -> Self {
        self.page_size = page_size.clamp(1, MAX_PAGE_SIZE);
        self
    }

    /// Another clock for the times a sync state is written at.
    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Catches up if one is due ([`Gaps`]); `None` when none is. A catch-up that fails, or leaves a conversation
    /// unsynced, keeps one due.
    ///
    /// # Errors
    ///
    /// As [`CatchUp::catch_up`].
    pub async fn catch_up_if_due(
        &self,
        store: &mut Store,
        gaps: &mut Gaps,
    ) -> Result<Option<CatchUpReport>, CatchUpError> {
        if !gaps.is_due() {
            return Ok(None);
        }
        let report = self.catch_up(store).await?;
        if report.failed.is_empty() {
            gaps.caught_up();
        }
        Ok(Some(report))
    }

    /// Reads the conversation list (the whole one the first time, what changed since the last catch-up after that)
    /// and syncs every conversation it brought (see the module docs).
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Read`] when the list can't be read, [`CatchUpError::TooManyPages`] when it doesn't end,
    /// [`CatchUpError::Store`] when the store fails to keep its sync link. A conversation that can't be synced is in
    /// [`CatchUpReport::failed`] instead.
    pub async fn catch_up(&self, store: &mut Store) -> Result<CatchUpReport, CatchUpError> {
        let reads = ChatReads::new(&self.client);
        let kept = store.conversation_list_sync_link()?;
        let mut list = match kept.as_deref() {
            Some(link) => reads.conversations_from(link).await?,
            None => reads.conversations().await?,
        };
        let sync_link = list.sync_state().map(str::to_owned).or(kept);
        let mut conversations = std::mem::take(&mut list.conversations);
        let mut report = CatchUpReport {
            skipped: list.unreadable.len(),
            ..CatchUpReport::default()
        };
        let mut pages = 1;
        while let Some(next) = list.forward_link().map(str::to_owned) {
            if pages == MAX_PAGES {
                return Err(CatchUpError::TooManyPages);
            }
            list = reads.conversations_from(&next).await?;
            pages += 1;
            conversations.append(&mut list.conversations);
            report.skipped += list.unreadable.len();
        }
        report.conversations = conversations.len();
        for conversation in &conversations {
            match self.catch_up_conversation(store, conversation).await {
                Ok(synced) => report.messages.add(synced),
                Err(error) => report.failed.push(ConversationFailed {
                    conversation: conversation.id.clone(),
                    error,
                }),
            }
        }
        if report.failed.is_empty() {
            store.set_conversation_list_sync_link(sync_link.as_deref(), (self.clock)())?;
        }
        Ok(report)
    }

    /// Syncs one conversation's messages from the service (see the module docs): the newest page the first time, what
    /// changed since after that. A conversation the store doesn't know is read as a thread first.
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Read`] when a read fails, [`CatchUpError::Store`] or [`CatchUpError::Outbox`] when the store
    /// does; the sync state then stays as it was.
    pub async fn sync_messages(&self, store: &mut Store, conversation: &str) -> Result<Synced, CatchUpError> {
        if store.conversation(conversation)?.is_none() {
            self.add_conversation(store, conversation).await?;
        }
        let reads = ChatReads::new(&self.client);
        let kept = store.sync_state(conversation)?;
        let Some((kept, link)) = kept.and_then(|state| state.sync_link.clone().map(|link| (state, link))) else {
            return self.first_read(store, &reads, conversation).await;
        };
        let page = reads.messages_from(conversation, &link).await?;
        let mut synced = self.apply_page(store, conversation, &page)?;
        let left = if synced.changed() > 0 {
            self.follow_back(store, &reads, conversation, page.backward_link(), &mut synced)
                .await?
        } else {
            None
        };
        let state = SyncState {
            conversation_id: conversation.to_owned(),
            sync_link: page.sync_state().map(str::to_owned).or(Some(link)),
            history_complete: kept.history_complete && left.is_none(),
            backward_link: left.or(kept.backward_link),
            updated: (self.clock)(),
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Reads the next older page of a conversation's history, from the `backwardLink` its sync kept; `None` when there
    /// is none to read (the whole history is stored, or the conversation was never synced).
    ///
    /// # Errors
    ///
    /// As [`CatchUp::sync_messages`].
    pub async fn load_older(&self, store: &mut Store, conversation: &str) -> Result<Option<Synced>, CatchUpError> {
        let Some(kept) = store.sync_state(conversation)? else {
            return Ok(None);
        };
        let Some(link) = kept.backward_link.clone() else {
            return Ok(None);
        };
        let page = ChatReads::new(&self.client).messages_from(conversation, &link).await?;
        let synced = self.apply_page(store, conversation, &page)?;
        let older = page.backward_link().map(str::to_owned);
        let state = SyncState {
            history_complete: older.is_none(),
            backward_link: older,
            updated: (self.clock)(),
            ..kept
        };
        store.set_sync_state(&state)?;
        Ok(Some(synced))
    }

    /// Stores a message that came in (a sync page's entry, or a Trouter delivery's resource) in `conversation`,
    /// through [`Outbox::receive`]; `None` when it isn't stored (see [`Synced::skipped`]).
    ///
    /// # Errors
    ///
    /// [`CatchUpError::Outbox`] when the store fails, an unknown conversation included (a delivery for one is
    /// followed by [`CatchUp::sync_messages`], which reads it).
    pub fn receive_message(
        &self,
        store: &mut Store,
        conversation: &str,
        message: &chat::Message,
    ) -> Result<Option<Received>, CatchUpError> {
        let Some(row) = message_row(conversation, message) else {
            return Ok(None);
        };
        let reactions = reaction_set(message);
        Ok(Some(self.outbox.receive(store, &row, reactions.as_deref())?))
    }

    /// The first read of a conversation's messages: its newest page, with the page's `backwardLink` kept for older
    /// history.
    async fn first_read(
        &self,
        store: &mut Store,
        reads: &ChatReads<'_, C>,
        conversation: &str,
    ) -> Result<Synced, CatchUpError> {
        let page = reads.messages(conversation, self.page_size).await?;
        let synced = self.apply_page(store, conversation, &page)?;
        let state = SyncState {
            conversation_id: conversation.to_owned(),
            sync_link: page.sync_state().map(str::to_owned),
            backward_link: page.backward_link().map(str::to_owned),
            history_complete: page.backward_link().is_none(),
            updated: (self.clock)(),
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Follows a sync answer's `backwardLink`s while each page brings anything new or newer; returns the link left
    /// unfollowed at [`MAX_PAGES`], if any.
    async fn follow_back(
        &self,
        store: &mut Store,
        reads: &ChatReads<'_, C>,
        conversation: &str,
        link: Option<&str>,
        synced: &mut Synced,
    ) -> Result<Option<String>, CatchUpError> {
        let mut link = link.map(str::to_owned);
        while let Some(next) = link {
            if synced.pages >= MAX_PAGES {
                return Ok(Some(next));
            }
            let page = reads.messages_from(conversation, &next).await?;
            let brought = self.apply_page(store, conversation, &page)?;
            synced.add(brought);
            link = page
                .backward_link()
                .filter(|_| brought.changed() > 0)
                .map(str::to_owned);
        }
        Ok(None)
    }

    /// Stores a page's messages, the oldest first.
    fn apply_page(&self, store: &mut Store, conversation: &str, page: &MessagePage) -> Result<Synced, CatchUpError> {
        let mut synced = Synced {
            skipped: page.unreadable.len(),
            pages: 1,
            ..Synced::default()
        };
        for message in page.messages.iter().rev() {
            match self.receive_message(store, conversation, message)? {
                Some(received) => {
                    synced.count(received.applied);
                    if has_unreadable_reactions(message) {
                        synced.skipped += 1;
                    }
                }
                None => synced.skipped += 1,
            }
        }
        Ok(synced)
    }

    /// One conversation of the list: kept, its messages synced when it is new or changed (before its new version is
    /// stored, so a sync that fails is tried again), then its read horizon and members.
    async fn catch_up_conversation(
        &self,
        store: &mut Store,
        listed: &chat::Conversation,
    ) -> Result<Synced, CatchUpError> {
        let stored = store.conversation(&listed.id)?;
        let version = known_i64(&listed.version);
        if stored.is_none() {
            store.upsert_conversation(&conversation_row(listed, 0))?;
        }
        let changed = match (&stored, version) {
            (Some(stored), Some(version)) => version > stored.version,
            _ => true,
        };
        let synced = if changed || store.sync_state(&listed.id)?.is_none() {
            self.sync_messages(store, &listed.id).await?
        } else {
            Synced::default()
        };
        // an unreadable version keeps the stored row: as new as itself, only a later last activity is taken
        let kept = stored.map_or(0, |stored| stored.version);
        store.upsert_conversation(&conversation_row(listed, version.unwrap_or(kept)))?;
        self.store_horizon(store, listed)?;
        self.refresh_members(store, listed).await?;
        Ok(synced)
    }

    /// A conversation the store doesn't know, read as a thread: kept at version 0 (the list's version replaces it),
    /// with its members.
    async fn add_conversation(&self, store: &mut Store, id: &str) -> Result<(), CatchUpError> {
        let thread = ChatReads::new(&self.client).thread(id).await?;
        let row = Conversation {
            id: id.to_owned(),
            kind: store_kind(&thread.kind()),
            topic: None,
            version: 0,
            last_activity: None,
        };
        store.upsert_conversation(&row)?;
        if !thread.kind().is_read_only() {
            store_members(store, &thread)?;
        }
        Ok(())
    }

    /// The account's own read horizon from the list, when both its parts can be read.
    fn store_horizon(&self, store: &mut Store, listed: &chat::Conversation) -> Result<(), CatchUpError> {
        let Some(horizon) = listed.consumption_horizon() else {
            return Ok(());
        };
        let (Some(last_read), Some(updated)) = (horizon.last_read_arrival, horizon.updated) else {
            return Ok(());
        };
        let (Ok(last_read_arrival), Ok(updated)) = (i64::try_from(last_read), i64::try_from(updated)) else {
            return Ok(());
        };
        store.set_read_horizon(&ReadHorizon {
            conversation_id: listed.id.clone(),
            user: self.me.as_str().to_owned(),
            last_read_arrival,
            updated,
            raw: Some(horizon.raw),
        })?;
        Ok(())
    }

    /// A chat's members, read as a thread when the list's `rosterVersion` is newer than the stored one (or there is
    /// none stored, or the list's can't be read while none is stored). Channels and other read-only conversations
    /// keep none.
    async fn refresh_members(&self, store: &mut Store, listed: &chat::Conversation) -> Result<(), CatchUpError> {
        if listed.is_read_only() {
            return Ok(());
        }
        let listed_roster = listed
            .thread_properties
            .get("rosterVersion")
            .and_then(number)
            .and_then(|version| i64::try_from(version).ok());
        let newer = match (store.roster_version(&listed.id)?, listed_roster) {
            (None, _) => true,
            (Some(stored), Some(listed)) => listed > stored,
            (Some(_), None) => false,
        };
        if newer {
            let thread = ChatReads::new(&self.client).thread(&listed.id).await?;
            store_members(store, &thread)?;
        }
        Ok(())
    }
}

impl<C: Credentials, T: Transport<Error = HttpError> + Send> ConversationSync for CatchUp<C, T> {
    async fn sync_conversation(&self, store: &mut Store, conversation: &str) -> Result<(), SyncError> {
        self.sync_messages(store, conversation)
            .await
            .map(|_| ())
            .map_err(SyncError::from)
    }
}

/// A thread's members by its `rosterVersion`, when that can be read.
fn store_members(store: &mut Store, thread: &chat::Thread) -> Result<(), CatchUpError> {
    let Some(roster) = known_i64(&thread.roster_version) else {
        return Ok(());
    };
    let members: Vec<Member> = thread
        .members
        .iter()
        .map(|member| Member::new(&member.id, member.role.as_deref()))
        .collect();
    store.replace_members(&thread.id, roster, &members)?;
    Ok(())
}

/// A conversation of the list as the store keeps it, at `version`.
fn conversation_row(listed: &chat::Conversation, version: i64) -> Conversation {
    Conversation {
        id: listed.id.clone(),
        kind: store_kind(&listed.kind()),
        // a chat's topic was never captured
        topic: None,
        version,
        last_activity: listed.last_message().and_then(|message| message.arrival_time_ms()),
    }
}

/// The store's kind for the model's: a kind it doesn't name is kept as text the store's own names can't be.
fn store_kind(kind: &chat::ConversationKind) -> ConversationKind {
    match kind {
        chat::ConversationKind::OneToOne => ConversationKind::OneToOne,
        chat::ConversationKind::Group => ConversationKind::Group,
        chat::ConversationKind::Channel => ConversationKind::Channel,
        chat::ConversationKind::Notifications => ConversationKind::Other("notifications".to_owned()),
        chat::ConversationKind::Unknown(thread_type) => ConversationKind::Other(format!("thread_type:{thread_type}")),
    }
}

/// A message as read, as the store keeps it in `conversation`; `None` when it can't be: it names another
/// conversation, or its version or its arrival time can't be read (the arrival time falls back to the id, which is
/// that time).
fn message_row(conversation: &str, message: &chat::Message) -> Option<Message> {
    if message.conversation_id.as_deref().is_some_and(|id| id != conversation) {
        return None;
    }
    let version = known_i64(&message.version)?;
    let arrival_time = message.arrival_time_ms().or_else(|| message.id.parse().ok())?;
    let properties = (!message.properties.is_empty()).then(|| Value::Object(message.properties.clone()).to_string());
    Some(Message {
        conversation_id: conversation.to_owned(),
        id: message.id.clone(),
        version,
        client_message_id: message.client_message_id.as_known().cloned(),
        sender: message.sender().map(str::to_owned),
        sender_name: message.display_name.clone(),
        message_type: message.message_type.as_str().to_owned(),
        content: message.content.clone(),
        properties,
        arrival_time,
        compose_time: message.compose_time_ms(),
        edit_time: known_i64(&message.edit_time()),
        delete_time: known_i64(&message.delete_time()),
    })
}

/// A message's whole reaction set (`properties.emotions`), when it is there and every time in it can be read; `None`
/// leaves the stored reactions as they are.
fn reaction_set(message: &chat::Message) -> Option<Vec<Reaction>> {
    let Field::Known(keys) = message.reactions() else {
        return None;
    };
    let mut set = Vec::new();
    for reaction in &keys {
        for user in &reaction.users {
            set.push(Reaction {
                key: reaction.key.clone(),
                user: user.mri.clone(),
                time: known_i64(&user.time)?,
            });
        }
    }
    Some(set)
}

/// Whether a message has a reaction set that isn't stored: one in a form this build can't read.
fn has_unreadable_reactions(message: &chat::Message) -> bool {
    !matches!(message.reactions(), Field::Absent) && reaction_set(message).is_none()
}

fn known_i64(field: &Field<u64>) -> Option<i64> {
    field.as_known().and_then(|value| i64::try_from(*value).ok())
}

/// A number as the service writes it: a JSON number, or a string of digits.
fn number(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => text.parse().ok(),
        _ => None,
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}
