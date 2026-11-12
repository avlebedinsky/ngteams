//! Loki, the persona card service (`docs/protocol/loki.md`), made up from the synthetic
//! `tests/fixtures/loki/persona-card.json`: its configuration on the global host's path (naming the regional base
//! [`LOKI_REGION`], which the transport's mock origin sends here too) and, for each person a test gives a card, their
//! five reads on the regional base's paths, asked by their object id. Only a request with Loki's client type
//! (`X-ClientType: Teams`) and a bearer token is answered; anything else falls through to the world, which
//! doesn't know it.

use serde_json::{Value, json};
use wiremock::matchers::{header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The regional base the mock's configuration names.
pub const LOKI_REGION: &str = "https://nam.loki.delve.office.com/";

/// The fixture the answers are shaped like.
const FIXTURE: &str = include_str!("../../../tests/fixtures/loki/persona-card.json");

/// A person whose card the mock serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LokiCard<'a> {
    /// Their MRI, `8:orgid:<object id>`.
    pub mri: &'a str,
    /// Their display name.
    pub name: &'a str,
    /// Their address (mail, IM and sign-in name alike).
    pub email: &'a str,
}

impl LokiCard<'_> {
    fn object_id(&self) -> &str {
        self.mri.strip_prefix("8:orgid:").unwrap_or(self.mri)
    }

    /// A working-with entry naming them.
    fn entry(&self) -> Value {
        json!({
            "aadObjectId": self.object_id(),
            "fullName": self.name,
            "email": self.email,
            "userPrincipalName": self.email,
            "profilePictureUrl": format!("{LOKI_REGION}api/v2/personaphoto?made-up"),
            "permanentEntryId": "made-up-entry-id",
        })
    }
}

/// The fixture, its placeholders left in.
fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap_or(Value::Null)
}

/// A mock of `path_is` taking only a request with Loki's client type and a token.
fn loki(path_is: &str) -> wiremock::MockBuilder {
    Mock::given(method("GET"))
        .and(path(path_is))
        .and(header("x-clienttype", "Teams"))
        .and(header_exists("authorization"))
}

/// Mounts the configuration and every card's reads (the people it works with: every other card's).
pub(crate) async fn mount(server: &MockServer, cards: &[LokiCard<'_>]) {
    let fixture = fixture();
    let mut configuration = fixture["configuration"]["answer"].clone();
    configuration["LokiUrl"] = json!(LOKI_REGION);
    loki("/api/v1/livepersonacard/configuration")
        .respond_with(ResponseTemplate::new(200).set_body_json(configuration))
        .with_priority(1)
        .mount(server)
        .await;
    for card in cards {
        for (read, answer) in answers(&fixture, card, cards) {
            loki(fixture[read]["path"].as_str().unwrap_or_default())
                .and(query_param("aadObjectId", card.object_id()))
                .respond_with(ResponseTemplate::new(200).set_body_json(answer))
                .with_priority(1)
                .mount(server)
                .await;
        }
    }
}

/// `card`'s five answers, by the fixture's read names.
fn answers(fixture: &Value, card: &LokiCard<'_>, cards: &[LokiCard<'_>]) -> [(&'static str, Value); 5] {
    let mut person = fixture["person"]["answer"].clone();
    person["person"]["names"][0]["value"] = json!({"displayName": card.name, "givenName": card.name, "surname": ""});
    person["person"]["emailAddresses"][0]["value"]["name"] = json!(card.email);
    person["person"]["emailAddresses"][0]["value"]["address"] = json!(card.email);
    person["person"]["imAddresses"][0]["value"] =
        json!({"displayText": card.email, "url": format!("sip:{}", card.email)});
    person["person"]["userPrincipalName"] = json!(card.email);
    person["person"]["aadObjectId"] = json!(card.object_id());
    let others: Vec<Value> = cards
        .iter()
        .filter(|other| other.mri != card.mri)
        .map(LokiCard::entry)
        .collect();
    let mut oof = fixture["oofstatus"]["answer"].clone();
    oof["emailAddress"] = json!(card.email);
    [
        ("person", person),
        ("organization", fixture["organization"]["answer"].clone()),
        ("workingwith", json!({ "value": others })),
        ("schedule", fixture["schedule"]["answer"].clone()),
        ("oofstatus", oof),
    ]
}
