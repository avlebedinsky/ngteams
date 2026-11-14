use teams_proto::TeamWrite;

use super::{Seat, refusal, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn public_teams_are_found_and_joined_and_a_private_one_is_asked_for() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (_, private_group) = ada.make_team("Rocket", false).await;
        let (open, open_group) = ada.make_team("Open house", true).await;
        ada.make_team("Élan club", true).await;

        let suggested = bert.engine.joinable_teams(None).await.unwrap();
        let names: Vec<&str> = suggested.iter().map(|team| team.name.as_str()).collect();
        assert_eq!(names, ["Open house", "Élan club"], "a private team isn't offered");
        assert!(suggested.iter().all(|team| team.public));
        assert_eq!(suggested[0].group, open_group);
        assert_eq!(suggested[0].description, "Open house's people");
        let found = bert.engine.joinable_teams(Some("éLAN".into())).await.unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            bert.engine
                .joinable_teams(Some("100%_".into()))
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            ada.engine.joinable_teams(None).await.unwrap().is_empty(),
            "not the teams one is on"
        );

        let join = |group: &str| TeamWrite::Join { group: group.into() };
        bert.engine.team_write(join(&open_group), None).await.unwrap();
        let joined = bert.team(&open).await;
        assert!(!joined.owner);
        assert_eq!(joined.about.people, 2);
        assert_eq!(bert.engine.joinable_teams(None).await.unwrap().len(), 1);
        bert.engine.team_write(join(&open_group), None).await.unwrap();
        assert_eq!(bert.team(&open).await.about.people, 2, "joining again changes nothing");

        // a private team's owners decide
        bert.engine.team_write(join(&private_group), None).await.unwrap();
        assert_eq!(bert.listed().await.len(), 1);
        let asking = ada.engine.team_join_requests(private_group.as_str()).await.unwrap();
        assert_eq!(asking.len(), 1);
        assert_eq!(asking[0].mri, bert.mri);
        let unknown = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            refusal(bert.engine.team_write(join(&unknown), None).await).as_deref(),
            Some("NotFound")
        );
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_code_lets_people_in_until_it_is_taken_away() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, _) = ada.make_team("Rocket", false).await;
        assert_eq!(ada.engine.team_code(team.as_str()).await.unwrap(), None);

        let reset = TeamWrite::ResetCode { team: team.clone() };
        ada.engine.team_write(reset.clone(), None).await.unwrap();
        let code = ada.engine.team_code(team.as_str()).await.unwrap().unwrap();
        assert!(code.len() >= 7, "{code}");
        let by_code = |code: &str| TeamWrite::JoinByCode { code: code.into() };
        bert.engine.team_write(by_code(&code), None).await.unwrap();
        assert_eq!(bert.team(&team).await.about.people, 2);
        // the code is its owners' to read and change
        assert_eq!(bert.engine.team_code(team.as_str()).await.unwrap(), None);
        assert!(bert.engine.team_write(reset.clone(), None).await.is_err());

        ada.engine.team_write(reset, None).await.unwrap();
        let second = ada.engine.team_code(team.as_str()).await.unwrap().unwrap();
        assert_ne!(second, code);
        assert_eq!(
            refusal(cora.engine.team_write(by_code(&code), None).await).as_deref(),
            Some("InvalidJoinCode"),
            "a replaced code opens nothing"
        );
        let remove = TeamWrite::RemoveCode { team: team.clone() };
        ada.engine.team_write(remove, None).await.unwrap();
        assert_eq!(ada.engine.team_code(team.as_str()).await.unwrap(), None);
        assert!(cora.engine.team_write(by_code(&second), None).await.is_err());
        assert!(cora.listed().await.is_empty());
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
