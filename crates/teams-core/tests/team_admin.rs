//! The team pages through the engine, on the mock (`teams_core::team_admin`, `teams_core::team_apps`): each read goes
//! where the web client's page sends it and comes back as the page shows it, a failed one as nothing; a write managing
//! a team goes through the outbox, its answer read and Teams' refusal told by its code; the account's own view of a
//! team changes the stored teams at once.
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
use teams_mock::{
    CHAT_BASE, CSA_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, MIDDLE_TIER_BASE, PROVISIONING_BASE, USER_PROFILE_BASE,
};
use teams_proto::Mri;
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

struct Rig {
    mock: ChatServiceMock,
    handle: EngineHandle,
    _dir: tempfile::TempDir,
}

async fn setup() -> Rig {
    let mock = ChatServiceMock::start().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tokens = Arc::new(Tokens(mock.token(LAB_A).unwrap()));
    let origin = mock.origin();
    let http = || {
        HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .for_account(Mri::new(LAB_A))
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
    let rig = setup().await;
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
async fn a_team_write_is_sent_and_its_change_is_listed_at_once() {
    let mut rig = setup().await;
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
    let mut rig = setup().await;
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
    let rig = setup().await;
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
    let mut rig = setup().await;
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
async fn hiding_a_team_for_the_account_reaches_nobody_and_lists_it_hidden() {
    let mut rig = setup().await;
    rig.settled().await;
    let requests = rig.handle.requests();
    rig.mock
        .answer_next(
            "GET",
            "/v1.0/me",
            200,
            &json!({"id": LAB_A.strip_prefix("8:orgid:").unwrap()}),
        )
        .await;
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
    rig.mock
        .answer_next(
            "GET",
            "/v1.0/me",
            200,
            &json!({"id": LAB_A.strip_prefix("8:orgid:").unwrap()}),
        )
        .await;
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
    let show = rig.sent(|path| path.ends_with("conversations/show")).await;
    assert_eq!(show.len(), 1, "the actor was verified and the service refused the show");
    assert_eq!(show[0].0, "PUT");
}

#[tokio::test]
async fn a_team_write_turned_away_for_its_token_goes_once_more() {
    let rig = setup().await;
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
    let rig = setup().await;
    let not_added = requests_of(&rig)
        .add_channel_members(GROUP, "not-a-channel-group", vec![LAB_B.to_owned()])
        .await
        .unwrap();
    assert_eq!(not_added, [LAB_B]);
    // a people search the service refuses finds nobody
    rig.mock.refuse_next("POST", "users/profilesearch", 403).await;
    assert!(requests_of(&rig).team_people(TEAM, "lab").await.unwrap().is_empty());
    // An explicit service rejection reports everyone as not added and is not retried.
    let refused = setup().await;
    refused.mock.refuse_next("POST", "/users", 403).await;
    let not_added = requests_of(&refused)
        .add_channel_members(
            GROUP,
            "OID:00000000-0000-4000-8000-0000000000cc@00000000-0000-4000-8000-0000000000d0",
            vec![LAB_B.to_owned()],
        )
        .await
        .unwrap();
    assert_eq!(not_added, [LAB_B]);
    assert_eq!(
        refused
            .mock
            .received_requests()
            .await
            .iter()
            .filter(|request| request.url.path().ends_with("/users"))
            .count(),
        1,
        "one rejected attempt"
    );
}

const MANAGED: &str = "19:channel0002@thread.tacv2";
fn management_context() -> serde_json::Value {
    json!({"metadata":{"isPartialData":false},"teams":[{
        "id":TEAM,"displayName":"Synthetic team","memberRoleString":"admin",
        "teamSiteInformation":{"groupId":GROUP},"tenantId":"00000000-0000-4000-8000-0000000000d0",
        "channels":[{"id":MANAGED,"displayName":"Current","description":"Before","channelType":0,"isGeneral":false}]
    }]})
}
fn management(action: teams_proto::ChannelChange) -> TeamWrite {
    TeamWrite::Channel(teams_proto::ChannelWrite {
        target: teams_proto::ChannelTarget {
            team: TEAM.into(),
            team_group: GROUP.into(),
            channel: MANAGED.into(),
            channel_group: None,
            mailbox: None,
        },
        action,
    })
}
async fn fresh_context(rig: &Rig, body: &serde_json::Value) {
    rig.mock.answer_next("GET", "api/v3/teams/users/me", 200, body).await;
}
#[tokio::test]
async fn fresh_channel_edit_updates_only_the_selected_stored_channel() {
    let mut rig = setup().await;
    rig.settled().await;
    fresh_context(&rig, &management_context()).await;
    rig.mock
        .answer_next("PATCH", &format!("/channels/{MANAGED}"), 200, &json!({}))
        .await;
    rig.handle
        .requests()
        .team_write(
            management(teams_proto::ChannelChange::Edit {
                name: "Renamed".into(),
                description: "New description".into(),
            }),
            Some(TeamChange::ChannelEdited {
                team: TEAM.into(),
                channel: MANAGED.into(),
                name: "Renamed".into(),
                description: "New description".into(),
            }),
        )
        .await
        .unwrap();
    let team = rig.stored_team().await;
    let row = team.channels.iter().find(|row| row.id == MANAGED).unwrap();
    assert_eq!((&*row.name, &*row.about.description), ("Renamed", "New description"));
    assert_eq!(team.channels.iter().find(|row| row.general).unwrap().name, "General");
    let sent = rig.sent(|path| path.ends_with(&format!("/channels/{MANAGED}"))).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "PATCH");
}
#[tokio::test]
async fn fresh_channel_context_rejects_stale_group_role_type_and_missing_channel() {
    for fault in ["group", "role", "private", "missing", "archived", "partial"] {
        let mut rig = setup().await;
        rig.settled().await;
        let mut body = management_context();
        match fault {
            "group" => body["teams"][0]["teamSiteInformation"]["groupId"] = json!("changed"),
            "role" => body["teams"][0]["memberRoleString"] = json!("user"),
            "private" => body["teams"][0]["channels"][0]["channelType"] = json!(1),
            "missing" => body["teams"][0]["channels"] = json!([]),
            "archived" => body["teams"][0]["isArchived"] = json!(true),
            "partial" => body = json!({"metadata":{"isPartialData":true},"teams":[]}),
            _ => unreachable!(),
        }
        fresh_context(&rig, &body).await;
        let failed = rig
            .handle
            .requests()
            .team_write(
                management(teams_proto::ChannelChange::Edit {
                    name: "After".into(),
                    description: "After".into(),
                }),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(failed, EngineError::TeamNotChanged { .. }),
            "{fault}: {failed:?}"
        );
        let writes = rig.sent(|path| path.ends_with(&format!("/channels/{MANAGED}"))).await;
        assert!(writes.is_empty(), "{fault}: {writes:?}");
    }
}
#[tokio::test]
async fn reconciled_archive_and_delete_do_not_repeat_the_remote_mutation() {
    for gone in [false, true] {
        let mut rig = setup().await;
        rig.settled().await;
        let mut body = management_context();
        let (action, change) = if gone {
            body["teams"][0]["channels"] = json!([]);
            (
                teams_proto::ChannelChange::Delete,
                TeamChange::ChannelGone {
                    team: TEAM.into(),
                    channel: MANAGED.into(),
                },
            )
        } else {
            body["teams"][0]["channels"][0]["isArchived"] = json!(true);
            (
                teams_proto::ChannelChange::Archive {
                    archived: true,
                    site_read_only: false,
                },
                TeamChange::ChannelArchived {
                    team: TEAM.into(),
                    channel: MANAGED.into(),
                    archived: true,
                },
            )
        };
        fresh_context(&rig, &body).await;
        rig.handle
            .requests()
            .team_write(management(action), Some(change))
            .await
            .unwrap();
        let team = rig.stored_team().await;
        let channel = team.channels.iter().find(|row| row.id == MANAGED);
        if gone {
            assert!(channel.is_none());
        } else {
            assert!(channel.unwrap().about.archived);
        }
        let writes: Vec<_> = rig
            .mock
            .received_requests()
            .await
            .into_iter()
            .filter(|r| r.method.as_str() != "GET")
            .collect();
        assert!(writes.is_empty(), "{writes:?}");
    }
}
#[tokio::test]
async fn channel_membership_partial_success_is_reported_without_replay() {
    for body in [
        json!({"multipleStatus":[{"id":LAB_B}]}),
        json!({"failedMembers":[LAB_B]}),
        json!({"updatedMembersError":[LAB_B]}),
        json!({"results":[{"status":"Failed"}]}),
    ] {
        let mut rig = setup().await;
        rig.settled().await;
        fresh_context(&rig, &management_context()).await;
        rig.mock
            .answer_next("PUT", &format!("/channels/{MANAGED}/members"), 200, &body)
            .await;
        let failed = rig
            .handle
            .requests()
            .team_write(
                management(teams_proto::ChannelChange::Moderator {
                    mri: LAB_B.into(),
                    moderator: true,
                }),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(failed,EngineError::TeamNotChanged{code:Some(code)} if code=="MemberChangeRejected"));
        assert_eq!(
            rig.sent(|path| path.ends_with(&format!("/channels/{MANAGED}/members")))
                .await
                .len(),
            1
        );
    }
}
fn hierarchy() -> serde_json::Value {
    json!({"isPartialResponse":false,"folderHierarchyVersion":42,"conversationFolderOrder":["favorites"],"conversationFolders":[{"id":"favorites","name":"Favorites","folderType":"Favorites","conversationFolderItems":[{"conversationId":MANAGED}]}]})
}
#[tokio::test]
async fn channel_sections_use_fresh_version_and_independent_actor_once() {
    let mut rig = setup().await;
    rig.settled().await;
    rig.mock
        .answer_next("GET", "conversationFolders", 200, &hierarchy())
        .await;
    rig.mock
        .answer_next(
            "GET",
            "/v1.0/me",
            200,
            &json!({"id":LAB_A.strip_prefix("8:orgid:").unwrap()}),
        )
        .await;
    rig.mock
        .answer_next("PATCH", "batchUpdateExpandedState", 200, &json!({}))
        .await;
    rig.handle
        .requests()
        .channel_section(teams_proto::ChannelSectionAction::Expanded {
            section: "favorites".into(),
            expanded: false,
        })
        .await
        .unwrap();
    let sent = rig.mock.received_requests().await;
    let written: Vec<_> = sent
        .iter()
        .filter(|r| r.url.path().ends_with("batchUpdateExpandedState"))
        .collect();
    assert_eq!(written.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&written[0].body).unwrap();
    assert_eq!(body["folderHierarchyVersion"], 42);
    assert_eq!(body["isExpanded"], false);
    assert!(sent.iter().any(|r| r.url.path() == "/v1.0/me"));
}
#[tokio::test]
async fn channel_sections_reject_partial_unknown_and_changed_actor_without_write() {
    for fault in ["partial", "section", "actor"] {
        let mut rig = setup().await;
        rig.settled().await;
        let mut body = hierarchy();
        if fault == "partial" {
            body["isPartialResponse"] = json!(true);
        }
        rig.mock.answer_next("GET", "conversationFolders", 200, &body).await;
        rig.mock
            .answer_next(
                "GET",
                "/v1.0/me",
                200,
                &json!({"id":LAB_B.strip_prefix("8:orgid:").unwrap()}),
            )
            .await;
        let section = if fault == "section" { "unknown" } else { "favorites" };
        assert!(
            rig.handle
                .requests()
                .channel_section(teams_proto::ChannelSectionAction::Expanded {
                    section: section.into(),
                    expanded: false
                })
                .await
                .is_err(),
            "{fault}"
        );
        assert!(
            !rig.mock
                .received_requests()
                .await
                .iter()
                .any(|r| r.method.as_str() == "PATCH")
        );
    }
}

#[tokio::test]
async fn channel_dialog_reads_reject_wrong_deleted_and_malformed_target_evidence() {
    use teams_proto::ChannelRead;
    for fault in ["wrong", "deleted", "malformed", "network", "valid"] {
        let mut rig = setup().await;
        rig.settled().await;
        let route = format!("threads/{MANAGED}");
        let body = match fault {
            "wrong" => json!({"id":TEAM}),
            "deleted" => json!({"id":MANAGED,"isDeleted":true}),
            "malformed" => json!(["not an object"]),
            _ => json!({"id":MANAGED,"properties":{"channelSettings":"{}"}}),
        };
        rig.mock
            .answer_next("GET", &route, if fault == "network" { 403 } else { 200 }, &body)
            .await;
        let result = rig
            .handle
            .requests()
            .channel_data(ChannelRead::Thread {
                channel: MANAGED.into(),
            })
            .await;
        if fault == "valid" {
            assert_eq!(result.unwrap().0["id"], MANAGED);
        } else {
            assert!(result.is_err(), "{fault}");
        }
        assert!(
            rig.mock
                .received_requests()
                .await
                .iter()
                .all(|r| r.method.as_str() == "GET")
        );
    }
}
#[tokio::test]
async fn channel_section_remove_reconciles_absent_items_and_sends_one_versioned_present_change() {
    for present in [false, true] {
        let mut rig = setup().await;
        rig.settled().await;
        let mut body = hierarchy();
        if !present {
            body["conversationFolders"][0]["conversationFolderItems"] = json!([]);
        }
        rig.mock.answer_next("GET", "conversationFolders", 200, &body).await;
        rig.mock
            .answer_next(
                "GET",
                "/v1.0/me",
                200,
                &json!({"id":LAB_A.strip_prefix("8:orgid:").unwrap()}),
            )
            .await;
        rig.mock
            .answer_next("POST", "conversationFolders", 200, &json!({}))
            .await;
        rig.handle
            .requests()
            .channel_section(teams_proto::ChannelSectionAction::Remove {
                channel: MANAGED.into(),
            })
            .await
            .unwrap();
        let requests = rig.mock.received_requests().await;
        let sent: Vec<_> = requests
            .iter()
            .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("conversationFolders"))
            .collect();
        assert_eq!(sent.len(), usize::from(present));
        if let Some(write) = sent.first() {
            let body: serde_json::Value = serde_json::from_slice(&write.body).unwrap();
            assert_eq!(body["folderHierarchyVersion"], 42);
            assert_eq!(body["actions"][0]["action"], "RemoveItem");
        }
    }
}
#[tokio::test]
async fn private_channel_edits_follow_fresh_channel_ownership_and_exact_substrate_group() {
    const PRIVATE: &str = "00000000-0000-4000-8000-0000000000cc";
    const TENANT: &str = "00000000-0000-4000-8000-0000000000d0";
    for fault in ["valid", "not-owner", "group-mismatch", "missing-group", "unknown-type"] {
        let mut rig = setup().await;
        rig.settled().await;
        let mut body = management_context();
        body["teams"][0]["channels"][0]["channelType"] = json!(if fault == "unknown-type" { 99 } else { 1 });
        body["teams"][0]["channels"][0]["memberRole"] = json!(if fault == "not-owner" { 1 } else { 2 });
        if fault != "missing-group" {
            body["teams"][0]["channels"][0]["substrateGroupId"] = json!(PRIVATE);
        }
        fresh_context(&rig, &body).await;
        let TeamWrite::Channel(mut write) = management(teams_proto::ChannelChange::Edit {
            name: "Private renamed".into(),
            description: "Private description".into(),
        }) else {
            unreachable!()
        };
        write.target.channel_group = Some(format!(
            "OID:{}@{TENANT}",
            if fault == "group-mismatch" { GROUP } else { PRIVATE }
        ));
        rig.mock.answer_next("PATCH", "", 200, &json!({})).await;
        let result = rig.handle.requests().team_write(TeamWrite::Channel(write), None).await;
        assert_eq!(result.is_ok(), fault == "valid", "{fault}: {result:?}");
        assert_eq!(
            rig.mock
                .received_requests()
                .await
                .iter()
                .filter(|r| r.method.as_str() == "PATCH")
                .count(),
            usize::from(fault == "valid")
        );
    }
}
#[tokio::test]
async fn standard_channel_settings_email_and_archive_use_fresh_operational_context() {
    use teams_proto::ChannelChange;
    let cases = [
        (
            ChannelChange::Settings(json!({"moderationEnabled":true})),
            "PUT",
            "/properties",
        ),
        (
            ChannelChange::Archive {
                archived: true,
                site_read_only: true,
            },
            "POST",
            "/archive",
        ),
        (ChannelChange::CreateEmail, "POST", "/email"),
        (
            ChannelChange::UpdateEmail {
                allowed_sender_type: "members".into(),
                allowed_domains: vec!["example.test".into()],
            },
            "PUT",
            "/email",
        ),
        (ChannelChange::RemoveEmail, "DELETE", "/email"),
    ];
    for (action, verb, suffix) in cases {
        let mut rig = setup().await;
        rig.settled().await;
        fresh_context(&rig, &management_context()).await;
        rig.mock.answer_next(verb, suffix, 200, &json!({})).await;
        rig.handle
            .requests()
            .team_write(management(action), None)
            .await
            .unwrap();
        assert_eq!(
            rig.mock
                .received_requests()
                .await
                .iter()
                .filter(|r| r.method.as_str() == verb && r.url.path().ends_with(suffix))
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn workflow_engine_delegates_reads_and_single_attempt_mutations_to_native_routes() {
    use teams_proto::{WorkflowFailure, WorkflowRequest};
    let rig = setup().await;
    let base = "/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows";
    rig.mock.answer_next("GET", base, 200, &json!({"value":[]})).await;
    let read = rig
        .handle
        .requests()
        .workflow_request(WorkflowRequest::List {
            environment: "Default-synthetic".into(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read, json!({"value":[]}));
    for (status, expected) in [
        (403, WorkflowFailure::Forbidden),
        (429, WorkflowFailure::Throttled),
        (503, WorkflowFailure::Service(503)),
    ] {
        rig.mock
            .answer_next("POST", "/triggers/manual/run", status, &json!({}))
            .await;
        let result = rig
            .handle
            .requests()
            .workflow_request(WorkflowRequest::Run {
                environment: "Default-synthetic".into(),
                flow: "synthetic".into(),
                trigger: "manual".into(),
            })
            .await
            .unwrap();
        assert_eq!(result, Err(expected));
    }
    assert_eq!(rig.sent(|path| path.ends_with("/triggers/manual/run")).await.len(), 3);
}
