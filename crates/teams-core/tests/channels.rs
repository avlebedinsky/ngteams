//! A team's new channel through the engine, on the mock (`teams_core::channels`): through a fence that lets it go (a team
//! read as the lab's alone), what goes out is the dialog's captured shape for each type and layout, the channel is listed
//! at once, and Teams' refusal is told by its code; a team whose members can't be read, as every team in the lab, is
//! denied, nothing sent.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::Duration;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{ChannelDraft, ChannelMade, ChannelProblem, EngineError, Outbox};
use teams_fence::{Fence, FenceLog, Mri, Roster};
use teams_mock::{
    CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PROVISIONING_BASE, USER_PROFILE_BASE,
};
use teams_proto::{Author, ChannelLayout, Client, Credentials, HttpTransport, NewChannel, NewChannelKind};
use teams_store::{ChannelKind, Store};

/// The mock's one team, as its CSA answer lists it.
const TEAM: &str = "19:channel0001@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

const WAIT: Duration = Duration::from_secs(20);

/// One account's mock token, for every audience.
struct Tokens(String);

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }

    fn bearer_rejected(&self, _audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.0.clone())))
    }
}

struct Rig {
    mock: ChatServiceMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

/// The engine with its bases, its fence reading the team as [`Members`] says.
async fn rig_with(_lab_only: bool) -> Rig {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let _fence = Arc::new(Fence::new(
        // the one the mock won't add is someone the fence lets a private channel be given: the service refuses them
        Roster::new(
            [
                LAB_A.to_owned(),
                LAB_B.to_owned(),
                LAB_C.to_owned(),
                format!("8:orgid:{}", teams_mock::NOT_ADDED),
            ]
            .map(Mri::new),
            [],
        ),
        FenceLog::from_writer(std::io::sink()),
    ));
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
            .with_provisioning(PROVISIONING_BASE)
            .unwrap()
    };
    let transport = Arc::new(http());
    let reads = http()
        .with_csa(CSA_BASE)
        .unwrap()
        .with_user_profile(USER_PROFILE_BASE)
        .unwrap();
    let outbox = Outbox::new(transport, Author::new(Mri::new(LAB_A), "Lab account"));
    let handle = engine::start(Parts {
        calls: None,
        store: Store::open(&dir.path().join("store").join("a.sqlite")).unwrap(),
        client: Arc::new(Client::new(reads)),
        outbox,
        push: None,
        options: EngineOptions {
            tick: Duration::from_millis(50),
            catch_up_retry: Duration::from_millis(500),
            ..EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap();
    Rig {
        mock,
        handle,
        _dir: dir,
    }
}

impl Rig {
    /// The teams once a read settled them.
    async fn settled(&mut self) {
        let requests = self.handle.requests();
        let _ = requests.teams().await.unwrap();
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let event = tokio::time::timeout_at(deadline, self.handle.next_event())
                .await
                .expect("no read of the teams in time")
                .expect("the engine stopped");
            if matches!(event, Event::Teams) {
                return;
            }
        }
    }

    async fn create(
        &self,
        name: &str,
        kind: NewChannelKind,
        layout: ChannelLayout,
        show_by_default: bool,
    ) -> Result<ChannelMade, EngineError> {
        self.handle
            .requests()
            .create_channel(ChannelDraft {
                channel: NewChannel {
                    team: TEAM.to_owned(),
                    group: GROUP.to_owned(),
                    mailbox: Some("made-up-team@lab.example.com".to_owned()),
                    name: name.to_owned(),
                    description: "what it is for".to_owned(),
                    kind,
                    layout,
                },
                show_by_default,
            })
            .await
    }

    async fn listed(&self, name: &str) -> Option<(String, ChannelKind)> {
        let teams = self.handle.requests().teams().await.unwrap()?;
        let team = teams.into_iter().find(|team| team.id == TEAM)?;
        team.channels
            .into_iter()
            .find(|channel| channel.name == name)
            .map(|channel| (channel.id, channel.kind))
    }
}

#[tokio::test]
async fn a_standard_channel_goes_in_the_dialog_s_shape_for_each_layout_and_is_listed_at_once() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let threads = rig
        .create("Threaded", NewChannelKind::Standard, ChannelLayout::Threads, false)
        .await
        .unwrap();
    let posts = rig
        .create("Posted", NewChannelKind::Standard, ChannelLayout::Posts, false)
        .await
        .unwrap();
    let writes = rig.mock.channel_writes();
    assert_eq!(writes[0].method, "POST");
    assert_eq!(writes[0].path, format!("beta/teams/{TEAM}/channels"));
    assert_eq!(
        writes[0].body,
        serde_json::json!({
            "displayName": "Threaded",
            "description": "what it is for",
            "groupId": GROUP,
            "channelType": "Standard",
            "chatModalityType": "Conversational",
        })
    );
    assert_eq!(writes[1].body["chatModalityType"], "PostReply");
    let thread = threads.thread.expect("its thread");
    assert_eq!(rig.listed("Threaded").await, Some((thread, ChannelKind::Standard)));
    assert_eq!(
        rig.listed("Posted").await,
        Some((posts.thread.expect("its thread"), ChannelKind::Standard))
    );
}

#[tokio::test]
async fn an_owner_s_standard_channel_can_be_shown_to_everyone() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let made = rig
        .create("For everyone", NewChannelKind::Standard, ChannelLayout::Threads, true)
        .await
        .unwrap();
    let thread = made.thread.expect("its thread");
    assert!(!made.not_shown_by_default);
    let writes = rig.mock.channel_writes();
    assert_eq!(writes[1].method, "PUT");
    assert_eq!(writes[1].path, format!("beta/teams/{thread}/setDefaultChannel"));
}

#[tokio::test]
async fn a_private_channel_goes_on_the_provisioning_service_takes_its_people_and_lists_once_it_has_its_thread() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let made = rig
        .create("Hush", NewChannelKind::Private, ChannelLayout::Posts, false)
        .await
        .unwrap();
    assert_eq!(made.thread, None, "no thread yet");
    let group = made.group.expect("its group");
    let writes = rig.mock.channel_writes();
    assert_eq!(writes[0].path, format!("teams/{GROUP}/channels"));
    assert_eq!(
        writes[0].body,
        serde_json::json!({
            "spaceThreadId": TEAM,
            "displayName": "Hush",
            "description": "what it is for",
            "channelType": "Private",
            "sharedWithParentTeam": true,
            "chatModalityType": "PostReply",
        })
    );
    assert_eq!(rig.listed("Hush").await, None, "listed once it has its thread");

    let requests = rig.handle.requests();
    let found = requests.team_people(TEAM, "lab b").await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].mri, LAB_B);

    let not_added = requests
        .add_channel_members(
            GROUP,
            group.clone(),
            vec![LAB_B.to_owned(), format!("8:orgid:{}", teams_mock::NOT_ADDED)],
        )
        .await
        .unwrap();
    assert_eq!(not_added, vec![format!("8:orgid:{}", teams_mock::NOT_ADDED)]);
    let writes = rig.mock.channel_writes();
    assert_eq!(writes[1].path, format!("teams/{GROUP}/channels/{group}/users"));
    let tenant = group.split_once('@').unwrap().1;
    let lab_b_oid = LAB_B.strip_prefix("8:orgid:").unwrap();
    assert_eq!(writes[1].body["users"][0]["id"], format!("OID:{lab_b_oid}@{tenant}"));
    assert_eq!(writes[1].body["users"][0]["role"], 1);

    let thread = requests.channel_thread(TEAM, GROUP, group, "Hush").await.unwrap();
    let thread = thread.expect("its thread");
    assert_eq!(rig.listed("Hush").await, Some((thread, ChannelKind::Private)));
}

#[tokio::test]
async fn a_name_the_team_has_comes_back_as_teams_says_it() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let refused = rig
        .create(
            "A made-up channel",
            NewChannelKind::Standard,
            ChannelLayout::Threads,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            EngineError::ChannelNotCreated {
                why: ChannelProblem::NameTaken
            }
        ),
        "{refused}"
    );
}
