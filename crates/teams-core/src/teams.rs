//! The account's teams and their channels (M2 Task 16), read-only: CSA's one read ([`CsaTeams`], `GET`, on the
//! transport's CSA base) in the background, kept in the store and read again by age; a channel's posts and their reply
//! chains are the chat service's, synced like a chat's ([`crate::sync`]: each channel message's post recorded as it is
//! stored).
//!
//! - **Asked for by a view** ([`EngineRequests::teams`](crate::EngineRequests::teams)): answered from the store at once
//!   (`None` before the first read settled the list), and a read started when the stored list is missing or older
//!   than [`TEAMS_MAX_AGE`], unless one is under way or one failed less than the engine's
//!   [`EngineOptions::teams_retry`](crate::EngineOptions::teams_retry) ago. Its result is stored whole; a list that
//!   changed is told as [`Event::Teams`](crate::Event::Teams).
//! - **No write**: a channel has no sealed write type (`teams-proto`), so nothing the native client sends can post,
//!   reply or react there; the fence would deny one besides (a channel's members are never read complete).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teams_proto::{Client, Credentials, CsaAnswerError, CsaTeams, HttpError, HttpTransport};
use teams_store::{Applied, Store, StoreError, Team, TeamChannel};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// How old the stored teams may get before a view's asking reads them again: an hour.
pub const TEAMS_MAX_AGE: Duration = Duration::from_hours(1);

/// How long after a read that failed the teams are read again, at the soonest: five minutes.
pub const TEAMS_RETRY: Duration = Duration::from_mins(5);

/// Why the teams couldn't be read.
#[derive(Debug, thiserror::Error)]
pub enum TeamsReadError {
    #[error(transparent)]
    Http(#[from] HttpError),
    #[error(transparent)]
    Answer(#[from] CsaAnswerError),
    /// The read's task ended without an answer (it panicked or was cancelled).
    #[error("the read ended without an answer")]
    Lost,
}

/// The background read of the teams.
pub(crate) struct Teams<C> {
    client: Arc<Client<HttpTransport<C>>>,
    running: JoinSet<Result<Vec<Team>, TeamsReadError>>,
    /// After a failed read: not before then.
    retry_at: Option<Instant>,
    retry: Duration,
}

impl<C: Credentials + 'static> Teams<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, retry: Duration) -> Self {
        Self {
            client,
            running: JoinSet::new(),
            retry_at: None,
            retry,
        }
    }

    /// The stored teams (`None` before the first read settled them); a read starts when they are due one (see the
    /// module docs).
    pub(crate) fn teams(&mut self, store: &Store) -> Result<Option<Vec<Team>>, StoreError> {
        let read_at = store.teams_read_at()?;
        let old = read_at.is_none_or(|read_at| {
            Duration::from_millis(u64::try_from(now_ms().saturating_sub(read_at)).unwrap_or(0)) > TEAMS_MAX_AGE
        });
        let waiting = self.retry_at.is_some_and(|at| Instant::now() < at);
        if old && !waiting && self.running.is_empty() {
            let client = Arc::clone(&self.client);
            self.running.spawn(async move { read(&client).await });
        }
        match read_at {
            Some(_) => store.teams().map(Some),
            None => Ok(None),
        }
    }

    /// Whether a read runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The read's result once it is over; never while none runs ([`Self::busy`]).
    pub(crate) async fn next(&mut self) -> Option<Result<Vec<Team>, TeamsReadError>> {
        let joined = self.running.join_next().await?;
        Some(joined.unwrap_or(Err(TeamsReadError::Lost)))
    }

    /// Stores a read's teams; whether the list changed. A failed read waits [`Self::retry`] before the next.
    pub(crate) fn store(
        &mut self,
        store: &mut Store,
        read: Result<Vec<Team>, TeamsReadError>,
    ) -> Result<Result<bool, TeamsReadError>, StoreError> {
        match read {
            Ok(teams) => {
                self.retry_at = None;
                let applied = store.replace_teams(&teams, now_ms())?;
                Ok(Ok(applied != Applied::Ignored))
            }
            Err(error) => {
                self.retry_at = Some(Instant::now() + self.retry);
                Ok(Err(error))
            }
        }
    }
}

/// CSA's teams read, as the store keeps its teams.
async fn read<C: Credentials>(client: &Client<HttpTransport<C>>) -> Result<Vec<Team>, TeamsReadError> {
    let answer = client.read(&CsaTeams).await?;
    let read = CsaTeams::read_answer(answer.body())?;
    Ok(read
        .teams
        .into_iter()
        .map(|team| Team {
            id: team.id,
            name: team.name,
            archived: team.archived,
            channels: team
                .channels
                .into_iter()
                .map(|channel| TeamChannel {
                    id: channel.id,
                    name: channel.name,
                    general: channel.general,
                })
                .collect(),
        })
        .collect())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
