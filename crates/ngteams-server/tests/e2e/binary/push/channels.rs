//! A team's channels on the push channel: what a member writes in a channel reaches the clients of the people who
//! may read it, live, and nobody else's.

use teams_core::engine::{Action, Event, MessageChange};
use teams_proto::chat::MessageText;
use teams_proto::{
    ChannelChange, ChannelTarget, ChannelWrite, MemberUpdate, NewChannelKind, NewTeam, TeamRole, TeamWrite,
    reply_chain_id,
};

use super::support::{Rig, User};
use crate::support::backends;
use crate::teams::new_channel;

/// A team of `owner`'s with `people` as its members; its thread and group.
pub(super) async fn team(owner: &User, people: &[&User]) -> (String, String) {
    let create = TeamWrite::Create(NewTeam {
        name: "Rocket".into(),
        description: "Rocket's people".into(),
        first_channel: "General".into(),
        public: false,
    });
    let written = owner.engine().team_write(create, None).await.unwrap();
    let (thread, group) = written.created.unwrap();
    let group = group.unwrap();
    let add = TeamWrite::AddMembers {
        team: thread.clone(),
        group: group.clone(),
        members: people.iter().map(|user| (user.mri.clone(), TeamRole::Member)).collect(),
    };
    let written = owner.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
    (thread, group)
}

/// A channel of the team; its thread and, for a private one, its group.
pub(super) async fn channel(
    owner: &User,
    team: &str,
    group: &str,
    name: &str,
    kind: NewChannelKind,
) -> (String, Option<String>) {
    let draft = teams_core::ChannelDraft {
        channel: new_channel(team, group, name, kind),
        show_by_default: false,
    };
    let made = owner.engine().create_channel(draft).await.unwrap();
    (made.thread.unwrap(), made.group)
}

/// How many messages of `conversation` the client was told of since `mark`, however they came.
fn told(user: &User, mark: usize, conversation: &str) -> usize {
    user.count(
        mark,
        |event| matches!(event, Event::Message { conversation: of, .. } if of == conversation),
    )
}

/// After a quiet stretch, `user`'s client was told of nothing in `conversation` since `mark` and stores none of
/// `messages`.
async fn heard_nothing(user: &mut User, mark: usize, conversation: &str, messages: &[&str]) {
    user.settle().await;
    assert_eq!(told(user, mark, conversation), 0, "{}", user.upn);
    for id in messages {
        let stored = user.engine().message(conversation, *id).await.unwrap();
        assert!(stored.is_none(), "{}: {id} is stored", user.upn);
    }
}

fn edit(conversation: &str, id: &str, text: &str) -> Action {
    Action::Edit {
        conversation: conversation.into(),
        message_id: id.into(),
        text: MessageText::plain(text).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channels_post_its_reply_and_their_changes_reach_the_teams_people_live_and_nobody_else() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(&rig.first, "c@own.test").await;
        let mut d = rig.team_user(rig.other(), "d@own.test").await;
        let (team, group) = team(&a, &[&b, &c]).await;
        let (launch, _) = channel(&a, &team, &group, "Launch", NewChannelKind::Standard).await;
        let on_d = d.mark();

        // a post, in the channel and in the team's general channel
        let post = a.send(&launch, "Fuel is loaded").await;
        let welcome = a.send(&team, "Welcome aboard").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &launch, &post, MessageChange::New).await;
            user.shows(&launch, &post, "Fuel is loaded").await;
            user.pushed(0, &team, &welcome, MessageChange::New).await;
        }

        // a reply in the post's chain is the channel's message, under its post
        let chain = reply_chain_id(&launch, &post).unwrap();
        let reply = c.send(&chain, "Go for launch").await;
        for user in [&mut a, &mut b] {
            user.pushed(0, &launch, &reply, MessageChange::New).await;
            user.shows(&launch, &reply, "Go for launch").await;
            let roots = user.engine().reply_roots(launch.as_str()).await.unwrap();
            let root = roots.iter().find(|(message, _)| *message == reply);
            assert_eq!(root.map(|(_, root)| root.as_str()), Some(post.as_str()), "{}", user.upn);
        }

        // its edit, a reaction to it and its delete
        c.write(edit(&launch, &reply, "Go!")).await;
        for user in [&mut a, &mut b] {
            user.pushed(0, &launch, &reply, MessageChange::Edit).await;
            user.shows(&launch, &reply, "Go!").await;
        }
        a.write(Action::React {
            conversation: launch.clone(),
            message_id: reply.clone(),
            key: "like".into(),
        })
        .await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &launch, &reply, MessageChange::Reaction).await;
        }
        c.write(Action::Delete {
            conversation: launch.clone(),
            message_id: reply.clone(),
        })
        .await;
        for user in [&mut a, &mut b] {
            user.pushed(0, &launch, &reply, MessageChange::Delete).await;
        }

        // someone the owner took off the team is pushed nothing more
        let removed = TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: c.mri.clone(),
            update: MemberUpdate::Removed,
        };
        a.engine().team_write(removed, None).await.unwrap();
        c.settle().await;
        let on_c = c.mark();
        let later = a.send(&launch, "Lift-off").await;
        let general = a.send(&team, "We are up").await;
        b.pushed(0, &launch, &later, MessageChange::New).await;
        b.pushed(0, &team, &general, MessageChange::New).await;
        heard_nothing(&mut c, on_c, &launch, &[&later]).await;
        heard_nothing(&mut c, on_c, &team, &[&general]).await;

        // and someone who never was on the team nothing at all
        heard_nothing(&mut d, on_d, &launch, &[&post, &later]).await;
        heard_nothing(&mut d, on_d, &team, &[&welcome]).await;
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_private_channels_posts_and_replies_reach_its_own_people_alone() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(rig.other(), "c@own.test").await;
        let (team, group) = team(&a, &[&b, &c]).await;
        let (secret, secret_group) = channel(&a, &team, &group, "Secret", NewChannelKind::Private).await;
        let secret_group = secret_group.unwrap();
        let people = vec![c.mri.as_str().to_owned()];
        let failed = a
            .engine()
            .add_channel_members(group.as_str(), secret_group.as_str(), people);
        assert!(failed.await.unwrap().is_empty());
        let on_b = b.mark();

        // c, in the channel, hears the post and a's client c's reply; b, only on the team, neither
        let post = a.send(&secret, "For our eyes only").await;
        c.pushed(0, &secret, &post, MessageChange::New).await;
        c.shows(&secret, &post, "For our eyes only").await;
        let chain = reply_chain_id(&secret, &post).unwrap();
        let reply = c.send(&chain, "Understood").await;
        a.pushed(0, &secret, &reply, MessageChange::New).await;
        a.write(edit(&secret, &post, "For our eyes alone")).await;
        c.pushed(0, &secret, &post, MessageChange::Edit).await;
        heard_nothing(&mut b, on_b, &secret, &[&post, &reply]).await;

        // out of the channel, c hears no more of it; it stays on the team and hears that
        let out = TeamWrite::Channel(ChannelWrite {
            target: ChannelTarget {
                team: team.clone(),
                team_group: group.clone(),
                channel: secret.clone(),
                channel_group: Some(secret_group),
                mailbox: None,
            },
            action: ChannelChange::RemoveMember {
                mri: c.mri.as_str().to_owned(),
            },
        });
        a.engine().team_write(out, None).await.unwrap();
        c.settle().await;
        let on_c = c.mark();
        let later = a.send(&secret, "Between us").await;
        let open = a.send(&team, "For everyone").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &team, &open, MessageChange::New).await;
        }
        heard_nothing(&mut c, on_c, &secret, &[&later]).await;
        heard_nothing(&mut b, on_b, &secret, &[&later]).await;
        rig.stop().await;
    }
}
