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

#[tokio::test(flavor = "multi_thread")]
async fn pending_join_requests_keep_the_order_asked_and_ties_read_one_way() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let one = Seat::new(&server, "one@own.test", "One").await;
        let two = Seat::new(&server, "two@own.test", "Two").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        // asked against the ids' order, so an id-ordered read can't pass for the asked one
        let (low, high) = if one.id < two.id { (&one, &two) } else { (&two, &one) };
        let ask = TeamWrite::RequestMembers {
            group: group.clone(),
            members: vec![Mri::new(&high.mri), Mri::new(&low.mri)],
        };
        bert.engine.team_write(ask, None).await.unwrap();
        let asking = || async {
            let asking = ada.engine.team_join_requests(group.as_str()).await.unwrap();
            asking.into_iter().map(|member| member.mri).collect::<Vec<_>>()
        };
        assert_eq!(asking().await, [high.mri.clone(), low.mri.clone()], "the order asked");

        stamp_join_requests_alike(&server).await;
        assert_eq!(asking().await, [low.mri.clone(), high.mri.clone()], "a tie reads by id");
        for seat in [ada, bert, one, two] {
            seat.stop();
        }
        server.stop().await;
    }
}

async fn stamp_join_requests_alike(server: &crate::support::TestServer) {
    use diesel_async::SimpleAsyncConnection as _;
    use ngteams_server::db::Pool;
    const TIE: &str = "UPDATE team_join_requests SET created_at = 0";
    match server.app.db.pool() {
        Pool::Postgres(pool) => pool.get().await.unwrap().batch_execute(TIE).await.unwrap(),
        Pool::Sqlite(pool) => pool.get().await.unwrap().batch_execute(TIE).await.unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_people_pickers_find_accounts_to_add_and_a_teams_own_people() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert Élan").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let gone = server.add_user("bertram@own.test", "Bertram").await;
        ngteams_server::admin::set_disabled(&server.app.db, &*server.app.fanout, "bertram@own.test", true)
            .await
            .unwrap();
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;

        // Add member's box: every enabled account, by a part of its name or address, any case
        let found = ada.engine.addable_people("BERT").await.unwrap();
        assert_eq!(found.len(), 1, "{gone} can't sign in, so it isn't offered");
        assert_eq!(found[0].mri, bert.mri);
        assert_eq!(found[0].name, "Bert Élan");
        assert_eq!(found[0].email.as_deref(), Some("bert@own.test"));
        assert_eq!(ada.engine.addable_people("élan").await.unwrap().len(), 1);
        assert_eq!(ada.engine.addable_people("own.test").await.unwrap().len(), 3);
        assert!(
            ada.engine.addable_people("%").await.unwrap().is_empty(),
            "a wildcard is a character"
        );
        assert!(ada.engine.addable_people("  ").await.unwrap().is_empty());

        // a private channel's box: the team's people only, for the team's people only
        let inside = ada.engine.team_people(team.as_str(), "o").await.unwrap();
        let mris: Vec<&str> = inside.iter().map(|person| person.mri.as_str()).collect();
        assert_eq!(mris, [ada.mri.as_str(), bert.mri.as_str()], "Cora isn't on the team");
        assert!(ada.engine.team_people(team.as_str(), "cora").await.unwrap().is_empty());
        assert!(cora.engine.team_people(team.as_str(), "o").await.unwrap().is_empty());
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_add_member_request_changes_roles_and_a_team_keeps_an_owner() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let roles = |role| TeamWrite::AddMembers {
            team: team.clone(),
            group: group.clone(),
            members: vec![(Mri::new(&ada.mri), role), (Mri::new(&bert.mri), role)],
        };

        let written = ada.engine.team_write(roles(TeamRole::Member), None).await.unwrap();
        assert_eq!(written.not_added, std::slice::from_ref(&ada.mri), "the only owner");
        assert!(ada.team(&team).await.owner);
        let written = ada.engine.team_write(roles(TeamRole::Owner), None).await.unwrap();
        assert!(written.not_added.is_empty() && bert.team(&team).await.owner);
        // of two owners named at once, the last one stays
        let written = ada.engine.team_write(roles(TeamRole::Member), None).await.unwrap();
        assert_eq!(written.not_added, std::slice::from_ref(&bert.mri));
        assert!(!ada.team(&team).await.owner && bert.team(&team).await.owner);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
