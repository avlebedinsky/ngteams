use teams_proto::{Mri, TeamWrite};

use super::support::{Rig, Seat};
use crate::support::backends;
use crate::teams::refusal;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_teams_code_lets_people_in_until_it_is_replaced_or_taken_away() {
    for backend in backends() {
        let rig = Rig::start(backend, "joining").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        {
            let team: &str = &team;
            assert_eq!(a.engine().team_code(team).await.unwrap(), None);
            let reset = TeamWrite::ResetCode { team: team.into() };
            a.engine().team_write(reset.clone(), None).await.unwrap();
            let code = a.engine().team_code(team).await.unwrap().unwrap();
            assert_eq!(code.len(), 16, "{code}");
            let by_code = |code: &str| TeamWrite::JoinByCode { code: code.into() };
            let wrong = c.engine().team_write(by_code("0000000000000000"), None).await;
            assert_eq!(refusal(wrong).as_deref(), Some("InvalidJoinCode"));
            b.engine().team_write(by_code(&code), None).await.unwrap();
            let joined = b.team(team).await;
            assert!(!joined.owner);
            assert_eq!(joined.about.people, 2);
            b.sync().await;
            b.stored_teams(|teams| teams.iter().any(|listed| listed.id == team))
                .await;
            b.send(team, "Joined by the code").await;
            // the code is its owners' to read and change
            assert_eq!(b.engine().team_code(team).await.unwrap(), None);
            assert!(b.engine().team_write(reset.clone(), None).await.is_err());

            a.engine().team_write(reset, None).await.unwrap();
            let second = a.engine().team_code(team).await.unwrap().unwrap();
            assert_ne!(second, code);
            let replaced = c.engine().team_write(by_code(&code), None).await;
            assert_eq!(refusal(replaced).as_deref(), Some("InvalidJoinCode"));
            a.engine()
                .team_write(TeamWrite::RemoveCode { team: team.into() }, None)
                .await
                .unwrap();
            assert_eq!(a.engine().team_code(team).await.unwrap(), None);
            assert!(c.engine().team_write(by_code(&second), None).await.is_err());
            assert!(c.listed().await.is_empty());
        }

        let _ = group;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_teams_owner_lets_in_or_refuses_those_who_ask_to_join() {
    for backend in backends() {
        let rig = Rig::start(backend, "requests").await;
        let [a, b, c] = &rig.seats;
        // a second private team: b and c ask, its owner lets b in and refuses c
        let (closed, closed_group) = a.make_team("Vault", false).await;
        let join = || TeamWrite::Join {
            group: closed_group.clone(),
        };
        for seat in [b, c] {
            seat.engine().team_write(join(), None).await.unwrap();
            assert!(
                seat.listed().await.iter().all(|listed| listed.id != closed),
                "asking lets nobody in"
            );
        }
        let theirs = b.engine().team_join_requests(closed_group.as_str()).await.unwrap();
        assert!(theirs.is_empty(), "the owners' to read");
        let asking = a.engine().team_join_requests(closed_group.as_str()).await.unwrap();
        let asking: Vec<&str> = asking.iter().map(|member| member.mri.as_str()).collect();
        assert_eq!(asking, [b.mri.as_str(), c.mri.as_str()]);
        let answer = |seat: &Seat, accept| TeamWrite::AnswerJoinRequest {
            group: closed_group.clone(),
            member: Mri::new(&seat.mri),
            accept,
        };
        assert!(
            c.engine().team_write(answer(c, true), None).await.is_err(),
            "nobody lets itself in"
        );
        a.engine().team_write(answer(b, true), None).await.unwrap();
        a.engine().team_write(answer(c, false), None).await.unwrap();
        assert!(
            a.engine()
                .team_join_requests(closed_group.as_str())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            a.engine().team_write(answer(c, true), None).await.is_err(),
            "answered once"
        );
        assert_eq!(b.team(&closed).await.about.people, 2);
        assert!(c.listed().await.is_empty());
        b.sync().await;
        let stored = b
            .stored_teams(|teams| teams.iter().any(|listed| listed.id == closed))
            .await;
        assert!(stored.iter().any(|listed| listed.id == closed && !listed.owner));
        b.send(&closed, "Thanks for letting me in").await;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_teams_tags_are_made_changed_and_deleted_as_its_tag_settings_allow() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "tags").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;
        let create = |name: &str, members: &[&Seat]| TeamWrite::CreateTag {
            team: team.clone(),
            name: name.into(),
            description: "Who flies".into(),
            members: members.iter().map(|seat| Mri::new(&seat.mri)).collect(),
        };
        let empty = a.engine().team_tags(team.as_str()).await.unwrap();
        assert!(empty.tags.is_empty() && empty.owners_only);
        let refused = b.engine().team_write(create("Mine", &[b]), None).await;
        assert_eq!(refusal(refused).as_deref(), Some("Forbidden"));
        a.engine().team_write(create("Crew", &[a, b]), None).await.unwrap();
        let taken = a.engine().team_write(create("crew", &[a]), None).await;
        assert_eq!(refusal(taken).as_deref(), Some("TagNameAlreadyExists"));
        assert!(
            a.engine().team_write(create("Guests", &[c]), None).await.is_err(),
            "people of the team only"
        );

        let tag = b.engine().team_tags(team.as_str()).await.unwrap().tags[0].clone();
        assert_eq!(
            (tag.name.as_str(), tag.description.as_str(), tag.members),
            ("Crew", "Who flies", 2)
        );
        let edit = |version| TeamWrite::EditTag {
            team: team.clone(),
            tag: tag.id.clone(),
            version,
            name: "Flight crew".into(),
            description: "Seats one to three".into(),
        };
        a.engine().team_write(edit(tag.version), None).await.unwrap();
        let stale = a.engine().team_write(edit(tag.version), None).await;
        assert_eq!(refusal(stale).as_deref(), Some("TagVersionMismatch"));
        let settings = TeamWrite::TagSettings {
            team: team.clone(),
            owners_only: false,
            automatic: false,
        };
        assert!(b.engine().team_write(settings.clone(), None).await.is_err());
        a.engine().team_write(settings, None).await.unwrap();
        b.engine().team_write(create("Ground", &[b]), None).await.unwrap();

        // the tags are the server's: they are there after its nodes start again
        rig.restart_nodes().await;
        let [a, b, c] = &rig.seats;
        let read = b.engine().team_tags(team.as_str()).await.unwrap();
        let mut names: Vec<&str> = read.tags.iter().map(|tag| tag.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["Flight crew", "Ground"]);
        assert!(!read.owners_only);
        let delete = TeamWrite::DeleteTag {
            team: team.clone(),
            tag: tag.id.clone(),
        };
        b.engine().team_write(delete.clone(), None).await.unwrap();
        assert!(b.engine().team_write(delete, None).await.is_err());
        assert_eq!(a.engine().team_tags(team.as_str()).await.unwrap().tags.len(), 1);
        assert!(
            c.engine().team_tags(team.as_str()).await.unwrap().tags.is_empty(),
            "nothing for an outsider"
        );
        rig.stop().await;
    }
}
