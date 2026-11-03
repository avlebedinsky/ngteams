//! The team pages through the engine, on the mock (`teams_core::team_admin`, `teams_core::team_apps`): each read goes
//! where the web client's page sends it and comes back as the page shows it, a failed one as nothing; a write managing
//! a team goes through the outbox under the fence, its answer read, Teams' refusal told by its code and a write the
//! fence denies never sent; the account's own view of a team changes the stored teams at once.
//!
//! Every id, name and token here is the mock's made-up one.

#![cfg(test)]

use std::future::{self, Future};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_core::engine::{self, EngineHandle, EngineOptions, Event, Parts};
use teams_core::{EngineError, Outbox, TeamChange};
use teams_fence::{Fence, FenceLog, Membership, MembershipFuture, MembershipSource, Mri, PartialReason, Roster, Scope};
use teams_mock::{
    CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PROVISIONING_BASE, USER_PROFILE_BASE,
};
use teams_proto::{
    AnalyticsPeriod, Author, Client, Credentials, HttpTransport, MemberUpdate, NewTeam, OwnTeamView, PastedFormat,
    TeamPicture, TeamRole, TeamWrite,
};
use teams_store::Store;

/// The mock's one team, as its CSA answer lists it.
const TEAM: &str = "19:channel0001@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";
const MT_PATH: &str = "/api/mt/part/amer-03";

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

/// What the fence reads of a team: the lab accounts alone, or, as in the lab, nothing it can take as complete.
struct Members {
    lab_only: bool,
}

impl MembershipSource for Members {
    fn members<'a>(&'a self, _scope: &'a Scope) -> MembershipFuture<'a> {
        let lab_only = self.lab_only;
        Box::pin(async move {
            if lab_only {
                Membership::Complete([LAB_A, LAB_B, LAB_C].map(Mri::new).to_vec())
            } else {
                Membership::Partial(PartialReason::Unsupported)
            }
        })
    }
}

struct Rig {
    mock: ChatServiceMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

async fn rig_with(lab_only: bool) -> Rig {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fence = Arc::new(Fence::new(
        Roster::new([LAB_A, LAB_B, LAB_C].map(Mri::new), []),
        FenceLog::from_writer(std::io::sink()),
    ));
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), Arc::clone(&fence), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
            .with_provisioning(PROVISIONING_BASE)
            .unwrap()
    };
    let transport = Arc::new(http().with_csa(CSA_BASE).unwrap());
    let reads = http()
        .with_csa(CSA_BASE)
        .unwrap()
        .with_user_profile(USER_PROFILE_BASE)
        .unwrap();
    let outbox = Outbox::new(
        transport,
        fence,
        Arc::new(Members { lab_only }),
        Author::new(Mri::new(LAB_A), "Lab account"),
    );
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
        let _ = self.handle.requests().teams().await.unwrap();
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

    /// The paths (below the origin) and query of every request the mock took so far that `pick` keeps.
    async fn sent(&self, pick: impl Fn(&str) -> bool) -> Vec<(String, String, String)> {
        self.mock
            .received_requests()
            .await
            .into_iter()
            .filter(|request| pick(request.url.path()))
            .map(|request| {
                (
                    request.method.to_string(),
                    request.url.path().to_owned(),
                    request.url.query().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    async fn stored_team(&self) -> teams_store::Team {
        let teams = self.handle.requests().teams().await.unwrap().expect("teams read");
        teams.into_iter().find(|team| team.id == TEAM).expect("the mock's team")
    }
}

#[tokio::test]
async fn a_team_s_people_come_owners_first_named_bots_left_out() {
    let rig = rig_with(true).await;
    // the mock keeps no profiles: the lookup is answered here, c's left out
    rig.mock
        .answer_next(
            "POST",
            "fetchShortProfile",
            200,
            &json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": [
                {"mri": LAB_A, "displayName": "Lab A", "email": "lab-a@lab.example.com"},
                {"mri": LAB_B, "displayName": "Lab B", "userPrincipalName": "lab-b@lab.example.com",
                 "jobTitle": "Made-up title", "officeLocation": "Made-up office"},
            ]}),
        )
        .await;
    let roster = rig.handle.requests().team_roster(TEAM).await.unwrap();
    let mris: Vec<_> = roster.iter().map(|member| member.mri.as_str()).collect();
    assert_eq!(mris, [LAB_A, LAB_B, LAB_C]);
    assert!(roster[0].owner && !roster[1].owner && !roster[2].owner);
    let names: Vec<_> = roster.iter().map(|member| member.name.as_str()).collect();
    assert_eq!(names, ["Lab A", "Lab B", "Unknown user"]);
    assert_eq!(roster[0].email.as_deref(), Some("lab-a@lab.example.com"));
    assert_eq!(roster[1].email.as_deref(), Some("lab-b@lab.example.com"));
    assert_eq!(roster[1].title.as_deref(), Some("Made-up title"));
    assert_eq!(roster[1].location.as_deref(), Some("Made-up office"));
    assert_eq!(roster[2].email, None);
    assert!(roster.iter().all(|member| !member.guest));
    assert!(roster.iter().all(|member| !member.name.is_empty()));
    let reads = rig
        .sent(|path| path.ends_with(&format!("threads/{TEAM}/members")))
        .await;
    assert_eq!(reads.len(), 2);
    assert!(reads[0].2.contains("selectMemberRoles=Admin"), "{reads:?}");
    assert!(reads[1].2.contains("selectMemberRoles=User%7CGuest"), "{reads:?}");
}

#[tokio::test]
async fn a_team_s_settings_tags_code_and_channels_are_read_as_the_pages_show_them() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    rig.mock
        .answer_next(
            "GET",
            &format!("threads/{TEAM}"),
            200,
            &json!({"properties": {"spaceAdminSettings": "{\"made\":\"up\"}"}}),
        )
        .await;
    assert_eq!(
        requests.team_settings(TEAM).await.unwrap().as_deref(),
        Some("{\"made\":\"up\"}")
    );

    // the mock's CSA answers only the teams read, so the tags are answered here
    rig.mock
        .answer_next(
            "GET",
            "tagCards",
            200,
            &json!({
                "tagCards": [{"id": "tag-1", "tagName": "Release", "tagDescription": "Made up", "memberCount": 2,
                              "version": 3}],
                "teamTagSettings": {"onlyOwnersCanUpdate": false, "enableAutomaticTags": true},
            }),
        )
        .await;
    let tags = requests.team_tags(TEAM).await.unwrap();
    assert_eq!(tags.tags.len(), 1);
    assert_eq!((tags.tags[0].name.as_str(), tags.tags[0].version), ("Release", 3));
    assert!(!tags.owners_only && tags.automatic);

    assert_eq!(requests.team_code(TEAM).await.unwrap(), None, "no code yet: a 204");
    rig.mock
        .answer_next("GET", "joinCode", 200, &json!({"joinCode": "madeup1"}))
        .await;
    assert_eq!(requests.team_code(TEAM).await.unwrap().as_deref(), Some("madeup1"));

    let channels = requests.team_channels(GROUP).await.unwrap();
    let names: Vec<_> = channels.iter().map(|channel| channel.name.as_str()).collect();
    assert_eq!(names, ["General", "Made-up Channel"]);
    assert!(
        channels
            .iter()
            .all(|channel| !channel.archived && channel.kind == "Standard")
    );

    assert!(requests.pending_invites().await.unwrap().is_empty());
    rig.mock
        .answer_next(
            "GET",
            "users/me/invitations",
            200,
            &json!({"invitations": [{"id": "inv-1", "channelDisplayName": "Shared", "teamDisplayName": "Made-up",
                                      "invitedBy": "Lab B"}]}),
        )
        .await;
    let invites = requests.pending_invites().await.unwrap();
    assert_eq!(invites.len(), 1);
    assert_eq!((invites[0].id.as_str(), invites[0].from.as_str()), ("inv-1", "Lab B"));
}

#[tokio::test]
async fn a_read_the_service_refuses_comes_back_as_nothing() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    rig.mock.refuse_next("GET", "tagCards", 403).await;
    assert_eq!(
        requests.team_tags(TEAM).await.unwrap(),
        teams_proto::TagsRead::default()
    );
    rig.mock
        .refuse_next("GET", &format!("groups/{GROUP}/channels"), 403)
        .await;
    assert!(requests.team_channels(GROUP).await.unwrap().is_empty());
    rig.mock.refuse_next("GET", "users/me/invitations", 403).await;
    assert!(requests.pending_invites().await.unwrap().is_empty());
    rig.mock.refuse_next("GET", "summary-timeseries", 403).await;
    assert_eq!(
        requests
            .team_analytics(GROUP, AnalyticsPeriod::LastSevenDays)
            .await
            .unwrap(),
        None
    );
    rig.mock.refuse_next("GET", "summary-timeseries", 403).await;
    assert!(
        requests
            .all_teams_analytics(AnalyticsPeriod::LastSevenDays)
            .await
            .unwrap()
            .is_empty()
    );
    rig.mock.refuse_next("GET", "teams/suggested", 403).await;
    assert!(requests.joinable_teams(None).await.unwrap().is_empty());
    rig.mock.refuse_next("POST", "users/searchV2", 403).await;
    assert!(requests.addable_people("lab").await.unwrap().is_empty());
    rig.mock.refuse_next("POST", "findJoinRequests", 403).await;
    assert!(requests.team_join_requests(GROUP).await.unwrap().is_empty());
    rig.mock.refuse_next("POST", "aggregatedEntitlements", 403).await;
    assert!(requests.team_apps(TEAM).await.unwrap().is_empty());
    rig.mock.refuse_next("POST", "apps/store", 403).await;
    assert!(requests.app_store().await.unwrap().is_empty());
    rig.mock.refuse_next("GET", "en-us", 403).await;
    assert!(requests.team_templates().await.unwrap().is_empty());
    rig.mock.refuse_next("GET", &format!("threads/{TEAM}"), 403).await;
    assert_eq!(requests.team_settings(TEAM).await.unwrap(), None);
    rig.mock.refuse_next("GET", "joinCode", 403).await;
    assert_eq!(requests.team_code(TEAM).await.unwrap(), None);
    rig.mock.refuse_next("GET", "usergroups", 403).await;
    assert_eq!(requests.owned_groups().await.unwrap(), None);
    rig.mock.refuse_next("GET", "Public/en-US", 403).await;
    assert_eq!(requests.team_template("made.up.template.project").await.unwrap(), None);
}

#[tokio::test]
async fn analytics_of_a_team_a_channel_and_all_teams_are_read_for_the_period() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    let team = requests
        .team_analytics(GROUP, AnalyticsPeriod::LastThirtyDays)
        .await
        .unwrap()
        .expect("a team's analytics");
    assert_eq!(team.active_users, 5);
    assert_eq!(team.active_users_by_day.len(), 7);
    assert_eq!((team.posts, team.replies, team.mentions), (12, 20, 7));
    let all = requests
        .all_teams_analytics(AnalyticsPeriod::LastNinetyDays)
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "Made-up Team");
    let channel = requests
        .channel_analytics(GROUP, TEAM, AnalyticsPeriod::LastSevenDays)
        .await
        .unwrap()
        .expect("a channel's analytics");
    assert_eq!(channel.meetings, 1);
    let sent = rig.sent(|path| path.ends_with("summary-timeseries")).await;
    let periods: Vec<_> = sent
        .iter()
        .map(|(_, _, query)| {
            query
                .split('&')
                .find(|pair| pair.starts_with("timeperiod="))
                .unwrap_or("")
        })
        .collect();
    assert_eq!(
        periods,
        [
            "timeperiod=last-thirty-days",
            "timeperiod=last-ninety-days",
            "timeperiod=last-seven-days"
        ]
    );
    assert!(sent[2].1.contains(&format!("/channels/{TEAM}/")), "{sent:?}");
}

#[tokio::test]
async fn the_teams_to_join_and_the_people_to_add_are_found() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    let suggested = requests.joinable_teams(None).await.unwrap();
    let names: Vec<_> = suggested.iter().map(|team| team.name.as_str()).collect();
    assert_eq!(names, ["Made-up Public Team", "Second Made-up Team"]);
    rig.mock
        .answer_next(
            "GET",
            "teams/find",
            200,
            &json!({"value": [{"id": "00000000-0000-4000-8000-0000000000ad", "displayName": "Found",
                               "accessType": "Private"}]}),
        )
        .await;
    let found = requests.joinable_teams(Some("found".to_owned())).await.unwrap();
    assert_eq!(found.len(), 1);
    assert!(!found[0].public);
    let sent = rig.sent(|path| path.ends_with("teams/find")).await;
    assert!(sent[0].2.contains("keywords=found"), "{sent:?}");

    let people = requests.addable_people("lab").await.unwrap();
    let mris: Vec<_> = people.iter().map(|person| person.mri.as_str()).collect();
    assert_eq!(mris, [LAB_B, LAB_C]);
}

#[tokio::test]
async fn the_people_asking_to_join_are_named() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    assert!(requests.team_join_requests(GROUP).await.unwrap().is_empty());
    rig.mock
        .answer_next(
            "POST",
            "findJoinRequests",
            200,
            &json!({"joinRequestsPerTeam": {GROUP: [{"userMri": LAB_B}, {"UserMri": LAB_C}, {"other": 1}]}}),
        )
        .await;
    let asking = requests.team_join_requests(GROUP).await.unwrap();
    let mris: Vec<_> = asking.iter().map(|member| member.mri.as_str()).collect();
    assert_eq!(mris, [LAB_B, LAB_C]);
    assert!(asking.iter().all(|member| !member.owner && !member.guest));

    // someone whose profile isn't read is still listed, unnamed
    let outsider = "8:orgid:00000000-0000-4000-8000-0000000000ff";
    rig.mock
        .answer_next(
            "POST",
            "findJoinRequests",
            200,
            &json!({"joinRequestsPerTeam": {GROUP: [{"userMri": outsider}]}}),
        )
        .await;
    let asking = requests.team_join_requests(GROUP).await.unwrap();
    assert_eq!(asking.len(), 1);
    assert_eq!(asking[0].mri, outsider);
    assert_eq!(asking[0].name, "Unknown user");
    assert_eq!(asking[0].email, None);
}

#[tokio::test]
async fn a_team_s_picture_is_read_when_it_is_a_picture() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    let picture = TeamPicture {
        own: "00000000-0000-4000-8000-0000000000a1".to_owned(),
        group: GROUP.to_owned(),
        name: "Made-up".to_owned(),
        etag: "e".to_owned(),
    };
    assert!(
        requests.team_picture(picture.clone()).await.unwrap().is_none(),
        "nothing served"
    );
    rig.mock
        .answer_next(
            "GET",
            &format!("profilepicturev2/teams/{GROUP}"),
            200,
            &json!("not a picture"),
        )
        .await;
    assert!(
        requests.team_picture(picture.clone()).await.unwrap().is_none(),
        "bytes of no picture's format"
    );
    let path = format!("{MT_PATH}/beta/users/{}/profilepicturev2/teams/{GROUP}", picture.own);
    rig.mock
        .serve_bytes(&path, "image/png", b"\x89PNG made up".to_vec())
        .await;
    let read = requests
        .team_picture(picture.clone())
        .await
        .unwrap()
        .expect("a picture");
    assert_eq!(read.format, PastedFormat::Png);
    assert_eq!(read.bytes, b"\x89PNG made up");

    assert_eq!(requests.default_team_picture("Made-up").await.unwrap(), None);
    rig.mock
        .serve_bytes(
            &format!("{MT_PATH}/beta/teams/defaultProfilePicture"),
            "image/png",
            b"\x89PNG default".to_vec(),
        )
        .await;
    assert_eq!(
        requests.default_team_picture("Made-up").await.unwrap().as_deref(),
        Some(&b"\x89PNG default"[..])
    );
}

#[tokio::test]
async fn a_team_s_apps_the_store_and_templates_are_read() {
    let rig = rig_with(true).await;
    let requests = rig.handle.requests();
    let apps = requests.team_apps(TEAM).await.unwrap();
    assert_eq!(apps.len(), 2, "the app Teams keeps for itself isn't listed");
    let store = requests.app_store().await.unwrap();
    assert_eq!(store.len(), 3);
    let templates = requests.team_templates().await.unwrap();
    assert!(!templates.is_empty());
    let template = requests.team_template(templates[0].id.clone()).await.unwrap();
    assert!(template.is_some());
    assert_eq!(requests.owned_groups().await.unwrap(), Some(0));
}

#[tokio::test]
async fn a_team_write_goes_under_the_fence_and_its_change_is_listed_at_once() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let requests = rig.handle.requests();
    rig.mock.answer_next("PATCH", "", 200, &json!({})).await;
    let written = requests
        .team_write(
            TeamWrite::Edit {
                team: TEAM.to_owned(),
                name: Some("Renamed".to_owned()),
                description: "made-up words".to_owned(),
                public: None,
            },
            Some(TeamChange::Edited {
                team: TEAM.to_owned(),
                name: Some("Renamed".to_owned()),
                description: "made-up words".to_owned(),
                public: None,
            }),
        )
        .await
        .unwrap();
    assert_eq!(written, teams_core::TeamWritten::default());
    let team = rig.stored_team().await;
    assert_eq!(team.name, "Renamed");
    assert_eq!(team.about.description, "made-up words");
}

#[tokio::test]
async fn a_new_team_is_listed_with_the_thread_its_answer_names() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let requests = rig.handle.requests();
    rig.mock
        .answer_next(
            "POST",
            "",
            200,
            &json!({"value": {"objectId": "19:madeup0009@thread.tacv2", "siteInfo": {"groupId": "made-up-group"}}}),
        )
        .await;
    let draft = teams_store::Team {
        id: "draft".to_owned(),
        name: "Brand new".to_owned(),
        ..teams_store::Team::default()
    };
    let written = requests
        .team_write(
            TeamWrite::Create(NewTeam {
                name: "Brand new".to_owned(),
                description: String::new(),
                first_channel: "General".to_owned(),
                public: false,
            }),
            Some(TeamChange::Created(draft)),
        )
        .await
        .unwrap();
    assert_eq!(
        written.created,
        Some((
            "19:madeup0009@thread.tacv2".to_owned(),
            Some("made-up-group".to_owned())
        ))
    );
    let teams = requests.teams().await.unwrap().expect("teams");
    let made = teams
        .iter()
        .find(|team| team.id == "19:madeup0009@thread.tacv2")
        .expect("listed at once");
    assert_eq!(made.group_id.as_deref(), Some("made-up-group"));
}

#[tokio::test]
async fn people_added_that_the_answer_leaves_out_are_told() {
    let rig = rig_with(true).await;
    rig.mock
        .answer_next("PUT", "", 200, &json!({"value": {"failedMembers": [LAB_C]}}))
        .await;
    rig.mock
        .answer_next("POST", "", 200, &json!({"value": {"failedMembers": [LAB_C]}}))
        .await;
    let written = rig
        .handle
        .requests()
        .team_write(
            TeamWrite::AddMembers {
                team: TEAM.to_owned(),
                group: GROUP.to_owned(),
                members: vec![(Mri::new(LAB_B), TeamRole::Member), (Mri::new(LAB_C), TeamRole::Owner)],
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(written.not_added, [LAB_C]);
}

#[tokio::test]
async fn teams_refusal_comes_back_with_its_code_and_nothing_changes() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let requests = rig.handle.requests();
    for verb in ["PUT", "POST", "DELETE"] {
        rig.mock
            .answer_next(verb, "", 403, &json!({"errorCode": "MadeUpRefusal"}))
            .await;
    }
    let refused = requests
        .team_write(
            TeamWrite::UpdateMember {
                team: TEAM.to_owned(),
                group: GROUP.to_owned(),
                member: Mri::new(LAB_B),
                update: MemberUpdate::Removed,
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, EngineError::TeamNotChanged { code: Some(code) } if code == "MadeUpRefusal"),
        "{refused:?}"
    );
    assert_eq!(refused.kind(), "team not changed");
}

#[tokio::test]
async fn a_team_write_the_lab_fence_denies_is_never_sent() {
    let mut rig = rig_with(false).await;
    rig.settled().await;
    let before = rig.mock.received_requests().await.len();
    let refused = rig
        .handle
        .requests()
        .team_write(
            TeamWrite::Edit {
                team: TEAM.to_owned(),
                name: Some("Not here".to_owned()),
                description: String::new(),
                public: None,
            },
            Some(TeamChange::Edited {
                team: TEAM.to_owned(),
                name: Some("Not here".to_owned()),
                description: String::new(),
                public: None,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EngineError::TeamNotChanged { code: None }),
        "{refused:?}"
    );
    let sent = rig.mock.received_requests().await;
    assert!(
        sent[before..].iter().all(|request| request.method.as_str() == "GET"),
        "no write went"
    );
    assert_ne!(rig.stored_team().await.name, "Not here");
}

#[tokio::test]
async fn hiding_a_team_for_the_account_reaches_nobody_and_lists_it_hidden() {
    let mut rig = rig_with(true).await;
    rig.settled().await;
    let requests = rig.handle.requests();
    rig.mock.answer_next("PUT", "hide", 200, &json!({})).await;
    requests
        .own_team_view(
            vec![OwnTeamView::HideAll { team: TEAM.to_owned() }],
            Some(TeamChange::Hidden {
                team: TEAM.to_owned(),
                hidden: true,
            }),
        )
        .await
        .unwrap();
    let team = rig.stored_team().await;
    assert!(team.about.hidden);
    assert!(team.channels.iter().all(|channel| !channel.shown));
    let sent = rig.sent(|path| path.ends_with(&format!("team/{TEAM}/hide"))).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "PUT");

    // a refused one changes nothing
    rig.mock.refuse_next("PUT", "conversations/show", 403).await;
    let refused = requests
        .own_team_view(
            vec![OwnTeamView::ShowAll { team: TEAM.to_owned() }],
            Some(TeamChange::Hidden {
                team: TEAM.to_owned(),
                hidden: false,
            }),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(refused, EngineError::TeamNotChanged { code: None }),
        "{refused:?}"
    );
    assert!(rig.stored_team().await.about.hidden);
}

#[tokio::test]
async fn a_team_write_turned_away_for_its_token_goes_once_more() {
    let rig = rig_with(true).await;
    rig.mock.answer_next("PATCH", "", 401, &json!({})).await;
    rig.mock.answer_next("PATCH", "", 200, &json!({})).await;
    rig.handle
        .requests()
        .team_write(
            TeamWrite::Edit {
                team: TEAM.to_owned(),
                name: None,
                description: "made-up words".to_owned(),
                public: None,
            },
            None,
        )
        .await
        .unwrap();
    let sent = rig.sent(|path| path.ends_with(&format!("beta/teams/{TEAM}"))).await;
    let patches = sent.iter().filter(|(verb, _, _)| verb == "PATCH").count();
    assert_eq!(patches, 2, "{sent:?}");
}

#[tokio::test]
async fn people_a_channel_can_t_be_given_come_back_as_not_added() {
    let requests_of = |rig: &Rig| rig.handle.requests();
    // a channel's group in no form the service takes: nothing sent, nobody added
    let rig = rig_with(true).await;
    let not_added = requests_of(&rig)
        .add_channel_members(GROUP, "not-a-channel-group", vec![LAB_B.to_owned()])
        .await
        .unwrap();
    assert_eq!(not_added, [LAB_B]);
    // a people search the service refuses finds nobody
    rig.mock.refuse_next("POST", "users/profilesearch", 403).await;
    assert!(requests_of(&rig).team_people(TEAM, "lab").await.unwrap().is_empty());
    // the lab fence denies it: nothing sent, nobody added
    let denied = rig_with(false).await;
    let not_added = requests_of(&denied)
        .add_channel_members(
            GROUP,
            "OID:00000000-0000-4000-8000-0000000000cc@00000000-0000-4000-8000-0000000000d0",
            vec![LAB_B.to_owned()],
        )
        .await
        .unwrap();
    assert_eq!(not_added, [LAB_B]);
    assert!(
        denied
            .mock
            .received_requests()
            .await
            .iter()
            .all(|request| !request.url.path().ends_with("/users")),
        "nothing sent"
    );
}
