//! QA of the live team, channel and group chat people lists, as users of four running ngteams clients on two nodes
//! meet them: each change shows within a few seconds in the list of everyone it concerns (the one who made it, a
//! bystander, the one it was done to) and in nobody else's, with odd names; a burst of channels and a burst of people
//! end right in every list, and each client's reads of the teams and of the chat stay a few rather than one per
//! change (counted at a relay of its own in front of its node, which writes down each request it passes). And a
//! Microsoft account's engine on the mock beside an own server's in one process: each reads only on its own pushes.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use teams_core::engine::{Action, ChatHistory, EngineHandle};
use teams_mock::{ABC_CHAT, CHAT_BASE, CSA_BASE, LAB_A, MIDDLE_TIER_BASE};
use teams_proto::{
    ChannelChange, ChannelTarget, ChannelWrite, MemberUpdate, Mri, NewChannelKind, NewTeam, TeamRole, TeamWrite,
};
use teams_store::Team;

use super::channels::channel;
use super::people::knows_people;
use super::support::{Rig, Tally, User};
use crate::chatsvc::members::RemoveMember;
use crate::support::{PASSWORD, backends};

/// How long a change may take to show in a running client's list: "a few seconds".
const LIVE: Duration = Duration::from_secs(6);
/// For each of `tallies`, the GET requests it passed whose line holds every one of `parts`.
fn each(tallies: &[Tally], parts: &[&str]) -> Vec<usize> {
    let decoded = |line: &str| line.replace("%3A", ":").replace("%3a", ":").replace("%40", "@");
    let wanted = |line: &String| {
        let line = decoded(line);
        line.starts_with("GET ") && parts.iter().all(|part| line.contains(part))
    };
    tallies
        .iter()
        .map(|tally| tally.lines().iter().filter(|line| wanted(line)).count())
        .collect()
}

fn since(now: &[usize], before: &[usize]) -> Vec<usize> {
    now.iter().zip(before).map(|(now, before)| now - before).collect()
}

const TEAMS_READ: &str = "/api/v3/teams/users/me";

/// The client's stored teams once its first read settled them.
async fn listed(user: &mut User) -> Vec<Team> {
    let until = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(teams) = user.engine().teams().await.unwrap() {
            return teams;
        }
        assert!(
            tokio::time::Instant::now() < until,
            "{}: the teams were never read",
            user.upn
        );
        user.settle().await;
    }
}

fn shape(teams: &[Team]) -> Vec<(String, Vec<String>)> {
    let channels = |team: &Team| team.channels.iter().map(|c| c.name.clone()).collect();
    teams.iter().map(|team| (team.name.clone(), channels(team))).collect()
}

/// Waits up to [`LIVE`] until `user`'s running client lists what `pick` takes; how long it took.
async fn shows(user: &mut User, what: &str, pick: impl Fn(&[Team]) -> bool) -> Duration {
    let start = tokio::time::Instant::now();
    loop {
        let teams = listed(user).await;
        if pick(&teams) {
            return start.elapsed();
        }
        assert!(
            start.elapsed() < LIVE,
            "{}: the list never showed {what} within {LIVE:?}: {:?}",
            user.upn,
            shape(&teams)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        user.settle_briefly().await;
    }
}

fn team_of<'a>(teams: &'a [Team], id: &str) -> Option<&'a Team> {
    teams.iter().find(|team| team.id == id)
}

fn channels_of(teams: &[Team], id: &str) -> Vec<String> {
    team_of(teams, id)
        .map(|team| team.channels.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default()
}

fn target(team: &str, group: &str, channel: &str, channel_group: Option<&str>) -> ChannelTarget {
    ChannelTarget {
        team: team.into(),
        team_group: group.into(),
        channel: channel.into(),
        channel_group: channel_group.map(str::to_owned),
        mailbox: None,
    }
}

async fn change_channel(owner: &User, target: ChannelTarget, action: ChannelChange) {
    let write = TeamWrite::Channel(ChannelWrite { target, action });
    owner.engine().team_write(write, None).await.unwrap();
}

/// A team `name` of `owner`'s with `people` as members; its thread and group.
async fn make_team(owner: &User, name: &str, people: &[&User]) -> (String, String) {
    let create = TeamWrite::Create(NewTeam {
        name: name.into(),
        description: "QA's made-up team".into(),
        first_channel: "General".into(),
        public: false,
    });
    let (thread, group) = owner.engine().team_write(create, None).await.unwrap().created.unwrap();
    let group = group.unwrap();
    if !people.is_empty() {
        add_to_team(owner, &thread, &group, people).await;
    }
    (thread, group)
}

async fn add_to_team(owner: &User, team: &str, group: &str, people: &[&User]) {
    let add = TeamWrite::AddMembers {
        team: team.into(),
        group: group.into(),
        members: people.iter().map(|user| (user.mri.clone(), TeamRole::Member)).collect(),
    };
    let written = owner.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
}

/// The longest wait any check took, said at the end.
#[derive(Default)]
struct Slowest(Duration, String);

impl Slowest {
    fn note(&mut self, took: Duration, what: &str) {
        if took > self.0 {
            *self = Self(took, what.to_owned());
        }
    }
}

const TEAM_NAME: &str = "Ünïcødé — 🚀 Team <&> \"quoted\" 'n' %20 \\ slash/";
const CHANNEL: &str = "🚀 Launch — ünï (beta) [x] = 1; ok!";
const RENAMED: &str = "قناة الإطلاق ← right to left";
const TEAM_RENAMED: &str = "日本語のチーム 🧪 renamed";
const PRIVATE: &str = "Ŝecret 🔒 private";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_team_change_shows_live_for_the_one_who_made_it_a_bystander_and_the_one_it_was_done_to_and_nobody_else() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        // the owner and an outsider on the first node, a member and the one changed on the other
        let mut a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.team_user(rig.other(), "b@own.test").await;
        let mut c = rig.team_user(rig.other(), "c@own.test").await;
        let mut d = rig.team_user(&rig.first, "d@own.test").await;
        for user in [&mut a, &mut b, &mut c, &mut d] {
            assert!(listed(user).await.is_empty(), "{}", user.upn);
        }
        let mut slowest = Slowest::default();

        // a team of a's with b in it: a and b list it with its odd name
        let (team, group) = make_team(&a, TEAM_NAME, &[&b]).await;
        for user in [&mut a, &mut b] {
            let took = shows(user, "the new team", |teams| {
                team_of(teams, &team).is_some_and(|team| team.name == TEAM_NAME)
            })
            .await;
            slowest.note(took, "new team");
        }

        // c added later: c's list shows it, with its channels
        add_to_team(&a, &team, &group, &[&c]).await;
        let took = shows(&mut c, "the team it was added to", |teams| {
            channels_of(teams, &team) == ["General"]
        })
        .await;
        slowest.note(took, "added to the team");

        makes_and_renames([&mut a, &mut b, &mut c], &team, &group, &mut slowest).await;
        in_and_out_of_a_private_channel([&mut a, &mut b, &mut c], &team, &group, &mut slowest).await;

        // c taken off the team: it goes from c's list alone
        let removed = TeamWrite::UpdateMember {
            team: team.clone(),
            group: group.clone(),
            member: c.mri.clone(),
            update: MemberUpdate::Removed,
        };
        a.engine().team_write(removed, None).await.unwrap();
        let took = shows(&mut c, "the team gone", |teams| team_of(teams, &team).is_none()).await;
        slowest.note(took, "taken off the team");
        for user in [&mut a, &mut b] {
            assert!(team_of(&listed(user).await, &team).is_some(), "{}", user.upn);
        }

        // the team deleted: it goes from a's and b's lists
        let delete = TeamWrite::Delete { team: team.clone() };
        a.engine().team_write(delete, None).await.unwrap();
        for user in [&mut a, &mut b] {
            let took = shows(user, "the deleted team gone", |teams| team_of(teams, &team).is_none()).await;
            slowest.note(took, "team deleted");
        }

        // d, in none of it, never listed the team
        d.settle().await;
        assert!(listed(&mut d).await.is_empty(), "{:?}", shape(&listed(&mut d).await));
        eprintln!("{backend:?}: slowest change to show: {:?} ({})", slowest.0, slowest.1);
        drop((a, b, c, d));
        rig.stop().await;
    }
}

/// a makes a channel, renames it (right to left) and renames the team: a, b and c list each.
async fn makes_and_renames(users: [&mut User; 3], team: &str, group: &str, slowest: &mut Slowest) {
    let [a, b, c] = users;
    // a channel made, then renamed (right to left), then the team renamed: everyone in it sees each
    let (launch, _) = channel(a, team, group, CHANNEL, NewChannelKind::Standard).await;
    for user in [&mut *a, &mut *b, &mut *c] {
        let took = shows(user, "the new channel", |teams| {
            channels_of(teams, team).contains(&CHANNEL.to_owned())
        })
        .await;
        slowest.note(took, "new channel");
    }
    let rename = ChannelChange::Edit {
        name: RENAMED.into(),
        description: "renamed".into(),
    };
    change_channel(a, target(team, group, &launch, None), rename).await;
    for user in [&mut *a, &mut *b, &mut *c] {
        let took = shows(user, "the channel's new name", |teams| {
            let names = channels_of(teams, team);
            names.contains(&RENAMED.to_owned()) && !names.contains(&CHANNEL.to_owned())
        })
        .await;
        slowest.note(took, "channel renamed");
    }
    let edit = TeamWrite::Edit {
        team: team.to_owned(),
        name: Some(TEAM_RENAMED.into()),
        description: "renamed".into(),
        public: None,
    };
    a.engine().team_write(edit, None).await.unwrap();
    for user in [&mut *a, &mut *b, &mut *c] {
        let took = shows(user, "the team's new name", |teams| {
            team_of(teams, team).is_some_and(|team| team.name == TEAM_RENAMED)
        })
        .await;
        slowest.note(took, "team renamed");
    }
}

/// a puts c into a private channel and takes it out again: a and c list it, b never does; c's list drops it after.
async fn in_and_out_of_a_private_channel(users: [&mut User; 3], team: &str, group: &str, slowest: &mut Slowest) {
    let [a, b, c] = users;
    // a private channel with c in it: a and c list it, b doesn't
    let (secret, secret_group) = channel(a, team, group, PRIVATE, NewChannelKind::Private).await;
    let secret_group = secret_group.unwrap();
    let failed = a
        .engine()
        .add_channel_members(group, &secret_group, vec![c.mri.as_str().to_owned()]);
    assert!(failed.await.unwrap().is_empty());
    for user in [&mut *a, &mut *c] {
        let took = shows(user, "the private channel", |teams| {
            channels_of(teams, team).contains(&PRIVATE.to_owned())
        })
        .await;
        slowest.note(took, "private channel in");
    }
    b.settle().await;
    assert!(!channels_of(&listed(b).await, team).contains(&PRIVATE.to_owned()));

    // c out of the private channel: it goes from c's list, a keeps it
    let out = ChannelChange::RemoveMember {
        mri: c.mri.as_str().to_owned(),
    };
    change_channel(a, target(team, group, &secret, Some(&secret_group)), out).await;
    let took = shows(c, "the private channel gone", |teams| {
        let names = channels_of(teams, team);
        names.contains(&RENAMED.to_owned()) && !names.contains(&PRIVATE.to_owned())
    })
    .await;
    slowest.note(took, "private channel out");
    assert!(channels_of(&listed(a).await, team).contains(&PRIVATE.to_owned()));
}

/// How many channels the burst makes.
const BURST: usize = 30;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thirty_channels_made_quickly_end_right_in_every_members_list_with_a_few_teams_reads_each() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let tallies = [
            Tally::to(&rig.first).await,
            Tally::to(rig.other()).await,
            Tally::to(rig.other()).await,
            Tally::to(&rig.first).await,
        ];
        let mut a = rig.user_via(&rig.first, "a@own.test", Some(&tallies[0]), true).await;
        let mut b = rig.user_via(rig.other(), "b@own.test", Some(&tallies[1]), true).await;
        let mut c = rig.user_via(rig.other(), "c@own.test", Some(&tallies[2]), true).await;
        let mut d = rig.user_via(&rig.first, "d@own.test", Some(&tallies[3]), true).await;
        let (team, group) = make_team(&a, "Burst team", &[&b, &c, &d]).await;
        for user in [&mut a, &mut b, &mut c, &mut d] {
            shows(user, "the team", |teams| team_of(teams, &team).is_some()).await;
        }
        for user in [&mut a, &mut b, &mut c, &mut d] {
            user.settle().await;
        }
        let before = each(&tallies, &[TEAMS_READ]);

        let started = tokio::time::Instant::now();
        let names: Vec<String> = (1..=BURST).map(|n| format!("Burst {n:02} — ç{n}")).collect();
        let mut made_at = Vec::new();
        for name in &names {
            channel(&a, &team, &group, name, NewChannelKind::Standard).await;
            made_at.push(tokio::time::Instant::now());
        }
        let made = started.elapsed();
        let pauses = pauses(&made_at);
        let mut expected: Vec<String> = names.clone();
        expected.push("General".into());
        expected.sort();
        for user in [&mut a, &mut b, &mut c, &mut d] {
            lists_exactly(user, &team, &expected).await;
        }
        let settled = started.elapsed();
        // a quiet stretch after it: no read owed is left running
        for user in [&mut a, &mut b, &mut c, &mut d] {
            user.settle().await;
        }
        let reads = since(&each(&tallies, &[TEAMS_READ]), &before);
        eprintln!(
            "{backend:?}: {BURST} channels made in {made:?} with {pauses} pauses, every list right after {settled:?}; \
             teams reads since the burst began, by a (who made them), b, c, d: {reads:?}"
        );
        // every list moved by a read the relay saw: a relay that counts nothing fails here
        assert!(
            reads.iter().all(|reads| *reads >= 1),
            "{reads:?}: a client's teams reads were not counted"
        );
        // the members only hear of them: gathered, a read per 5 s of the burst, one per pause and one after, not one
        // per channel
        let allowed = usize::try_from(settled.as_secs() / 5).unwrap_or(usize::MAX) + pauses + 2;
        // the maker reads them after each channel it made (as before the pushes); the pushes add the members' few
        assert!(
            reads[0] <= BURST + allowed,
            "a: {} teams reads for its {BURST} channels over {settled:?}",
            reads[0]
        );
        for (upn, reads) in ["b", "c", "d"].iter().zip(&reads[1..]) {
            assert!(
                *reads <= allowed,
                "{upn}: {reads} teams reads for {BURST} channels over {settled:?}"
            );
        }
        drop((a, b, c, d));
        rig.stop().await;
    }
}

/// How long a client waits for the next pushed team change before it reads the teams (`teams_core`'s
/// `PUSHED_QUIET`).
const PUSHED_QUIET: Duration = Duration::from_millis(500);
/// How much later or earlier than its write a push may come to a member.
const PUSH_JITTER: Duration = Duration::from_millis(250);

/// The pauses between the writes made at `made_at` that a member's client may see as a pause of its pushes, and read
/// the teams at: on a busy server a channel takes longer than [`PUSHED_QUIET`] to make.
fn pauses(made_at: &[tokio::time::Instant]) -> usize {
    made_at
        .windows(2)
        .filter(|made| made[1] - made[0] + PUSH_JITTER >= PUSHED_QUIET)
        .count()
}

/// Waits until `user`'s list names exactly `expected` (sorted) as the team's channels.
async fn lists_exactly(user: &mut User, team: &str, expected: &[String]) {
    let until = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let mut listed_now = channels_of(&listed(user).await, team);
        listed_now.sort();
        if listed_now == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < until,
            "{}: after the burst the list has {} channels: {listed_now:?}",
            user.upn,
            listed_now.len()
        );
        user.settle_briefly().await;
    }
}

/// How many people the group chat's burst adds at once.
const PEOPLE: usize = 20;

/// Adds the made-up people of the burst; their UPNs and MRIs.
fn add_people(rig: &Rig) -> Vec<(String, Mri)> {
    (1..=PEOPLE)
        .map(|n| {
            let upn = format!("p{n:02}@own.test");
            let name = format!("Pérson {n:02} — 名前 🧑‍💻");
            let added = rig
                .first
                .cli(&["user", "add", &upn, "--name", &name], &format!("{PASSWORD}\n"));
            let oid = added
                .rsplit_once('(')
                .unwrap()
                .1
                .trim()
                .trim_end_matches(')')
                .to_owned();
            (upn, Mri::new(format!("8:orgid:{oid}")))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn twenty_people_added_to_a_group_chat_at_once_show_in_every_running_client_with_one_read_each() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let people = add_people(&rig);
        let tallies = [
            Tally::to(&rig.first).await,
            Tally::to(rig.other()).await,
            Tally::to(rig.other()).await,
            Tally::to(&rig.first).await,
            Tally::to(rig.other()).await,
        ];
        let mut a = rig.user_via(&rig.first, "a@own.test", Some(&tallies[0]), false).await;
        let mut b = rig.user_via(rig.other(), "b@own.test", Some(&tallies[1]), false).await;
        let mut c = rig.user_via(rig.other(), "c@own.test", Some(&tallies[2]), false).await;
        // two of the people added run clients too, one on each node
        let mut p1 = rig.user_via(&rig.first, &people[0].0, Some(&tallies[3]), false).await;
        let mut p2 = rig.user_via(rig.other(), &people[1].0, Some(&tallies[4]), false).await;
        let chat = a.create(&[&b, &c], Some("Twenty more 🎉")).await;
        let first = a.send(&chat, "before the twenty").await;
        for user in [&mut b, &mut c] {
            user.eventually_stored(&chat, &first).await;
        }
        for user in [&mut a, &mut b, &mut c] {
            knows_people(user, &chat, 3).await;
            user.settle().await;
        }
        let before = each(&tallies, &["/v1/threads/", &chat]);

        let started = tokio::time::Instant::now();
        a.write(Action::AddMembers {
            conversation: chat.clone(),
            members: people.iter().map(|(_, mri)| mri.clone()).collect(),
            history: ChatHistory::All,
        })
        .await;
        for user in [&mut a, &mut b, &mut c, &mut p1, &mut p2] {
            people_within(user, &chat, 3 + PEOPLE, LIVE).await;
        }
        let shown = started.elapsed();
        for user in [&mut a, &mut b, &mut c, &mut p1, &mut p2] {
            user.settle().await;
        }
        let reads = since(&each(&tallies, &["/v1/threads/", &chat]), &before);
        eprintln!(
            "{backend:?}: {PEOPLE} people added at once, shown by all 5 clients after {shown:?}; chat reads by a (who \
             added them), b, c, p01, p02: {reads:?}"
        );
        for user in [&a, &b, &c] {
            let members = user.engine().members(&chat).await.unwrap();
            for (upn, mri) in &people {
                assert!(
                    members.iter().any(|member| member.mri == mri.as_str()),
                    "{}: lacks {upn}",
                    user.upn
                );
            }
        }
        // a read or two each, not one per person (and each counted: none is zero)
        assert!(
            reads.iter().all(|reads| (1..=2).contains(reads)),
            "{reads:?} reads of the chat for one add of {PEOPLE}"
        );

        let users = [&mut a, &mut b, &mut c, &mut p1, &mut p2];
        ten_taken_out(users, &tallies, &chat, &people[10..]).await;
        drop((a, b, c, p1, p2));
        rig.stop().await;
    }
}

/// a takes `gone` out of `chat` one after another, quickly: every list ends with the 13 left, each client reading the
/// chat at most once per change.
async fn ten_taken_out(users: [&mut User; 5], tallies: &[Tally], chat: &str, gone: &[(String, Mri)]) {
    // ten of them taken out one after another, quickly: every list ends with the 13 left
    let before = each(tallies, &["/v1/threads/", chat]);
    for (_, mri) in gone {
        let request = RemoveMember {
            chat: chat.to_owned(),
            member: mri.as_str().to_owned(),
        };
        assert_eq!(users[0].client().write(request).await.unwrap().status(), 200);
    }
    for user in users {
        people_within(user, chat, 3 + 10, LIVE).await;
        user.settle().await;
    }
    let reads = since(&each(tallies, &["/v1/threads/", chat]), &before);
    eprintln!("10 taken out one by one; chat reads by a, b, c, p01, p02: {reads:?}");
    // at most one each per change, and each counted
    assert!(
        reads.iter().all(|reads| (1..=10).contains(reads)),
        "{reads:?} reads of the chat for 10 removals"
    );
}

/// Waits up to `wait` until `user`'s client lists `count` people in `chat`.
async fn people_within(user: &mut User, chat: &str, count: usize, wait: Duration) {
    let until = tokio::time::Instant::now() + wait;
    loop {
        let known = user.engine().members(chat).await.unwrap().len();
        if known == count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < until,
            "{}: lists {known} people of {chat}, not {count}, after {wait:?}",
            user.upn
        );
        user.settle_briefly().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channels_made_steadily_without_a_pause_still_show_in_a_members_list_while_they_come() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let tally = Tally::to(rig.other()).await;
        let a = rig.team_user(&rig.first, "a@own.test").await;
        let mut b = rig.user_via(rig.other(), "b@own.test", Some(&tally), true).await;
        let (team, group) = make_team(&a, "Steady team", &[&b]).await;
        shows(&mut b, "the team", |teams| team_of(teams, &team).is_some()).await;
        b.settle().await;
        let before = each(std::slice::from_ref(&tally), &[TEAMS_READ])[0];

        // one channel every 300 ms for 12 s: the changes never pause for the half second the gathering waits
        let started = tokio::time::Instant::now();
        let mut first_seen = None;
        let mut n = 0;
        while started.elapsed() < Duration::from_secs(12) {
            n += 1;
            channel(&a, &team, &group, &format!("Steady {n:02}"), NewChannelKind::Standard).await;
            let next = started + Duration::from_millis(300) * n;
            while tokio::time::Instant::now() < next {
                b.settle_briefly().await;
            }
            if first_seen.is_none() && channels_of(&listed(&mut b).await, &team).contains(&"Steady 01".to_owned()) {
                first_seen = Some(started.elapsed());
            }
        }
        let seen = first_seen.unwrap_or_else(|| panic!("b's list showed none of the {n} channels while they came"));
        let all: Vec<String> = (1..=n).map(|n| format!("Steady {n:02}")).collect();
        shows(&mut b, "every channel", |teams| {
            let names = channels_of(teams, &team);
            all.iter().all(|name| names.contains(name))
        })
        .await;
        b.settle().await;
        let reads = each(std::slice::from_ref(&tally), &[TEAMS_READ])[0] - before;
        eprintln!("{backend:?}: {n} channels over 12 s; b listed the first after {seen:?}; b's teams reads: {reads}");
        // at most the gathering's 5 s after the first, with a tick and a read on top
        assert!(
            seen <= Duration::from_secs(7),
            "b listed the first channel only after {seen:?}"
        );
        assert!(
            (1..=5).contains(&reads),
            "b read the teams {reads} times for {n} channels over 12 s"
        );
        drop((a, b));
        rig.stop().await;
    }
}

/// A Microsoft account's made-up tokens on the mock.
struct MockTokens(String, tokio::sync::broadcast::Sender<teams_auth::session::Renewal>);

impl teams_proto::Credentials for MockTokens {
    fn bearer(
        &self,
        _audience: &str,
    ) -> impl Future<Output = Result<teams_auth::secrets::Secret, teams_auth::token::TokenError>> + Send {
        std::future::ready(Ok(teams_auth::secrets::Secret::new(self.0.clone())))
    }

    fn bearer_rejected(
        &self,
        audience: &str,
        _: &teams_auth::secrets::Secret,
    ) -> impl Future<Output = Result<teams_auth::secrets::Secret, teams_auth::token::TokenError>> + Send {
        self.bearer(audience)
    }
}

impl teams_trouter::TrouterCredentials for MockTokens {
    fn ic3(
        &self,
    ) -> impl Future<Output = Result<teams_auth::session::AccessToken, teams_auth::token::TokenError>> + Send {
        std::future::ready(Ok(teams_auth::session::AccessToken {
            token: teams_auth::secrets::Secret::new(self.0.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }))
    }

    fn ic3_after(
        &self,
        _old: &teams_auth::secrets::Secret,
    ) -> impl Future<Output = Result<teams_auth::session::AccessToken, teams_auth::token::TokenError>> + Send {
        self.ic3()
    }

    fn renewals(&self) -> tokio::sync::broadcast::Receiver<teams_auth::session::Renewal> {
        self.1.subscribe()
    }
}

/// Lab account a's engine on the Microsoft mock, its reads reaching CSA, beside the own-server clients in this process,
/// as the app's one window runs a Microsoft account and an own server's.
fn microsoft_engine(mock: &teams_mock::ChatServiceMock, trouter: &teams_mock::TrouterMock, dir: &Path) -> EngineHandle {
    use std::sync::Arc;
    let tokens = Arc::new(MockTokens(
        mock.token(LAB_A).unwrap(),
        tokio::sync::broadcast::channel(4).0,
    ));
    let origin = mock.origin();
    let http = || {
        teams_proto::HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin)
            .unwrap()
            .with_middle_tier(MIDDLE_TIER_BASE)
            .unwrap()
            .for_account(Mri::new(LAB_A))
    };
    let transport = Arc::new(http());
    let push = teams_trouter::Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())
        .unwrap()
        .with_options(teams_trouter::Options {
            backoff_base: Duration::from_millis(50),
            backoff_cap: Duration::from_millis(200),
            backoff_jitter_percent: 0,
            ..teams_trouter::Options::default()
        });
    teams_core::engine::start(teams_core::engine::Parts {
        calls: None,
        store: teams_store::Store::open(&dir.join("microsoft.sqlite")).unwrap(),
        client: Arc::new(teams_proto::Client::new(http().with_csa(CSA_BASE).unwrap())),
        outbox: teams_core::Outbox::new(Arc::clone(&transport), teams_proto::Author::new(Mri::new(LAB_A), "Lab")),
        push: Some(teams_core::engine::Push::new(push)),
        options: teams_core::engine::EngineOptions {
            tick: Duration::from_millis(20),
            ..teams_core::engine::EngineOptions::default()
        },
        known_names: Vec::new(),
    })
    .unwrap()
}

/// The GETs the mock got whose decoded path ends with `end`.
async fn mock_gets(mock: &teams_mock::ChatServiceMock, end: &str) -> usize {
    let decoded = |path: &str| path.replace("%3A", ":").replace("%40", "@");
    let requests = mock.received_requests().await;
    let gets = requests
        .iter()
        .filter(|request| request.method.as_str() == "GET" && decoded(request.url.path()).ends_with(end));
    gets.count()
}

/// Waits until the mock got more than `than` GETs ending with `end`.
async fn mock_read_more(mock: &teams_mock::ChatServiceMock, end: &str, than: usize) {
    let until = tokio::time::Instant::now() + Duration::from_secs(20);
    while mock_gets(mock, end).await <= than {
        assert!(
            tokio::time::Instant::now() < until,
            "the Microsoft account never read {end} again"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

const MOCK_TEAMS: &str = "/api/v3/teams/users/me";

/// Waits until the Microsoft account is up: its chats and its teams read, its push channel registered.
async fn microsoft_up(
    microsoft: &mut EngineHandle,
    mock: &teams_mock::ChatServiceMock,
    trouter: &teams_mock::TrouterMock,
) {
    assert_eq!(microsoft.teams().await.unwrap(), None);
    mock_read_more(mock, MOCK_TEAMS, 0).await;
    let until = tokio::time::Instant::now() + Duration::from_secs(20);
    while microsoft.members(ABC_CHAT).await.unwrap().is_empty() || trouter.registrations().is_empty() {
        assert!(
            tokio::time::Instant::now() < until,
            "the Microsoft account never came up"
        );
        let _ = tokio::time::timeout(Duration::from_millis(50), microsoft.next_event()).await;
    }
}

/// The own server's team and group chat change: `own`'s client follows them; the team, the chat and the one added
/// to it.
async fn own_changes(rig: &Rig, owner: &User, own: &mut User, c: &User) -> (String, String, User) {
    let (team, group) = make_team(owner, "Own team", &[&*own]).await;
    shows(own, "the own team", |teams| team_of(teams, &team).is_some()).await;
    channel(owner, &team, &group, "Own channel", NewChannelKind::Standard).await;
    shows(own, "the own channel", |teams| {
        channels_of(teams, &team).contains(&"Own channel".to_owned())
    })
    .await;
    let chat = owner.create(&[&*own, c], Some("Own chat")).await;
    let first = owner.send(&chat, "hello, own chat").await;
    own.eventually_stored(&chat, &first).await;
    knows_people(own, &chat, 3).await;
    let d = rig.user(&rig.first, "d@own.test").await;
    owner
        .write(Action::AddMembers {
            conversation: chat.clone(),
            members: vec![d.mri.clone()],
            history: ChatHistory::None,
        })
        .await;
    people_within(own, &chat, 4, LIVE).await;
    own.settle().await;
    (team, chat, d)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_microsoft_account_and_an_own_servers_account_in_one_process_each_follow_only_their_own_pushes() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let tally = Tally::to(&rig.first).await;
        let mock = teams_mock::ChatServiceMock::start().await.unwrap();
        let trouter = mock.start_trouter().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut microsoft = microsoft_engine(&mock, &trouter, dir.path());
        let owner = rig.team_user(rig.other(), "b@own.test").await;
        let mut own = rig.user_via(&rig.first, "a@own.test", Some(&tally), true).await;
        let c = rig.user(rig.other(), "c@own.test").await;

        microsoft_up(&mut microsoft, &mock, &trouter).await;
        let (microsoft_teams, microsoft_chat) = (
            mock_gets(&mock, MOCK_TEAMS).await,
            mock_gets(&mock, &format!("/v1/threads/{ABC_CHAT}")).await,
        );

        let (team, chat, d) = own_changes(&rig, &owner, &mut own, &c).await;
        // ... and the Microsoft account's engine read nothing for them
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            mock_gets(&mock, MOCK_TEAMS).await,
            microsoft_teams,
            "a Microsoft teams read for an own change"
        );
        assert_eq!(
            mock_gets(&mock, &format!("/v1/threads/{ABC_CHAT}")).await,
            microsoft_chat,
            "a Microsoft chat read for an own change"
        );

        // Microsoft's pushes, a channel's and a group chat's (their id alone): its engine reads, the own one doesn't
        let own_teams = each(std::slice::from_ref(&tally), &[TEAMS_READ])[0];
        let own_threads = each(std::slice::from_ref(&tally), &["/v1/threads/"])[0];
        // the own changes were read through the relay: a relay that counts nothing fails here
        assert!(
            own_teams >= 1 && own_threads >= 1,
            "own reads not counted: teams {own_teams}, threads {own_threads}"
        );
        for thread in [teams_mock::CHANNEL, ABC_CHAT] {
            let update =
                serde_json::json!({"type": "EventMessage", "resourceType": "ThreadUpdate", "resource": {"id": thread}});
            trouter.deliver_chat(LAB_A, &update);
        }
        mock_read_more(&mock, &format!("/v1/threads/{ABC_CHAT}"), microsoft_chat).await;
        mock_read_more(&mock, MOCK_TEAMS, microsoft_teams).await;
        own.settle().await;
        assert_eq!(
            each(std::slice::from_ref(&tally), &[TEAMS_READ])[0],
            own_teams,
            "an own teams read for a Microsoft push"
        );
        assert_eq!(
            each(std::slice::from_ref(&tally), &["/v1/threads/"])[0],
            own_threads,
            "an own thread read"
        );
        // the own team and chat stay as they were
        assert!(team_of(&listed(&mut own).await, &team).is_some());
        assert_eq!(own.engine().members(&chat).await.unwrap().len(), 4);
        drop((owner, own, c, d));
        tokio::task::spawn_blocking(move || microsoft.shutdown()).await.unwrap();
        rig.stop().await;
    }
}
