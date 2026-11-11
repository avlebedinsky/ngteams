//! The mock's Loki (`ChatServiceMock::serve_loki`) through the native transport's mock origin: the configuration names
//! the regional base, each card's reads answer in the fixture's shapes, the people a card works with are the other
//! cards', and a person without a card isn't served (the world doesn't know the read). Every id, name and address here
//! is made up.

use std::future::{self, Future};
use std::sync::Arc;

use teams_auth::secrets::Secret;
use teams_auth::token::TokenError;
use teams_mock::{CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, LOKI_REGION, LokiCard};
use teams_proto::{
    Client, Credentials, FreeBusyStatus, HttpError, HttpTransport, LokiConfiguration, LokiOrganization,
    LokiOutOfOffice, LokiPerson, LokiSchedule, LokiWorkingWith, OutOfOfficeState,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Token;

impl Credentials for Token {
    fn bearer(&self, _: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new("made-up-loki-token".to_owned())))
    }

    fn bearer_rejected(&self, audience: &str, _: &Secret) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        self.bearer(audience)
    }
}

#[tokio::test]
async fn the_mock_serves_each_card_and_the_people_it_works_with() -> TestResult {
    let mock = ChatServiceMock::start().await?;
    let cards = [
        LokiCard {
            mri: LAB_B,
            name: "Lab Bee",
            email: "lab-b@lab.example.test",
        },
        LokiCard {
            mri: LAB_C,
            name: "Lab Sea",
            email: "lab-c@lab.example.test",
        },
    ];
    mock.serve_loki(&cards).await;
    let transport = HttpTransport::with_mock_origin(Arc::new(Token), CHAT_BASE, &mock.origin())?.with_loki();
    let client = Client::new(transport);

    let configuration = client.read(&LokiConfiguration).await?;
    let region = LokiConfiguration::read_answer(configuration.body()).ok_or("no region")?;
    assert_eq!(region, LOKI_REGION);
    client.transport().set_loki_region(&region)?;

    let person = LokiPerson::of(LAB_B).ok_or("no read")?;
    let details = person.read_answer(client.read(&person).await?.body())?;
    assert_eq!(details.display_name.as_deref(), Some("Lab Bee"));
    assert_eq!(details.emails, ["lab-b@lab.example.test"]);
    let working = client.read(&LokiWorkingWith::of(LAB_B).ok_or("no read")?).await?;
    let people = LokiWorkingWith::read_answer(working.body())?;
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].mri(), LAB_C);
    let schedule = client.read(&LokiSchedule::of(LAB_C).ok_or("no read")?).await?;
    assert_eq!(
        LokiSchedule::read_answer(schedule.body())?.current,
        Some(FreeBusyStatus::Free)
    );
    let organization = client.read(&LokiOrganization::of(LAB_C).ok_or("no read")?).await?;
    assert!(LokiOrganization::read_answer(organization.body())?.managers.is_empty());
    let oof = client.read(&LokiOutOfOffice::of(LAB_C).ok_or("no read")?).await?;
    assert_eq!(LokiOutOfOffice::read_answer(oof.body())?, OutOfOfficeState::Disabled);

    let unserved = client.read(&LokiPerson::of(LAB_A).ok_or("no read")?).await;
    assert!(matches!(unserved, Err(HttpError::Status(answer)) if !answer.is_success()));
    Ok(())
}
