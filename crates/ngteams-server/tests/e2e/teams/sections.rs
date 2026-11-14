use teams_proto::{
    ChannelData, ChannelRead, ChannelSectionAction, ChannelSectionWrite, ChannelSections, CsaTeams, Transport as _,
};

use super::{Seat, server};
use crate::support::backends;

async fn sections(seat: &Seat) -> ChannelSections {
    let data = seat.engine.channel_data(ChannelRead::Sections).await.unwrap();
    ChannelSections::read(&data.0).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_keeps_channels_in_sections_of_its_own() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;

        let fresh = sections(&ada).await;
        assert_eq!(fresh.sections.len(), 1);
        assert!(fresh.sections[0].favorite && fresh.sections[0].items.is_empty());
        let favorites = fresh.sections[0].id.clone();

        let create = ChannelSectionAction::Create {
            channel: channel.clone(),
            name: "Mission".into(),
        };
        ada.engine.channel_section(create).await.unwrap();
        let made = sections(&ada).await;
        assert!(made.version > fresh.version);
        assert_eq!(made.sections.len(), 2);
        let mission = &made.sections[1];
        assert_eq!(mission.name, "Mission");
        assert!(!mission.favorite && mission.expanded);
        assert_eq!(mission.items, [channel.clone()]);

        let fold = ChannelSectionAction::Expanded {
            section: mission.id.clone(),
            expanded: false,
        };
        ada.engine.channel_section(fold).await.unwrap();
        assert!(!sections(&ada).await.sections[1].expanded);

        let to_favorites = ChannelSectionAction::Move {
            channel: channel.clone(),
            section: favorites.clone(),
        };
        ada.engine.channel_section(to_favorites.clone()).await.unwrap();
        let moved = sections(&ada).await;
        assert_eq!(moved.sections[0].items, [channel.clone()]);
        assert!(moved.sections[1].items.is_empty());
        let answer = ada.client.read(&CsaTeams).await.unwrap();
        let listed = CsaTeams::read_answer(answer.body()).unwrap();
        assert_eq!(
            listed.favorites,
            Some(vec![channel.clone()]),
            "the teams read lists the favourites"
        );

        // a change made against an older hierarchy is refused, not applied over the newer one
        let stale = ChannelSectionWrite::new(&to_favorites, &made).unwrap();
        let refused = ada.client.transport().channel_own(&stale).await.unwrap();
        assert_eq!(refused.status(), 409);
        assert_eq!(sections(&ada).await, moved);

        let remove = ChannelSectionAction::Remove {
            channel: channel.clone(),
        };
        ada.engine.channel_section(remove).await.unwrap();
        assert!(
            sections(&ada)
                .await
                .sections
                .iter()
                .all(|section| section.items.is_empty())
        );

        let theirs = sections(&bert).await;
        assert_eq!(theirs.sections.len(), 1, "sections are the account's own");
        let _ = ChannelData::default();
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
