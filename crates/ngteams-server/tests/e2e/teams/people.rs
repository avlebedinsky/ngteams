use teams_proto::chat::ChatReads;
use teams_proto::{MemberUpdate, Mri, TeamRole, TeamWrite};

use super::{Seat, refusal, server};
use crate::support::backends;

fn update(team: &str, group: &str, member: &Seat, update: MemberUpdate) -> TeamWrite {
    TeamWrite::UpdateMember {
        team: team.into(),
        group: group.into(),
        member: Mri::new(&member.mri),
        update,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_adds_people_with_roles_and_the_roster_names_them() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let nobody = format!("8:orgid:{}", uuid::Uuid::new_v4());
        let written = ada
            .engine
            .team_write(
                TeamWrite::AddMembers {
                    team: team.clone(),
                    group: group.clone(),
                    members: vec![
                        (Mri::new(&bert.mri), TeamRole::Member),
                        (Mri::new(&cora.mri), TeamRole::Owner),
                        (Mri::new(&nobody), TeamRole::Member),
                    ],
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(written.not_added, [nobody]);

        let roster = ada.engine.team_roster(team.as_str()).await.unwrap();
        let owners: Vec<&str> = roster.iter().filter(|m| m.owner).map(|m| m.mri.as_str()).collect();
        let members: Vec<&str> = roster.iter().filter(|m| !m.owner).map(|m| m.mri.as_str()).collect();
        assert_eq!(owners, [ada.mri.as_str(), cora.mri.as_str()]);
        assert_eq!(members, [bert.mri.as_str()]);

        let theirs = bert.team(&team).await;
        assert!(!theirs.owner);
        assert_eq!(theirs.about.people, 3);
        assert!(cora.team(&team).await.owner);
        // someone added is in the team's channels made before
        let reads = ChatReads::new(&*bert.client);
        assert_eq!(reads.thread(&channel).await.unwrap().id, channel);
        assert_eq!(reads.members(&team).await.unwrap().members.len(), 3);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn roles_change_people_leave_and_a_team_keeps_an_owner() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;
        let write = |seat: &Seat, change| update(&team, &group, seat, change);

        // a member manages nobody
        let refused = bert.engine.team_write(write(&cora, MemberUpdate::Removed), None).await;
        assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));
        let refused = bert.engine.team_write(write(&bert, MemberUpdate::Promoted), None).await;
        assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));

        // the only owner stays one
        for change in [
            MemberUpdate::Demoted,
            MemberUpdate::DemotedAndLeft,
            MemberUpdate::Removed,
        ] {
            let refused = ada.engine.team_write(write(&ada, change), None).await;
            assert_eq!(
                refusal(refused).as_deref(),
                Some("LastOwnerCannotBeRemoved"),
                "{change:?}"
            );
        }

        ada.engine
            .team_write(write(&bert, MemberUpdate::Promoted), None)
            .await
            .unwrap();
        assert!(bert.team(&team).await.owner);
        ada.engine
            .team_write(write(&ada, MemberUpdate::Demoted), None)
            .await
            .unwrap();
        assert!(!ada.team(&team).await.owner);
        bert.engine
            .team_write(write(&ada, MemberUpdate::Promoted), None)
            .await
            .unwrap();

        bert.engine
            .team_write(write(&cora, MemberUpdate::Removed), None)
            .await
            .unwrap();
        assert!(cora.listed().await.is_empty());
        let gone = ChatReads::new(&*cora.client).thread(&team).await.unwrap_err();
        assert_eq!(gone.status(), Some(403));

        bert.engine
            .team_write(write(&bert, MemberUpdate::DemotedAndLeft), None)
            .await
            .unwrap();
        assert!(bert.listed().await.is_empty());
        assert_eq!(ada.team(&team).await.about.people, 1);
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_private_teams_member_asks_its_owners_to_let_someone_in() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let dave = Seat::new(&server, "dave@own.test", "Dave").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;

        let direct = TeamWrite::AddMembers {
            team: team.clone(),
            group: group.clone(),
            members: vec![(Mri::new(&cora.mri), TeamRole::Member)],
        };
        assert_eq!(
            refusal(bert.engine.team_write(direct, None).await).as_deref(),
            Some("Forbidden")
        );
        let ask = TeamWrite::RequestMembers {
            group: group.clone(),
            members: vec![Mri::new(&cora.mri), Mri::new(&dave.mri)],
        };
        bert.engine.team_write(ask, None).await.unwrap();
        assert!(cora.listed().await.is_empty(), "a request puts nobody on the team");

        assert!(bert.engine.team_join_requests(group.as_str()).await.unwrap().is_empty());
        let asking = ada.engine.team_join_requests(group.as_str()).await.unwrap();
        let asking: Vec<&str> = asking.iter().map(|member| member.mri.as_str()).collect();
        assert_eq!(asking, [cora.mri.as_str(), dave.mri.as_str()]);

        let answer = |seat: &Seat, accept| TeamWrite::AnswerJoinRequest {
            group: group.clone(),
            member: Mri::new(&seat.mri),
            accept,
        };
        assert!(bert.engine.team_write(answer(&cora, true), None).await.is_err());
        ada.engine.team_write(answer(&cora, true), None).await.unwrap();
        ada.engine.team_write(answer(&dave, false), None).await.unwrap();
        assert_eq!(cora.listed().await.len(), 1);
        assert!(dave.listed().await.is_empty());
        assert!(ada.engine.team_join_requests(group.as_str()).await.unwrap().is_empty());
        assert!(
            ada.engine.team_write(answer(&dave, true), None).await.is_err(),
            "answered once"
        );
        for seat in [ada, bert, cora, dave] {
            seat.stop();
        }
        server.stop().await;
    }
}
