//! The engine's requests as they come in, shared by its loop and its catch-up. The loop takes them one at a time, in
//! the order they were made ([`Inbox::next`]). While a read of the catch-up waits on the service (a catch-up, a
//! conversation's sync, older history, the outbox's check), the inbox answers what the views read from the store as it
//! stands, and keeps every other request, in order, for the loop to take once the read is over ([`Meanwhile`]): an
//! account whose catch-up runs for minutes still opens its chats, channels and lists at once.
//!
//! - **Answered meanwhile**: every query, and the three store reads that also start a read of their own in the
//!   background: a person ([`EngineRequests::person`](super::EngineRequests::person)), the teams and the chat
//!   listing. Those are answered from the store alone ([`profiles::stored_person`], [`teams::stored_teams`],
//!   [`teams::stored_listing`]), a person `looking` when their stored profile is due a lookup; the request is then
//!   kept with its answer already given, so the loop starts that lookup or read once the catch-up's read is over, as
//!   if it had been asked then.
//! - **Kept**: everything else (a write, older history, a sync, presence, a call, the shutdown).
//! - Such an answer comes before a request made earlier that is kept (a write's row isn't in it yet); what that request
//!   changes is told by its events, as always.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;

use teams_store::{Store, StoreError};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::{EngineError, Query, Request, Responder};
use crate::sync::Meanwhile;
use crate::{profiles, teams};

/// See the module docs.
pub(super) struct Inbox {
    /// The account's MRI, whose read horizon and unread counts the queries answer with.
    me: String,
    /// The names the binary knows ([`Parts::known_names`](super::Parts::known_names)), for a person answered meanwhile.
    known: HashMap<String, String>,
    requests: Mutex<Requests>,
}

/// The requests not taken yet: those kept while a read waited first, then the channel's.
struct Requests {
    received: mpsc::UnboundedReceiver<Request>,
    kept: VecDeque<Request>,
}

impl Inbox {
    pub(super) fn new(me: String, known: HashMap<String, String>, received: mpsc::UnboundedReceiver<Request>) -> Self {
        Self {
            me,
            known,
            requests: Mutex::new(Requests {
                received,
                kept: VecDeque::new(),
            }),
        }
    }

    /// The next request, in the order they were made; `None` once every request handle is gone and every request
    /// was taken. Loses nothing when dropped while it waits.
    pub(super) async fn next(&self) -> Option<Request> {
        let mut requests = self.requests.lock().await;
        if let Some(kept) = requests.kept.pop_front() {
            return Some(kept);
        }
        requests.received.recv().await
    }

    /// Answers a request from the store alone when it can be (see the module docs); the request to keep, if any.
    fn meanwhile(&self, store: &Store, request: Request) -> Option<Request> {
        match request {
            Request::Query(query) => {
                self.answer(store, query);
                None
            }
            Request::Person(mri, reply) => {
                let person = store.profile(&mri).and_then(|stored| {
                    profiles::stored_person(store, &mri, &self.known, profiles::due(stored.as_ref()))
                });
                let _ = reply.send(person.map_err(EngineError::from));
                Some(Request::Person(mri, answered()))
            }
            Request::Teams(reply) => {
                let _ = reply.send(teams::stored_teams(store).map_err(EngineError::from));
                Some(Request::Teams(answered()))
            }
            Request::ChatListing(reply) => {
                let _ = reply.send(teams::stored_listing(store).map_err(EngineError::from));
                Some(Request::ChatListing(answered()))
            }
            other => Some(other),
        }
    }

    /// Answers a query from the store.
    pub(super) fn answer(&self, store: &Store, query: Query) {
        fn give<T>(reply: Responder<T>, answer: Result<T, StoreError>) {
            let _ = reply.send(answer.map_err(EngineError::from));
        }
        let me = self.me.as_str();
        match query {
            Query::Conversations(limit, reply) => give(reply, store.conversations_by_activity(limit)),
            Query::Messages {
                conversation,
                before,
                limit,
                reply,
            } => give(reply, store.messages_before(&conversation, before.as_ref(), limit)),
            Query::Message {
                conversation,
                id,
                reply,
            } => give(reply, store.message(&conversation, &id)),
            Query::Reactions {
                conversation,
                id,
                reply,
            } => give(reply, store.reactions(&conversation, &id)),
            Query::Members(conversation, reply) => give(reply, store.members(&conversation)),
            Query::RecentReactions(limit, reply) => give(reply, store.recent_reactions(me, limit)),
            Query::FrequentReactions(limit, reply) => give(reply, store.frequent_reactions(me, limit)),
            Query::UnreadCount(conversation, reply) => give(reply, store.unread_count(&conversation, me)),
            Query::DisplayName(mri, reply) => give(reply, store.display_name(&mri)),
            Query::ReadHorizon(conversation, reply) => give(reply, store.read_horizon(&conversation, me)),
            Query::HistoryComplete(conversation, reply) => give(
                reply,
                store
                    .sync_state(&conversation)
                    .map(|state| state.map(|state| state.history_complete)),
            ),
            Query::OutboxEntry(id, reply) => give(reply, store.outbox_entry(id)),
            Query::OutboxUnsettled(conversation, reply) => give(reply, store.outbox_unsettled(&conversation)),
            Query::LeftByEarlierRuns(reply) => give(reply, store.outbox_left_by_earlier_runs()),
            Query::ReplyRoots(conversation, reply) => give(reply, store.reply_roots(&conversation)),
            Query::Search {
                query,
                within,
                limit,
                reply,
            } => give(
                reply,
                match within {
                    Some(within) => store.search_in(&query, &within, limit),
                    None => store.search(&query, limit),
                },
            ),
        }
    }
}

impl Meanwhile for Inbox {
    /// Answers what comes while a read waits, and keeps the rest for the loop (see the module docs). Ends at once
    /// when the loop is taking a request (it never is while the catch-up runs, which the loop awaits), and once every
    /// request handle is gone. `recv` loses nothing when dropped, and an answer is given whole before the next wait.
    fn serve<'a>(&'a self, store: &'a mut Store) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let Ok(mut requests) = self.requests.try_lock() else {
                return;
            };
            while let Some(request) = requests.received.recv().await {
                if let Some(kept) = self.meanwhile(store, request) {
                    requests.kept.push_back(kept);
                }
            }
        })
    }
}

/// The responder of a request whose answer was given already: the loop's answer to it goes nowhere.
fn answered<T>() -> Responder<T> {
    oneshot::channel().0
}
