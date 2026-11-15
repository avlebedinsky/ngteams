//! People: the client's typed profile, picture and person card reads and its engine's lookups against the
//! in-process server's directory.

use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ngteams_server::people::{self, Profile};
use serde_json::{Value, json};
use teams_core::Availability;
use teams_core::engine::{Action, Event, OwnStatus};
use teams_proto::{
    Client, GRAPH_AUDIENCE, GraphPicture, GraphUser, LOKI_AUDIENCE, LokiConfiguration, LokiOrganization,
    LokiOutOfOffice, LokiPerson, LokiSchedule, LokiWorkingWith, MIDDLE_TIER_AUDIENCE, Mri, OutOfOfficeState,
    ShortProfileLookup,
};

use crate::presence::{Seat, Transport, transport};
use crate::support::{Backend, TestServer, backends};
use crate::trouter::{ORIGIN, server_with};

const WAIT: Duration = Duration::from_secs(40);

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

fn status_of(error: teams_proto::HttpError) -> Option<u16> {
    match error {
        teams_proto::HttpError::Status(answer) => Some(answer.status()),
        _ => None,
    }
}

/// A PNG of `width` by `height`.
fn png(width: u32, height: u32) -> Bytes {
    let picture = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([u8::try_from(x % 256).unwrap(), u8::try_from(y % 256).unwrap(), 90])
    });
    let mut bytes = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes.into()
}

/// Ada, Bert (who reports to Cora, with a title, an office and a picture) and Cora, Ada and Bert in a group chat
/// with Dan.
struct Office {
    server: TestServer,
    a: String,
    b: String,
    c: String,
    d: String,
}

impl Office {
    async fn open(backend: Backend) -> Self {
        let server = server_with(backend, |_| {}).await;
        let a = server.add_user("ada@own.test", "Ada Lovelace").await;
        let b = server.add_user("bert@own.test", "Bert Sample").await;
        let c = server.add_user("cora@own.test", "Cora Chief").await;
        let d = server.add_user("dan@own.test", "Dan Fourth").await;
        let bert = Profile {
            user_id: b.clone(),
            given_name: Some("Bert".into()),
            surname: Some("Sample".into()),
            job_title: Some("Engineer".into()),
            office_location: Some("Room 4".into()),
            manager_id: Some(c.clone()),
            ..Profile::default()
        };
        people::repo::put_profile(&server.app.db, &bert).await.unwrap();
        people::set_picture(&server.app.db, server.app.storage.as_ref(), &b, png(200, 120))
            .await
            .unwrap();
        Self { server, a, b, c, d }
    }

    /// `upn`'s typed client, with Loki's region as the server's configuration names it.
    async fn client(&self, upn: &str, id: &str) -> Client<Transport> {
        let session = Arc::new(self.server.session(upn).await);
        let transport = transport(&session, &self.server, id).await.with_loki();
        let client = Client::new(transport);
        let configuration = client.read(&LokiConfiguration).await.unwrap();
        let body: Value = serde_json::from_slice(configuration.body()).unwrap();
        assert_eq!(body["LokiUrl"], format!("{ORIGIN}/loki/"));
        client
            .transport()
            .set_loki_region(body["LokiUrl"].as_str().unwrap())
            .unwrap();
        client
    }

    async fn token(&self, upn: &str, audience: &str) -> String {
        let session = self.server.session(upn).await;
        session.access_token(audience).await.unwrap().token.expose().to_owned()
    }

    async fn get(&self, token: &str, path: &str) -> (u16, Value) {
        let response = reqwest::Client::new()
            .get(self.server.url(path))
            .bearer_auth(token)
            .header("X-ClientType", "Teams")
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn short_profiles_name_people_by_mri_and_by_address_with_their_title_and_office() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let ada = office.client("ada@own.test", &office.a).await;
        let stranger = mri(&uuid::Uuid::new_v4().to_string());
        let lookup = ShortProfileLookup::new([Mri::new(mri(&office.b)), Mri::new(stranger), Mri::new(mri(&office.a))])
            .unwrap();
        let answer = ada.post_read(lookup.clone()).await.unwrap();
        let profiles = lookup.read_answer(answer.body()).unwrap();
        assert_eq!(profiles.len(), 2);
        let bert = &profiles[0];
        assert_eq!(bert.mri.as_str(), mri(&office.b));
        assert_eq!(bert.display_name.as_deref(), Some("Bert Sample"));
        assert_eq!(bert.given_name.as_deref(), Some("Bert"));
        assert_eq!(bert.surname.as_deref(), Some("Sample"));
        assert_eq!(bert.email.as_deref(), Some("bert@own.test"));
        assert_eq!(bert.user_principal_name.as_deref(), Some("bert@own.test"));
        assert_eq!(bert.object_id.as_deref(), Some(office.b.as_str()));
        assert_eq!(bert.user_type.as_deref(), Some("Member"));
        assert_eq!(bert.job_title.as_deref(), Some("Engineer"));
        assert_eq!(bert.office_location.as_deref(), Some("Room 4"));
        assert_eq!(profiles[1].given_name, None);
        assert_eq!(profiles[1].job_title, None);

        // the same lookup by address
        let token = office.token("ada@own.test", MIDDLE_TIER_AUDIENCE).await;
        let by_address = |query: &'static str, body: Value| {
            let url = office
                .server
                .url(&format!("/api/mt/part/own-01/beta/users/fetchShortProfile{query}"));
            let request = reqwest::Client::new().post(url).bearer_auth(&token).json(&body);
            async move {
                let response = request.send().await.unwrap();
                (response.status().as_u16(), response.json::<Value>().await.unwrap_or(Value::Null))
            }
        };
        let (status, found) = by_address("?isMailAddress=true", json!(["BERT@own.test", "nobody@own.test"])).await;
        assert_eq!(status, 200);
        assert_eq!(found["type"], "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity");
        assert_eq!(found["value"].as_array().unwrap().len(), 1);
        assert_eq!(found["value"][0]["mri"], mri(&office.b));
        assert_eq!(found["value"][0]["jobTitle"], "Engineer");
        // an address is no MRI, and too many names are refused
        let (_, none) = by_address("", json!(["bert@own.test"])).await;
        assert_eq!(none["value"], json!([]));
        let many: Vec<String> = (0..501).map(|n| format!("8:orgid:{n}")).collect();
        assert_eq!(by_address("", json!(many)).await.0, 400);

        // a disabled account keeps its name for what it wrote; a deleted one is nobody
        let fanout = office.server.app.fanout.as_ref();
        ngteams_server::admin::set_disabled(&office.server.app.db, fanout, "bert@own.test", true)
            .await
            .unwrap();
        ngteams_server::admin::delete_by_upn(&office.server.app.db, fanout, "cora@own.test")
            .await
            .unwrap();
        let lookup = ShortProfileLookup::new([Mri::new(mri(&office.b)), Mri::new(mri(&office.c))]).unwrap();
        let answer = ada.post_read(lookup.clone()).await.unwrap();
        let profiles = lookup.read_answer(answer.body()).unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].display_name.as_deref(), Some("Bert Sample"));
        office.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn graph_names_a_user_in_the_forms_the_client_reads_and_serves_their_picture() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let ada = office.client("ada@own.test", &office.a).await;
        let read = GraphUser::of(&mri(&office.b)).unwrap();
        let answer = ada.read(&read).await.unwrap();
        assert_eq!(read.read_answer(answer.body()).unwrap().as_deref(), Some("Bert Sample"));
        let selected: Value = serde_json::from_slice(answer.body()).unwrap();
        let mut keys: Vec<&str> = selected.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["@odata.context", "displayName", "id"]);

        let picture = GraphPicture::of(&mri(&office.b)).unwrap();
        let answer = ada.read(&picture).await.unwrap();
        let read = picture.read_answer(&answer).unwrap();
        let decoded = image::load_from_memory(&read.bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (96, 96));
        let none = GraphPicture::of(&mri(&office.c)).unwrap();
        assert_eq!(status_of(ada.read(&none).await.unwrap_err()), Some(404));

        let token = office.token("ada@own.test", GRAPH_AUDIENCE).await;
        let user = format!("/graph/v1.0/users/{}", office.b);
        let (status, whole) = office.get(&token, &user).await;
        assert_eq!(status, 200);
        assert_eq!(
            whole,
            json!({
                "@odata.context": format!("{ORIGIN}/graph/v1.0/$metadata#users/$entity"),
                "id": office.b, "displayName": "Bert Sample", "userPrincipalName": "bert@own.test",
                "mail": "bert@own.test", "businessPhones": [], "givenName": "Bert", "surname": "Sample",
                "jobTitle": "Engineer", "mobilePhone": null, "officeLocation": "Room 4", "preferredLanguage": null,
            })
        );
        let (_, checked) = office
            .get(&token, &format!("{user}?$select=id,userPrincipalName,mail,nonsense"))
            .await;
        assert_eq!(
            checked,
            json!({
                "@odata.context": format!("{ORIGIN}/graph/v1.0/$metadata#users(id,userPrincipalName,mail)/$entity"),
                "id": office.b, "userPrincipalName": "bert@own.test", "mail": "bert@own.test",
            })
        );
        let (_, me) = office.get(&token, "/graph/v1.0/me?$select=id,userPrincipalName").await;
        assert_eq!(me["id"], office.a);
        assert_eq!(me["userPrincipalName"], "ada@own.test");
        assert_eq!(me.get("displayName"), None);

        let nobody = format!("/graph/v1.0/users/{}", uuid::Uuid::new_v4());
        let (status, missing) = office.get(&token, &nobody).await;
        assert_eq!((status, &missing["error"]["code"]), (404, &json!("Request_ResourceNotFound")));
        let (status, missing) = office.get(&token, &format!("{nobody}/photos/96x96/$value")).await;
        assert_eq!((status, &missing["error"]["code"]), (404, &json!("ImageNotFound")));
        let (status, _) = office.get(&token, &format!("{user}/photos/48x48/$value")).await;
        assert_eq!(status, 404);
        let other = office.token("ada@own.test", MIDDLE_TIER_AUDIENCE).await;
        for path in [user.clone(), format!("{user}/photos/96x96/$value"), "/graph/v1.0/me".to_owned()] {
            assert_eq!(office.get(&other, &path).await.0, 401, "{path}");
        }

        // a picture taken away is gone, from the store too
        people::remove_picture(&office.server.app.db, office.server.app.storage.as_ref(), &office.b)
            .await
            .unwrap();
        assert_eq!(status_of(ada.read(&picture).await.unwrap_err()), Some(404));
        office.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picture_is_kept_only_when_it_is_one_and_replaces_the_one_before() {
    let office = Office::open(Backend::Sqlite).await;
    let (db, storage) = (&office.server.app.db, office.server.app.storage.as_ref());
    let first = people::repo::profile(db, &office.b).await.unwrap();
    let key = first.picture_key.clone().unwrap();
    assert!(storage.get(&key).await.unwrap().is_some());
    assert!(first.picture_size > 0);
    assert_eq!(first.job_title.as_deref(), Some("Engineer"));

    for refused in [Bytes::from_static(b"not a picture"), Bytes::from(vec![0_u8; 5 * 1024 * 1024])] {
        let error = people::set_picture(db, storage, &office.b, refused).await.unwrap_err();
        assert!(matches!(error, people::PictureError::NotAPicture | people::PictureError::TooLarge), "{error}");
    }
    let nobody = people::set_picture(db, storage, "nobody", png(10, 10)).await.unwrap_err();
    assert!(matches!(nobody, people::PictureError::NoAccount), "{nobody}");
    assert!(storage.get(&key).await.unwrap().is_some());

    people::set_picture(db, storage, &office.b, png(40, 300)).await.unwrap();
    let second = people::repo::profile(db, &office.b).await.unwrap();
    assert_ne!(second.picture_key, first.picture_key);
    assert_eq!(storage.get(&key).await.unwrap(), None);
    assert_eq!(second.manager_id.as_deref(), Some(office.c.as_str()));
    // an account without a profile row gets one for its picture
    people::set_picture(db, storage, &office.d, png(96, 96)).await.unwrap();
    assert!(people::repo::profile(db, &office.d).await.unwrap().picture_key.is_some());
    assert!(!people::remove_picture(db, storage, &office.a).await.unwrap());
    assert!(people::remove_picture(db, storage, &office.d).await.unwrap());
    office.server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_person_card_reads_come_from_the_directory_and_the_chats_shared_with_the_reader() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let ada = office.client("ada@own.test", &office.a).await;
        let bert = mri(&office.b);

        let person = LokiPerson::of(&bert).unwrap();
        let answer = ada.read(&person).await.unwrap();
        let details = person.read_answer(answer.body()).unwrap();
        assert_eq!(details.display_name.as_deref(), Some("Bert Sample"));
        assert_eq!(details.given_name.as_deref(), Some("Bert"));
        assert_eq!(details.surname.as_deref(), Some("Sample"));
        assert_eq!(details.emails, ["bert@own.test"]);
        assert_eq!(details.im_addresses, ["bert@own.test"]);
        assert_eq!(details.user_principal_name.as_deref(), Some("bert@own.test"));
        assert_eq!(details.user_type.as_deref(), Some("Member"));

        // Bert reports to Cora, and Cora's direct report is Bert
        let answer = ada.read(&LokiOrganization::of(&bert).unwrap()).await.unwrap();
        let organization = LokiOrganization::read_answer(answer.body()).unwrap();
        assert_eq!(organization.managers.len(), 1);
        assert_eq!(organization.managers[0].object_id, office.c);
        assert_eq!(organization.managers[0].name.as_deref(), Some("Cora Chief"));
        assert_eq!(organization.managers[0].email.as_deref(), Some("cora@own.test"));
        assert!(organization.directs.is_empty());
        let answer = ada.read(&LokiOrganization::of(&mri(&office.c)).unwrap()).await.unwrap();
        let organization = LokiOrganization::read_answer(answer.body()).unwrap();
        assert!(organization.managers.is_empty());
        assert_eq!(organization.directs.len(), 1);
        assert_eq!(organization.directs[0].object_id, office.b);

        // whom Bert works with, as Ada may know it: the others of the chats they are both in
        let working = LokiWorkingWith::of(&bert).unwrap();
        let answer = ada.read(&working).await.unwrap();
        assert!(LokiWorkingWith::read_answer(answer.body()).unwrap().is_empty());
        let ada_seat = Seat::start(&office.server, &office.server, "ada@own.test", &office.a).await;
        let bert_seat = Seat::start(&office.server, &office.server, "bert@own.test", &office.b).await;
        let group = |seat: &Seat, others: [&String; 2], topic: &str| {
            let action = Action::CreateChat {
                others: others.iter().map(|id| Mri::new(mri(id))).collect(),
                topic: Some(topic.into()),
            };
            let write = seat.engine.write(action);
            async move { write.await.unwrap().message_id.unwrap() }
        };
        let group_id = group(&ada_seat, [&office.b, &office.d], "Launch").await;
        group(&bert_seat, [&office.c, &office.d], "Board").await;
        ada_seat.engine.shutdown();
        bert_seat.engine.shutdown();
        let answer = ada.read(&working).await.unwrap();
        let with = LokiWorkingWith::read_answer(answer.body()).unwrap();
        assert_eq!(with.len(), 1, "{group_id}: only Dan is in a chat with both");
        assert_eq!(with[0].object_id, office.d);
        assert_eq!(with[0].name.as_deref(), Some("Dan Fourth"));

        // no calendar says when Bert is free; automatic replies are off until the directory says otherwise
        let answer = ada.read(&LokiSchedule::of(&bert).unwrap()).await.unwrap();
        let schedule = LokiSchedule::read_answer(answer.body()).unwrap();
        assert_eq!(schedule.current, Some(teams_proto::FreeBusyStatus::Unknown));
        assert_eq!(schedule.until_ms, None);
        assert!(schedule.work_days.is_empty());
        let out = LokiOutOfOffice::of(&bert).unwrap();
        let answer = ada.read(&out).await.unwrap();
        assert_eq!(LokiOutOfOffice::read_answer(answer.body()).unwrap(), OutOfOfficeState::Disabled);
        let mut profile = people::repo::profile(&office.server.app.db, &office.b).await.unwrap();
        profile.out_of_office = true;
        people::repo::put_profile(&office.server.app.db, &profile).await.unwrap();
        let answer = ada.read(&out).await.unwrap();
        assert_eq!(
            LokiOutOfOffice::read_answer(answer.body()).unwrap(),
            OutOfOfficeState::Other("AlwaysEnabled".into())
        );
        let answer = ada.read(&LokiSchedule::of(&bert).unwrap()).await.unwrap();
        let schedule = LokiSchedule::read_answer(answer.body()).unwrap();
        assert_eq!(schedule.current, Some(teams_proto::FreeBusyStatus::OutOfOffice));

        // Loki's own refusals
        let token = office.token("ada@own.test", LOKI_AUDIENCE).await;
        let query = format!("teamsMri={bert}&aadObjectId={}&personaType=User", office.b);
        let bare = reqwest::Client::new()
            .get(office.server.url(&format!("/loki/api/v2/person?{query}")))
            .bearer_auth(&token);
        let response = bare.send().await.unwrap();
        assert_eq!(response.status(), 400);
        assert!(response.text().await.unwrap().contains("ClientTypeMissing"));
        let nobody = uuid::Uuid::new_v4();
        for path in [
            format!("/loki/api/v2/person?teamsMri=8:orgid:{nobody}&aadObjectId={nobody}&personaType=User"),
            "/loki/api/v1/schedule?personaType=User".to_owned(),
            format!("/loki/api/v1/oofstatus?aadObjectId={}&teamsMri=8:orgid:{nobody}", office.b),
        ] {
            assert_eq!(office.get(&token, &path).await.0, 404, "{path}");
        }
        let graph = office.token("ada@own.test", GRAPH_AUDIENCE).await;
        assert_eq!(office.get(&graph, &format!("/loki/api/v2/person?{query}")).await.0, 401);
        office.server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_names_and_draws_a_colleague_and_reads_their_card() {
    for backend in backends() {
        let office = Office::open(backend).await;
        let mut ada = Seat::start(&office.server, &office.server, "ada@own.test", &office.a).await;
        let bert = Seat::start(&office.server, &office.server, "bert@own.test", &office.b).await;
        bert.report(OwnStatus::Available).await;

        // the name and the picture come from the server's Graph
        let asked = ada.engine.person(bert.mri.clone()).await.unwrap();
        assert!(asked.looking, "{asked:?}");
        let told = tokio::time::timeout(WAIT, async {
            loop {
                match ada.engine.next_event().await {
                    Some(Event::Profile { mri }) if mri == bert.mri => return,
                    Some(_) => {}
                    None => panic!("the engine stopped"),
                }
            }
        })
        .await;
        assert!(told.is_ok(), "no profile came");
        let known = ada.engine.person(bert.mri.clone()).await.unwrap();
        assert_eq!(known.name.as_deref(), Some("Bert Sample"));
        assert!(known.picture.is_some());

        // the card: the presence service's read and the short profile's title and office
        let card = ada.engine.profile_card(bert.mri.clone()).await.unwrap();
        assert_eq!(card.name.as_deref(), Some("Bert Sample"));
        assert_eq!(card.job_title.as_deref(), Some("Engineer"));
        assert_eq!(card.office.as_deref(), Some("Room 4"));
        assert_eq!(card.user_principal_name.as_deref(), Some("bert@own.test"));
        let presence = card.presence.unwrap();
        assert_eq!(presence.availability, Availability::Available);
        assert_eq!(card.out_of_office, Some(false));
        ada.engine.shutdown();
        bert.engine.shutdown();
        office.server.stop().await;
    }
}

/// The admin command line on `server`'s database and file store.
async fn cli(server: &TestServer, args: &[&str]) -> Result<String, ngteams_server::cli::CliError> {
    use clap::Parser as _;
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("server.toml");
    let written = format!(
        "database_url = \"{}\"\npublic_origin = \"{}\"\nstorage_url = \"{}\"\n",
        server.app.config.database_url, server.origin, server.app.config.storage_url
    );
    std::fs::write(&config, written).unwrap();
    let mut command = vec!["ngteams-server", "--config", config.to_str().unwrap()];
    command.extend_from_slice(args);
    let mut output = Vec::new();
    let cli = ngteams_server::cli::Cli::try_parse_from(command).expect("the command line isn't one the server takes");
    ngteams_server::cli::run(cli, &mut "".as_bytes(), &mut output, &mut Vec::new(), std::future::pending()).await?;
    Ok(String::from_utf8(output).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_sets_a_profile_and_a_picture_on_the_command_line_and_clients_read_them() {
    let blobs = tempfile::tempdir().unwrap();
    let store = format!("file://{}", blobs.path().display());
    let server = server_with(Backend::Sqlite, |config| config.storage_url = store).await;
    let a = server.add_user("ada@own.test", "Ada Lovelace").await;
    let b = server.add_user("bert@own.test", "Bert Sample").await;
    let c = server.add_user("cora@own.test", "Cora Chief").await;
    let set = [
        "profile", "set", "Bert@own.test", "--given-name", "Bert", "--surname", " Sample ", "--job-title", "Engineer",
        "--department", "Research", "--office", "Room 4", "--company", "Own", "--mobile-phone", "+1 555 0100",
        "--business-phone", "+1 555 0101", "--manager", "cora@own.test", "--out-of-office", "true",
    ];
    assert_eq!(cli(&server, &set).await.unwrap(), "updated bert@own.test\n");
    let file = blobs.path().join("bert.png");
    std::fs::write(&file, png(300, 200)).unwrap();
    let pictured = cli(&server, &["profile", "picture", "bert@own.test", file.to_str().unwrap()])
        .await
        .unwrap();
    assert!(pictured.starts_with("picture set for bert@own.test ("), "{pictured}");
    let shown = cli(&server, &["profile", "show", "bert@own.test"]).await.unwrap();
    for line in [
        "name\tBert Sample\n", "given name\tBert\n", "surname\tSample\n", "job title\tEngineer\n",
        "department\tResearch\n", "office\tRoom 4\n", "company\tOwn\n", "mobile phone\t+1 555 0100\n",
        "business phone\t+1 555 0101\n", "manager\tcora@own.test\n", "out of office\tyes\n", "picture\tyes\n",
    ] {
        assert!(shown.contains(line), "{line:?} isn't in {shown:?}");
    }

    // what the operator set is what a client reads
    let session = Arc::new(server.session("ada@own.test").await);
    let ada = Client::new(transport(&session, &server, &a).await.with_loki());
    ada.transport().set_loki_region(&format!("{ORIGIN}/loki/")).unwrap();
    let bert = Mri::new(mri(&b));
    let lookup = ShortProfileLookup::new([bert.clone()]).unwrap();
    let answer = ada.post_read(lookup.clone()).await.unwrap();
    let profile = lookup.read_answer(answer.body()).unwrap().remove(0);
    assert_eq!(profile.job_title.as_deref(), Some("Engineer"));
    assert_eq!(profile.surname.as_deref(), Some("Sample"));
    let picture = GraphPicture::of(bert.as_str()).unwrap();
    assert!(picture.read_answer(&ada.read(&picture).await.unwrap()).is_ok());
    let answer = ada.read(&LokiOrganization::of(bert.as_str()).unwrap()).await.unwrap();
    assert_eq!(LokiOrganization::read_answer(answer.body()).unwrap().managers[0].object_id, c);
    let presence = teams_proto::PresenceRead::new([bert.clone()]).unwrap();
    let answer = ada.post_read(presence.clone()).await.unwrap();
    assert_eq!(presence.read_answer(answer.body()).unwrap()[0].out_of_office, Some(true));

    // an empty value clears a field, and the others stay
    let cleared = ["profile", "set", "bert@own.test", "--job-title", "", "--manager", "", "--out-of-office", "false"];
    cli(&server, &cleared).await.unwrap();
    let kept = people::repo::profile(&server.app.db, &b).await.unwrap();
    assert_eq!((kept.job_title, kept.manager_id, kept.out_of_office), (None, None, false));
    assert_eq!(kept.office_location.as_deref(), Some("Room 4"));
    assert!(kept.picture_key.is_some());
    let shown = cli(&server, &["profile", "show", "bert@own.test"]).await.unwrap();
    assert!(shown.contains("job title\t\n") && shown.contains("out of office\tno\n"), "{shown}");

    // nobody reports to themselves, directly or around
    cli(&server, &["profile", "set", "bert@own.test", "--manager", "cora@own.test"]).await.unwrap();
    for refused in [
        vec!["profile", "set", "cora@own.test", "--manager", "bert@own.test"],
        vec!["profile", "set", "cora@own.test", "--manager", "cora@own.test"],
        vec!["profile", "set", "cora@own.test", "--manager", "nobody@own.test"],
        vec!["profile", "set", "nobody@own.test", "--job-title", "Ghost"],
        vec!["profile", "set", "cora@own.test", "--job-title", "a\u{7}b"],
        vec!["profile", "show", "nobody@own.test"],
        vec!["profile", "picture", "nobody@own.test", file.to_str().unwrap()],
        vec!["profile", "picture", "cora@own.test", "/no/such/file.png"],
        vec!["profile", "picture", "cora@own.test", blobs.path().join("../server.toml").to_str().unwrap()],
        vec!["profile", "remove-picture", "nobody@own.test"],
    ] {
        assert!(cli(&server, &refused).await.is_err(), "{refused:?}");
    }
    let long = "x".repeat(257);
    assert!(cli(&server, &["profile", "set", "cora@own.test", "--office", &long]).await.is_err());
    let text = blobs.path().join("notes.txt");
    std::fs::write(&text, "not a picture").unwrap();
    assert!(cli(&server, &["profile", "picture", "cora@own.test", text.to_str().unwrap()]).await.is_err());

    assert_eq!(
        cli(&server, &["profile", "remove-picture", "bert@own.test"]).await.unwrap(),
        "picture removed for bert@own.test\n"
    );
    assert_eq!(
        cli(&server, &["profile", "remove-picture", "bert@own.test"]).await.unwrap(),
        "bert@own.test has no picture\n"
    );
    assert_eq!(status_of(ada.read(&picture).await.unwrap_err()), Some(404));
    server.stop().await;
}
