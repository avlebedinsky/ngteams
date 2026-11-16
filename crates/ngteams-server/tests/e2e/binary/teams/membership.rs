use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, Mri, SendMessage};

use super::support::{Names, Rig};
use crate::support::backends;
use crate::teams::security::OUTSIDE;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_team_shows_for_the_people_on_it_and_an_outsider_is_refused_everywhere() {
    for backend in backends() {
        let rig = Rig::start(backend, "membership").await;
        let [a, b, c] = &rig.seats;
        assert!(a.listed().await.is_empty());
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b]).await;

        b.sync().await;
        let stored = b.stored_teams(|teams| teams.iter().any(|t| t.id == team)).await;
        assert_eq!(stored.len(), 1);
        let shown = &stored[0];
        assert_eq!(shown.name, "Rocket");
        assert_eq!(shown.group_id.as_deref(), Some(group.as_str()));
        assert!(!shown.owner && !shown.archived);
        assert_eq!(shown.about.description, "Rocket's people");
        assert_eq!(shown.about.people, 2);
        assert_eq!(shown.channels.len(), 1);
        assert_eq!(
            (shown.channels[0].id.as_str(), shown.channels[0].name.as_str()),
            (team.as_str(), "General")
        );
        assert!(shown.channels[0].general && shown.channels[0].shown);
        assert!(b.engine().conversation(team.clone()).await.unwrap().is_some());
        let own = a.stored_teams(|teams| teams.iter().any(|t| t.id == team)).await;
        assert!(own[0].owner && own[0].can_add_channel);

        let channel = a.make_channel(&team, &group, "Launch", true).await;
        a.send(&team, "Welcome aboard").await;
        a.send(&channel, "Fuel is loaded").await;

        c.sync().await;
        assert!(c.listed().await.is_empty());
        assert!(c.stored_teams(|_| true).await.is_empty());
        assert!(c.engine().conversation(team.clone()).await.unwrap().is_none());
        let stored = c.engine().conversations(50).await.unwrap();
        assert!(stored.iter().all(|entry| crate::support::own_stream(&entry.id)));
        assert!(c.engine().team_channels(group.as_str()).await.unwrap().is_empty());
        {
            let team: &str = &team;
            let group: &str = &group;
            let channel: &str = &channel;
            let names = Names::default()
                .with("team", team)
                .with("group", group)
                .with("channel", channel)
                .with("cora", &c.mri);
            for (method, path, body) in OUTSIDE {
                let (status, answer) = c.call(method, path, body, &names).await;
                assert_eq!(status, 404, "{method} {path}: {answer}");
                assert_eq!(answer["errorCode"], "NotFound", "{method} {path}");
            }
            let settings = r#"{"spaceAdminSettings":"{\"userSettings\":{}}"}"#;
            let path = "{CHAT}/threads/{team}/properties?name=spaceAdminSettings";
            assert_eq!(c.call("PUT", path, settings, &names).await.0, 403);
            let path = "{CHAT}/threads/{channel}/members/{cora}";
            assert_eq!(c.call("PUT", path, r#"{"interest":"Interested"}"#, &names).await.0, 403);

            let reads = ChatReads::new(&*c.client);
            for thread in [team, channel] {
                assert_eq!(reads.thread(thread).await.unwrap_err().status(), Some(403), "{thread}");
                assert_eq!(reads.members(thread).await.unwrap_err().status(), Some(403), "{thread}");
                assert_eq!(
                    reads.messages(thread, 10).await.unwrap_err().status(),
                    Some(403),
                    "{thread}"
                );
                let author = Author::new(Mri::new(&c.mri), "QA Operator");
                let send = SendMessage::new(thread, author, MessageText::plain("intrusion").unwrap());
                assert_eq!(c.client.write(send).await.unwrap().status(), 403, "{thread}");
                assert!(c.engine().sync_conversation(thread).await.is_err(), "{thread}");
            }
            assert!(crate::support::only_own_streams(
                &reads.conversations().await.unwrap().conversations
            ));
        }

        // nothing of it changed, and its people still read it
        let after = a.team(&team).await;
        assert_eq!((after.channels.len(), after.about.people), (2, 2));
        assert_eq!(after.name, "Rocket");
        assert_eq!(
            b.engine().sync_conversation(channel.as_str()).await.unwrap().inserted,
            1
        );
        rig.stop().await;
    }
}
