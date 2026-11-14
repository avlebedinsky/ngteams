use teams_proto::chat::{ChatReads, MessageText};
use teams_proto::{Author, ChannelType, Mri, SendMessage, reply_chain_id};

use super::support::{Names, Rig, Seat};
use crate::support::backends;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_channel_is_listed_read_and_posted_in_by_its_people_alone() {
    for backend in backends() {
        let mut rig = Rig::start(backend, "private").await;
        let [a, b, c] = &rig.seats;
        let (team, group) = a.make_team("Rocket", false).await;
        a.add(&team, &group, &[b, c]).await;
        let (secret, secret_group) = a.make_private(&team, &group, "Secret").await;
        let people = vec![c.mri.clone()];
        let failed = a
            .engine()
            .add_channel_members(group.as_str(), secret_group.as_str(), people)
            .await;
        assert!(failed.unwrap().is_empty());

        // c, whom its owner added, has it; b, on the team but not in it, is told of no such channel
        let listed = async |seat: &Seat| {
            let team = seat.team(&team).await;
            team.channels.into_iter().find(|channel| channel.id == secret)
        };
        let own = listed(a).await.unwrap();
        assert_eq!(own.kind, ChannelType::Private);
        assert!(own.owner);
        assert!(!listed(c).await.unwrap().owner);
        assert!(listed(b).await.is_none());
        let managed = async |seat: &Seat| {
            let channels = seat.engine().team_channels(group.as_str()).await.unwrap();
            channels.iter().any(|row| row.id == secret)
        };
        assert!(managed(c).await && !managed(b).await);
        for seat in [b, c] {
            seat.sync().await;
            let stored = seat.stored_teams(|teams| teams.iter().any(|t| t.id == team)).await;
            let has = stored[0].channels.iter().any(|channel| channel.id == secret);
            assert_eq!(has, seat.mri == c.mri, "the engine's list");
        }

        let post = a.send(&secret, "For our eyes only").await;
        assert_eq!(c.engine().sync_conversation(secret.as_str()).await.unwrap().inserted, 1);
        let chain = reply_chain_id(&secret, &post).unwrap();
        let reply = c.send(&chain, "Understood").await;
        let own_post = c.send(&secret, "One more thing").await;
        a.engine().sync_conversation(secret.as_str()).await.unwrap();
        assert_eq!(
            a.stored_ids(&secret).await,
            [own_post.clone(), reply.clone(), post.clone()]
        );
        assert_eq!(
            ChatReads::new(&*c.client).members(&secret).await.unwrap().members.len(),
            2
        );

        kept_out(
            b,
            &secret,
            &chain,
            &Names::default().with("group", &group).with("secret", &secret),
        )
        .await;
        let page = ChatReads::new(&*a.client).messages(&secret, 200).await.unwrap();
        assert_eq!(page.messages.len(), 3, "nothing of b's got in");

        // the same after b's and c's clients start again on their stores
        for seat in &mut rig.seats[1..] {
            seat.restart();
        }
        let [_, b, c] = &rig.seats;
        assert!(b.stored_ids(&secret).await.is_empty());
        let all = [own_post, reply, post];
        assert_eq!(c.stored_ids(&secret).await, all, "kept in c's store");
        assert_eq!(c.engine().sync_conversation(secret.as_str()).await.unwrap().inserted, 0);
        assert_eq!(c.stored_ids(&secret).await, all);
        assert!(b.engine().sync_conversation(secret.as_str()).await.is_err());
        rig.stop().await;
    }
}

/// Someone on the team but not in its private channel: no read, no post, no reply, no change.
async fn kept_out(b: &Seat, secret: &str, chain: &str, names: &Names) {
    let reads = ChatReads::new(&*b.client);
    assert_eq!(reads.thread(secret).await.unwrap_err().status(), Some(403));
    assert_eq!(reads.members(secret).await.unwrap_err().status(), Some(403));
    assert_eq!(reads.messages(secret, 200).await.unwrap_err().status(), Some(403));
    assert_eq!(reads.messages(chain, 200).await.unwrap_err().status(), Some(403));
    for conversation in [secret, chain] {
        let author = Author::new(Mri::new(&b.mri), "QA Operator");
        let send = SendMessage::new(conversation, author, MessageText::plain("Let me in").unwrap());
        assert_eq!(b.client.write(send).await.unwrap().status(), 403, "{conversation}");
        assert!(b.engine().sync_conversation(conversation).await.is_err());
    }
    no_changes(b, names).await;
}

/// The routes that name the channel answer as if there were none.
async fn no_changes(b: &Seat, names: &Names) {
    for (method, path, body) in [
        ("POST", "{PROV}/teams/{group}/channels/{secret}/archive", ""),
        ("PUT", "{PROV}/channels/{secret}/members", r#"{"users":[]}"#),
        (
            "PATCH",
            "{MT}/teams/{secret}/channels/{secret}",
            r#"{"displayName":"Mine"}"#,
        ),
        (
            "GET",
            "{MT}/analytics/teams/{group}/channels/{secret}/summary-timeseries",
            "",
        ),
    ] {
        let (status, answer) = b.call(method, path, body, names).await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
    }
}
