use teams_core::engine::Action;
use teams_core::{ChannelDraft, ChannelProblem, EngineError};
use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Mri, NewChannelKind, TeamRole, TeamWrite};
use teams_store::OutboxState;

use super::{Seat, new_channel, refusal, server};
use crate::support::backends;

/// An archived team takes no channel and nobody new.
async fn archived_refuses(ada: &Seat, cora: &Seat, team: &str, group: &str) {
    let refused = ada
        .engine
        .create_channel(ChannelDraft {
            channel: new_channel(team, group, "Late", NewChannelKind::Standard),
            show_by_default: false,
        })
        .await;
    assert!(
        matches!(
            refused,
            Err(EngineError::ChannelNotCreated {
                why: ChannelProblem::TeamArchived
            })
        ),
        "{refused:?}"
    );
    let add = TeamWrite::AddMembers {
        team: team.into(),
        group: group.into(),
        members: vec![(Mri::new(&cora.mri), TeamRole::Member)],
    };
    assert_eq!(
        refusal(ada.engine.team_write(add, None).await).as_deref(),
        Some("OperationNotAllowedOnArchivedTeam")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_owner_edits_archives_restores_and_deletes_a_team() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;

        let edit = |name: Option<&str>, public| TeamWrite::Edit {
            team: team.clone(),
            name: name.map(str::to_owned),
            description: "To the moon".into(),
            public,
        };
        assert_eq!(
            refusal(bert.engine.team_write(edit(Some("Mine"), None), None).await).as_deref(),
            Some("Forbidden")
        );
        ada.engine
            .team_write(edit(Some("Apollo"), Some(true)), None)
            .await
            .unwrap();
        let listed = bert.team(&team).await;
        assert_eq!(listed.name, "Apollo");
        assert_eq!(listed.about.description, "To the moon");
        assert!(listed.about.public);
        ada.engine.team_write(edit(None, Some(false)), None).await.unwrap();
        let listed = bert.team(&team).await;
        assert_eq!(listed.name, "Apollo", "a name left out stays");
        assert!(!listed.about.public);

        let archive = TeamWrite::Archive {
            group: group.clone(),
            site_read_only: true,
        };
        assert!(bert.engine.team_write(archive.clone(), None).await.is_err());
        ada.engine.team_write(archive, None).await.unwrap();
        let archived = bert.team(&team).await;
        assert!(archived.archived && !archived.can_add_channel);
        archived_refuses(&ada, &cora, &team, &group).await;
        let post = |seat: &Seat, text: &str| {
            seat.engine.write(Action::Send {
                conversation: channel.clone(),
                text: MessageText::plain(text).unwrap(),
            })
        };
        assert_ne!(post(&bert, "Anyone?").await.unwrap().state, OutboxState::Sent);

        let restore = TeamWrite::Restore { group: group.clone() };
        ada.engine.team_write(restore, None).await.unwrap();
        assert!(!bert.team(&team).await.archived);
        assert_eq!(post(&bert, "We are back").await.unwrap().state, OutboxState::Sent);

        let delete = TeamWrite::Delete { team: team.clone() };
        assert_eq!(
            refusal(bert.engine.team_write(delete.clone(), None).await).as_deref(),
            Some("Forbidden")
        );
        ada.engine.team_write(delete, None).await.unwrap();
        assert!(ada.listed().await.is_empty() && bert.listed().await.is_empty());
        let reads = ChatReads::new(&*bert.client);
        for thread in [&team, &channel] {
            assert_eq!(reads.thread(thread).await.unwrap_err().status(), Some(404));
        }
        assert!(reads.conversations().await.unwrap().conversations.is_empty());
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
}
