//! A team managed (`docs/protocol/teams.md`): what the team pages ask of the engine, each as the web client's pages do it.
//!
//! - **Writes** ([`TeamWrite`]): sealed `teams-proto` writes through the outbox's one-attempt kind
//!   ([`crate::outbox::Outbox::try_channel`]), as a new channel's: a fresh `Fence::check` with its `fence.log` line for
//!   every attempt, no row, nothing sent again by a later run, one more attempt only after a 401 or a short throttle. An
//!   answer that isn't a 2xx is [`EngineError::TeamNotChanged`] with Teams' code.
//! - **The account's own view** ([`OwnTeamView`]): its channels hidden or shown, which reaches nobody, sent as plain
//!   requests.
//! - **The list** ([`TeamChange`]): what a write changed goes into the stored teams at once, through the engine's loop, and
//!   is kept against reads that don't show it yet ([`crate::teams::Teams`]), since CSA's list follows a change late.
//! - **Reads** ([`TeamAsk`]): read when a page opens, kept nowhere: a team's people, settings, tags, code, channels and
//!   analytics, the invitations waiting, the teams one can join, the people one can add.

use std::sync::Arc;

use teams_fence::Mri;
use teams_proto::{
    AddablePeopleSearch, AllTeamsAnalyticsRead, AnalyticsPeriod, Client, Credentials, HttpError, HttpTransport,
    JoinableTeam, JoinableTeams, ManagedChannel, Mutating, OwnTeamView, PastedFormat, PastedPicture, PendingInvite,
    PendingInvites, RosterMember, ShortProfileLookup, TagsRead, TeamAnalytics, TeamAnalyticsRead, TeamChannels,
    TeamCode, TeamJoinRequests, TeamPerson, TeamPicture, TeamRoster, TeamTags, TeamThread, TeamWrite, Transport,
    team_created, team_error_code,
};
use teams_store::{Team, TeamChannel};

use crate::channels::{Channels, Finished};
use crate::engine::{EngineError, Responder};
use crate::outbox::{CallRetries, CallTried, Outbox};
use crate::pictures::PictureBytes;

/// What a write changed, for the stored teams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamChange {
    /// A new team.
    Created(Team),
    /// A team deleted, or left.
    Gone(String),
    /// A team's name, description and privacy.
    Edited {
        team: String,
        name: Option<String>,
        description: String,
        public: Option<bool>,
    },
    /// A team archived, or restored.
    Archived { team: String, archived: bool },
    /// All a team's channels hidden, or the team shown again.
    Hidden { team: String, hidden: bool },
    /// One channel shown, or hidden.
    ChannelShown { team: String, channel: String, shown: bool },
}

impl TeamChange {
    /// Whether `teams` show it already.
    fn shown_in(&self, teams: &[Team]) -> bool {
        let of = |id: &str| teams.iter().find(|team| team.id == id);
        match self {
            Self::Created(team) => of(&team.id).is_some(),
            Self::Gone(team) => of(team).is_none(),
            Self::Edited {
                team,
                name,
                description,
                public,
            } => of(team).is_some_and(|team| {
                name.as_ref().is_none_or(|name| *name == team.name)
                    && team.about.description == *description
                    && public.is_none_or(|public| public == team.about.public)
            }),
            Self::Archived { team, archived } => of(team).is_some_and(|team| team.archived == *archived),
            Self::Hidden { team, hidden } => of(team).is_some_and(|team| team.about.hidden == *hidden),
            Self::ChannelShown { team, channel, shown } => of(team)
                .and_then(|team| team.channels.iter().find(|listed| listed.id == *channel))
                .is_some_and(|listed| listed.shown == *shown),
        }
    }

    /// Puts it into `teams`.
    pub(crate) fn apply(&self, teams: &mut Vec<Team>) {
        match self {
            Self::Created(team) => {
                if !teams.iter().any(|listed| listed.id == team.id) {
                    teams.push(team.clone());
                }
            }
            Self::Gone(team) => teams.retain(|listed| listed.id != *team),
            Self::Edited {
                team,
                name,
                description,
                public,
            } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    if let Some(name) = name {
                        listed.name.clone_from(name);
                    }
                    listed.about.description.clone_from(description);
                    if let Some(public) = public {
                        listed.about.public = *public;
                    }
                }
            }
            Self::Archived { team, archived } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    listed.archived = *archived;
                }
            }
            Self::Hidden { team, hidden } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    listed.about.hidden = *hidden;
                    if *hidden {
                        listed.channels.iter_mut().for_each(|channel| channel.shown = false);
                    } else if let Some(general) = listed.channels.iter_mut().find(|channel| channel.general) {
                        general.shown = true;
                    }
                }
            }
            Self::ChannelShown { team, channel, shown } => {
                if let Some(listed) = teams.iter_mut().find(|listed| listed.id == *team) {
                    if let Some(entry) = listed.channels.iter_mut().find(|entry| entry.id == *channel) {
                        entry.shown = *shown;
                    }
                    if *shown {
                        listed.about.hidden = false;
                    } else if listed.channels.iter().all(|entry: &TeamChannel| !entry.shown) {
                        listed.about.hidden = true;
                    }
                }
            }
        }
    }
}

/// Puts into `teams` each change they don't show yet; one they show, or kept long enough, is let go.
pub(crate) fn keep_changes(changes: &mut Vec<(TeamChange, tokio::time::Instant)>, teams: &mut Vec<Team>) {
    let now = tokio::time::Instant::now();
    changes.retain(|(change, until)| {
        if *until <= now || change.shown_in(teams) {
            return false;
        }
        change.apply(teams);
        true
    });
}

/// A write's answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamWritten {
    /// A new team's thread and group.
    pub created: Option<(String, Option<String>)>,
    /// The people the answer says weren't added.
    pub not_added: Vec<String>,
}

/// Someone on a team, as Manage team lists them. `Debug` leaves out everything but the MRI.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub mri: String,
    pub name: String,
    pub email: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
    pub owner: bool,
    pub guest: bool,
}

impl std::fmt::Debug for TeamMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamMember")
            .field("mri", &self.mri)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

/// What the team pages ask (see the module docs).
pub(crate) enum TeamAsk {
    Write {
        write: TeamWrite,
        change: Option<TeamChange>,
        reply: Responder<TeamWritten>,
    },
    OwnView {
        requests: Vec<OwnTeamView>,
        change: Option<TeamChange>,
        reply: Responder<()>,
    },
    Roster {
        team: String,
        reply: Responder<Vec<TeamMember>>,
    },
    Settings {
        team: String,
        reply: Responder<Option<String>>,
    },
    Tags {
        team: String,
        reply: Responder<TagsRead>,
    },
    Code {
        team: String,
        reply: Responder<Option<String>>,
    },
    Channels {
        group: String,
        reply: Responder<Vec<ManagedChannel>>,
    },
    Invites {
        reply: Responder<Vec<PendingInvite>>,
    },
    Analytics {
        group: String,
        period: AnalyticsPeriod,
        reply: Responder<Option<TeamAnalytics>>,
    },
    AllAnalytics {
        period: AnalyticsPeriod,
        reply: Responder<Vec<TeamAnalytics>>,
    },
    Joinable {
        keywords: Option<String>,
        reply: Responder<Vec<JoinableTeam>>,
    },
    Addable {
        keyword: String,
        reply: Responder<Vec<TeamPerson>>,
    },
    JoinRequests {
        group: String,
        reply: Responder<Vec<TeamMember>>,
    },
    Picture {
        team: TeamPicture,
        reply: Responder<Option<PictureBytes>>,
    },
}

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    /// Answers `ask`.
    pub(crate) fn team_ask(&mut self, ask: TeamAsk) {
        match ask {
            TeamAsk::Write { write, change, reply } => self.team_write(write, change, reply),
            TeamAsk::OwnView {
                requests,
                change,
                reply,
            } => self.own_view(requests, change, reply),
            TeamAsk::Roster { team, reply } => self.spawn_read(reply, async move |client| roster(client, team).await),
            TeamAsk::Settings { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamThread { team })
                    .await
                    .and_then(|body| TeamThread::read_settings(&body))
            }),
            TeamAsk::Tags { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamTags { team })
                    .await
                    .map_or_else(TagsRead::default, |body| TeamTags::read_answer(&body))
            }),
            TeamAsk::Code { team, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamCode { team })
                    .await
                    .and_then(|body| TeamCode::read_answer(&body))
            }),
            TeamAsk::Channels { group, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamChannels { group })
                    .await
                    .map_or_else(Vec::new, |body| TeamChannels::read_answer(&body))
            }),
            TeamAsk::Invites { reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &PendingInvites)
                    .await
                    .map_or_else(Vec::new, |body| PendingInvites::read_answer(&body))
            }),
            TeamAsk::Analytics { group, period, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &TeamAnalyticsRead { group, period })
                    .await
                    .and_then(|body| TeamAnalyticsRead::read_answer(&body))
            }),
            TeamAsk::AllAnalytics { period, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &AllTeamsAnalyticsRead { period })
                    .await
                    .map_or_else(Vec::new, |body| AllTeamsAnalyticsRead::read_answer(&body))
            }),
            TeamAsk::Joinable { keywords, reply } => self.spawn_read(reply, async move |client| {
                read_body(client, &JoinableTeams { keywords })
                    .await
                    .map_or_else(Vec::new, |body| JoinableTeams::read_answer(&body))
            }),
            TeamAsk::Addable { keyword, reply } => self.spawn_read(reply, async move |client| {
                send_body(client, &AddablePeopleSearch { keyword })
                    .await
                    .map_or_else(Vec::new, |body| AddablePeopleSearch::read_answer(&body))
            }),
            TeamAsk::JoinRequests { group, reply } => self.spawn_read(reply, async move |client| {
                let asking = send_body(client, &TeamJoinRequests { group })
                    .await
                    .map_or_else(Vec::new, |body| TeamJoinRequests::read_answer(&body));
                named(client, asking.into_iter().map(|mri| (mri, false, false)).collect()).await
            }),
            TeamAsk::Picture { team, reply } => self.spawn_read(reply, async move |client| {
                let answer = client.read(&team).await.ok()?;
                let format = picture_format(answer.body())?;
                Some(Arc::new(PastedPicture {
                    format,
                    bytes: answer.body().to_vec(),
                }))
            }),
        }
    }

    /// Runs `read` beside the engine's loop and answers `reply` with what it found.
    pub(crate) fn spawn_read<V, F>(&self, reply: Responder<V>, read: F)
    where
        V: Send + 'static,
        F: AsyncFnOnce(&Client<HttpTransport<C>>) -> V + 'static,
    {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let found = read(&client).await;
            let _ = reply.send(Ok(found));
        });
    }

    fn team_write(&mut self, write: TeamWrite, change: Option<TeamChange>, reply: Responder<TeamWritten>) {
        let outbox = Arc::clone(&self.outbox);
        self.running.spawn_local(async move {
            let sent = send(&outbox, &write).await;
            let answer = sent.map(|body| written(&write, &body));
            let change = change_of(answer.as_ref().ok(), change);
            Finished {
                created: None,
                team: answer.is_ok().then_some(change).flatten().map(Box::new),
                reread: answer.is_ok(),
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }

    fn own_view(&mut self, requests: Vec<OwnTeamView>, change: Option<TeamChange>, reply: Responder<()>) {
        let client = Arc::clone(&self.client);
        self.running.spawn_local(async move {
            let mut failed = None;
            for request in &requests {
                match client.transport().send(request).await {
                    Ok(answer) if (200..300).contains(&answer.status()) => {}
                    Ok(answer) => failed = Some(format!("{}", answer.status())),
                    Err(error) => failed = Some(error.to_string()),
                }
            }
            let answer = match failed {
                Some(why) => {
                    log::info!("the account's view of a team wasn't changed: {why}");
                    Err(EngineError::TeamNotChanged { code: None })
                }
                None => Ok(()),
            };
            Finished {
                created: None,
                team: answer.is_ok().then_some(change).flatten().map(Box::new),
                reread: answer.is_ok(),
                reply: Box::new(move || {
                    let _ = reply.send(answer);
                }),
            }
        });
    }
}

/// The change for the stored teams: a new team's, with the thread and group its answer names, once it has them.
fn change_of(written: Option<&TeamWritten>, change: Option<TeamChange>) -> Option<TeamChange> {
    let (
        Some(TeamWritten {
            created: Some((thread, group)),
            ..
        }),
        Some(TeamChange::Created(team)),
    ) = (written, &change)
    else {
        return change;
    };
    let mut team = team.clone();
    team.id.clone_from(thread);
    team.group_id.clone_from(group);
    if let Some(general) = team.channels.iter_mut().find(|channel| channel.general) {
        general.id.clone_from(thread);
    }
    Some(TeamChange::Created(team))
}

/// What a write's answer `body` says.
fn written(write: &TeamWrite, body: &[u8]) -> TeamWritten {
    match write {
        TeamWrite::Create(_) => TeamWritten {
            created: team_created(body),
            not_added: Vec::new(),
        },
        TeamWrite::AddMembers { .. } => TeamWritten {
            created: None,
            not_added: not_added(body),
        },
        _ => TeamWritten::default(),
    }
}

/// The people an answer adding them says weren't added: `value.failedMembers`, by the bundle's reading of it, or
/// `updatedMembersError`.
fn not_added(body: &[u8]) -> Vec<String> {
    let Ok(answer) = serde_json::from_slice::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let value = answer.get("value").unwrap_or(&answer);
    ["failedMembers", "updatedMembersError", "errors"]
        .iter()
        .filter_map(|key| value.get(*key))
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(|failed| {
            failed
                .as_str()
                .or_else(|| failed.get("id").and_then(serde_json::Value::as_str))
                .or_else(|| failed.get("mri").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

/// One write, again only as [`CallRetries`] says: its answer's body on a 2xx.
async fn send<T, R>(outbox: &Outbox<T>, write: &R) -> Result<Vec<u8>, EngineError>
where
    T: Transport<Error = HttpError>,
    R: Mutating + Clone + Send,
{
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_channel(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) if (200..300).contains(&answer.status()) => Ok(answer.body().to_vec()),
            CallTried::Answered(answer) => {
                let code = team_error_code(answer.body());
                log::info!(
                    "a team's write was refused: {} {}",
                    answer.status(),
                    code.as_deref().unwrap_or("no code")
                );
                Err(EngineError::TeamNotChanged { code })
            }
            CallTried::Denied(reason) => {
                log::info!("a team's write was refused: {reason}");
                Err(EngineError::TeamNotChanged { code: None })
            }
            CallTried::NotSent(why) | CallTried::Uncertain(why) => {
                log::info!("a team's write didn't go: {why}");
                Err(EngineError::TeamNotChanged { code: None })
            }
            CallTried::Throttled(_) => Err(EngineError::TeamNotChanged { code: None }),
        };
    }
}

pub(crate) async fn read_body<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    read: &(impl teams_proto::Read + Sync),
) -> Option<Vec<u8>> {
    match client.read(read).await {
        Ok(answer) => Some(answer.body().to_vec()),
        Err(error) => {
            log::info!("a team page's read failed: {error}");
            None
        }
    }
}

pub(crate) async fn send_body<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    request: &(impl teams_proto::Request + Sync),
) -> Option<Vec<u8>> {
    match client.transport().send(request).await {
        Ok(answer) if (200..300).contains(&answer.status()) => Some(answer.body().to_vec()),
        Ok(answer) => {
            log::info!("a team page's read answered {}", answer.status());
            None
        }
        Err(error) => {
            log::info!("a team page's read failed: {error}");
            None
        }
    }
}

/// A team's people, owners first, with their names; bots left out, as the web client's list does.
async fn roster<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, team: String) -> Vec<TeamMember> {
    let mut listed: Vec<RosterMember> = Vec::new();
    for owners in [true, false] {
        let read = TeamRoster {
            team: team.clone(),
            owners,
        };
        if let Some(body) = read_body(client, &read).await {
            listed.extend(TeamRoster::read_answer(&body).into_iter().filter(|member| !member.bot));
        }
    }
    named(
        client,
        listed
            .into_iter()
            .map(|member| (member.mri, member.owner, member.guest))
            .collect(),
    )
    .await
}

/// `people` (MRI, owner, guest) with their names, from their profiles read ([`ShortProfileLookup`], a read by `POST`).
async fn named<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    people: Vec<(String, bool, bool)>,
) -> Vec<TeamMember> {
    let mut profiles = Vec::new();
    for page in people.chunks(100) {
        if let Some(lookup) = ShortProfileLookup::new(page.iter().map(|(mri, _, _)| Mri::new(mri.as_str()))) {
            match client.lookup(lookup.clone()).await {
                Ok(answer) => profiles.extend(lookup.read_answer(answer.body()).unwrap_or_default()),
                Err(error) => log::info!("a team's people's profiles couldn't be read: {error}"),
            }
        }
    }
    people
        .into_iter()
        .map(|(mri, owner, guest)| {
            let profile = profiles.iter().find(|profile| profile.mri.as_str() == mri);
            TeamMember {
                name: profile
                    .and_then(|profile| profile.display_name.clone())
                    .unwrap_or_else(|| "Unknown user".to_owned()),
                email: profile
                    .and_then(|profile| profile.email.clone().or_else(|| profile.user_principal_name.clone())),
                title: profile.and_then(|profile| profile.job_title.clone()),
                location: profile.and_then(|profile| profile.office_location.clone()),
                mri,
                owner,
                guest,
            }
        })
        .collect()
}

/// The format of a picture's bytes, by its first ones.
fn picture_format(bytes: &[u8]) -> Option<PastedFormat> {
    match bytes {
        [0xff, 0xd8, ..] => Some(PastedFormat::Jpeg),
        [0x89, b'P', b'N', b'G', ..] => Some(PastedFormat::Png),
        _ => None,
    }
}
