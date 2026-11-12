//! Loki, the persona card service, against a local mock: its configuration on the global host, the regional base taken
//! from it once, the person reads on that base with their query, headers and token, what is refused, and every answer
//! read from the synthetic fixture `tests/fixtures/loki/persona-card.json`. Every id, name and address here is made
//! up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::future::{self, Future};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_proto::{
    BuildError, Client, ClockTime, Credentials, Endpoint, FreeBusyStatus, HttpError, HttpTransport, LOKI_AUDIENCE,
    LokiAnswerError, LokiConfiguration, LokiOrganization, LokiOutOfOffice, LokiPerson, LokiSchedule, LokiWorkingWith,
    Method, OutOfOfficeState, Read, Refusal, Request, Service, Weekday,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CHAT_BASE: &str = "https://teams.microsoft.com/api/chatsvc/amer";
const REGION: &str = "https://nam.loki.delve.office.com/";
const A: &str = "00000000-0000-4000-8000-00000000000a";
const C: &str = "00000000-0000-4000-8000-00000000000c";
const MRI_A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const FIXTURE: &str = include_str!("../../../tests/fixtures/loki/persona-card.json");

/// The fixture with its placeholders filled in.
fn fixture() -> Value {
    serde_json::from_str(&FIXTURE.replace("<guid:a>", A).replace("<guid:c>", C)).unwrap()
}

/// One read's answer from the fixture, as bytes.
fn answer(read: &str) -> Vec<u8> {
    serde_json::to_vec(&fixture()[read]["answer"]).unwrap()
}

/// Records every audience a token was asked for.
#[derive(Default)]
struct FakeTokens {
    audiences: Mutex<Vec<String>>,
}

impl FakeTokens {
    fn asked(&self) -> Vec<String> {
        self.audiences.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Credentials for FakeTokens {
    fn bearer(&self, audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.audiences
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(audience.to_owned());
        future::ready(Ok(Secret::new("placeholder-loki-token".to_owned())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

fn transport(server: &MockServer, tokens: &Arc<FakeTokens>) -> HttpTransport<FakeTokens> {
    HttpTransport::with_mock_origin(Arc::clone(tokens), CHAT_BASE, &server.uri()).unwrap()
}

/// A mock of `path` that takes only a request with Loki's headers and token.
fn loki(path_is: &str) -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path(path_is))
        .and(header("authorization", "Bearer placeholder-loki-token"))
        .and(header("accept", "application/json"))
        .and(header("x-clienttype", "Teams"))
        .and(header("x-clientfeature", "LivePersonaCard"))
        .and(header("x-clientarchitectureversion", "v2"))
        .and(header("x-hostappplatform", "Web"))
}

/// A mock of a person read of a's at `path_is`.
fn persona(path_is: &str) -> wiremock::MockBuilder {
    loki(path_is)
        .and(query_param("teamsMri", MRI_A))
        .and(query_param("aadObjectId", A))
        .and(query_param("personaType", "User"))
}

#[tokio::test]
async fn the_configuration_names_the_region_and_the_card_is_read_there() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_loki());
    let fixture = fixture();
    loki("/api/v1/livepersonacard/configuration")
        .and(query_param("cultureName", "en-us"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fixture["configuration"]["answer"]))
        .expect(1)
        .mount(&server)
        .await;
    for read in ["person", "organization", "workingwith", "schedule", "oofstatus"] {
        let mock = persona(fixture[read]["path"].as_str().unwrap());
        let mock = if read == "workingwith" {
            mock.and(query_param("limit", "12"))
        } else {
            mock
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(&fixture[read]["answer"]))
            .expect(1)
            .mount(&server)
            .await;
    }

    let configuration = client.read(&LokiConfiguration).await.unwrap();
    let region = LokiConfiguration::read_answer(configuration.body()).unwrap();
    assert_eq!(region, REGION);
    client.transport().set_loki_region(&region).unwrap();
    assert_eq!(client.transport().loki_region(), Some(REGION));

    let person = LokiPerson::of(MRI_A).unwrap();
    let details = person.read_answer(client.read(&person).await.unwrap().body()).unwrap();
    assert_eq!(details.display_name.as_deref(), Some("Lab Account A"));
    let organization = client.read(&LokiOrganization::of(MRI_A).unwrap()).await.unwrap();
    assert!(
        LokiOrganization::read_answer(organization.body())
            .unwrap()
            .managers
            .is_empty()
    );
    let working = client.read(&LokiWorkingWith::of(MRI_A).unwrap()).await.unwrap();
    assert_eq!(LokiWorkingWith::read_answer(working.body()).unwrap().len(), 1);
    let schedule = client.read(&LokiSchedule::of(MRI_A).unwrap()).await.unwrap();
    assert_eq!(LokiSchedule::read_answer(schedule.body()).unwrap().work_days.len(), 5);
    let oof = client.read(&LokiOutOfOffice::of(MRI_A).unwrap()).await.unwrap();
    assert_eq!(
        LokiOutOfOffice::read_answer(oof.body()).unwrap(),
        OutOfOfficeState::Disabled
    );
    assert!(tokens.asked().iter().all(|audience| audience == LOKI_AUDIENCE));
    assert_eq!(tokens.asked().len(), 6);
}

#[tokio::test]
async fn nothing_goes_to_loki_before_its_region_is_set_or_on_a_transport_not_given_it() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let person = LokiPerson::of(MRI_A).unwrap();
    let without = Client::new(transport(&server, &tokens));
    for refused in [without.read(&LokiConfiguration).await, without.read(&person).await] {
        assert!(matches!(refused, Err(HttpError::Refused(Refusal::NoLoki))));
    }
    assert!(!without.transport().has_loki());
    assert!(matches!(
        without.transport().set_loki_region(REGION),
        Err(BuildError::LokiBase)
    ));
    let with = Client::new(transport(&server, &tokens).with_loki());
    assert!(with.transport().has_loki());
    assert_eq!(with.transport().loki_region(), None);
    assert!(matches!(
        with.read(&person).await,
        Err(HttpError::Refused(Refusal::NoLoki))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.asked().is_empty());
}

#[test]
fn only_a_regional_loki_base_is_taken_and_only_once() {
    let tokens = Arc::new(FakeTokens::default());
    let transport = HttpTransport::new(Arc::clone(&tokens), CHAT_BASE).unwrap().with_loki();
    for base in [
        "http://nam.loki.delve.office.com/",
        "https://loki.delve.office.com/",
        "https://.loki.delve.office.com/",
        "https://-nam.loki.delve.office.com/",
        "https://nam-.loki.delve.office.com/",
        "https://a.b.loki.delve.office.com/",
        "https://nam_1.loki.delve.office.com/",
        "https://nam.loki.delve.office.com.example.test/",
        "https://nam.loki.delve.office.com/api",
        "https://nam.loki.delve.office.com/?x",
        "https://nam.loki.delve.office.com/#x",
        "https://nam.loki.delve.office.com:8443/",
        "https://user@nam.loki.delve.office.com/",
        "https://NAM.loki.delve.office.com/",
        " https://nam.loki.delve.office.com/",
        "nam.loki.delve.office.com",
    ] {
        assert!(
            matches!(transport.set_loki_region(base), Err(BuildError::LokiBase)),
            "{base}"
        );
    }
    assert_eq!(transport.loki_region(), None);
    transport
        .set_loki_region("https://eur-2.loki.delve.office.com")
        .unwrap();
    assert_eq!(transport.loki_region(), Some("https://eur-2.loki.delve.office.com/"));
    transport
        .set_loki_region("https://eur-2.loki.delve.office.com/")
        .unwrap();
    assert!(matches!(transport.set_loki_region(REGION), Err(BuildError::LokiBase)));
    assert_eq!(transport.loki_region(), Some("https://eur-2.loki.delve.office.com/"));
    assert!(format!("{transport:?}").contains("eur-2.loki"));
}

/// A request to Loki that isn't a typed read.
struct Plain(Service, Method);

impl Request for Plain {
    fn service(&self) -> Service {
        self.0
    }

    fn method(&self) -> Method {
        self.1
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["api", "v1", "organization", "manager"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({}))
    }
}

/// A read of the global host other than its configuration.
struct GlobalPerson;

impl Read for GlobalPerson {
    fn service(&self) -> Service {
        Service::LokiGlobal
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["api", "v2", "person"])
    }
}

#[tokio::test]
async fn loki_takes_typed_reads_only_and_its_global_host_its_configuration_only() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_loki());
    client.transport().set_loki_region(REGION).unwrap();
    for service in [Service::Loki, Service::LokiGlobal] {
        let sent = client.send(&Plain(service, Method::Post)).await;
        assert!(matches!(sent, Err(HttpError::Refused(Refusal::Loki))), "{service:?}");
        let written = client.write(Plain(service, Method::Post)).await;
        assert!(matches!(written, Err(HttpError::Refused(Refusal::Loki))), "{service:?}");
    }
    assert!(matches!(
        client.read(&GlobalPerson).await,
        Err(HttpError::Refused(Refusal::Loki))
    ));
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(tokens.asked().is_empty());
}

#[tokio::test]
async fn a_read_only_transport_still_reads_the_card() {
    let server = MockServer::start().await;
    let tokens = Arc::new(FakeTokens::default());
    let client = Client::new(transport(&server, &tokens).with_loki().read_only());
    client.transport().set_loki_region(REGION).unwrap();
    persona("/api/v1/oofstatus")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"outOfOfficeState": "Disabled"})))
        .expect(1)
        .mount(&server)
        .await;
    assert!(client.read(&LokiOutOfOffice::of(MRI_A).unwrap()).await.is_ok());
}

#[test]
fn every_read_is_of_an_orgid_mri_and_its_debug_leaves_the_id_out() {
    for mri in [
        "8:orgid:not-a-guid",
        "28:00000000-0000-4000-8000-00000000000a",
        "8:live:someone",
        "",
    ] {
        assert!(LokiPerson::of(mri).is_none(), "{mri}");
        assert!(LokiOrganization::of(mri).is_none(), "{mri}");
        assert!(LokiWorkingWith::of(mri).is_none(), "{mri}");
        assert!(LokiSchedule::of(mri).is_none(), "{mri}");
        assert!(LokiOutOfOffice::of(mri).is_none(), "{mri}");
    }
    let shown = [
        format!("{:?}", LokiPerson::of(MRI_A).unwrap()),
        format!("{:?}", LokiOrganization::of(MRI_A).unwrap()),
        format!("{:?}", LokiWorkingWith::of(MRI_A).unwrap()),
        format!("{:?}", LokiSchedule::of(MRI_A).unwrap()),
        format!("{:?}", LokiOutOfOffice::of(MRI_A).unwrap()),
    ];
    for shown in shown {
        assert!(!shown.contains(A), "{shown}");
    }
    let read = LokiWorkingWith::of(MRI_A).unwrap();
    assert_eq!(read.service(), Service::Loki);
    let query: Vec<(String, String)> = read.endpoint().query().to_vec();
    assert_eq!(
        query,
        [
            ("teamsMri".to_owned(), MRI_A.to_owned()),
            ("aadObjectId".to_owned(), A.to_owned()),
            ("personaType".to_owned(), "User".to_owned()),
            ("limit".to_owned(), "12".to_owned()),
        ]
    );
    assert_eq!(LokiConfiguration.service(), Service::LokiGlobal);
}

#[test]
fn the_configuration_gives_only_a_regional_base() {
    assert_eq!(
        LokiConfiguration::read_answer(&answer("configuration")).as_deref(),
        Some(REGION)
    );
    for body in [
        json!({"LokiUrl": "https://loki.example.test/"}),
        json!({"LokiUrl": 7}),
        json!({}),
        json!([]),
    ] {
        assert_eq!(
            LokiConfiguration::read_answer(&serde_json::to_vec(&body).unwrap()),
            None
        );
    }
    assert_eq!(LokiConfiguration::read_answer(b"not json"), None);
}

#[test]
fn a_person_s_names_and_addresses_are_read_and_nobody_else_s() {
    let person = LokiPerson::of(MRI_A).unwrap();
    let details = person.read_answer(&answer("person")).unwrap();
    assert_eq!(details.display_name.as_deref(), Some("Lab Account A"));
    assert_eq!(details.given_name.as_deref(), Some("Lab"));
    assert_eq!(details.surname, None, "an empty surname is none");
    assert_eq!(details.emails, ["lab-a@lab.example.com"]);
    assert_eq!(details.im_addresses, ["lab-a@lab.example.com"]);
    assert_eq!(details.user_principal_name.as_deref(), Some("lab-a@lab.example.com"));
    assert_eq!(details.user_type.as_deref(), Some("Member"));
    let shown = format!("{details:?}");
    assert!(!shown.contains("lab-a") && !shown.contains("Lab Account"), "{shown}");

    let twice = json!({"person": {"emailAddresses": [
        {"value": {"address": "one@lab.example.com"}},
        {"value": {"address": "ONE@lab.example.com"}},
        {"value": {"address": ""}},
        {"value": {}},
        {"value": {"address": "two@lab.example.com"}},
    ]}});
    let details = person.read_answer(&serde_json::to_vec(&twice).unwrap()).unwrap();
    assert_eq!(details.emails, ["one@lab.example.com", "two@lab.example.com"]);
    assert_eq!(details.display_name, None);

    let other = json!({"person": {"aadObjectId": C}});
    assert_eq!(
        person.read_answer(&serde_json::to_vec(&other).unwrap()),
        Err(LokiAnswerError::OtherUser)
    );
    let upper = json!({"person": {"aadObjectId": A.to_uppercase()}});
    assert!(person.read_answer(&serde_json::to_vec(&upper).unwrap()).is_ok());
    for body in [&b"{}"[..], b"{\"person\": []}", b"[]", b"not json"] {
        assert_eq!(person.read_answer(body), Err(LokiAnswerError::Malformed));
    }
}

#[test]
fn the_people_a_person_works_with_are_read_with_their_object_ids() {
    let people = LokiWorkingWith::read_answer(&answer("workingwith")).unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].object_id, C);
    assert_eq!(people[0].mri(), format!("8:orgid:{C}"));
    assert_eq!(people[0].name.as_deref(), Some("Lab Account C"));
    assert_eq!(people[0].email.as_deref(), Some("lab-c@lab.example.com"));
    assert_eq!(people[0].user_principal_name.as_deref(), Some("lab-c@lab.example.com"));
    let shown = format!("{:?}", people[0]);
    assert!(shown.contains(C) && !shown.contains("Lab Account"), "{shown}");

    let many: Vec<Value> = (0..20)
        .map(|n| json!({"aadObjectId": format!("00000000-0000-4000-8000-{n:012}"), "fullName": "Made Up"}))
        .chain([json!({"fullName": "No Id"}), json!({"aadObjectId": "not-a-guid"})])
        .collect();
    let people = LokiWorkingWith::read_answer(&serde_json::to_vec(&json!({"value": many})).unwrap()).unwrap();
    assert_eq!(people.len(), 12, "at most the limit asked for");
    for body in [json!({}), json!({"value": {}}), json!([])] {
        assert_eq!(
            LokiWorkingWith::read_answer(&serde_json::to_vec(&body).unwrap()),
            Err(LokiAnswerError::Malformed)
        );
    }
}

#[test]
fn an_organization_s_entries_are_read_leniently() {
    let empty = LokiOrganization::read_answer(&answer("organization")).unwrap();
    assert!(empty.managers.is_empty() && empty.directs.is_empty());
    let given = json!({
        "managers": [{"aadObjectId": C, "fullName": "Lab Account C", "email": "lab-c@lab.example.com"}],
        "directs": [{"fullName": "No Id"}, {"aadObjectId": A}],
    });
    let organization = LokiOrganization::read_answer(&serde_json::to_vec(&given).unwrap()).unwrap();
    assert_eq!(organization.managers.len(), 1);
    assert_eq!(organization.managers[0].name.as_deref(), Some("Lab Account C"));
    assert_eq!(organization.directs.len(), 1);
    assert_eq!(organization.directs[0].name, None);
    let missing = LokiOrganization::read_answer(br#"{"managers": null}"#).unwrap();
    assert!(missing.managers.is_empty());
    for body in [&br#"{"managers": {}}"#[..], br#"{"directs": 1}"#, b"[]", b"no"] {
        assert_eq!(LokiOrganization::read_answer(body), Err(LokiAnswerError::Malformed));
    }
}

#[test]
fn a_schedule_s_statuses_and_working_hours_are_read() {
    let schedule = LokiSchedule::read_answer(&answer("schedule")).unwrap();
    assert_eq!(schedule.current, Some(FreeBusyStatus::Free));
    assert_eq!(schedule.next, Some(FreeBusyStatus::Free));
    assert_eq!(schedule.until_ms, None, "0001-01-01 is no time");
    assert_eq!(schedule.work_days, Weekday::WORK_WEEK);
    assert_eq!(schedule.work_start, Some(ClockTime { hour: 8, minute: 0 }));
    assert_eq!(schedule.work_end, Some(ClockTime { hour: 17, minute: 0 }));
    assert_eq!(schedule.utc_offset_minutes, Some(180));
    assert_eq!(schedule.zone_name.as_deref(), Some("GMT+03:00"));

    let busy = json!({"nextAvailability": {"utcDateTime": "1970-01-02T00:00:00", "currentStatus": "busy",
        "nextStatus": "Oof"}});
    let schedule = LokiSchedule::read_answer(&serde_json::to_vec(&busy).unwrap()).unwrap();
    assert_eq!(schedule.current, Some(FreeBusyStatus::Busy));
    assert_eq!(schedule.next, Some(FreeBusyStatus::OutOfOffice));
    assert_eq!(schedule.until_ms, Some(86_400_000));
    assert!(schedule.work_days.is_empty());
    assert_eq!(schedule.work_start, None);
    assert_eq!(LokiSchedule::read_answer(b"[]"), Err(LokiAnswerError::Malformed));
}

#[test]
fn a_status_is_graph_s_word_in_any_case_and_another_word_is_kept() {
    for (word, status) in [
        ("Tentative", FreeBusyStatus::Tentative),
        ("workingElsewhere", FreeBusyStatus::WorkingElsewhere),
        ("Unknown", FreeBusyStatus::Unknown),
        ("Napping", FreeBusyStatus::Other("Napping".to_owned())),
    ] {
        assert_eq!(FreeBusyStatus::of(word), status);
    }
}

#[test]
fn automatic_replies_are_off_only_when_disabled() {
    assert_eq!(
        LokiOutOfOffice::read_answer(&answer("oofstatus")),
        Ok(OutOfOfficeState::Disabled)
    );
    assert_eq!(
        LokiOutOfOffice::read_answer(br#"{"outOfOfficeState": "Scheduled"}"#),
        Ok(OutOfOfficeState::Other("Scheduled".to_owned()))
    );
    for body in [&b"{}"[..], br#"{"outOfOfficeState": 1}"#, b"[]"] {
        assert_eq!(LokiOutOfOffice::read_answer(body), Err(LokiAnswerError::Malformed));
    }
}
