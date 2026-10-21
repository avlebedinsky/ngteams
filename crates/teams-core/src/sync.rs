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
//!   followed to its end, so a gap longer than a page is read through whatever its pages hold: a page that changes
//!   nothing (its messages came as deliveries, or by an earlier attempt that failed further back) doesn't stop it.
//!   Past [`MAX_PAGES`] the link left is kept as the conversation's gap ([`SyncState::gap_link`]), apart from its
//!   older history's `backwardLink`, so the sync link moves on only past what is stored or kept to be read. The next
//!   sync reads on from a kept gap first, and so does [`CatchUp::load_older`] before older history; a sync answer cut
//!   again while a gap is still unread leaves the sync link where it was ([`CatchUpError::GapPending`]). A
//!   conversation the store doesn't know yet is first read as a thread.
//! - **One path for what comes in** ([`CatchUp::receive_message`]): a message as read (a sync page, or the resource of
//!   a Trouter delivery) becomes a store row and goes through [`Outbox::receive`], which stores it by its id and
//!   version and matches our own to its outbox row by `clientmessageid`. So a delivery and a sync that bring the same
//!   message, in either order, leave one row at the newest version.
//! - **Versions are never guessed**: a message whose `version` this build can't read ([`Field::Unknown`], or none at
//!   all) isn't stored, since it could be older or newer than the stored row; it is counted in [`Synced::skipped`],
//!   and so is a reaction set with a time it can't read (its reactions stay as they were). A conversation whose
//!   version can't be read keeps its stored row (only a later last activity is taken) and has its messages synced.
//!   Our own message among those not stored is still matched to its outbox row by its id, `clientmessageid`,
//!   sender and conversation ([`Outbox::match_arrival`]), and inside the outbox's check a page holding an entry it
//!   can't read at all that carries the `clientmessageid` of a pending row of this run that had an attempt fails the
//!   check before it is applied ([`CatchUpError::CheckFellShort`]); an ordinary catch-up applies such a page and so
//!   loses that evidence (`docs/backlog/catch-up-consumes-an-unreadable-entry-of-a-pending-row.md`).
//! - **When**: on start and after every reconnect of the push channel ([`Gaps`], [`CatchUp::catch_up_if_due`]). The
//!   channel is connected first and the catch-up runs after, so nothing falls between the two: what arrives after the
//!   connect comes as a delivery, what came before by the catch-up, and what comes both ways is stored once.
//! - **The outbox's check** ([`ConversationSync`]): before a new attempt whose earlier one may have arrived, the outbox
//!   syncs the conversation through [`CatchUp::sync_messages`] and looks for the row's `clientmessageid`. Whether
//!   that is enough is told by the stored sync state, not by what this one sync read: older pages may be left by it
//!   or by any read before it (a first read is one page, a sync answer is cut at [`MAX_PAGES`], an earlier check fell
//!   short). So the check reads any kept gap to its end and older history until its pages reached a message older
//!   than the row (less [`CLOCK_SKEW_MS`]; [`SyncState::backward_oldest`]), and fails after [`MAX_PAGES`] more, so
//!   the row waits rather than going out twice.
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
    Applied, Conversation, ConversationKind, Member, Message, OutboxKind, OutboxState, Reaction, ReadHorizon, Store,
    StoreError, SyncState,
};

use crate::outbox::{Arrival, Clock, ConversationSync, Outbox, OutboxError, Received, SyncError, now_ms};

/// The most pages one read follows: the conversation list's `forwardLink`s, a sync answer's `backwardLink`s, or the
/// older pages the outbox's check reads.
pub const MAX_PAGES: usize = 50;

/// How far the service's clock (a message's arrival time) may be behind ours (an outbox row's `created_at`): the
/// outbox's check reads back to a message older than its row by this much. Five minutes, Kerberos's default tolerance.
pub const CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;

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
    /// The outbox's check paged back [`MAX_PAGES`] pages without reaching a message older than its row (or the end
    /// of a gap), or read a page holding an entry it can't read that carries a pending row's `clientmessageid`:
    /// whether the row's message arrived can't be told.
    #[error(
        "the outbox's check read {MAX_PAGES} older pages without reaching its row's time, or a page holding an \
         entry it can't read with a pending row's clientmessageid"
    )]
    CheckFellShort,
    /// A sync answer ran past [`MAX_PAGES`] while the rest of an earlier one was still unread: its sync link wasn't
    /// moved on (the pages it read are stored). The next sync reads on from the earlier one first.
    #[error("a sync answer ran past {MAX_PAGES} pages while an earlier one's rest was still unread")]
    GapPending,
}

/// Who a page is read for: the outbox's check guards its pages against entries it can't read
/// ([`CatchUp::guard_page`]), an ordinary catch-up doesn't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reader {
    CatchUp,
    Check,
}

/// Which of a conversation's kept links an older page is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chain {
    /// The rest of a sync answer cut at [`MAX_PAGES`] ([`SyncState::gap_link`]).
    Gap,
    /// Older history ([`SyncState::backward_link`]).
    History,
}

impl Chain {
    /// The chain the next older page comes from: a gap before older history.
    fn next(state: &SyncState) -> Self {
        if state.gap_link.is_some() {
            Self::Gap
        } else {
            Self::History
        }
    }

    /// The chain the outbox's check still has to read for a message that arrived no earlier than `stop`, decided by
    /// what is stored, not by the read just made: any gap (a sync answer's order is not relied on), then older
    /// history until its pages reached a message older than `stop`. `None`: everything since `stop` is stored.
    fn unread_since(state: &SyncState, stop: i64) -> Option<Self> {
        if state.gap_link.is_some() {
            return Some(Self::Gap);
        }
        let reached = state.backward_oldest.is_some_and(|oldest| oldest < stop);
        (state.backward_link.is_some() && !reached).then_some(Self::History)
    }
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
    /// does; the sync state then stays as it was (a kept gap may have been read on). [`CatchUpError::GapPending`]
    /// when the answer is cut at [`MAX_PAGES`] while an earlier gap is still unread: the sync link stays.
    pub async fn sync_messages(&self, store: &mut Store, conversation: &str) -> Result<Synced, CatchUpError> {
        self.sync_messages_for(store, conversation, Reader::CatchUp).await
    }

    /// [`CatchUp::sync_messages`], its pages read for `reader`.
    async fn sync_messages_for(
        &self,
        store: &mut Store,
        conversation: &str,
        reader: Reader,
    ) -> Result<Synced, CatchUpError> {
        if store.conversation(conversation)?.is_none() {
            self.add_conversation(store, conversation).await?;
        }
        let reads = ChatReads::new(&self.client);
        let kept = store.sync_state(conversation)?;
        let Some((kept, link)) = kept.and_then(|state| state.sync_link.clone().map(|link| (state, link))) else {
            return self.first_read(store, &reads, conversation, reader).await;
        };
        let mut synced = Synced::default();
        let kept = self.read_gap(store, kept, &mut synced, reader).await?;
        let page = reads.messages_from(conversation, &link).await?;
        synced.add(self.apply_page(store, conversation, &page, reader)?);
        let (left, walked) = self
            .follow_back(store, conversation, page.backward_link(), MAX_PAGES - 1, reader)
            .await?;
        synced.add(walked);
        if left.is_some() && kept.gap_link.is_some() {
            // one gap link at a time: the earlier one's progress is kept, this answer is read again next time
            return Err(CatchUpError::GapPending);
        }
        let gap_link = left.or_else(|| kept.gap_link.clone());
        let state = SyncState {
            sync_link: page.sync_state().map(str::to_owned).or(Some(link)),
            history_complete: kept.backward_link.is_none() && gap_link.is_none(),
            gap_link,
            updated: (self.clock)(),
            ..kept
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Reads on from the rest of an earlier sync answer, if one is kept, for up to [`MAX_PAGES`] pages; returns the
    /// sync state with the gap as far as it is now read (stored).
    async fn read_gap(
        &self,
        store: &mut Store,
        kept: SyncState,
        synced: &mut Synced,
        reader: Reader,
    ) -> Result<SyncState, CatchUpError> {
        if kept.gap_link.is_none() {
            return Ok(kept);
        }
        let conversation = kept.conversation_id.clone();
        let (gap_link, walked) = self
            .follow_back(store, &conversation, kept.gap_link.as_deref(), MAX_PAGES, reader)
            .await?;
        synced.add(walked);
        let state = SyncState {
            history_complete: kept.backward_link.is_none() && gap_link.is_none(),
            gap_link,
            updated: (self.clock)(),
            ..kept
        };
        store.set_sync_state(&state)?;
        Ok(state)
    }

    /// Reads the next older page of a conversation from the links its sync kept, the rest of a cut sync answer before
    /// older history; `None` when there is none to read (the whole history is stored, or the conversation was never
    /// synced).
    ///
    /// # Errors
    ///
    /// As [`CatchUp::sync_messages`].
    pub async fn load_older(&self, store: &mut Store, conversation: &str) -> Result<Option<Synced>, CatchUpError> {
        let Some(kept) = store.sync_state(conversation)? else {
            return Ok(None);
        };
        let chain = Chain::next(&kept);
        self.older_page(store, kept, chain, Reader::CatchUp).await
    }

    /// Reads the next page of one of a conversation's kept chains; `None` when that chain has no link.
    async fn older_page(
        &self,
        store: &mut Store,
        kept: SyncState,
        chain: Chain,
        reader: Reader,
    ) -> Result<Option<Synced>, CatchUpError> {
        let link = match chain {
            Chain::Gap => kept.gap_link.as_deref(),
            Chain::History => kept.backward_link.as_deref(),
        };
        let Some(link) = link else {
            return Ok(None);
        };
        let conversation = kept.conversation_id.as_str();
        let page = ChatReads::new(&self.client).messages_from(conversation, link).await?;
        let synced = self.apply_page(store, conversation, &page, reader)?;
        let older = page.backward_link().map(str::to_owned);
        let state = match chain {
            Chain::Gap => SyncState {
                history_complete: kept.backward_link.is_none() && older.is_none(),
                gap_link: older,
                updated: (self.clock)(),
                ..kept
            },
            Chain::History => SyncState {
                history_complete: kept.gap_link.is_none() && older.is_none(),
                // a page without a time it can read is older than the one before it all the same
                backward_oldest: oldest_arrival(&page).or(kept.backward_oldest),
                backward_link: older,
                updated: (self.clock)(),
                ..kept
            },
        };
        store.set_sync_state(&state)?;
        Ok(Some(synced))
    }

    /// The outbox's check (see the module docs): syncs `conversation`, then reads older pages until the stored state
    /// says everything since `since` less [`CLOCK_SKEW_MS`] is stored ([`Chain::unread_since`]), whatever this sync
    /// itself read: a gap or a history read an earlier sync, check or start left unread is read here. Every page it
    /// reads, its first sync's included, is guarded before it is applied ([`CatchUp::guard_page`]).
    async fn sync_since(&self, store: &mut Store, conversation: &str, since: i64) -> Result<(), CatchUpError> {
        let stop = since.saturating_sub(CLOCK_SKEW_MS);
        self.sync_messages_for(store, conversation, Reader::Check).await?;
        let mut pages = 0;
        loop {
            // a sync that succeeded kept a state: none can't be told complete
            let state = store.sync_state(conversation)?.ok_or(CatchUpError::CheckFellShort)?;
            let Some(chain) = Chain::unread_since(&state, stop) else {
                return Ok(());
            };
            if pages == MAX_PAGES {
                return Err(CatchUpError::CheckFellShort);
            }
            self.older_page(store, state, chain, Reader::Check).await?;
            pages += 1;
        }
    }

    /// Stores a message that came in (a sync page's entry, or a Trouter delivery's resource) in `conversation`,
    /// through [`Outbox::receive`]; `None` when it isn't stored (see [`Synced::skipped`]). One of ours that isn't
    /// stored, because its version or its arrival time can't be read, is still matched to its outbox row
    /// ([`Outbox::match_arrival`]).
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
            if let Some(arrival) = arrival(conversation, message) {
                self.outbox.match_arrival(store, &arrival)?;
            }
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
        reader: Reader,
    ) -> Result<Synced, CatchUpError> {
        let kept_gap = store.sync_state(conversation)?.and_then(|state| state.gap_link);
        let page = reads.messages(conversation, self.page_size).await?;
        let synced = self.apply_page(store, conversation, &page, reader)?;
        let backward_link = page.backward_link().map(str::to_owned);
        let state = SyncState {
            conversation_id: conversation.to_owned(),
            sync_link: page.sync_state().map(str::to_owned),
            history_complete: backward_link.is_none() && kept_gap.is_none(),
            backward_link,
            backward_oldest: oldest_arrival(&page),
            gap_link: kept_gap,
            updated: (self.clock)(),
        };
        store.set_sync_state(&state)?;
        Ok(synced)
    }

    /// Follows a chain of `backwardLink`s from `from` to its end, for up to `budget` pages, whatever each page brings
    /// (a page that changes nothing may still have older messages behind it the store never had); returns the link
    /// left unfollowed, if any, and what the pages read did.
    async fn follow_back(
        &self,
        store: &mut Store,
        conversation: &str,
        from: Option<&str>,
        budget: usize,
        reader: Reader,
    ) -> Result<(Option<String>, Synced), CatchUpError> {
        let reads = ChatReads::new(&self.client);
        let mut synced = Synced::default();
        let mut link = from.map(str::to_owned);
        let mut read = 0;
        while let Some(next) = link {
            if read == budget {
                return Ok((Some(next), synced));
            }
            let page = reads.messages_from(conversation, &next).await?;
            synced.add(self.apply_page(store, conversation, &page, reader)?);
            read += 1;
            link = page.backward_link().map(str::to_owned);
        }
        Ok((None, synced))
    }

    /// Stores a page's messages, the oldest first; a page read for the outbox's check is guarded first
    /// ([`CatchUp::guard_page`]).
    fn apply_page(
        &self,
        store: &mut Store,
        conversation: &str,
        page: &MessagePage,
        reader: Reader,
    ) -> Result<Synced, CatchUpError> {
        if reader == Reader::Check {
            Self::guard_page(store, conversation, page)?;
        }
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

    /// The outbox's check can't tell from a page whether a row's message arrived when the page holds an entry it can't
    /// read at all that carries the `clientmessageid` of a pending message post of this run in `conversation` that
    /// had an attempt: [`CatchUpError::CheckFellShort`], before the page is applied or any link moves past it. Who
    /// sent such an entry isn't inferred, and its row isn't matched from it.
    fn guard_page(store: &Store, conversation: &str, page: &MessagePage) -> Result<(), CatchUpError> {
        for entry in &page.unreadable {
            let Some(client_message_id) = entry.get("clientmessageid").and_then(Value::as_str) else {
                continue;
            };
            let Some(row) = store.outbox_by_client_message_id(client_message_id)? else {
                continue;
            };
            let awaits_check = row.kind == OutboxKind::Send
                && row.state == OutboxState::Pending
                && row.run_id == store.run_id()
                && row.attempts > 0
                && row.conversation_id.as_deref() == Some(conversation);
            if awaits_check {
                return Err(CatchUpError::CheckFellShort);
            }
        }
        Ok(())
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

    /// A chat's members, read as a thread when the list's `rosterVersion` is newer than the stored one, or there is
    /// none stored, or the list's can't be read (it may be newer). A list without one keeps the stored members.
    /// Channels and other read-only conversations keep none.
    async fn refresh_members(&self, store: &mut Store, listed: &chat::Conversation) -> Result<(), CatchUpError> {
        if listed.is_read_only() {
            return Ok(());
        }
        let newer = match (store.roster_version(&listed.id)?, listed.roster_version()) {
            (None, _) => true,
            (Some(_), Field::Absent) => false,
            (Some(stored), listed) => known_i64(&listed).is_none_or(|listed| listed > stored),
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

    async fn sync_conversation_since(
        &self,
        store: &mut Store,
        conversation: &str,
        since: i64,
    ) -> Result<(), SyncError> {
        self.sync_since(store, conversation, since)
            .await
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
    let arrival_time = arrival_time(message)?;
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

/// What a message that can't be stored is matched to an outbox row by: `None` when it names another conversation, or
/// has no id.
fn arrival<'a>(conversation: &'a str, message: &'a chat::Message) -> Option<Arrival<'a>> {
    if message.conversation_id.as_deref().is_some_and(|id| id != conversation) || message.id.is_empty() {
        return None;
    }
    Some(Arrival {
        conversation,
        id: &message.id,
        client_message_id: message.client_message_id.as_known().map(String::as_str),
        sender: message.sender(),
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

/// A message's arrival time: its `originalarrivaltime`, or else its id, which is that time.
fn arrival_time(message: &chat::Message) -> Option<i64> {
    message.arrival_time_ms().or_else(|| message.id.parse().ok())
}

/// The oldest arrival time among a page's messages.
fn oldest_arrival(page: &MessagePage) -> Option<i64> {
    page.messages.iter().filter_map(arrival_time).min()
}

fn known_i64(field: &Field<u64>) -> Option<i64> {
    field.as_known().and_then(|value| i64::try_from(*value).ok())
}
