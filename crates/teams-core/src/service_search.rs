//! The service's search (`docs/protocol/search.md`): what the title bar's search asks of Substrate as the user types
//! (its suggestions) and once the query is entered (the results of every kind, or of one tab's), read now and answered
//! whole; nothing is kept. A search is a read: it reaches nobody, whoever its answer lists, and goes in the read-only
//! mode too ([`teams_proto::HttpTransport::search`]).
//!
//! - **What is asked** ([`SearchScope`]): the suggestions for a query being typed, or the results of one entered, of
//!   every kind (the All tab: a few of each) or of one kind (its tab: a page of it).
//! - **What comes back** ([`SearchAnswer`]): the messages (their chat and id, so the app opens the chat at the
//!   message), the people, the files, the group chats and the teams and channels found, each as `teams_proto` reads
//!   it; whether the service has more.
//! - **A failure** (no answer, an answer that doesn't read) is [`EngineError::SearchFailed`].

use std::sync::Arc;

use teams_proto::{ALL_PAGE, Client, Credentials, Found, HttpError, HttpTransport, PAGE, SearchKind, SubstrateSearch};

use crate::engine::{EngineError, Responder};

/// What the title bar's search asks of the service.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchScope {
    /// The suggestions for a query being typed.
    Suggestions,
    /// The results of a query entered, a few of every kind (the All tab).
    All,
    /// The results of one kind, a page of them (that kind's tab).
    Only(SearchKind),
}

/// What the service found.
pub type SearchAnswer = Found;

/// The service's search for one account.
pub(crate) struct ServiceSearch<C> {
    client: Arc<Client<HttpTransport<C>>>,
}

impl<C> ServiceSearch<C>
where
    C: Credentials + 'static,
{
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self { client }
    }

    /// Answers `reply` with what the service finds for `query` in `scope`.
    pub(crate) fn search(&self, query: String, scope: SearchScope, reply: Responder<SearchAnswer>) {
        let client = Arc::clone(&self.client);
        // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
        tokio::task::spawn_local(async move {
            let _ = reply.send(search(&client, &query, scope).await);
        });
    }
}

/// The request `scope` asks for `query`.
#[must_use]
pub fn request(query: &str, scope: SearchScope) -> SubstrateSearch {
    match scope {
        SearchScope::Suggestions => SubstrateSearch::suggestions(query),
        SearchScope::All => SubstrateSearch::query(
            query,
            &[
                SearchKind::Messages,
                SearchKind::People,
                SearchKind::Files,
                SearchKind::Chats,
                SearchKind::Channels,
            ],
            ALL_PAGE,
            0,
        ),
        SearchScope::Only(kind) => SubstrateSearch::query(query, &[kind], PAGE, 0),
    }
}

async fn search<C: Credentials>(
    client: &Client<HttpTransport<C>>,
    query: &str,
    scope: SearchScope,
) -> Result<SearchAnswer, EngineError> {
    if query.trim().is_empty() {
        return Ok(SearchAnswer::default());
    }
    let request = request(query, scope);
    match client.transport().search(&request).await {
        Ok(response) => request.read_answer(response.body()).map_err(|error| {
            log::info!("the search's answer wasn't taken: {error}");
            EngineError::SearchFailed
        }),
        Err(error) => {
            log::info!("the search couldn't be read: {}", shown(&error));
            Err(EngineError::SearchFailed)
        }
    }
}

/// An error as the log says it: its status when the service answered one.
fn shown(error: &HttpError) -> String {
    match error {
        HttpError::Status(response) => format!("HTTP {}", response.status()),
        other => other.to_string(),
    }
}
