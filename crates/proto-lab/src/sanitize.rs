//! The sanitizer: what a raw capture ([`crate::capture`]) has to go through before any of it may enter the repository
//! as a fixture (`proto-lab export`, which also runs the fixture guard on the result).
//!
//! It works on the capture's JSON events, with one [`Sanitizer`] per capture, so every placeholder is stable within the
//! capture: the same email, MRI, GUID or name gets the same placeholder in every event.
//! - **Secrets are removed**: JWTs, skypetokens, `Bearer`/`Basic` credentials, cookie values, and any value under a
//!   secret-looking key (`*token*`, `*secret*`, `*password*`, `*cookie*`, `*credential*`, `authorization`, `sig`, the
//!   ICE `pwd`/`ufrag`, the TURN `username`, …), in JSON, in form and query strings, and in text. Each becomes
//!   `<token:N>`; a scheme is kept (`Bearer <token:1>`, `skypetoken=<token:2>`), and so is a cookie's name. A long value
//!   under a generic key (`code`, `key`, `state`) is a secret too, as is any long high-entropy run of letters and
//!   digits nothing else recognized. Raw header text (`headersText`) is dropped, and binary content (base64 bodies
//!   that aren't UTF-8 text, binary WebSocket frames) becomes `<binary:LEN>`.
//! - **Identifiers become placeholders**: emails `<user:N>`, MRIs `<mri:N>` (`8:orgid:…`, `8:live:…`, `28:…`, phone
//!   `4:…`, `tel:`), conversation ids `19:<thread:N>@thread.v2`, GUIDs `<guid:N>`, tenant domains
//!   (`*.onmicrosoft.com`, `*.sharepoint.com`, the lab's own) `<domain:N>`, IP addresses other than loopback `<ip:N>`,
//!   names `<name:N>` and other personal fields (titles, topics, phones, departments) `<pii:N>`. Percent-encoded forms
//!   (`8%3Aorgid%3A…`, `someone%40…`) are recognized too. An MRI that holds a GUID shares the GUID's number, so
//!   `<mri:3>` and `<guid:3>` are the same object. The lab's own accounts get their alias instead of a number
//!   (`<user:a>`, `<mri:b>`, `<guid:c>`), and an approved service participant its name (`<mri:echo>`).
//! - **Names** are learned from name fields (`displayName`, `givenName`, `surname`, `imdisplayname`, …) across the whole
//!   capture first; then every occurrence of a learned name, or of a word of it with 3 letters or more, in any case and
//!   as a whole word (`_` separates words too), is replaced in every text (a message body that says "Hi Jane", a
//!   `OneDrive` path with `jane_doe`). A name only ever seen under a generic key such as `name` isn't learned.
//!
//! JSON inside a string (a request or response body, a WebSocket frame, also after a text prefix such as Trouter's
//! `3:::`) is sanitized as JSON and written back compactly, so keys are seen wherever they are; object keys go through
//! the identifier rules as well (maps keyed by MRI).
//!
//! Sanitizing is idempotent: placeholders are never matched again, so a sanitized capture comes out unchanged.
//! Whatever this misses, the fixture guard is the last check before a fixture is written.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use base64::Engine as _;
use regex::{Captures, Regex};
use serde_json::{Map, Value};

use crate::config::{Alias, LabConfig};

/// Every kind of placeholder the sanitizer writes.
const KINDS: &str = "token|user|mri|guid|thread|domain|ip|name|pii|binary|omitted";

/// Separators as captures write them: plain, percent-encoded, encoded twice.
const COLON: &str = "(?::|%3[aA]|%253[aA])";
const AT: &str = "(?:@|%40|%2540)";
const GUID: &str = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";

/// Key fragments whose values are always secrets (keys are compared lowercased).
const SECRET_PARTS: [&str; 21] = [
    "token",
    "secret",
    "password",
    "passwd",
    "passcode",
    "cookie",
    "credential",
    "signature",
    "authorization",
    "authentication",
    "apikey",
    "api_key",
    "api-key",
    "accesskey",
    "privatekey",
    "private_key",
    "client_info",
    "assertion",
    "canary",
    "pwd",
    "ufrag",
];

/// Whole keys whose values are always secrets.
const SECRET_KEYS: [&str; 8] = ["sig", "username", "ctx", "sctx", "esctx", "sft", "nonce", "verifier"];

/// Whole keys whose values are secrets when long ([`WEAK_MIN`]): an OAuth `code` is, an error `code` isn't.
const WEAK_KEYS: [&str; 4] = ["code", "key", "state", "session_state"];

/// How long a value under a [`WEAK_KEYS`] key has to be to count as a secret.
const WEAK_MIN: usize = 40;

/// Words that a credential may start with, and that are no secret on their own (`"token_type": "Bearer"`).
const SCHEMES: [&str; 4] = ["bearer", "basic", "negotiate", "pop"];

/// Key fragments (lowercased, without `_` and `-`) of person-name fields: their values are learned as names.
const NAME_PARTS: [&str; 10] = [
    "displayname",
    "givenname",
    "surname",
    "firstname",
    "lastname",
    "fullname",
    "middlename",
    "familyname",
    "nickname",
    "friendlyname",
];

/// Key fragments (lowercased, without `_` and `-`) of other personal fields.
const PII_PARTS: [&str; 8] = [
    "jobtitle",
    "department",
    "officelocation",
    "companyname",
    "tenantname",
    "phone",
    "streetaddress",
    "postalcode",
];

/// Whole keys (lowercased, without `_` and `-`) of other personal fields.
const PII_KEYS: [&str; 3] = ["title", "topic", "city"];

/// Raw header text, which repeats the structured headers in a form no key rule reaches.
const RAW_KEYS: [&str; 2] = ["headerstext", "requestheaderstext"];

/// Cookie attributes, whose values are no secret.
const COOKIE_ATTRIBUTES: [&str; 9] = [
    "path",
    "domain",
    "expires",
    "max-age",
    "samesite",
    "secure",
    "httponly",
    "priority",
    "partitioned",
];

/// How long an unrecognized run of letters and digits has to be before it may count as a token.
const OPAQUE_MIN: usize = 32;
/// The bits of entropy per character above which such a run counts as a token (random base64 has about 4.5 at 32
/// characters, English camel case well under 4).
const OPAQUE_ENTROPY: f64 = 4.0;

/// A lab account the sanitizer knows by name: its UPN and MRI get the alias as their placeholder label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownAccount {
    pub alias: Alias,
    pub upn: String,
    pub mri: Option<String>,
}

/// An approved service participant: its MRI gets its name as the placeholder label (`<mri:echo>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownService {
    pub name: String,
    pub mri: String,
}

/// What the sanitizer knows of the lab up front. Without it every identity gets a number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identities {
    pub accounts: Vec<KnownAccount>,
    pub services: Vec<KnownService>,
    /// The lab tenant's own domain (the UPNs' domain): `<domain:tenant>`.
    pub tenant_domain: Option<String>,
}

impl Identities {
    /// The lab's accounts, approved services and tenant domain from `lab.toml`.
    #[must_use]
    pub fn from_lab(config: &LabConfig) -> Self {
        Self {
            accounts: config
                .accounts()
                .iter()
                .map(|account| KnownAccount {
                    alias: account.alias,
                    upn: account.upn.clone(),
                    mri: account.mri.clone(),
                })
                .collect(),
            services: config
                .service_participants()
                .iter()
                .filter_map(|service| {
                    let mri = service.mri.clone()?;
                    Some(KnownService {
                        name: service.name.clone(),
                        mri,
                    })
                })
                .collect(),
            tenant_domain: Some(config.tenant_domain().to_owned()).filter(|domain| !domain.is_empty()),
        }
    }
}

/// One kind of identity: each distinct value gets a label, a seeded one or the next number.
#[derive(Default)]
struct Table {
    labels: HashMap<String, String>,
    next: usize,
}

impl Table {
    fn label(&mut self, key: &str) -> String {
        if let Some(label) = self.labels.get(key) {
            return label.clone();
        }
        self.next += 1;
        let label = self.next.to_string();
        self.labels.insert(key.to_owned(), label.clone());
        label
    }

    fn seed(&mut self, key: String, label: &str) {
        self.labels.entry(key).or_insert_with(|| label.to_owned());
    }
}

/// The compiled patterns.
struct Rules {
    placeholder: Regex,
    whole_placeholder: Regex,
    scheme: Regex,
    bearer: Regex,
    secret_value: Regex,
    jwt: Regex,
    mri: Regex,
    guid: Regex,
    thread: Regex,
    email: Regex,
    tenant: Option<Regex>,
    hosted_domain: Regex,
    ipv4_run: Regex,
    ipv6_run: Regex,
    ufrag: Regex,
    opaque_run: Regex,
}

impl Rules {
    fn new(tenant_domain: Option<&str>) -> Result<Self, regex::Error> {
        let secret_parts = SECRET_PARTS.map(regex::escape).join("|");
        let whole_keys = SECRET_KEYS.iter().chain(&WEAK_KEYS).map(|key| regex::escape(key));
        let whole_keys = whole_keys.collect::<Vec<_>>().join("|");
        Ok(Self {
            placeholder: Regex::new(&format!("<(?:{KINDS}):[A-Za-z0-9]+>"))?,
            whole_placeholder: Regex::new(&format!("^<(?:{KINDS}):[A-Za-z0-9]+>$"))?,
            scheme: Regex::new(r"(?is)^(bearer\s+|basic\s+|negotiate\s+|pop\s+|skypetoken=|token=)(.+)$")?,
            bearer: Regex::new(r"(?i)\b(bearer|basic|negotiate)(\s+)([A-Za-z0-9\-._~+/]{16,}=*)")?,
            secret_value: Regex::new(&format!(
                r#"(?i)(^|[^A-Za-z0-9_\-])([A-Za-z0-9_\-]*(?:{secret_parts})[A-Za-z0-9_\-]*|(?:{whole_keys}))(\\?["']?\s*[:=]\s*\\?["']?)([^"'&\s;,}}\])\\<>]+)"#
            ))?,
            jwt: Regex::new(r"eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]*")?,
            mri: Regex::new(&format!(
                r"(?i)(^|[^0-9])(8{COLON}(?:orgid|live|teamsvisitor|acs|gcch|dod|sfb|guest|spaces){COLON}[A-Za-z0-9._\-+]+|28{COLON}(?:[a-z]+{COLON})?{GUID}|4{COLON}\+?[0-9]{{6,}}|tel:\+?[0-9][0-9\-]{{5,}})"
            ))?,
            guid: Regex::new(GUID)?,
            thread: Regex::new(&format!(
                r"(^|[^0-9])(19{COLON})([A-Za-z0-9_\-.=]+)({AT}[A-Za-z0-9][A-Za-z0-9.]*)"
            ))?,
            email: Regex::new(&format!(
                r"([A-Za-z0-9._%+\-]+){AT}([A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{{2,}})\b"
            ))?,
            tenant: tenant_domain
                .map(|domain| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(domain))))
                .transpose()?,
            hosted_domain: Regex::new(
                r"(?i)\b([a-z0-9][a-z0-9\-]*?)(-my|-admin)?(\.onmicrosoft\.com|\.sharepoint\.com)\b",
            )?,
            ipv4_run: Regex::new(r"[0-9.]+")?,
            ipv6_run: Regex::new(r"[0-9A-Fa-f:.]+")?,
            ufrag: Regex::new(r"(?i)(\bufrag\s+)([^\s<>]+)")?,
            opaque_run: Regex::new(r"[A-Za-z0-9_\-]+")?,
        })
    }
}

/// The labels handed out so far, and the names learned.
#[derive(Default)]
struct State {
    tokens: Table,
    users: Table,
    /// GUIDs and MRIs share one table: an MRI holding a GUID takes that GUID's label.
    ids: Table,
    threads: Table,
    domains: Table,
    ips: Table,
    names: Table,
    pii: Table,
    /// Every text that stands for a learned name (the name, its encoded forms, its longer words), lowercased, with the
    /// name's label.
    variants: HashMap<String, String>,
    names_regex: Option<Regex>,
    names_changed: bool,
}

impl State {
    fn token(&mut self, value: &str) -> String {
        format!("<token:{}>", self.tokens.label(value))
    }

    fn learn_name(&mut self, name: &str) {
        let name = name.trim();
        if name.chars().count() < 2 || self.names.labels.contains_key(name) {
            return;
        }
        let label = self.names.label(name);
        let mut variants = vec![name.to_owned(), name.replace(' ', "%20"), name.replace(' ', "+")];
        variants.extend(
            name.split_whitespace()
                .filter(|word| word.chars().count() >= 3)
                .map(str::to_owned),
        );
        for variant in variants {
            self.variants
                .entry(variant.to_lowercase())
                .or_insert_with(|| label.clone());
        }
        self.names_changed = true;
    }

    fn rebuild_names(&mut self) -> Result<(), regex::Error> {
        if !self.names_changed {
            return Ok(());
        }
        let mut variants: Vec<&String> = self.variants.keys().collect();
        // the longest first, so a full name wins over its words
        variants.sort_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
        let alternation: Vec<String> = variants.iter().map(|variant| regex::escape(variant)).collect();
        // case-insensitive, and bounded by hand ([`names`]): `_` separates words too (`jane_doe` in a OneDrive URL)
        self.names_regex = Some(Regex::new(&format!("(?i)(?:{})", alternation.join("|")))?);
        self.names_changed = false;
        Ok(())
    }
}

/// How a value is treated, from the key it sits under.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Plain,
    /// Under a secret key: every string in it is a credential.
    Secret,
    /// A `Cookie` or `Set-Cookie` header.
    Cookie,
    /// Under a [`WEAK_KEYS`] key.
    Weak,
    Name,
    Pii,
}

/// Turns raw capture events into sanitized ones; see the module docs. Use one per capture: [`Self::learn`] every
/// event, then [`Self::sanitize`] each ([`sanitize_events`] does both).
pub struct Sanitizer {
    rules: Rules,
    state: State,
}

impl Sanitizer {
    /// A sanitizer that knows the lab's own identities.
    ///
    /// # Errors
    /// A pattern doesn't compile; that is a bug here, caught by the tests.
    pub fn new(identities: &Identities) -> Result<Self, regex::Error> {
        let rules = Rules::new(identities.tenant_domain.as_deref())?;
        let mut state = State::default();
        for account in &identities.accounts {
            let alias = account.alias.as_str();
            state.users.seed(account.upn.to_lowercase(), alias);
            if let Some(mri) = &account.mri {
                seed_mri(&rules, &mut state, mri, alias);
            }
        }
        for service in &identities.services {
            let label: String = service
                .name
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_lowercase();
            let clashes = label.is_empty()
                || label.bytes().all(|byte| byte.is_ascii_digit())
                || Alias::ALL.iter().any(|alias| alias.as_str() == label);
            if !clashes {
                seed_mri(&rules, &mut state, &service.mri, &label);
            }
        }
        if let Some(domain) = &identities.tenant_domain {
            state.domains.seed(domain.to_lowercase(), "tenant");
        }
        Ok(Self { rules, state })
    }

    /// Learns the names in one event (see the module docs), so that every event sanitized after replaces them.
    pub fn learn(&mut self, event: &Value) {
        match event {
            Value::Object(map) => {
                for (key, value) in map {
                    match (key_mode(key), value) {
                        (Mode::Name, Value::String(name)) if !self.is_placeholder(name) => self.state.learn_name(name),
                        _ => self.learn(value),
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| self.learn(item)),
            Value::String(text) => {
                if let Some(json) = parse_container(text) {
                    self.learn(&json);
                } else if let Some((_, json)) = json_suffix(text) {
                    self.learn(&json);
                }
            }
            _ => {}
        }
    }

    /// One event, sanitized.
    ///
    /// # Errors
    /// The learned names don't compile into a pattern (too many of them).
    pub fn sanitize(&mut self, event: &Value) -> Result<Value, regex::Error> {
        self.state.rebuild_names()?;
        Ok(self.value(event, Mode::Plain))
    }

    fn is_placeholder(&self, text: &str) -> bool {
        self.rules.whole_placeholder.is_match(text)
    }

    fn value(&mut self, value: &Value, mode: Mode) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(text, mode)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.value(item, mode)).collect()),
            Value::Object(map) => Value::Object(self.object(map, mode)),
            other => other.clone(),
        }
    }

    fn object(&mut self, map: &Map<String, Value>, mode: Mode) -> Map<String, Value> {
        let base64 = map.get("base64Encoded") == Some(&Value::Bool(true));
        let binary_frame = map.get("opcode").and_then(Value::as_u64) == Some(2);
        let mut out = Map::new();
        for (key, value) in map {
            let lower = key.to_lowercase();
            let child = match mode {
                Mode::Secret | Mode::Cookie => Mode::Secret,
                _ => key_mode(key),
            };
            let sanitized = match value {
                Value::String(text) if base64 && (key == "body" || key == "postData") => self.base64_body(text, child),
                Value::String(text) if (binary_frame && key == "payloadData") || key == "bytes" => {
                    if self.is_placeholder(text) {
                        Value::String(text.clone())
                    } else {
                        binary(decoded_len(text))
                    }
                }
                Value::String(_) if RAW_KEYS.contains(&lower.as_str()) => Value::String("<omitted:raw>".to_owned()),
                other => self.value(other, child),
            };
            out.insert(self.key(key), sanitized);
        }
        if base64 {
            out.insert("base64Encoded".to_owned(), Value::Bool(false));
        }
        out
    }

    /// A base64 body: its text, sanitized, if it is UTF-8 text, else `<binary:LEN>`.
    fn base64_body(&mut self, encoded: &str, mode: Mode) -> Value {
        let decoded = base64::engine::general_purpose::STANDARD.decode(encoded.trim());
        match decoded.map(String::from_utf8) {
            Ok(Ok(text)) => Value::String(self.string(&text, mode)),
            Ok(Err(error)) => binary(error.into_bytes().len()),
            Err(_) => binary(encoded.len()),
        }
    }

    fn key(&mut self, key: &str) -> String {
        let (rules, state) = (&self.rules, &mut self.state);
        KEY_RULES.iter().fold(key.to_owned(), |text, rule| {
            outside_placeholders(rules, &text, |gap| rule(rules, state, gap))
        })
    }

    fn string(&mut self, text: &str, mode: Mode) -> String {
        if self.is_placeholder(text) {
            return text.to_owned();
        }
        match mode {
            Mode::Secret => self.credential(text),
            Mode::Cookie => self.cookie(text),
            Mode::Weak if text.len() >= WEAK_MIN => self.state.token(text),
            Mode::Name if !text.trim().is_empty() => format!("<name:{}>", self.state.names.label(text.trim())),
            Mode::Pii if !text.trim().is_empty() => format!("<pii:{}>", self.state.pii.label(text.trim())),
            _ => self.text(text),
        }
    }

    /// A credential: its scheme kept, the rest a token.
    fn credential(&mut self, text: &str) -> String {
        if text.is_empty() || SCHEMES.contains(&text.trim().to_lowercase().as_str()) {
            return text.to_owned();
        }
        let Some(captures) = self.rules.scheme.captures(text) else {
            return self.state.token(text);
        };
        let (scheme, rest) = (&captures[1], &captures[2]);
        if self.is_placeholder(rest) {
            return text.to_owned();
        }
        format!("{scheme}{}", self.state.token(rest))
    }

    /// A `Cookie` or `Set-Cookie` header: each cookie's value a token, attributes kept.
    fn cookie(&mut self, text: &str) -> String {
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| {
                let parts: Vec<String> = line.split(';').map(|part| self.cookie_part(part)).collect();
                parts.join(";")
            })
            .collect();
        lines.join("\n")
    }

    fn cookie_part(&mut self, part: &str) -> String {
        let Some((name, value)) = part.split_once('=') else {
            return part.to_owned();
        };
        let name = self.key(name);
        if COOKIE_ATTRIBUTES.contains(&name.trim().to_lowercase().as_str()) {
            return format!("{name}={}", self.text(value));
        }
        if value.is_empty() || self.is_placeholder(value) {
            return format!("{name}={value}");
        }
        format!("{name}={}", self.state.token(value))
    }

    /// Free text: JSON in it as JSON, the rest through every text rule.
    fn text(&mut self, text: &str) -> String {
        if let Some(json) = parse_container(text) {
            return self.value(&json, Mode::Plain).to_string();
        }
        if let Some((prefix, json)) = json_suffix(text) {
            let prefix = self.text_rules(prefix);
            return format!("{prefix}{}", self.value(&json, Mode::Plain));
        }
        self.text_rules(text)
    }

    fn text_rules(&mut self, text: &str) -> String {
        let (rules, state) = (&self.rules, &mut self.state);
        TEXT_RULES.iter().fold(text.to_owned(), |text, rule| {
            outside_placeholders(rules, &text, |gap| rule(rules, state, gap))
        })
    }
}

/// Sanitizes a whole capture with one [`Sanitizer`]: learns every event, then sanitizes each.
///
/// # Errors
/// As [`Sanitizer::new`] and [`Sanitizer::sanitize`].
pub fn sanitize_events(events: &[Value], identities: &Identities) -> Result<Vec<Value>, regex::Error> {
    let mut sanitizer = Sanitizer::new(identities)?;
    for event in events {
        sanitizer.learn(event);
    }
    events.iter().map(|event| sanitizer.sanitize(event)).collect()
}

fn seed_mri(rules: &Rules, state: &mut State, mri: &str, label: &str) {
    let normalized = normalize_mri(mri);
    match rules.guid.find(&normalized) {
        Some(guid) => state.ids.seed(guid.as_str().to_lowercase(), label),
        None => state.ids.seed(format!("mri:{normalized}"), label),
    }
}

fn normalize_mri(mri: &str) -> String {
    mri.to_lowercase().replace("%253a", ":").replace("%3a", ":")
}

fn binary(len: usize) -> Value {
    Value::String(format!("<binary:{len}>"))
}

/// The byte length of base64 text, or its own length when it isn't base64.
fn decoded_len(text: &str) -> usize {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_or(text.len(), |bytes| bytes.len())
}

/// The mode for a key's value.
fn key_mode(key: &str) -> Mode {
    let lower = key.to_lowercase();
    if lower == "cookie" || lower == "set-cookie" {
        return Mode::Cookie;
    }
    if SECRET_PARTS.iter().any(|part| lower.contains(part)) || SECRET_KEYS.contains(&lower.as_str()) {
        return Mode::Secret;
    }
    if WEAK_KEYS.contains(&lower.as_str()) {
        return Mode::Weak;
    }
    let bare: String = lower.chars().filter(|&char| char != '_' && char != '-').collect();
    if NAME_PARTS.iter().any(|part| bare.contains(part)) {
        return Mode::Name;
    }
    if PII_PARTS.iter().any(|part| bare.contains(part)) || PII_KEYS.contains(&bare.as_str()) {
        return Mode::Pii;
    }
    Mode::Plain
}

/// The JSON object or array a whole string holds.
fn parse_container(text: &str) -> Option<Value> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with(['{', '[']) {
        return None;
    }
    serde_json::from_str::<Value>(text)
        .ok()
        .filter(|value| value.is_object() || value.is_array())
}

/// A text prefix and the JSON object after it (`3:::{…}`).
fn json_suffix(text: &str) -> Option<(&str, Value)> {
    let start = text.find('{').filter(|&start| start > 0)?;
    let (prefix, rest) = text.split_at(start);
    let value: Value = serde_json::from_str(rest).ok()?;
    value.is_object().then_some((prefix, value))
}

type Rule = fn(&Rules, &mut State, &str) -> String;

/// The rules for free text, in order: secrets first (so a token is never taken apart), then identifiers, then names,
/// then whatever still looks like a token.
const TEXT_RULES: [Rule; 13] = [
    bearer,
    secret_values,
    jwts,
    mris,
    guids,
    threads,
    emails,
    domains,
    ipv4s,
    ipv6s,
    ufrags,
    names,
    opaque,
];

/// The rules for object keys: identifiers only (a key is never a secret's name to redact, and never a name).
const KEY_RULES: [Rule; 9] = [jwts, mris, guids, threads, emails, domains, ipv4s, ipv6s, opaque];

/// Applies `rule` to the text between placeholders, so no rule ever matches a placeholder (or part of one).
fn outside_placeholders(rules: &Rules, text: &str, mut rule: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for placeholder in rules.placeholder.find_iter(text) {
        out.push_str(&rule(&text[last..placeholder.start()]));
        out.push_str(placeholder.as_str());
        last = placeholder.end();
    }
    out.push_str(&rule(&text[last..]));
    out
}

/// Replaces each match `replace` gives a replacement for; a match it gives `None` stays as it is.
fn replace_each(regex: &Regex, text: &str, mut replace: impl FnMut(&Captures<'_>) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for captures in regex.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        if let Some(replacement) = replace(&captures) {
            out.push_str(&text[last..whole.start()]);
            out.push_str(&replacement);
            last = whole.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

fn bearer(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.bearer, text, |captures| {
        Some(format!("{}{}{}", &captures[1], &captures[2], state.token(&captures[3])))
    })
}

fn secret_values(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.secret_value, text, |captures| {
        let (key, value) = (captures[2].to_lowercase(), &captures[4]);
        let weak = WEAK_KEYS.contains(&key.as_str());
        if SCHEMES.contains(&value.to_lowercase().as_str()) || (weak && value.len() < WEAK_MIN) {
            return None;
        }
        Some(format!(
            "{}{}{}{}",
            &captures[1],
            &captures[2],
            &captures[3],
            state.token(value)
        ))
    })
}

fn jwts(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.jwt, text, |captures| Some(state.token(&captures[0])))
}

fn mris(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.mri, text, |captures| {
        let normalized = normalize_mri(&captures[2]);
        let label = match rules.guid.find(&normalized) {
            Some(guid) => state.ids.label(&guid.as_str().to_lowercase()),
            None => state.ids.label(&format!("mri:{normalized}")),
        };
        Some(format!("{}<mri:{label}>", &captures[1]))
    })
}

fn guids(rules: &Rules, state: &mut State, text: &str) -> String {
    let bytes = text.as_bytes();
    let hex_at = |index: Option<usize>| {
        index
            .and_then(|index| bytes.get(index))
            .is_some_and(u8::is_ascii_hexdigit)
    };
    replace_each(&rules.guid, text, |captures| {
        let whole = captures.get(0)?;
        // a GUID-shaped piece of a longer hex run isn't one
        if hex_at(whole.start().checked_sub(1)) || hex_at(Some(whole.end())) {
            return None;
        }
        Some(format!("<guid:{}>", state.ids.label(&whole.as_str().to_lowercase())))
    })
}

fn threads(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.thread, text, |captures| {
        let label = state.threads.label(&captures[3]);
        Some(format!(
            "{}{}<thread:{label}>{}",
            &captures[1], &captures[2], &captures[4]
        ))
    })
}

fn emails(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.email, text, |captures| {
        let domain = captures[2].to_lowercase();
        let reserved = ["example.com", "example.org", "example.net"]
            .iter()
            .any(|reserved| domain == *reserved || domain.ends_with(&format!(".{reserved}")));
        if reserved {
            return None;
        }
        let key = format!("{}@{domain}", percent_decode(&captures[1]).to_lowercase());
        Some(format!("<user:{}>", state.users.label(&key)))
    })
}

fn domains(rules: &Rules, state: &mut State, text: &str) -> String {
    let text = match &rules.tenant {
        Some(tenant) => replace_each(tenant, text, |captures| {
            Some(format!("<domain:{}>", state.domains.label(&captures[0].to_lowercase())))
        }),
        None => text.to_owned(),
    };
    replace_each(&rules.hosted_domain, &text, |captures| {
        // the `-my` left of a domain placeholder (`<domain:1>-my.sharepoint.com`) is no tenant
        if text[..captures.get(0)?.start()].ends_with('-') {
            return None;
        }
        let label = state.domains.label(&captures[1].to_lowercase());
        let suffix = captures.get(2).map_or("", |suffix| suffix.as_str());
        Some(format!("<domain:{label}>{suffix}{}", &captures[3]))
    })
}

fn ipv4s(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.ipv4_run, text, |captures| {
        let run = &captures[0];
        let canonical = run.split('.').all(|octet| octet == "0" || !octet.starts_with('0'));
        let address: Ipv4Addr = run.parse().ok().filter(|_| canonical)?;
        if address.is_loopback() || address.is_unspecified() {
            return None;
        }
        Some(format!("<ip:{}>", state.ips.label(&address.to_string())))
    })
}

fn ipv6s(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.ipv6_run, text, |captures| {
        let run = &captures[0];
        if run.matches(':').count() < 2 {
            return None;
        }
        let trimmed = run.trim_matches('.');
        let (lead, candidate) = match trimmed.parse::<Ipv6Addr>() {
            Ok(_) => ("", trimmed),
            Err(_) if trimmed.starts_with(':') && !trimmed.starts_with("::") => (":", &trimmed[1..]),
            Err(_) => return None,
        };
        let address: Ipv6Addr = candidate.parse().ok()?;
        if address.is_loopback() || address.is_unspecified() {
            return None;
        }
        let start = run.find(trimmed).unwrap_or_default();
        let (before, after) = (&run[..start], &run[start + trimmed.len()..]);
        let label = state.ips.label(&address.to_string());
        Some(format!("{before}{lead}<ip:{label}>{after}"))
    })
}

fn ufrags(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.ufrag, text, |captures| {
        Some(format!("{}{}", &captures[1], state.token(&captures[2])))
    })
}

/// Learned names, in any case, as whole words: neither the character before a match nor the one after it may be a
/// letter or a digit.
fn names(_rules: &Rules, state: &mut State, text: &str) -> String {
    let Some(regex) = &state.names_regex else {
        return text.to_owned();
    };
    replace_each(regex, text, |captures| {
        let whole = captures.get(0)?;
        let before = text[..whole.start()].chars().next_back();
        let after = text[whole.end()..].chars().next();
        if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
            return None;
        }
        state
            .variants
            .get(&whole.as_str().to_lowercase())
            .map(|label| format!("<name:{label}>"))
    })
}

fn opaque(rules: &Rules, state: &mut State, text: &str) -> String {
    replace_each(&rules.opaque_run, text, |captures| {
        let run = &captures[0];
        looks_random(run).then(|| state.token(run))
    })
}

/// Whether a run of letters, digits, `-` and `_` looks like a random token: long, mixed case with digits, and high in
/// entropy.
fn looks_random(run: &str) -> bool {
    let count = |test: fn(&u8) -> bool| run.bytes().filter(test).count();
    run.len() >= OPAQUE_MIN
        && count(u8::is_ascii_uppercase) > 0
        && count(u8::is_ascii_lowercase) > 0
        && count(u8::is_ascii_digit) >= 3
        && entropy(run) >= OPAQUE_ENTROPY
}

/// Shannon entropy in bits per character.
fn entropy(text: &str) -> f64 {
    let mut counts = [0_u32; 256];
    for byte in text.bytes() {
        counts[usize::from(byte)] += 1;
    }
    let total = f64::from(u32::try_from(text.len()).unwrap_or(u32::MAX));
    counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let share = f64::from(count) / total;
            -share * share.log2()
        })
        .sum()
}

/// `%XX` escapes decoded (lossily, for comparing identities).
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        if let (b'%', Some(decoded)) = (byte, hex) {
            out.push(decoded);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
