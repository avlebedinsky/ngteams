use teams_proto::{Mri, TeamWrite};

use super::{Seat, refusal, server};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_tags_are_made_changed_and_deleted_by_those_its_settings_allow() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;

        let empty = ada.engine.team_tags(team.as_str()).await.unwrap();
        assert!(empty.tags.is_empty() && empty.owners_only && !empty.automatic);

        let create = |name: &str, members: &[&Seat]| TeamWrite::CreateTag {
            team: team.clone(),
            name: name.into(),
            description: "Who flies".into(),
            members: members.iter().map(|seat| Mri::new(&seat.mri)).collect(),
        };
        assert_eq!(
            refusal(bert.engine.team_write(create("Mine", &[&bert]), None).await).as_deref(),
            Some("Forbidden")
        );
        ada.engine
            .team_write(create("Crew", &[&ada, &bert]), None)
            .await
            .unwrap();
        assert_eq!(
            refusal(ada.engine.team_write(create("crew", &[&ada]), None).await).as_deref(),
            Some("TagNameAlreadyExists")
        );
        assert!(
            ada.engine.team_write(create("Guests", &[&cora]), None).await.is_err(),
            "a tag is for people on the team"
        );
        let read = bert.engine.team_tags(team.as_str()).await.unwrap();
        assert_eq!(read.tags.len(), 1);
        let tag = read.tags[0].clone();
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
        ada.engine.team_write(edit(tag.version), None).await.unwrap();
        let changed = ada.engine.team_tags(team.as_str()).await.unwrap().tags[0].clone();
        assert_eq!(changed.name, "Flight crew");
        assert!(changed.version > tag.version);
        assert_eq!(
            refusal(ada.engine.team_write(edit(tag.version), None).await).as_deref(),
            Some("TagVersionMismatch"),
            "a change made against an older version is refused"
        );

        let settings = TeamWrite::TagSettings {
            team: team.clone(),
            owners_only: false,
            automatic: true,
        };
        assert!(bert.engine.team_write(settings.clone(), None).await.is_err());
        ada.engine.team_write(settings, None).await.unwrap();
        let read = bert.engine.team_tags(team.as_str()).await.unwrap();
        assert!(!read.owners_only && read.automatic);
        bert.engine.team_write(create("Ground", &[&bert]), None).await.unwrap();

        let delete = TeamWrite::DeleteTag {
            team: team.clone(),
            tag: tag.id.clone(),
        };
        bert.engine.team_write(delete.clone(), None).await.unwrap();
        assert!(bert.engine.team_write(delete, None).await.is_err());
        let left = ada.engine.team_tags(team.as_str()).await.unwrap();
        assert_eq!(left.tags.len(), 1);
        assert_eq!(left.tags[0].name, "Ground");
        assert!(cora.engine.team_tags(team.as_str()).await.unwrap().tags.is_empty());
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
