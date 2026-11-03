//! Synthetic token-exchange fixtures shared by protocol and isolated diagnostic-logger tests.

#![cfg(test)]

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use teams_auth::pkce::Pkce;
use teams_auth::refresh::refresh_token_key;
use teams_auth::secrets::{MemorySecretStore, Secret, SecretStore};
use teams_auth::token::{
    Authorization, EntraConfig, NATIVE_CLIENT_REDIRECT, Scope, TEAMS_NATIVE_CLIENT_ID, TokenClient,
};
use url::Url;
use wiremock::matchers::{header, method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/auth");
pub(super) const ACCOUNT: &str = "a@lab.example.test";
const TOKEN_SUFFIX: &str = "oauth2/v2.0/token";
pub(super) const OID: &str = "00000000-0000-4000-8000-00000000000a";
pub(super) const TENANT: &str = "00000000-0000-4000-8000-0000000000ff";

/// RFC 7636 Appendix B's octets: a fixed, valid verifier for the code exchange.
pub(super) const OCTETS: [u8; 32] = [
    116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37, 77, 105, 214, 191, 240,
    91, 88, 5, 88, 83, 132, 141, 121,
];

pub(super) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// One recorded exchange.
#[derive(Clone, Debug)]
pub(super) struct Fixture {
    pub(super) path: String,
    pub(super) form: BTreeMap<String, String>,
    pub(super) status: u16,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Value,
}

impl Fixture {
    pub(super) fn load(name: &str) -> TestResult<Self> {
        let file = format!("{FIXTURES}/{name}.json");
        let json: Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
        let request = &json["request"];
        let response = &json["response"];
        assert_eq!(request["method"], "POST", "{file}");
        let url = Url::parse(request["url"].as_str().ok_or("request.url")?)?;
        Ok(Self {
            path: url.path().to_owned(),
            form: strings(&request["form"])?,
            status: u16::try_from(response["status"].as_u64().ok_or("response.status")?)?,
            headers: strings(&response["headers"])?.into_iter().collect(),
            body: response["body"].clone(),
        })
    }

    pub(super) fn form(&self, key: &str) -> TestResult<&str> {
        Ok(self.form.get(key).ok_or_else(|| format!("request.form has no {key}"))?)
    }

    pub(super) fn scope(&self) -> TestResult<Scope> {
        Ok(Scope::exact(self.form("scope")?))
    }

    /// The authority the fixture's endpoint belongs to, moved onto the mock server.
    pub(super) fn config(&self, server: &MockServer) -> TestResult<EntraConfig> {
        let prefix = self.path.strip_suffix(TOKEN_SUFFIX).ok_or("not a v2 token endpoint")?;
        Ok(EntraConfig::teams_native()
            .with_authority(format!("{}{prefix}", server.uri()))
            .with_client_id(self.form("client_id")?)
            .with_redirect_uri(
                self.form
                    .get("redirect_uri")
                    .map_or(NATIVE_CLIENT_REDIRECT, String::as_str),
            ))
    }

    pub(super) fn client(&self, server: &MockServer) -> TestResult<TokenClient> {
        Ok(TokenClient::new(self.config(server)?)?)
    }

    pub(super) fn response(&self) -> TestResult<ResponseTemplate> {
        let body = serde_json::to_vec(&self.body)?;
        Ok(self
            .headers
            .iter()
            .fold(ResponseTemplate::new(self.status), |template, (name, value)| {
                template.insert_header(name.as_str(), value.as_str())
            })
            .set_body_raw(body, self.headers_value("content-type").unwrap_or("application/json")))
    }

    pub(super) fn headers_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Mounts the exchange: the fixture's response, only for a POST to its path whose form equals `form`.
    pub(super) async fn mount(&self, server: &MockServer, form: BTreeMap<String, String>, calls: u64) -> TestResult {
        Mock::given(method("POST"))
            .and(path(self.path.as_str()))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(FormIs(form))
            .respond_with(self.response()?)
            .expect(calls)
            .mount(server)
            .await;
        Ok(())
    }

    /// Mounts the exchange expecting exactly the fixture's form.
    pub(super) async fn mount_exact(&self, server: &MockServer) -> TestResult {
        self.mount(server, self.form.clone(), 1).await
    }

    /// Mounts a code exchange: the fixture's form, with the verifier the test actually sent.
    pub(super) async fn mount_code(&self, server: &MockServer, pkce: &Pkce) -> TestResult {
        let mut form = self.form.clone();
        form.insert("code_verifier".into(), pkce.verifier().expose().to_owned());
        self.mount(server, form, 1).await
    }

    /// The fixture's secret values: none of them may show up in an error or in `Debug`. An ID token's payload part
    /// counts on its own too.
    pub(super) fn secrets(&self) -> Vec<String> {
        let sent = ["code", "code_verifier", "refresh_token"]
            .into_iter()
            .filter_map(|key| self.form.get(key).cloned());
        let received = ["access_token", "refresh_token", "id_token"]
            .into_iter()
            .filter_map(|key| self.body[key].as_str().map(str::to_owned));
        let id_payload = self.body["id_token"]
            .as_str()
            .and_then(|token| token.split('.').nth(1))
            .filter(|payload| !payload.is_empty())
            .map(str::to_owned);
        sent.chain(received).chain(id_payload).collect()
    }

    /// The code fixture answering with an unsigned ID token carrying `claims`.
    pub(super) fn with_id_token(mut self, claims: &Value) -> TestResult<Self> {
        let body = self.body.as_object_mut().ok_or("response.body is not an object")?;
        body.insert("id_token".into(), Value::String(id_token(claims)?));
        Ok(self)
    }
}

/// An unsigned JWT with `claims`, built at run time: the fixture guard keeps JWTs out of the repo.
fn id_token(claims: &Value) -> TestResult<String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"RS256","kid":"test"}"#);
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?);
    Ok(format!("{header}.{payload}.c2lnbmF0dXJlLW5vdC1jaGVja2Vk"))
}

/// The claims of `username` signed in to the native client.
pub(super) fn claims_of(username: &str) -> Value {
    json!({
        "aud": TEAMS_NATIVE_CLIENT_ID,
        "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"),
        "oid": OID,
        "tid": TENANT,
        "preferred_username": username,
        "name": "Lab account",
    })
}

/// A JSON object of strings (a form or headers); a missing object is empty.
fn strings(value: &Value) -> TestResult<BTreeMap<String, String>> {
    let Some(map) = value.as_object() else {
        return Ok(BTreeMap::new());
    };
    map.iter()
        .map(|(key, value)| {
            let value = value.as_str().ok_or_else(|| format!("{key} is not a string"))?;
            Ok((key.clone(), value.to_owned()))
        })
        .collect()
}

/// Matches a request whose `application/x-www-form-urlencoded` body has exactly these fields.
struct FormIs(BTreeMap<String, String>);

impl Match for FormIs {
    fn matches(&self, request: &Request) -> bool {
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(&request.body).into_owned().collect();
        let fields: BTreeMap<String, String> = pairs.iter().cloned().collect();
        fields.len() == pairs.len() && fields == self.0
    }
}

pub(super) async fn store_with(refresh_token: &str) -> TestResult<MemorySecretStore> {
    let store = MemorySecretStore::new();
    store
        .put(&refresh_token_key(ACCOUNT), &Secret::new(refresh_token))
        .await?;
    Ok(store)
}

pub(super) fn authorization(config: &EntraConfig, fixture: &Fixture) -> TestResult<Authorization> {
    Ok(Authorization::with_pkce(
        config,
        ACCOUNT,
        fixture.scope()?,
        Pkce::from_bytes(&OCTETS),
        "state-1".into(),
    ))
}
