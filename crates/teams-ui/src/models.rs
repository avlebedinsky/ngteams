//! The models the views observe, kept up to date by the bridge's drain ([`Models::apply`], on GPUI's foreground).
//!
//! They hold what the engine told, not what the store holds: a view that shows a conversation asks the store again
//! (through [`Core`](crate::Core)) when its [`Chats::revision`] moves. So every view of the same data (the list, the
//! switcher, search) sees every change, and nothing shown comes from anywhere but the store.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use gpui::{App, AppContext as _, Entity, SharedString};
use teams_core::Event;
use teams_store::{Message, OutboxEntry};

/// The push channel, as the account's status shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// Not connected yet since the engine started.
    Starting,
    Connected,
    /// Down; it reconnects by itself.
    Down,
    /// Stopped for good.
    Stopped,
}

/// The engine itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Running,
    /// Stopped: its requests answer `Stopped`, nothing more goes out.
    Stopped,
}

/// Whether the account has tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Account {
    SignedIn,
    /// The session can't renew its tokens (an expired or revoked refresh token): the engine is stopped, nothing more
    /// is sent, and `hint` says what to do (the lab build: run `proto-lab auth <alias>`).
    SignedOut {
        hint: SharedString,
    },
}

/// The account's status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub link: Link,
    pub engine: Engine,
    pub account: Account,
    /// Catch-ups that ran on their own since the start.
    pub catch_ups: u64,
    /// What the engine last failed at on its own (a catch-up, …), until a catch-up runs clean.
    pub last_failure: Option<&'static str>,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            link: Link::Starting,
            engine: Engine::Running,
            account: Account::SignedIn,
            catch_ups: 0,
            last_failure: None,
        }
    }
}

/// What changed in the account's chats, by conversation.
#[derive(Debug, Default)]
pub struct Chats {
    /// Moves on every change the engine tells; a view asks the store again when the one it read at has moved.
    revision: u64,
    /// Per conversation: the revision of its last change.
    changed: HashMap<String, u64>,
    /// Per sender, by MRI: the revision of the last message told from them that carried a display name (which may be
    /// a name the store didn't have, or a new one).
    named: HashMap<String, u64>,
    /// Per conversation: who is typing now.
    typing: BTreeMap<String, BTreeSet<String>>,
    /// The outbox rows as last told, by id.
    outbox: BTreeMap<i64, OutboxEntry>,
}

impl Chats {
    /// The revision of the last change in any conversation.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The revision of `conversation`'s last change; 0 while none was told.
    #[must_use]
    pub fn changed_at(&self, conversation: &str) -> u64 {
        self.changed.get(conversation).copied().unwrap_or(0)
    }

    /// The revision of the last message told from `mri` that carried a display name, in any conversation; 0 while
    /// none was told.
    #[must_use]
    pub fn named_at(&self, mri: &str) -> u64 {
        self.named.get(mri).copied().unwrap_or(0)
    }

    /// Who is typing in `conversation` now, by MRI.
    pub fn typing(&self, conversation: &str) -> impl Iterator<Item = &str> {
        self.typing.get(conversation).into_iter().flatten().map(String::as_str)
    }

    /// An outbox row as last told.
    #[must_use]
    pub fn outbox_row(&self, id: i64) -> Option<&OutboxEntry> {
        self.outbox.get(&id)
    }

    fn change(&mut self, conversation: &str) {
        self.revision += 1;
        self.changed.insert(conversation.to_owned(), self.revision);
    }

    fn set_typing(&mut self, conversation: &str, from: &str, typing: bool) -> bool {
        let who = self.typing.entry(conversation.to_owned()).or_default();
        let changed = if typing {
            who.insert(from.to_owned())
        } else {
            who.remove(from)
        };
        if who.is_empty() {
            self.typing.remove(conversation);
        }
        changed
    }
}

/// The models, shared by every view (cheap to clone: entity handles).
#[derive(Clone, Debug)]
pub struct Models {
    pub status: Entity<Status>,
    pub chats: Entity<Chats>,
}

impl Models {
    #[must_use]
    pub fn new(cx: &mut App) -> Self {
        Self {
            status: cx.new(|_| Status::default()),
            chats: cx.new(|_| Chats::default()),
        }
    }

    /// Applies one of the engine's events; the models it changed notify their observers.
    pub fn apply(&self, event: Event, cx: &mut App) {
        match event {
            Event::Connected { .. } => self.set_link(Link::Connected, cx),
            Event::Disconnected(_) => self.set_link(Link::Down, cx),
            Event::PushStopped(_) => self.set_link(Link::Stopped, cx),
            Event::CaughtUp { .. } => self.status.update(cx, |status, cx| {
                status.catch_ups += 1;
                status.last_failure = None;
                cx.notify();
            }),
            Event::CatchUpFailed { .. } => self.failed("catch-up", cx),
            Event::Failed { during, .. } => self.failed(during, cx),
            Event::Message {
                conversation, message, ..
            } => self.message(&conversation, message.as_deref(), cx),
            Event::ReadHorizon { conversation } | Event::ConversationChanged { conversation } => {
                self.changed(&conversation, cx);
            }
            Event::Typing {
                conversation,
                from,
                typing,
            } => self.typing(&conversation, from.as_deref(), typing, cx),
            Event::Outbox(entry) => self.outbox(entry, cx),
            // how the push channel's registration went, and a waiting transient's send: nothing a view shows yet
            Event::Registration(_) | Event::Transient(..) => {}
        }
    }

    /// The engine stopped (its events ended, or it was shut down).
    pub fn engine_stopped(&self, cx: &mut App) {
        self.status.update(cx, |status, cx| {
            if status.engine != Engine::Stopped {
                status.engine = Engine::Stopped;
                cx.notify();
            }
        });
    }

    /// The account is signed out (see [`Account::SignedOut`]).
    pub fn signed_out(&self, hint: SharedString, cx: &mut App) {
        self.status.update(cx, |status, cx| {
            status.account = Account::SignedOut { hint };
            cx.notify();
        });
    }

    fn set_link(&self, link: Link, cx: &mut App) {
        self.status.update(cx, |status, cx| {
            if status.link != link {
                status.link = link;
                cx.notify();
            }
        });
    }

    fn failed(&self, during: &'static str, cx: &mut App) {
        self.status.update(cx, |status, cx| {
            status.last_failure = Some(during);
            cx.notify();
        });
    }

    fn changed(&self, conversation: &str, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            chats.change(conversation);
            cx.notify();
        });
    }

    /// A message stored or changed: its conversation changed, and its sender's name may have.
    fn message(&self, conversation: &str, message: Option<&Message>, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            chats.change(conversation);
            let named = message.and_then(|message| {
                let name = message.sender_name.as_deref().map(str::trim)?;
                message.sender.as_ref().filter(|_| !name.is_empty())
            });
            if let Some(sender) = named {
                chats.named.insert(sender.clone(), chats.revision);
            }
            cx.notify();
        });
    }

    fn typing(&self, conversation: &str, from: Option<&str>, typing: bool, cx: &mut App) {
        // a typing signal that names no one can't be shown against anyone
        let Some(from) = from else { return };
        self.chats.update(cx, |chats, cx| {
            if chats.set_typing(conversation, from, typing) {
                cx.notify();
            }
        });
    }

    fn outbox(&self, entry: OutboxEntry, cx: &mut App) {
        self.chats.update(cx, |chats, cx| {
            if let Some(conversation) = &entry.conversation_id {
                chats.change(conversation);
            }
            chats.outbox.insert(entry.id, entry);
            cx.notify();
        });
    }
}
