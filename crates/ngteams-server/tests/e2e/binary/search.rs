//! The title bar's search as users of ngteams clients meet it on the real server: real server processes (one node on
//! `SQLite`, two nodes on one Postgres and one Redis), synthetic users the operator added with `user add` and
//! `profile set`, and each client's session, engine, store and push channel. Ada searches at the first node; Bert,
//! Dan and Zoë write at the other node when there are two, so what one node took is found through the other.
//!
//! The office: Ada, Bert, Cora, Dan, Zoë (accents), Seán (quotes in the name), a person with a very long name and
//! twenty fillers; group chats "Picnic crew 🧺" (Ada, Bert, Cora) and "Café Ünïcode" (Ada, Bert, Zoë), Dan's den
//! (Dan, Bert, Cora: never Ada's), Ada's 1:1 chat with Bert; Ada's team "Rocket Team" with Bert and its channel
//! "Launch Plans", and Dan's team "Secret Lab" with its channel "Hidden Plans" (Ada isn't on it). All synthetic.

use std::time::{Duration, Instant};

use teams_core::engine::Action;
use teams_core::{SearchAnswer, SearchScope};
use teams_proto::chat::MessageText;
use teams_proto::{Cloud, NewTeam, SearchKind, TeamRole, TeamWrite};

use super::push::support::{Rig, User};
use crate::chatsvc::members::RemoveMember;
use crate::support::{Backend, PASSWORD, backends};

/// How long a message just sent may take to be found.
const FOUND_WITHIN: Duration = Duration::from_secs(3);

const ADA: &str = "ada.search@own.test";
const BERT: &str = "bert.search@own.test";
const CORA: &str = "cora.search@own.test";
const DAN: &str = "dan.search@own.test";
const ZOE: &str = "zoe.search@own.test";
const SEAN: &str = "sean.search@own.test";
const LONG: &str = "maximiliana.search@own.test";

const ZOE_NAME: &str = "Zoë Ångström-Øvergaard";
const SEAN_NAME: &str = "Seán \"Quotes\" O'Brien";
const LONG_NAME: &str = "Maximiliana Theodora Wilhelmina von Hohenzollern-Sigmaringen-Ostrowski-Papadopoulou the Third";
const FILLERS: usize = 20;

/// The operator's `user add` and `profile set` of every person of the office.
fn seed_people(rig: &Rig) {
    let people: [(&str, &str, &[&str]); 7] = [
        (ADA, "Ada Lovelace", &["--job-title", "Analyst"]),
        (
            BERT,
            "Bert Sample",
            &["--job-title", "Engineer", "--department", "Platform"],
        ),
        (CORA, "Cora Chief", &["--job-title", "Chief Architect"]),
        (DAN, "Dan Fourth", &[]),
        (
            ZOE,
            ZOE_NAME,
            &["--job-title", "Ingénieure", "--department", "Recherche & Développement"],
        ),
        (SEAN, SEAN_NAME, &["--department", "Quality \"QA\""]),
        (
            LONG,
            LONG_NAME,
            &["--job-title", "Principal Engineer of Overflowing Things"],
        ),
    ];
    for (upn, name, profile) in people {
        add(rig, upn, name);
        if !profile.is_empty() {
            let mut args = vec!["profile", "set", upn];
            args.extend_from_slice(profile);
            rig.first.cli(&args, "");
        }
    }
    for n in 0..FILLERS {
        add(
            rig,
            &format!("filler{n:02}.search@own.test"),
            &format!("Filler Person {n:02}"),
        );
    }
}

fn add(rig: &Rig, upn: &str, name: &str) {
    let added = rig
        .first
        .cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
    assert!(added.starts_with(&format!("added {upn} (")), "{added}");
}

async fn search(user: &User, query: &str, scope: SearchScope) -> SearchAnswer {
    user.engine().search_service(query, scope).await.unwrap()
}

async fn messages(user: &User, query: &str) -> Vec<String> {
    search(user, query, SearchScope::Only(SearchKind::Messages))
        .await
        .messages
        .into_iter()
        .map(|found| found.id)
        .collect()
}

/// A team made through `owner`'s engine, `people` on it, and a standard channel of it: `(team, channel)`.
async fn team(owner: &User, name: &str, people: &[&User], channel: &str) -> (String, String) {
    let create = TeamWrite::Create(NewTeam {
        name: name.into(),
        description: format!("{name}'s people"),
        first_channel: "General".into(),
        public: false,
    });
    let written = owner.engine().team_write(create, None).await.unwrap();
    let (team, group) = written.created.unwrap();
    let group = group.unwrap();
    if !people.is_empty() {
        let add = TeamWrite::AddMembers {
            team: team.clone(),
            group: group.clone(),
            members: people.iter().map(|user| (user.mri.clone(), TeamRole::Member)).collect(),
        };
        let written = owner.engine().team_write(add, None).await.unwrap();
        assert!(written.not_added.is_empty(), "{written:?}");
    }
    let draft = teams_core::ChannelDraft {
        channel: crate::teams::new_channel(&team, &group, channel, teams_proto::NewChannelKind::Standard),
        show_by_default: true,
    };
    let made = owner.engine().create_channel(draft).await.unwrap();
    (team, made.thread.unwrap())
}

/// The office of the module docs, signed in and seeded.
struct Office {
    rig: Rig,
    ada: User,
    bert: User,
    cora: User,
    dan: User,
    zoe: User,
    picnic: String,
    cafe: String,
    den: String,
    one_to_one: String,
    rocket: String,
    launch: String,
}

impl Office {
    async fn open(backend: Backend) -> Self {
        let rig = Rig::start(backend).await;
        seed_people(&rig);
        let other = rig.other();
        let ada = rig.team_user(&rig.first, ADA).await;
        let bert = rig.team_user(other, BERT).await;
        let cora = rig.team_user(&rig.first, CORA).await;
        let dan = rig.team_user(other, DAN).await;
        let zoe = rig.team_user(other, ZOE).await;
        let picnic = ada.create(&[&bert, &cora], Some("Picnic crew 🧺")).await;
        let cafe = ada.create(&[&bert, &zoe], Some("Café Ünïcode")).await;
        let den = dan.create(&[&bert, &cora], Some("Dan's den")).await;
        let one_to_one = ada.create(&[&bert], None).await;
        let (rocket, launch) = team(&ada, "Rocket Team", &[&bert], "Launch Plans").await;
        team(&dan, "Secret Lab", &[&bert], "Hidden Plans").await;
        Self {
            rig,
            ada,
            bert,
            cora,
            dan,
            zoe,
            picnic,
            cafe,
            den,
            one_to_one,
            rocket,
            launch,
        }
    }

    async fn stop(self) {
        let Self {
            rig,
            ada,
            bert,
            cora,
            dan,
            zoe,
            ..
        } = self;
        drop((ada, bert, cora, dan, zoe));
        rig.stop().await;
    }
}

/// Messages, people, chats, teams and channels as Ada finds them: marked words, only what she may read, case and
/// odd text, a new message soon, and what one node took found through the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_seeded_office_is_searched_as_its_people_search_it() {
    for backend in backends() {
        let office = Office::open(backend).await;
        found_messages(&office).await;
        found_soon(&office, backend).await;
        found_people(&office).await;
        found_chats_and_channels(&office).await;
        found_while_typed(&office).await;
        office.stop().await;
    }
}

/// Messages: what Ada may read, the newest first, its words marked, from both nodes; odd text; a page of many.
async fn found_messages(office: &Office) {
    let (ada, bert, zoe) = (&office.ada, &office.bert, &office.zoe);
    let review = bert
        .send(&office.picnic, "The quarterly <rocket> review is on Friday")
        .await;
    let dessert = zoe.send(&office.cafe, "Crème brûlée for dessert at the café").await;
    let quoted = bert.send(&office.cafe, "He said \"launch\" twice & 'abort' once").await;
    let countdown = ada
        .send(&office.launch, "Countdown starts at noon 🚀 rockets ready")
        .await;
    office.dan.send(&office.den, "the rocket secret stays here").await;
    let all = search(ada, "rocket", SearchScope::All).await;
    let found: Vec<(&str, &str, Option<&str>, Option<&str>)> = all
        .messages
        .iter()
        .map(|found| {
            let marked = found.matched.first().map(|range| &found.snippet[range.clone()]);
            (
                found.id.as_str(),
                found.conversation.as_str(),
                found.parent.as_deref(),
                marked,
            )
        })
        .collect();
    let wanted = [
        (
            countdown.as_str(),
            office.launch.as_str(),
            Some(countdown.as_str()),
            found.first().and_then(|first| first.3),
        ),
        (review.as_str(), office.picnic.as_str(), None, Some("rocket")),
    ];
    assert_eq!(found, wanted, "{:?}", all.messages);
    assert_eq!(all.messages[1].snippet, "The quarterly <rocket> review is on Friday");
    assert_eq!(all.messages[1].sender_name.as_deref(), Some("Bert Sample"));
    found_by_reader_and_text(office, &quoted, &dessert).await;
    found_in_a_page(office).await;
}

/// Case, who may read it, quotes, an ampersand, accents as written, an emoji alone.
async fn found_by_reader_and_text(office: &Office, quoted: &String, dessert: &String) {
    let (ada, bert) = (&office.ada, &office.bert);
    assert_eq!(messages(ada, "ROCKET").await, messages(ada, "rocket").await, "case");
    assert!(messages(ada, "secret").await.is_empty(), "Dan's den isn't Ada's");
    assert_eq!(messages(bert, "secret").await.len(), 1, "Bert is in Dan's den");
    for (query, wanted) in [
        ("\"launch\"", quoted),
        ("abort", quoted),
        ("crème brûlée", dessert),
        ("CAFÉ", dessert),
    ] {
        assert_eq!(messages(ada, query).await, std::slice::from_ref(wanted), "{query}");
    }
    assert!(messages(ada, "🚀").await.is_empty(), "an emoji alone is no word");
}

/// A tab of many: a page, the newest first, more left.
async fn found_in_a_page(office: &Office) {
    let mut reports = Vec::new();
    for n in 0..30 {
        let text = format!("status report number {n}");
        reports.push(office.bert.send(&office.one_to_one, &text).await);
    }
    let tab = search(&office.ada, "status report", SearchScope::Only(SearchKind::Messages)).await;
    assert_eq!(tab.messages.len(), teams_proto::PAGE as usize);
    assert!(tab.more);
    assert_eq!(Some(&tab.messages[0].id), reports.last());
    assert_eq!(
        messages(&office.ada, "number 17").await,
        std::slice::from_ref(&reports[17])
    );
}

/// A message just sent at the other node is found at the first within moments.
async fn found_soon(office: &Office, backend: Backend) {
    let sent = Instant::now();
    let fresh = office
        .bert
        .send(&office.picnic, "zeppelin sighted over the harbour")
        .await;
    while messages(&office.ada, "zeppelin").await != std::slice::from_ref(&fresh) {
        assert!(
            sent.elapsed() < FOUND_WITHIN,
            "not found {FOUND_WITHIN:?} after it was sent"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("search: {backend:?} found a new message after {:?}", sent.elapsed());
}

/// The one person `user` finds for `query`.
async fn one_person(user: &User, query: &str) -> teams_proto::FoundPerson {
    let found = search(user, query, SearchScope::Only(SearchKind::People)).await;
    assert_eq!(found.people.len(), 1, "{query}: {:?}", found.people);
    found.people.into_iter().next().unwrap()
}

/// People: by a name's start, by address, odd names; title and department for the card.
async fn found_people(office: &Office) {
    let ada = &office.ada;
    let bert = one_person(ada, "bert").await;
    let card = (
        bert.mri.as_deref(),
        bert.email.as_deref(),
        bert.job_title.as_deref(),
        bert.department.as_deref(),
    );
    let wanted = (
        Some(office.bert.mri.as_str()),
        Some(BERT),
        Some("Engineer"),
        Some("Platform"),
    );
    assert_eq!(card, wanted);
    let zoe = one_person(ada, "zoë").await;
    assert_eq!(zoe.department.as_deref(), Some("Recherche & Développement"));
    for (query, name) in [
        ("zoë", ZOE_NAME),
        ("ÅNGSTRÖM", ZOE_NAME),
        ("o'brien", SEAN_NAME),
        ("papadopoulou", LONG_NAME),
        ("bert@own", "Bert Sample"),
    ] {
        assert_eq!(one_person(ada, query).await.name, name, "{query}");
    }
    let fillers = search(ada, "filler person", SearchScope::Only(SearchKind::People)).await;
    assert_eq!((fillers.people.len(), fillers.more), (FILLERS, false));
    let nothing = search(ada, "nobody-here-zzqx", SearchScope::All).await;
    assert!(nothing == SearchAnswer::default(), "{nothing:?}");
}

/// The ids of the group chats `user` finds for `query`.
async fn chat_ids(user: &User, query: &str) -> Vec<String> {
    let found = search(user, query, SearchScope::Only(SearchKind::Chats)).await;
    found.chats.into_iter().map(|chat| chat.id).collect()
}

/// `(id, name, team)` of the teams and channels `user` finds for `query`.
async fn channels(user: &User, query: &str) -> Vec<(String, String, Option<String>)> {
    let found = search(user, query, SearchScope::Only(SearchKind::Channels)).await;
    found
        .channels
        .into_iter()
        .map(|channel| (channel.id, channel.name, channel.team))
        .collect()
}

/// Group chats by topic (emoji, accents) or member, never another's chat or a 1:1; Ada's teams and channels, never
/// Dan's team that she isn't on.
async fn found_chats_and_channels(office: &Office) {
    let (ada, bert) = (&office.ada, &office.bert);
    let picnic = search(ada, "picnic", SearchScope::Only(SearchKind::Chats)).await;
    let names: Vec<&str> = picnic.chats.iter().map(|chat| chat.name.as_str()).collect();
    assert_eq!(names, ["Picnic crew 🧺"]);
    for (user, query, wanted) in [
        (ada, "picnic", Some(&office.picnic)),
        (ada, "café", Some(&office.cafe)),
        (ada, "zoë", Some(&office.cafe)),
        (ada, "den", None),
        (bert, "den", Some(&office.den)),
    ] {
        let wanted: Vec<String> = wanted.into_iter().cloned().collect();
        assert_eq!(chat_ids(user, query).await, wanted, "{}: {query}", user.upn);
    }
    let launch = (
        office.launch.clone(),
        "Launch Plans".to_owned(),
        Some("Rocket Team".to_owned()),
    );
    assert_eq!(channels(ada, "plans").await, [launch]);
    let rocket = channels(ada, "rocket").await;
    assert_eq!(
        rocket.iter().map(|found| &found.0).collect::<Vec<_>>(),
        [&office.rocket]
    );
    assert!(channels(ada, "secret").await.is_empty(), "Dan's team isn't Ada's");
    assert_eq!(channels(bert, "plans").await.len(), 2, "Bert is on both teams");
}

/// The suggestions as the words are typed, one letter at a time.
async fn found_while_typed(office: &Office) {
    let (ada, bert) = (&office.ada, &office.bert);
    for typed in ["b", "be", "ber", "bert"] {
        let suggested = search(ada, typed, SearchScope::Suggestions).await;
        assert!(
            suggested
                .people
                .iter()
                .any(|found| found.mri.as_deref() == Some(bert.mri.as_str())),
            "{typed}: {suggested:?}"
        );
    }
    let suggested = search(ada, "picn", SearchScope::Suggestions).await;
    assert!(
        suggested.chats.iter().any(|found| found.id == office.picnic),
        "{suggested:?}"
    );
    let suggested = search(ada, "launch pl", SearchScope::Suggestions).await;
    assert!(
        suggested.channels.iter().any(|found| found.id == office.launch),
        "{suggested:?}"
    );
}

/// A deleted message, an edit, and a member who left: what each finds afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_edited_and_left_behind_messages_are_found_as_the_reader_may_read_them() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let (ada, bert, cora) = (&office.ada, &office.bert, &office.cora);
        let gone = ada.send(&office.picnic, "the password is swordfish").await;
        let lighthouse = ada.send(&office.picnic, "Meet at the lighthouse").await;
        assert_eq!(messages(cora, "swordfish").await, std::slice::from_ref(&gone));
        ada.holds(&office.picnic, &gone).await;
        ada.holds(&office.picnic, &lighthouse).await;
        ada.write(Action::Delete {
            conversation: office.picnic.clone(),
            message_id: gone.clone(),
        })
        .await;
        ada.write(Action::Edit {
            conversation: office.picnic.clone(),
            message_id: lighthouse.clone(),
            text: MessageText::plain("Meet at the harbour").unwrap(),
        })
        .await;
        for reader in [ada, bert, cora] {
            assert!(
                messages(reader, "swordfish").await.is_empty(),
                "{}: a deleted message",
                reader.upn
            );
            assert!(
                messages(reader, "lighthouse").await.is_empty(),
                "{}: the old text",
                reader.upn
            );
            assert_eq!(
                messages(reader, "harbour").await,
                std::slice::from_ref(&lighthouse),
                "{}",
                reader.upn
            );
        }

        // Cora leaves the chat: what was said before stays hers to find, nothing after, and not the chat
        let left = cora
            .client()
            .write(RemoveMember {
                chat: office.picnic.clone(),
                member: cora.mri.as_str().to_owned(),
            })
            .await
            .unwrap();
        assert!(left.is_success(), "{}", left.status());
        let after = bert.send(&office.picnic, "the kite is in the harbour").await;
        assert_eq!(messages(cora, "harbour").await, std::slice::from_ref(&lighthouse));
        assert!(messages(cora, "kite").await.is_empty());
        let chats = search(cora, "picnic", SearchScope::Only(SearchKind::Chats)).await;
        assert!(chats.chats.is_empty(), "{:?}", chats.chats);
        let suggested = search(cora, "picnic", SearchScope::Suggestions).await;
        assert!(suggested.chats.is_empty(), "{:?}", suggested.chats);
        assert_eq!(messages(ada, "harbour").await, [after.clone(), lighthouse.clone()]);
        office.stop().await;
    }
}

/// A file sent to a chat is found by its name by its sender and by the chat's other people, never by someone else.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_sent_to_a_chat_is_found_by_its_people_alone() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let cloud = Cloud::own(&office.rig.origin).unwrap();
        super::media::support::send_file(
            &office.zoe,
            &cloud,
            &office.cafe,
            "Budget Ünïcode forecast.xlsx",
            b"synthetic sheet",
        )
        .await;
        for reader in [&office.zoe, &office.ada, &office.bert] {
            let found = search(reader, "forecast", SearchScope::Only(SearchKind::Files)).await;
            assert_eq!(found.files.len(), 1, "{}: {:?}", reader.upn, found.files);
            assert_eq!(found.files[0].name, "Budget Ünïcode forecast.xlsx");
            assert_eq!(found.files[0].kind.as_deref(), Some("xlsx"));
            let all = search(reader, "budget", SearchScope::All).await;
            assert_eq!(all.files.len(), 1, "{}: {:?}", reader.upn, all.files);
        }
        for outsider in [&office.cora, &office.dan] {
            let found = search(outsider, "forecast", SearchScope::All).await;
            assert!(found.files.is_empty(), "{}: {:?}", outsider.upn, found.files);
        }
        office.stop().await;
    }
}

/// What a person without accents on the keyboard types finds what was written with them, as Teams finds it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "QA 188 Q1: names never match without accents; messages do on SQLite only"]
async fn words_typed_without_accents_find_what_was_written_with_them() {
    let mut all_misses = Vec::new();
    for backend in backends() {
        let office = Office::open(backend).await;
        let dessert = office
            .zoe
            .send(&office.cafe, "Crème brûlée for dessert at the café")
            .await;
        let mut misses = Vec::new();
        if messages(&office.ada, "creme brulee").await != std::slice::from_ref(&dessert) {
            misses.push("message 'creme brulee'");
        }
        if search(&office.ada, "angstrom", SearchScope::Only(SearchKind::People))
            .await
            .people
            .len()
            != 1
        {
            misses.push("person 'angstrom'");
        }
        if search(&office.ada, "cafe", SearchScope::Only(SearchKind::Chats))
            .await
            .chats
            .len()
            != 1
        {
            misses.push("chat 'cafe'");
        }
        office.stop().await;
        if !misses.is_empty() {
            all_misses.push(format!("{backend:?}: {misses:?}"));
        }
    }
    assert!(all_misses.is_empty(), "not found: {all_misses:?}");
}
