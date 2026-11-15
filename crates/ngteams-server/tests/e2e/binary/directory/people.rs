//! People in a large synthetic directory, read as ngteams reads them from the real binary: short profiles and Graph
//! users at the lists' caps, the person card (title, department, office, phone, the reporting line, direct reports,
//! out of office), the people search, pictures at each size, and what a disabled and a deleted account show.

use std::collections::BTreeSet;

use diesel::{ExpressionMethods as _, QueryDsl as _};
use diesel_async::RunQueryDsl as _;
use ngteams_server::db::schema::users;
use ngteams_server::db::users::User;
use ngteams_server::db::{Db, DbError};
use ngteams_server::people::{self, Profile};
use serde_json::{Value, json};
use teams_core::Availability;
use teams_proto::{
    GRAPH_AUDIENCE, GraphPicture, GraphUser, LokiOrganization, LokiOutOfOffice, MIDDLE_TIER_AUDIENCE, Mri,
    OutOfOfficeState, PRESENCE_AUDIENCE, ShortProfileLookup,
};

use super::super::media::support::Stage;
use super::super::support::Node;
use super::support::{Seat, bearer};
use crate::support::backends;

/// Synthetic accounts seeded beside a, b, c and d.
const CROWD: usize = 520;
/// The first accounts of the reporting line: `LINE_START + k` reports to `LINE_START + k + 1`, the last to nobody.
const LINE_START: usize = 20;
const LINE: usize = 24;
/// The head of a wide team: every account in `WIDE` reports to them.
const HEAD: usize = 60;
const WIDE: std::ops::Range<usize> = 61..241;
/// Accounts the operator disables and deletes.
const DISABLED: [usize; 3] = [300, 301, LINE_START + 5];
const DELETED: [usize; 3] = [310, 311, HEAD + 3];

fn odd_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "Élodie Ñúñez",
        "李小龍",
        "שרה כהן",
        "🦀 Ferris Crab",
        "Zoe\u{0308} Combined",
        "O'Brien \"Bob\" <b>bold</b>",
        "back\\slash & ampersand",
        "inner  double  spaces",
        "Ünïcödé ÅÄÖ",
        "null",
        "%_ wildcard_name%",
        "Robert'); DROP TABLE users;--",
        "\u{202e}reversed",
        "Ωmega Σigma",
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    names.push("L".repeat(256));
    names
}

async fn password_hash(db: &Db, upn: &str) -> Result<String, DbError> {
    let query = users::table
        .filter(users::upn.eq(upn.to_owned()))
        .select(users::password_hash);
    ngteams_server::with_conn!(db, |conn| Ok(query.first(conn).await?))
}

async fn insert(db: &Db, rows: &[User]) -> Result<(), DbError> {
    for row in rows {
        let query = diesel::insert_into(users::table).values(row.clone());
        ngteams_server::with_conn!(db, |conn| query.execute(conn).await?);
    }
    Ok(())
}

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// What the profile of the crowd's account `n` holds: some fields for some, none for others.
fn profile_of(n: usize, ids: &[String]) -> Option<Profile> {
    let manager = if (LINE_START..LINE_START + LINE - 1).contains(&n) {
        Some(ids[n + 1].clone())
    } else if WIDE.contains(&n) {
        Some(ids[HEAD].clone())
    } else {
        None
    };
    let some = |every: usize, value: String| n.is_multiple_of(every).then_some(value);
    let profile = Profile {
        user_id: ids[n].clone(),
        given_name: some(2, format!("Given{n}")),
        surname: some(3, format!("Sur{n}")),
        job_title: some(2, format!("Title {n}")),
        department: some(4, format!("Dept {}", n % 7)),
        office_location: some(5, format!("Room {n}")),
        company_name: some(6, "Synthetic Ltd".into()),
        mobile_phone: some(7, format!("+1 555 01{n:03}")),
        business_phone: some(8, format!("+1 555 02{n:03}")),
        manager_id: manager,
        out_of_office: n.is_multiple_of(11),
        ..Profile::default()
    };
    (n % 13 != 1 || profile.manager_id.is_some()).then_some(profile)
}

/// The real server with a, b, c and d, and the crowd, its profiles in the directory.
struct Directory {
    stage: Stage,
    ids: Vec<String>,
    names: Vec<String>,
    profiles: Vec<Option<Profile>>,
}

impl Directory {
    async fn seed(backend: crate::support::Backend) -> Self {
        let stage = Stage::start(backend, "").await;
        let db = Db::connect(&stage.rig.database, 2).await.unwrap();
        let hash = password_hash(&db, "a@own.test").await.unwrap();
        let odd = odd_names();
        let names: Vec<String> = (0..CROWD)
            .map(|n| odd.get(n).cloned().unwrap_or_else(|| format!("Crowd {n:03}")))
            .collect();
        let ids: Vec<String> = (0..CROWD).map(|_| uuid::Uuid::new_v4().to_string()).collect();
        let rows: Vec<User> = ids
            .iter()
            .zip(&names)
            .enumerate()
            .map(|(n, (id, name))| User {
                id: id.clone(),
                upn: format!("crowd-{n:03}@own.test"),
                display_name: name.clone(),
                password_hash: hash.clone(),
                password_version: 0,
                disabled: false,
                created_at: 0,
                updated_at: 0,
                is_admin: false,
                must_change_password: false,
                deleted_at: None,
            })
            .collect();
        insert(&db, &rows).await.unwrap();
        let profiles: Vec<Option<Profile>> = (0..CROWD).map(|n| profile_of(n, &ids)).collect();
        for profile in profiles.iter().flatten() {
            people::repo::put_profile(&db, profile).await.unwrap();
        }
        let directory = Self {
            stage,
            ids,
            names,
            profiles,
        };
        // pictures, set as the operator sets them, also on the accounts that are disabled and deleted next
        let png = crate::media::png(160, 120);
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), &png).unwrap();
        for n in [0, HEAD, LINE_START, DISABLED[0], DELETED[0]] {
            let upn = format!("crowd-{n:03}@own.test");
            let set = directory
                .node()
                .cli(&["profile", "picture", &upn, file.path().to_str().unwrap()], "");
            assert!(set.starts_with(&format!("picture set for {upn} (")), "{set}");
        }
        for n in DISABLED {
            directory
                .node()
                .cli(&["user", "disable", &format!("crowd-{n:03}@own.test")], "");
        }
        for n in DELETED {
            directory
                .node()
                .cli(&["user", "delete", &format!("crowd-{n:03}@own.test")], "");
        }
        directory
    }

    fn node(&self) -> &Node {
        &self.stage.rig.first
    }

    fn mri(&self, n: usize) -> String {
        mri(&self.ids[n])
    }

    fn gone(n: usize) -> bool {
        DELETED.contains(&n)
    }

    fn can_sign_in(n: usize) -> bool {
        !DELETED.contains(&n) && !DISABLED.contains(&n)
    }
}

async fn get(node: &Node, token: &str, path: &str) -> (u16, Value) {
    let response = node
        .http
        .get(format!("{}{path}", node.endpoint))
        .bearer_auth(token)
        .header("X-ClientType", "Teams")
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn post(node: &Node, token: &str, path: &str, body: &Value) -> (u16, Value) {
    let response = node
        .http
        .post(format!("{}{path}", node.endpoint))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

/// The account that reads the directory: its client, and its typed client.
struct Reader {
    a: Seat,
    client: teams_proto::Client<super::support::Transport>,
}

/// Short profiles at the cap, in the order asked.
async fn short_profiles(dir: &Directory, reader: &Reader) {
    let (rig, client) = (&dir.stage.rig, &reader.client);

    // short profiles: the most one lookup takes, in the order asked, the deleted left out, missing fields absent
    let asked: Vec<usize> = (0..500).rev().collect();
    let lookup = ShortProfileLookup::new(asked.iter().map(|n| Mri::new(dir.mri(*n)))).unwrap();
    let answer = client.post_read(lookup.clone()).await.unwrap();
    let profiles = lookup.read_answer(answer.body()).unwrap();
    let expected: Vec<usize> = asked.iter().copied().filter(|n| !Directory::gone(*n)).collect();
    assert_eq!(profiles.len(), expected.len());
    for (profile, n) in profiles.iter().zip(&expected) {
        assert_eq!(profile.mri.as_str(), dir.mri(*n));
        assert_eq!(profile.display_name.as_deref(), Some(dir.names[*n].as_str()), "{n}");
        let kept = dir.profiles[*n].clone().unwrap_or_default();
        assert_eq!(profile.job_title, kept.job_title, "{n}");
        assert_eq!(profile.office_location, kept.office_location, "{n}");
        assert_eq!(profile.given_name, kept.given_name, "{n}");
    }
    let token = bearer(rig.other(), "a@own.test", MIDDLE_TIER_AUDIENCE).await;
    let short = "/api/mt/part/own-01/beta/users/fetchShortProfile";
    let all: Vec<String> = (0..CROWD).map(|n| dir.mri(n)).collect();
    let (status, raw) = post(rig.other(), &token, short, &json!(all[..500])).await;
    assert_eq!(status, 200);
    let disabled = raw["value"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["mri"] == dir.mri(DISABLED[0]))
        .cloned()
        .unwrap();
    assert_eq!(disabled["accountEnabled"], false, "{disabled}");
    assert_eq!(post(rig.other(), &token, short, &json!(all[..501])).await.0, 400);
}

/// Graph's record of every account.
async fn graph_names(dir: &Directory, reader: &Reader) {
    let client = &reader.client;

    // Graph names every account; a deleted one is not found
    for n in 0..CROWD {
        let read = GraphUser::of(&dir.mri(n)).unwrap();
        match client.read(&read).await {
            Ok(answer) => {
                assert!(!Directory::gone(n), "{n} is deleted");
                assert_eq!(
                    read.read_answer(answer.body()).unwrap().as_deref(),
                    Some(dir.names[n].as_str())
                );
            }
            Err(error) => assert!(Directory::gone(n), "{n}: {error:?}"),
        }
    }
}

/// The person card's organization: a deep line and a wide team.
async fn reporting_lines(dir: &Directory, reader: &Reader) {
    let client = &reader.client;

    // the reporting line: 8 managers up from its foot, the line cut where a manager can't sign in
    let foot = LINE_START;
    let answer = client
        .read(&LokiOrganization::of(&dir.mri(foot)).unwrap())
        .await
        .unwrap();
    let line = LokiOrganization::read_answer(answer.body()).unwrap();
    let managers: Vec<&str> = line.managers.iter().map(|m| m.object_id.as_str()).collect();
    let expected: Vec<&str> = (foot + 1..foot + 5).map(|n| dir.ids[n].as_str()).collect();
    assert_eq!(
        managers,
        expected,
        "the line stops below the disabled {}",
        LINE_START + 5
    );
    let above = LINE_START + 6;
    let answer = client
        .read(&LokiOrganization::of(&dir.mri(above)).unwrap())
        .await
        .unwrap();
    let line = LokiOrganization::read_answer(answer.body()).unwrap();
    let managers: Vec<&str> = line.managers.iter().map(|m| m.object_id.as_str()).collect();
    let expected: Vec<&str> = (above + 1..above + 9).map(|n| dir.ids[n].as_str()).collect();
    assert_eq!(managers, expected, "the 8 nearest managers");
    assert_eq!(line.directs.len(), 0, "the one below is disabled");

    // the wide team: every direct report that can sign in, by name
    let answer = client
        .read(&LokiOrganization::of(&dir.mri(HEAD)).unwrap())
        .await
        .unwrap();
    let team = LokiOrganization::read_answer(answer.body()).unwrap();
    let directs: BTreeSet<&str> = team.directs.iter().map(|m| m.object_id.as_str()).collect();
    let expected: BTreeSet<&str> = WIDE
        .filter(|n| Directory::can_sign_in(*n))
        .map(|n| dir.ids[n].as_str())
        .collect();
    assert_eq!(directs.len(), expected.len());
    assert_eq!(directs, expected);
    let named: Vec<&str> = team.directs.iter().filter_map(|m| m.name.as_deref()).collect();
    let mut sorted = named.clone();
    sorted.sort_unstable();
    assert_eq!(named, sorted, "directs by name");
}

/// The card of someone with every field, and their Graph record.
async fn full_card(dir: &Directory, reader: &Reader) {
    let (rig, a) = (&dir.stage.rig, &reader.a);
    // the card of someone with every field, as the engine reads it, and their Graph record
    let full = (0..CROWD)
        .find(|n| *n % (2 * 3 * 4 * 5) == 0 && *n > 0 && Directory::can_sign_in(*n))
        .unwrap();
    let card = a.engine().profile_card(dir.mri(full)).await.unwrap();
    let kept = dir.profiles[full].clone().unwrap();
    assert_eq!(card.name.as_deref(), Some(dir.names[full].as_str()));
    assert_eq!(card.job_title, kept.job_title);
    assert_eq!(card.office, kept.office_location);
    assert_eq!(
        card.presence.as_ref().map(|p| p.availability.clone()),
        Some(Availability::Offline)
    );
    assert_eq!(card.out_of_office, Some(kept.out_of_office));
    let graph = bearer(rig.other(), "a@own.test", GRAPH_AUDIENCE).await;
    let (status, record) = get(rig.other(), &graph, &format!("/graph/v1.0/users/{}", dir.ids[full])).await;
    assert_eq!(status, 200);
    assert_eq!(record["jobTitle"], json!(kept.job_title));
    assert_eq!(record["officeLocation"], json!(kept.office_location));
    assert_eq!(record["mobilePhone"], json!(kept.mobile_phone));
    assert_eq!(
        record["businessPhones"],
        json!(kept.business_phone.into_iter().collect::<Vec<_>>())
    );
}

/// Out of office, as the card's Loki read and UPS say it.
async fn out_of_office(dir: &Directory, reader: &Reader) {
    let (client, a) = (&reader.client, &reader.a);
    let away = (1..CROWD)
        .find(|n| n.is_multiple_of(11) && Directory::can_sign_in(*n))
        .unwrap();
    let out = LokiOutOfOffice::of(&dir.mri(away)).unwrap();
    let state = LokiOutOfOffice::read_answer(client.read(&out).await.unwrap().body()).unwrap();
    assert_eq!(state, OutOfOfficeState::Other("AlwaysEnabled".into()));
    assert_eq!(a.read(&[&dir.mri(away)]).await[0].out_of_office, Some(true));
}

/// The people search the client does.
async fn people_search(dir: &Directory, reader: &Reader) {
    let a = &reader.a;
    // the people search the client does, over odd names, the disabled and the deleted
    for (query, wanted) in [
        ("élodie", Some(0)),
        ("李小龍", Some(1)),
        ("%_ wild", Some(10)),
        ("drop table", Some(11)),
    ] {
        let found = a.engine().addable_people(query).await.unwrap();
        let mris: Vec<&str> = found.iter().map(|person| person.mri.as_str()).collect();
        eprintln!("search {query:?}: {} found", mris.len());
        if let Some(n) = wanted {
            assert!(
                mris.contains(&dir.mri(n).as_str()),
                "{query:?} doesn't find {n}: {mris:?}"
            );
        }
    }
    let wildcard = a.engine().addable_people("%").await.unwrap();
    assert!(
        wildcard.iter().all(|person| person.name.contains('%')),
        "a % is no wildcard"
    );
    for n in DISABLED.iter().chain(&DELETED) {
        let upn = format!("crowd-{n:03}");
        let found = a.engine().addable_people(upn.as_str()).await.unwrap();
        assert!(found.is_empty(), "{upn} can't sign in and is still found");
    }
    let titled = a.engine().addable_people("crowd-002@").await.unwrap();
    eprintln!(
        "search crowd-002@: job title shown {:?}",
        titled.first().map(|p| p.job_title.clone())
    );
}

/// Presence of a disabled and a deleted account.
async fn gone_presence(dir: &Directory) {
    let rig = &dir.stage.rig;

    // presence of a disabled and a deleted account: none, as for nobody
    let presence = bearer(rig.other(), "a@own.test", PRESENCE_AUDIENCE).await;
    let asked: Vec<Value> = [DISABLED[0], DELETED[0], 2]
        .iter()
        .map(|n| json!({"mri": dir.mri(*n)}))
        .collect();
    let (status, read) = post(
        rig.other(),
        &presence,
        "/ups/own/v1/presence/getpresence/",
        &json!(asked),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(read[0]["status"], 40400, "{read}");
    assert_eq!(read[1]["status"], 40400, "{read}");
    assert_eq!(read[2]["status"], 20000, "{read}");
    assert_eq!(read[0].get("presence"), None);
}

/// Pictures at each size, and a disabled and a deleted account's.
async fn pictures(dir: &Directory, reader: &Reader) {
    let (rig, client) = (&dir.stage.rig, &reader.client);
    let graph = bearer(rig.other(), "a@own.test", GRAPH_AUDIENCE).await;
    // pictures: 96x96 is served as a JPEG; Teams' other sizes are not
    let pictured = GraphPicture::of(&dir.mri(HEAD)).unwrap();
    let picture = pictured.read_answer(&client.read(&pictured).await.unwrap()).unwrap();
    let decoded = image::load_from_memory(&picture.bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (96, 96));
    assert_eq!(image::guess_format(&picture.bytes).unwrap(), image::ImageFormat::Jpeg);
    for size in [
        "48x48", "64x64", "120x120", "240x240", "360x360", "432x432", "504x504", "648x648",
    ] {
        let path = format!("/graph/v1.0/users/{}/photos/{size}/$value", dir.ids[HEAD]);
        let (status, body) = get(rig.other(), &graph, &path).await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (404, &json!("ImageNotFound")),
            "{size}"
        );
    }
    let disabled_picture = GraphPicture::of(&dir.mri(DISABLED[0])).unwrap();
    let shown = client.read(&disabled_picture).await.map(|_| ());
    eprintln!("a disabled account's picture: {shown:?}");
    let deleted_picture = GraphPicture::of(&dir.mri(DELETED[0])).unwrap();
    assert!(
        client.read(&deleted_picture).await.is_err(),
        "a deleted account's picture is served"
    );
    let left: Vec<String> = dir
        .stage
        .blobs
        .keys()
        .await
        .into_iter()
        .filter(|key| key.contains(&dir.ids[DELETED[0]]))
        .collect();
    assert_eq!(left, Vec::<String>::new(), "a deleted account's picture is kept");
}

/// The engine's names, pictures and cards of the people it shows.
async fn engine_draws(dir: &Directory, reader: &mut Reader) {
    let a = &mut reader.a;
    // the engine draws the people it shows: name and picture, many at once
    for n in [0, 1, 2, 3, HEAD, LINE_START, 100, 200] {
        a.engine().person(dir.mri(n)).await.unwrap();
    }
    a.idle_for(std::time::Duration::from_secs(3)).await;
    for n in [0, 1, 2, 3, HEAD, LINE_START, 100, 200] {
        let person = a.engine().person(dir.mri(n)).await.unwrap();
        assert_eq!(person.name.as_deref(), Some(dir.names[n].as_str()), "{n}");
        assert_eq!(person.picture.is_some(), [0, HEAD, LINE_START].contains(&n), "{n}");
    }
    // the card of a deleted and of a disabled account
    let gone = a.engine().profile_card(dir.mri(DELETED[1])).await;
    eprintln!(
        "a deleted account's card: {:?}",
        gone.as_ref().map(|card| card.name.clone())
    );
    assert!(
        gone.is_err() || gone.as_ref().unwrap().name.is_none(),
        "a deleted account still has a card"
    );
    let off = a.engine().profile_card(dir.mri(DISABLED[1])).await;
    eprintln!(
        "a disabled account's card: name {:?}, presence {:?}",
        off.as_ref().ok().and_then(|card| card.name.clone()),
        off.as_ref().ok().and_then(|card| card.presence.clone())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_directory_reads_as_teams_shows_it_from_the_real_binary() {
    for backend in backends() {
        let dir = Directory::seed(backend).await;
        let rig = &dir.stage.rig;
        let a = Seat::sign_in(rig.other(), &rig.origin, "a@own.test").await;
        let client = a.client().await;
        let mut reader = Reader { a, client };
        short_profiles(&dir, &reader).await;
        graph_names(&dir, &reader).await;
        reporting_lines(&dir, &reader).await;
        full_card(&dir, &reader).await;
        out_of_office(&dir, &reader).await;
        people_search(&dir, &reader).await;
        gone_presence(&dir).await;
        pictures(&dir, &reader).await;
        engine_draws(&dir, &mut reader).await;
        drop(reader);
        dir.stage.stop().await;
    }
}
