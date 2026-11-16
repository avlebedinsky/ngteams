//! A lived-in server before the people of the tests sign in: hundreds of synthetic accounts, chats with them, their
//! messages, a team and its channels, files, pictures and calendar events, odd cases included. All synthetic.

use diesel::QueryDsl as _;
use diesel_async::RunQueryDsl as _;
use ngteams_server::db::Db;
use ngteams_server::db::schema::users;
use ngteams_server::db::users::User as Account;
use teams_core::engine::Action;
use teams_proto::chat::MessageText;
use teams_proto::{Cloud, Mri, NewTeam, TeamRole, TeamWrite};

use super::super::media::support::{paste, send_file};
use super::super::push::support::{User, options};
use super::support::Cluster;
use crate::calendar::meetings::invited;
use crate::calendar::{DAY, HOUR, coming_monday, draft};

/// How many synthetic accounts the server holds besides the tests' people.
pub const ACCOUNTS: usize = 300;
/// The group chats a seeder makes with the tests' people in them.
pub const GROUPS: usize = 8;
/// Messages in each seeded chat.
const MESSAGES: usize = 12;

/// Names people have: other scripts, right to left, emoji, combining marks, quotes, markup, an injection, spaces,
/// the longest a name may be.
fn odd_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "Zoë O'Brien-Ångström",
        "李 小龍",
        "ابن سينا",
        "שרה כהן",
        "Dr. 🦀 Ferris",
        "Robert'); DROP TABLE users;--",
        "<b>Bold</b> & \"Quoted\"",
        "Ana   Maria   de la Cruz",
        "Zoe\u{0308} Combined",
        "back\\slash",
        "null",
        "Ωmega ∑um",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    names.push("L".repeat(256));
    names
}

/// Texts people send: emoji, other scripts, markup that must stay text, a long one, links, odd spacing.
fn odd_texts() -> Vec<String> {
    let mut texts: Vec<String> = [
        "hello 👋",
        "ünïcödé — “quotes” and ‘single’",
        "مرحبا بالعالم",
        "<script>alert('x')</script> stays text",
        "a link: https://example.test/path?q=1&r=2#frag",
        "  leading and trailing spaces  ",
        "line one\nline two\n\nline four",
        "👨‍👩‍👧‍👦 family, 🏳️‍🌈 flag, 1️⃣ keycap",
        "Ze\u{0301}phyr combining accent",
        "tab\there",
        "100% & <b>not bold</b>",
    ]
    .iter()
    .map(|text| (*text).to_owned())
    .collect();
    texts.push("long ".repeat(1600));
    texts
}

/// What was seeded: the synthetic accounts' MRIs, and the chats the tests' people are in.
pub struct Seeded {
    pub accounts: Vec<String>,
    /// Chats each of the tests' people is in, by [`super::support::SRV`] index.
    pub chats: [Vec<String>; 3],
    pub team: String,
}

async fn password_hash(db: &Db, upn: &str) -> String {
    use diesel::ExpressionMethods as _;
    let query = users::table.filter(users::upn.eq(upn.to_owned())).select(users::password_hash);
    let hash: Result<String, ngteams_server::db::DbError> =
        async { ngteams_server::with_conn!(db, |conn| Ok(query.first(conn).await?)) }.await;
    hash.unwrap()
}

/// Inserts the synthetic accounts, all with the tests' password; their MRIs, the odd names first.
async fn accounts(cluster: &Cluster) -> Vec<String> {
    let db = Db::connect(&cluster.database, 2).await.unwrap();
    let hash = password_hash(&db, super::support::SRV[0].0).await;
    let odd = odd_names();
    let mut mris = Vec::new();
    for n in 0..ACCOUNTS {
        let id = uuid::Uuid::new_v4().to_string();
        let row = Account {
            id: id.clone(),
            upn: format!("seed-{n:03}@own.test"),
            display_name: odd.get(n).cloned().unwrap_or_else(|| format!("Seeded Person {n:03}")),
            password_hash: hash.clone(),
            password_version: 0,
            // the last few are disabled: in the directory, never in a chat (a chat with one is refused)
            disabled: n >= ACCOUNTS - 3,
            created_at: 0,
            updated_at: 0,
            is_admin: false,
            must_change_password: false,
            deleted_at: None,
        };
        let query = diesel::insert_into(users::table).values(row);
        let inserted: Result<usize, ngteams_server::db::DbError> =
            async { ngteams_server::with_conn!(&db, |conn| Ok(query.execute(conn).await?)) }.await;
        inserted.unwrap();
        mris.push(format!("8:orgid:{id}"));
    }
    mris
}

/// A seeder's client: a synthetic account's, its requests reaching the team services too.
async fn seeder(cluster: &Cluster, n: usize, node: usize) -> User {
    let node = &cluster.nodes[node % cluster.nodes.len()];
    let mut user = User::signed_in(node, &cluster.origin, &format!("seed-{n:03}@own.test"), options()).await;
    user.teams = true;
    user.start();
    user.online(0).await;
    user
}

/// The tests' people's MRIs, read with a sign-in of each.
async fn people(cluster: &Cluster) -> Vec<Mri> {
    let mut mris = Vec::new();
    for (upn, _) in super::support::SRV {
        let session = cluster.nodes[0].sign_in(upn, crate::support::PASSWORD).await;
        let skype = session.skypetoken().await.unwrap();
        mris.push(Mri::new(format!("8:{}", skype.skype_id.as_deref().unwrap())));
    }
    mris
}

async fn post_many(user: &User, chat: &str, texts: &[String], from: usize) -> Vec<String> {
    let mut ids = Vec::new();
    for step in 0..MESSAGES {
        let text = &texts[(from + step) % texts.len()];
        ids.push(user.send(chat, text).await);
    }
    ids
}

/// Seeds the server through its own API, as people using it would have: see the module docs.
pub async fn plant(cluster: &Cluster) -> Seeded {
    let started = std::time::Instant::now();
    let accounts = accounts(cluster).await;
    let srv = people(cluster).await;
    let cloud = Cloud::own(&cluster.origin).unwrap();
    let first = seeder(cluster, 0, 0).await;
    let second = seeder(cluster, 1, 1).await;
    let mut chats: [Vec<String>; 3] = Default::default();
    for (index, chat) in one_to_ones(&first, &srv).await.into_iter().enumerate() {
        chats[index].push(chat);
    }
    for chat in groups(&first, &second, &srv, &accounts).await {
        for list in &mut chats {
            list.push(chat.clone());
        }
    }
    files(&first, &cloud, &chats[0][3]).await;
    let team = seed_team(&first, &srv, &accounts).await;
    events(&first, &srv).await;
    eprintln!(
        "seeded {ACCOUNTS} accounts, {} chats, a team and events in {:?}",
        chats[0].len(),
        started.elapsed()
    );
    drop((first, second));
    Seeded { accounts, chats, team }
}

/// A 1:1 chat of `first` with each of `srv`, with a history: an edit and a deletion among it.
async fn one_to_ones(first: &User, srv: &[Mri]) -> Vec<String> {
    let texts = odd_texts();
    let mut chats = Vec::new();
    for (index, mri) in srv.iter().enumerate() {
        let chat = first
            .write(Action::CreateChat {
                others: vec![mri.clone()],
                topic: None,
            })
            .await
            .message_id
            .unwrap();
        let ids = post_many(first, &chat, &texts, index).await;
        first.holds(&chat, &ids[0]).await;
        first
            .write(Action::Edit {
                conversation: chat.clone(),
                message_id: ids[0].clone(),
                text: MessageText::plain("edited before anyone read it ✏️").unwrap(),
            })
            .await;
        first
            .write(Action::Delete {
                conversation: chat.clone(),
                message_id: ids[1].clone(),
            })
            .await;
        chats.push(chat);
    }
    chats
}

/// Group chats of `srv` and many synthetic people, odd topics, a reaction in each.
async fn groups(first: &User, second: &User, srv: &[Mri], accounts: &[String]) -> Vec<String> {
    let texts = odd_texts();
    let topics = [
        Some("Café Ünïcode ☕".to_owned()),
        Some("ﻣﺠﻤﻮﻋﺔ عربية".to_owned()),
        None,
        Some("<i>markup</i> & \"quotes\"".to_owned()),
        Some("T".repeat(200)),
        Some("🚀🚀🚀".to_owned()),
        Some("Robert'); DROP TABLE chats;--".to_owned()),
        Some("plain group".to_owned()),
    ];
    let mut chats = Vec::new();
    for (n, topic) in topics.iter().enumerate().take(GROUPS) {
        let maker = if n % 2 == 0 { first } else { second };
        let mut others: Vec<Mri> = srv.to_vec();
        others.extend(accounts.iter().skip(2 + n * 20).take(20).map(Mri::new));
        let chat = maker
            .write(Action::CreateChat {
                others,
                topic: topic.clone(),
            })
            .await
            .message_id
            .unwrap();
        let ids = post_many(maker, &chat, &texts, n).await;
        maker.holds(&chat, &ids[2]).await;
        maker
            .write(Action::React {
                conversation: chat.clone(),
                message_id: ids[2].clone(),
                key: "heart".into(),
            })
            .await;
        chats.push(chat);
    }
    chats
}

/// A picture and files with odd names in `chat`.
async fn files(first: &User, cloud: &Cloud, chat: &str) {
    let picture = image_png(64, 48);
    paste(first, cloud, chat, &picture, (64, 48)).await;
    for (name, bytes) in [
        ("résumé (final) v2.pdf", vec![0x25; 4096]),
        ("长文件名 with spaces.bin", (0..=255u8).cycle().take(70_000).collect()),
        ("one byte.txt", vec![b'x']),
    ] {
        send_file(first, cloud, chat, name, &bytes).await;
    }
}

/// Calendar events of `first` with `srv` invited, odd ones among them.
async fn events(first: &User, srv: &[Mri]) {
    let monday = coming_monday();
    for (n, (subject, start, minutes, all_day)) in [
        ("Planning 🗓️", monday + 9 * HOUR, 30, false),
        ("All day: offsite", monday + DAY, 24 * 60, true),
        ("Late night <b>sync</b>", monday + 2 * DAY + 23 * HOUR + 30 * 60_000, 90, false),
        ("ابن سينا review", monday + 3 * DAY + 13 * HOUR, 45, false),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = draft(subject, start, minutes);
        event.all_day = all_day;
        event.online = n % 2 == 0;
        event.attendees = srv
            .iter()
            .zip(super::support::SRV)
            .map(|(mri, (upn, name))| invited(upn, name, Some(mri.as_str()), true))
            .collect();
        let written = first
            .engine()
            .write_event(teams_core::EventWrite::Create(event))
            .await
            .unwrap();
        assert!(written.created.is_some(), "{subject}");
    }
}

async fn seed_team(owner: &User, srv: &[Mri], accounts: &[String]) -> String {
    let create = TeamWrite::Create(NewTeam {
        name: "Seeded team 🚀 Ünïcode".into(),
        description: "<b>synthetic</b> people & odd names".into(),
        first_channel: "General".into(),
        public: false,
    });
    let written = owner.engine().team_write(create, None).await.unwrap();
    let (team, group) = written.created.unwrap();
    let group = group.unwrap();
    let mut members: Vec<(Mri, TeamRole)> = srv.iter().map(|mri| (mri.clone(), TeamRole::Member)).collect();
    members.extend(accounts.iter().skip(2).take(30).map(|mri| (Mri::new(mri), TeamRole::Member)));
    let add = TeamWrite::AddMembers {
        team: team.clone(),
        group: group.clone(),
        members,
    };
    let written = owner.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
    for name in ["Ünïcode channel", "release-notes"] {
        let draft = teams_core::ChannelDraft {
            channel: crate::teams::new_channel(&team, &group, name, teams_proto::NewChannelKind::Standard),
            show_by_default: true,
        };
        let made = owner.engine().create_channel(draft).await.unwrap();
        let channel = made.thread.unwrap();
        for text in ["first post 📌", "second post with <markup>"] {
            owner.send(&channel, text).await;
        }
    }
    team
}

/// A PNG of `width`×`height`.
fn image_png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([u8::try_from(x % 256).unwrap(), u8::try_from(y % 256).unwrap(), 128])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
