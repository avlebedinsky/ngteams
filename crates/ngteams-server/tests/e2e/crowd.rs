//! A crowded directory, seeded with synthetic accounts: hundreds of people with odd names, long reporting lines and
//! many shared chats, read through the client's typed people and presence reads. What the small fixtures can't show:
//! the lists at their caps, the order the answers keep, names that need escaping, the rankings.

use std::sync::Arc;

use diesel::QueryDsl as _;
use diesel_async::RunQueryDsl as _;
use ngteams_server::db::schema::users;
use ngteams_server::db::users::User;
use ngteams_server::db::{Db, DbError};
use ngteams_server::people::{self, Profile};
use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_proto::{
    Client, GraphUser, LOKI_AUDIENCE, LokiOrganization, LokiPerson, LokiWorkingWith, Mri, PRESENCE_AUDIENCE,
    PresenceRead, ShortProfileLookup,
};

use crate::presence::{Seat, Transport, transport};
use crate::support::{TestServer, backends};
use crate::trouter::{ORIGIN, server_with};

/// How many synthetic accounts are seeded beside the reader.
const CROWD: usize = 660;

/// Names a directory meets: accents, other scripts, right-to-left, emoji, combining marks, quotes and markup,
/// backslashes, doubled spaces, the longest name an account may have; each as the admin path keeps it (trimmed).
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
    ]
    .iter()
    .map(|name| (*name).to_owned())
    .collect();
    names.push("L".repeat(256));
    names
}

/// The password hash of the account `id`.
async fn password_hash(db: &Db, id: &str) -> Result<String, DbError> {
    let query = users::table.find(id.to_owned()).select(users::password_hash);
    ngteams_server::with_conn!(db, |conn| Ok(query.first(conn).await?))
}

/// Inserts `rows` one by one (Diesel's SQLite takes no batch insert).
async fn insert(db: &Db, rows: &[User]) -> Result<(), DbError> {
    for row in rows {
        let query = diesel::insert_into(users::table).values(row.clone());
        ngteams_server::with_conn!(db, |conn| query.execute(conn).await?);
    }
    Ok(())
}

/// The crowd's object ids, in order; the first ones carry [`odd_names`], the others `Crowd <n>`.
struct Crowd {
    server: TestServer,
    reader: String,
    ids: Vec<String>,
    names: Vec<String>,
}

impl Crowd {
    async fn seed(backend: crate::support::Backend) -> Self {
        let server = server_with(backend, |_| {}).await;
        let reader = server.add_user("reader@own.test", "Reader").await;
        let hash = password_hash(&server.app.db, &reader).await.unwrap();
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
        insert(&server.app.db, &rows).await.unwrap();
        Self {
            server,
            reader,
            ids,
            names,
        }
    }

    fn mri(&self, n: usize) -> String {
        format!("8:orgid:{}", self.ids[n])
    }

    /// The reader's typed client, Loki's region set as the server names it.
    async fn client(&self) -> Client<Transport> {
        let session = Arc::new(self.server.session("reader@own.test").await);
        let client = Client::new(transport(&session, &self.server, &self.reader).await.with_loki());
        client.transport().set_loki_region(&format!("{ORIGIN}/loki/")).unwrap();
        client
    }

    async fn token(&self, audience: &str) -> String {
        let session = self.server.session("reader@own.test").await;
        session.access_token(audience).await.unwrap().token.expose().to_owned()
    }

    /// Sets the account `n` to report to the account `manager`.
    async fn reports(&self, n: usize, manager: usize) {
        let profile = Profile {
            user_id: self.ids[n].clone(),
            manager_id: Some(self.ids[manager].clone()),
            ..Profile::default()
        };
        people::repo::put_profile(&self.server.app.db, &profile).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn short_profiles_and_graph_names_keep_every_odd_name_and_the_order_asked_up_to_the_cap() {
    for backend in backends() {
        let crowd = Crowd::seed(backend).await;
        let client = crowd.client().await;
        // the most one lookup takes, asked from the end of the crowd back, so the answer's order is the asked one
        let asked: Vec<usize> = (0..500).rev().collect();
        let lookup = ShortProfileLookup::new(asked.iter().map(|n| Mri::new(crowd.mri(*n)))).unwrap();
        let answer = client.post_read(lookup.clone()).await.unwrap();
        let profiles = lookup.read_answer(answer.body()).unwrap();
        assert_eq!(profiles.len(), 500);
        for (profile, n) in profiles.iter().zip(&asked) {
            assert_eq!(profile.mri.as_str(), crowd.mri(*n));
            assert_eq!(profile.display_name.as_deref(), Some(crowd.names[*n].as_str()), "{n}");
            assert_eq!(
                profile.email.as_deref(),
                Some(format!("crowd-{n:03}@own.test").as_str())
            );
        }

        // each odd name reads back unchanged from Graph and from the person card
        for n in 0..odd_names().len() {
            let read = GraphUser::of(&crowd.mri(n)).unwrap();
            let answer = client.read(&read).await.unwrap();
            assert_eq!(
                read.read_answer(answer.body()).unwrap().as_deref(),
                Some(crowd.names[n].as_str())
            );
            let person = LokiPerson::of(&crowd.mri(n)).unwrap();
            let answer = client.read(&person).await.unwrap();
            let details = person.read_answer(answer.body()).unwrap();
            assert_eq!(details.display_name.as_deref(), Some(crowd.names[n].as_str()), "{n}");
        }
        crowd.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_presence_read_at_its_cap_answers_every_item_in_the_order_asked() {
    for backend in backends() {
        let crowd = Crowd::seed(backend).await;
        let client = crowd.client().await;
        let typed: Vec<Mri> = (0..40).map(|n| Mri::new(crowd.mri(n))).collect();
        let read = PresenceRead::new(typed.clone()).unwrap();
        let answer = client.post_read(read.clone()).await.unwrap();
        let read = read.read_answer(answer.body()).unwrap();
        assert_eq!(read.len(), 40);
        assert!(read.iter().zip(&typed).all(|(item, mri)| item.mri == *mri));
        assert!(read.iter().all(|item| item.availability.as_deref() == Some("Offline")));

        // 650 names, the most a read takes: known people, strangers and the same person twice, each answered
        let mut asked: Vec<String> = (0..640).map(|n| crowd.mri(n)).collect();
        asked.extend((0..9).map(|_| format!("8:orgid:{}", uuid::Uuid::new_v4())));
        asked.push(crowd.mri(0));
        let body: Vec<Value> = asked.iter().map(|mri| json!({"mri": mri})).collect();
        let token = crowd.token(PRESENCE_AUDIENCE).await;
        let response = reqwest::Client::new()
            .post(crowd.server.url("/ups/own/v1/presence/getpresence/"))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let items: Vec<Value> = response.json().await.unwrap();
        assert_eq!(items.len(), 650);
        for (item, mri) in items.iter().zip(&asked) {
            assert_eq!(item["mri"], *mri);
        }
        let statuses = |status: i64| items.iter().filter(|item| item["status"] == status).count();
        assert_eq!((statuses(20000), statuses(40400)), (641, 9));
        crowd.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_reporting_line_lists_the_nearest_managers_and_a_large_team_its_reports_by_name() {
    for backend in backends() {
        let crowd = Crowd::seed(backend).await;
        let client = crowd.client().await;
        // 20 to 31 is a line of 12, each reporting to the next
        for n in 20..31 {
            crowd.reports(n, n + 1).await;
        }
        // 40 reports for the top of the line, the odd names among them
        let team: Vec<usize> = (0..10).chain(100..130).collect();
        for n in &team {
            crowd.reports(*n, 31).await;
        }
        let organization = async |n: usize| {
            let answer = client
                .read(&LokiOrganization::of(&crowd.mri(n)).unwrap())
                .await
                .unwrap();
            LokiOrganization::read_answer(answer.body()).unwrap()
        };
        let bottom = organization(20).await;
        let managers: Vec<&str> = bottom.managers.iter().map(|person| person.object_id.as_str()).collect();
        let nearest: Vec<&str> = (21..29).map(|n| crowd.ids[n].as_str()).collect();
        assert_eq!(managers, nearest, "the 8 nearest, nearest first");
        let top = organization(31).await;
        assert!(top.managers.is_empty());
        assert_eq!(top.directs.len(), 41);
        let named: Vec<String> = top.directs.iter().map(|person| person.name.clone().unwrap()).collect();
        let mut sorted = named.clone();
        sorted.sort();
        assert_eq!(named, sorted, "by name");
        // a manager who can't sign in ends the line where it stands
        let fanout = crowd.server.app.fanout.as_ref();
        ngteams_server::admin::set_disabled(&crowd.server.app.db, fanout, "crowd-023@own.test", true)
            .await
            .unwrap();
        assert_eq!(organization(20).await.managers.len(), 2);
        crowd.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn whom_a_person_works_with_is_ranked_by_the_chats_shared_and_cut_at_the_limit_asked() {
    for backend in backends() {
        let crowd = Crowd::seed(backend).await;
        let seat = Seat::start(&crowd.server, &crowd.server, "reader@own.test", &crowd.reader).await;
        // the reader and the person (crowd 0) in 14 chats: crowd 1 in three of them, crowd 2 in two, 3 to 16 in one
        let mut chats: Vec<Vec<usize>> = (3..17).map(|n| vec![0, n]).collect();
        for chat in &mut chats[..3] {
            chat.push(1);
        }
        for chat in &mut chats[3..5] {
            chat.push(2);
        }
        for (k, chat) in chats.iter().enumerate() {
            let action = Action::CreateChat {
                others: chat.iter().map(|n| Mri::new(crowd.mri(*n))).collect(),
                topic: Some(format!("Room {k}")),
            };
            seat.engine.write(action).await.unwrap().message_id.unwrap();
        }
        seat.engine.shutdown();

        let client = crowd.client().await;
        let answer = client.read(&LokiWorkingWith::of(&crowd.mri(0)).unwrap()).await.unwrap();
        let with = LokiWorkingWith::read_answer(answer.body()).unwrap();
        let ids: Vec<&str> = with.iter().map(|person| person.object_id.as_str()).collect();
        assert_eq!(ids.len(), 12, "Teams' card asks for 12");
        assert_eq!(
            &ids[..2],
            [crowd.ids[1].as_str(), crowd.ids[2].as_str()],
            "most shared first"
        );
        let rest: Vec<&str> = with[2..].iter().map(|person| person.name.as_deref().unwrap()).collect();
        let mut sorted = rest.clone();
        sorted.sort_unstable();
        assert_eq!(rest, sorted, "then by name");

        let token = crowd.token(LOKI_AUDIENCE).await;
        let listed = async |limit: &str| {
            let url = crowd.server.url(&format!(
                "/loki/api/v1/workingwith?aadObjectId={}&limit={limit}",
                crowd.ids[0]
            ));
            let request = reqwest::Client::new()
                .get(url)
                .bearer_auth(&token)
                .header("X-ClientType", "Teams");
            let answer: Value = request.send().await.unwrap().json().await.unwrap();
            answer["value"].as_array().unwrap().len()
        };
        assert_eq!(listed("50").await, 16);
        assert_eq!(listed("0").await, 1);
        assert_eq!(listed("9999").await, 16);
        assert_eq!(listed("many").await, 12);
        crowd.server.stop().await;
    }
}
