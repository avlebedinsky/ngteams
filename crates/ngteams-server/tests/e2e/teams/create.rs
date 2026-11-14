use teams_proto::ChannelType;

use super::{Seat, server, stored_teams};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_creates_a_team_and_lists_it_with_its_general_channel() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        assert!(ada.listed().await.is_empty());
        let (thread, group) = ada.make_team("Rocket", false).await;
        assert!(
            thread.starts_with("19:") && thread.ends_with("@thread.tacv2"),
            "{thread}"
        );
        assert!(uuid::Uuid::parse_str(&group).is_ok(), "{group}");

        let stored = stored_teams(&ada.engine, |teams| teams.iter().any(|team| team.id == thread)).await;
        let team = stored.iter().find(|team| team.id == thread).unwrap();
        assert_eq!(team.name, "Rocket");
        assert_eq!(team.group_id.as_deref(), Some(group.as_str()));
        assert!(team.owner && team.can_add_channel && team.can_add_private_channel);
        assert!(!team.archived && !team.about.public && !team.about.hidden);
        assert_eq!(team.about.description, "Rocket's people");
        assert_eq!(team.about.people, 1);
        assert_eq!(team.channels.len(), 1);
        assert!(team.channels[0].general && team.channels[0].shown);
        assert_eq!(team.channels[0].id, thread);
        assert_eq!(team.channels[0].name, "General");

        let listed = ada.team(&thread).await;
        assert_eq!(listed.channels[0].kind, ChannelType::Standard);
        assert_eq!(
            listed.about.tenant,
            ngteams_server::db::meta::tenant(&server.app.db).await.unwrap()
        );
        assert!(bert.listed().await.is_empty(), "a team is listed for its people alone");

        let (public, _) = ada.make_team("Open house", true).await;
        assert!(ada.team(&public).await.about.public);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
