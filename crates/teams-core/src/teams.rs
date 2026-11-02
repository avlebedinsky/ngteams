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
//! - **And the favourites (chats and channels)**: the same answer's `Favorites` folder (the chats and channels Teams
//!   lists under "Favorites" above its chats, in the user's order), stored with the teams when it could be read
//!   ([`TeamsRead::favorites`]; a read that couldn't keeps the stored ones), a change told as
//!   [`Event::Teams`](crate::Event::Teams) too. The chat list asks for them
//!   ([`EngineRequests::chat_listing`](crate::EngineRequests::chat_listing)), which starts a read as asking for the
//!   teams does.
//! - **Writes**: a new channel's are [`crate::channels`]'; nothing here writes, and a chat write the engine takes for a channel's id (a post, a read receipt:
//!   sealed chat writes aimed at the channel) is denied by the fence, which never reads a channel's members complete.
//!   No write is mapped to `WriteKind::ChannelPost`. The fence, not a missing write type, keeps channels unwritten.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teams_proto::{ChannelType, Client, Credentials, CsaAnswerError, CsaTeams, HttpError, HttpTransport};
use teams_store::{Applied, ChannelKind, Store, StoreError, Team, TeamChannel};

use crate::channels::Created;
use tokio::task::JoinSet;
use tokio::time::Instant;

/// How old the stored teams may get before a view's asking reads them again: an hour.
pub const TEAMS_MAX_AGE: Duration = Duration::from_hours(1);

/// How long after a read that failed the teams are read again, at the soonest: five minutes.
pub const TEAMS_RETRY: Duration = Duration::from_mins(5);

/// What a failed read of the teams is told as (`Event::Failed`'s `during`): the Teams list says the teams can't be
/// read while none was ever stored.
pub const READING_TEAMS: &str = "reading the teams";

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

/// What the chat list shows besides the stored conversations ([`EngineRequests::chat_listing`](crate::EngineRequests::chat_listing)).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatListing {
    /// The favourites' ids (chats and channels), in the order Teams lists them under "Favorites" (`48:notes` is the
    /// account's own notes, its chat with itself); `None` before a read could read them.
    pub favorites: Option<Vec<String>>,
    /// The conversations the chat service last called empty (`isemptyconversation`): Teams lists none of them.
    pub empty: Vec<String>,
}

/// What one read of the teams gave: the teams, and the favourites (chats and channels) when the answer's folders could
/// be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TeamsRead {
    pub(crate) teams: Vec<Team>,
    pub(crate) favorites: Option<Vec<String>>,
}

/// How long a channel just made is kept in the list when a read doesn't name it yet.
const CREATED_KEPT: Duration = Duration::from_mins(10);

/// The background read of the teams.
pub(crate) struct Teams<C> {
    client: Arc<Client<HttpTransport<C>>>,
    running: JoinSet<Result<TeamsRead, TeamsReadError>>,
    /// After a failed read: not before then.
    retry_at: Option<Instant>,
    retry: Duration,
    /// The channels made here that no read has named yet, until when they are kept.
    created: Vec<(Created, Instant)>,
    /// A channel was made while a read ran: another read follows it, which may name it.
    read_again: bool,
}

impl<C: Credentials + 'static> Teams<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, retry: Duration) -> Self {
        Self {
            client,
            running: JoinSet::new(),
            retry_at: None,
            retry,
            created: Vec::new(),
            read_again: false,
        }
    }

    /// Stores a channel just made in its team's list, and reads the list again: the service's own list may not name it
    /// yet, so it is kept against reads that don't ([`CREATED_KEPT`]). Whether the stored list changed.
    pub(crate) fn note_created(&mut self, store: &mut Store, created: Created) -> Result<bool, StoreError> {
        self.created.push((created, Instant::now() + CREATED_KEPT));
        let mut teams = store.teams()?;
        keep_created(&mut self.created, &mut teams);
        let read_at = store.teams_read_at()?.unwrap_or_else(now_ms);
        let applied = store.replace_teams(&teams, read_at)?;
        self.read_now();
        Ok(applied != Applied::Ignored)
    }

    /// Starts a read now, whatever the stored list's age, or one more after the running one.
    fn read_now(&mut self) {
        if self.running.is_empty() {
            let client = Arc::clone(&self.client);
            self.running.spawn(async move { read(&client).await });
        } else {
            self.read_again = true;
        }
    }

    /// What the chat list needs besides the conversations: the stored favourite chats and channels (`None` before a
    /// read could read them) and the conversations the chat service calls empty; a read starts when the teams are due
    /// one, as [`Self::teams`] starts it.
    pub(crate) fn chat_listing(&mut self, store: &Store) -> Result<ChatListing, StoreError> {
        self.read_if_due(store)?;
        stored_listing(store)
    }

    /// The stored teams (`None` before the first read settled them); a read starts when they are due one (see the
    /// module docs).
    pub(crate) fn teams(&mut self, store: &Store) -> Result<Option<Vec<Team>>, StoreError> {
        self.read_if_due(store)?;
        stored_teams(store)
    }

    /// Starts a read when the stored teams are missing or old, unless one runs or a failed one's wait does.
    fn read_if_due(&mut self, store: &Store) -> Result<(), StoreError> {
        let old = store.teams_read_at()?.is_none_or(|read_at| {
            Duration::from_millis(u64::try_from(now_ms().saturating_sub(read_at)).unwrap_or(0)) > TEAMS_MAX_AGE
        });
        let waiting = self.retry_at.is_some_and(|at| Instant::now() < at);
        if old && !waiting && self.running.is_empty() {
            let client = Arc::clone(&self.client);
            self.running.spawn(async move { read(&client).await });
        }
        Ok(())
    }

    /// Whether a read runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The read's result once it is over; never while none runs ([`Self::busy`]).
    pub(crate) async fn next(&mut self) -> Option<Result<TeamsRead, TeamsReadError>> {
        let joined = self.running.join_next().await?;
        Some(joined.unwrap_or(Err(TeamsReadError::Lost)))
    }

    /// Stores a read's teams and favourite chats and channels (these only when it could read them); whether either
    /// changed. A failed read waits [`Self::retry`] before the next.
    pub(crate) fn store(
        &mut self,
        store: &mut Store,
        read: Result<TeamsRead, TeamsReadError>,
    ) -> Result<Result<bool, TeamsReadError>, StoreError> {
        match read {
            Ok(read) => {
                self.retry_at = None;
                let now = now_ms();
                let mut listed = read.teams;
                keep_created(&mut self.created, &mut listed);
                let teams = store.replace_teams(&listed, now)?;
                let favorites = match &read.favorites {
                    Some(favorites) => store.replace_favorites(favorites, now)?,
                    None => Applied::Ignored,
                };
                if std::mem::take(&mut self.read_again) {
                    self.read_now();
                }
                Ok(Ok(teams != Applied::Ignored || favorites != Applied::Ignored))
            }
            Err(error) => {
                self.retry_at = Some(Instant::now() + self.retry);
                Ok(Err(error))
            }
        }
    }
}

/// Puts into `teams` each channel just made that they don't name, in its team's list by name after the general one;
/// one a read names now, or kept long enough, is let go.
fn keep_created(created: &mut Vec<(Created, Instant)>, teams: &mut [Team]) {
    let now = Instant::now();
    created.retain(|(made, until)| {
        let Some(team) = teams.iter_mut().find(|team| team.id == made.team) else {
            return *until > now;
        };
        if team.channels.iter().any(|channel| channel.id == made.channel.id) {
            return false;
        }
        if *until <= now {
            return false;
        }
        let name = made.channel.name.to_lowercase();
        let at = team
            .channels
            .iter()
            .position(|channel| !channel.general && channel.name.to_lowercase() > name)
            .unwrap_or(team.channels.len());
        team.channels.insert(at, made.channel.clone());
        true
    });
}

/// The stored teams (`None` before the first read settled them); nothing is read.
pub(crate) fn stored_teams(store: &Store) -> Result<Option<Vec<Team>>, StoreError> {
    match store.teams_read_at()? {
        Some(_) => store.teams().map(Some),
        None => Ok(None),
    }
}

/// What the chat list needs besides the conversations, as stored ([`ChatListing`]); nothing is read.
pub(crate) fn stored_listing(store: &Store) -> Result<ChatListing, StoreError> {
    Ok(ChatListing {
        favorites: store.favorites()?,
        empty: store.empty_conversations()?,
    })
}

/// CSA's teams read, as the store keeps its teams and favourites (chats and channels).
async fn read<C: Credentials>(client: &Client<HttpTransport<C>>) -> Result<TeamsRead, TeamsReadError> {
    let answer = client.read(&CsaTeams).await?;
    let read = CsaTeams::read_answer(answer.body())?;
    let teams = read
        .teams
        .into_iter()
        .map(|team| Team {
            id: team.id,
            name: team.name,
            archived: team.archived,
            group_id: team.group_id,
            mailbox: team.mailbox,
            owner: team.owner,
            can_add_channel: team.can_add_channel,
            can_add_private_channel: team.can_add_private_channel,
            channels: team
                .channels
                .into_iter()
                .map(|channel| TeamChannel {
                    id: channel.id,
                    name: channel.name,
                    general: channel.general,
                    kind: match channel.kind {
                        ChannelType::Private => ChannelKind::Private,
                        ChannelType::Shared => ChannelKind::Shared,
                        ChannelType::Standard | ChannelType::Unknown => ChannelKind::Standard,
                    },
                })
                .collect(),
        })
        .collect();
    Ok(TeamsRead {
        teams,
        favorites: read.favorites,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
