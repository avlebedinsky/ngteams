//! The sanitizer: what a raw capture ([`crate::capture`]) goes through before `proto-lab export` writes its local
//! reference copy (outside the repository, with the fixture guard run on the result and its findings reported). It is
//! defence in depth: no
//! capture enters the repository, not even sanitized; committed fixtures are written synthetic-from-capture with
//! made-up values. Its known gaps are in `docs/backlog/sanitizer-defence-in-depth-known-gaps.md`.
//!
//! It works on the capture's JSON events, with one [`Sanitizer`] per capture, so every placeholder is stable within the
//! capture: the same email, MRI, GUID or name gets the same placeholder in every event.
//! - **Secrets are removed**: JWTs, skypetokens, `Bearer`/`Basic` credentials, cookie values, and any value under a
//!   secret-looking key (`*token*`, `*secret*`, `*password*`, `*cookie*`, `*credential*`, `authorization`, `sig`, the
//!   ICE `pwd`/`ufrag`/`usernameFragment`, the TURN `username`, …), in JSON, in form and query strings, and in text.
//!   Each becomes `<token:N>`; a scheme is kept (`Bearer <token:1>`, `skypetoken=<token:2>`), and so is a cookie's
//!   name. A long value under a generic key (`code`, `key`, `state`) is a secret too, as is any long high-entropy run
//!   of letters and digits nothing else recognized. Raw header text (`headersText`) is dropped, and binary content
//!   (base64 bodies that aren't UTF-8 text, binary WebSocket frames in CDP events and in the fence's verdicts, `bytes`)
//!   becomes `<binary:LEN>`.
//! - **Teams meetings** (`sanitize/meeting.rs`): a join link's passcode (`/meet/<code>?p=…`) and any passcode under its
//!   key or an invitation's label become `<token:N>`, of letters only too; the meeting's code (its numeric id, in the
//!   link, under `meetingCode`/`joinMeetingId`/`*conferenceId`, after `Meeting ID:`) `<meeting:N>`. Invitations are
//!   read in every language Teams writes them in, and by their structure next to a join link. Both are learned across
//!   the capture (at every layer of encoding, so a passcode that only shows a layer after its code is learned too) and
//!   replaced wherever else they show.
//! - **Identifiers become placeholders**: emails `<user:N>`, MRIs `<mri:N>` (`8:orgid:…`, `8:live:…`, `28:…`, phone
//!   `4:…`, `tel:`), conversation ids `19:<thread:N>@thread.v2`, GUIDs `<guid:N>`, tenant domains
//!   (`*.onmicrosoft.com`, `*.sharepoint.com`, the lab's own) `<domain:N>`, IP addresses other than loopback `<ip:N>`,
//!   names `<name:N>` and other personal fields (titles, topics, phones, departments) `<pii:N>`, under a JSON key or a
//!   `key=value`/`"key":"value"` in text. An MRI that holds a GUID shares the GUID's number, so `<mri:3>` and
//!   `<guid:3>` are the same object. The lab's own accounts get their alias instead of a number (`<user:a>`,
//!   `<mri:b>`, `<guid:c>`), and an approved service participant its name (`<mri:echo>`).
//! - **Names** are learned from name fields (`displayName`, `givenName`, `surname`, `imdisplayname`, …) across the whole
//!   capture first, base64 bodies, JSON inside strings and `displayName=…` in (encoded) text included; then every
//!   occurrence of a learned name, or of a word of it with 3 letters or more, in any case and as a whole word (`_`
//!   separates words too), is replaced in every text and every object key (a message body that says "Hi Jane", a
//!   `OneDrive` path with `jane_doe`, a query with `Hello%20Jane`, a map keyed by display name). A name's words are its
//!   runs of letters and digits, so punctuation never sticks to one (`Doe, Jane` gives `Doe`), but none of a part
//!   that is an identifier (an email or UPN, an MRI, a URL, a GUID, a domain: `jane.doe@contoso.com` is replaced whole
//!   and gives no `com`) and no run of digits alone (`sanitize/words.rs`); a "Last, First" name also stands for
//!   "First Last". A name is never replaced inside a host: a public protocol host (`teams.microsoft.com`) holds none,
//!   and replacing a word of any other host would destroy it, so there the name is left and [`sanitize_capture`]
//!   reports the event (the export warns about it). A name found in form data is learned both as written and
//!   form-decoded (`Marguerite+Fakename` also as `Marguerite Fakename`; `%2B` stays a plus), only here: to every other
//!   rule `+` stays `+`, so a base64 secret isn't split. A name only ever seen under a generic key such as `name` isn't
//!   learned. Every name value is also kept as it was found ([`Sanitizer::learned_names`]), for the export's own check
//!   ([`check_names`], `sanitize/backstop.rs`) of whether a word of one is left.
//!
//! **Every rule sees every layer of encoding** (`sanitize/layers.rs`). Text rules run on a text as written, then again
//! with one layer of escapes decoded (percent escapes, `%25XX` being one more layer, as UTF-8; backslash escapes; HTML
//! character references), and so on for up to 4 layers: a signature in a signed URL that is itself a query value
//! (`target=https%3A…%3Fsig%3Dab%252Bcd`) is seen as `sig=ab+cd`, a GUID after `%22` as one after `"`, an encoded name
//! (`Jos%C3%A9`) as the name. Only the ranges a rule replaces change; the rest is written back as it came, so the
//! replaced range of an encoded value is the whole of its encoding (`sig%3D<token:1>`). Checking the text as written
//! first keeps a value whole when an escape inside it decodes to a delimiter (`token=a%26b`). Identities are compared
//! decoded, so an email, a name or a token gets the same placeholder in every encoding.
//!
//! JSON inside a string (a request or response body, a WebSocket frame, also after a text prefix such as Trouter's
//! `3:::`) is sanitized as JSON and written back compactly, so keys are seen wherever they are; object keys go through
//! the identifier rules as well (maps keyed by MRI).
//!
//! Sanitizing is idempotent: placeholders are never matched again, so a sanitized capture comes out unchanged.
//! Whatever this misses, the export's name check and the fixture guard report: they are the last checks on the
//! reference copy, which only refuse it under `--strict`.

mod backstop;
mod layers;
mod meeting;
mod words;

use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::ops::Range;

use base64::Engine as _;
use regex::{Captures, Regex};
use serde_json::{Map, Value};

pub use self::backstop::{CHECK_ROUNDS, LearnedNames, NameFinding, check_names};
use self::layers::{DECODE_ROUNDS, Layered, decode_all};
use self::words::{host_of, hosts, learnable_words};
use crate::config::{Alias, LabConfig};

/// Every kind of placeholder the sanitizer writes.
const KINDS: &str = "token|user|mri|guid|thread|domain|ip|name|pii|meeting|binary|omitted";

/// Separators as captures write them: plain, percent-encoded, encoded twice.
const COLON: &str = "(?::|%3[aA]|%253[aA])";
const AT: &str = "(?:@|%40|%2540)";
const GUID: &str = "[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}";
/// A percent escape (single or double) of a character an email's local part may hold: `!`, `$`, `*`, `+`, `-`, `.`,
/// `^`, `_`, a backtick, `{`, `|`, `}`, `~`. Never of a URL delimiter (`:`, `/`, `=`, `?`, `&`, `#`), a quote, `%` or a
/// space, so an encoded email doesn't take in the encoded text before it.
const LOCAL_ESCAPE: &str = "%(?:25)?(?:2[14ABDEabde]|5[EFef]|60|7[B-Eb-e])";

/// Key fragments whose values are always secrets (keys are compared lowercased).
const SECRET_PARTS: [&str; 23] = [
    "token",
    "secret",
    "password",
    "passwd",
    "passcode",
    // a meeting's passcode, as Microsoft's own forms name it
    "meetingpw",
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
    // the ICE `usernameFragment` of a candidate: the same value as the `ufrag` in the SDP
    "usernamefragment",
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
    field: Regex,
    field_value: Regex,
    opaque_run: Regex,
    meeting: meeting::Patterns,
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
            // the escapes before the address (`%3A` of `sip%3Ajane%40…`) are captured to be kept, and not taken for
            // part of its local part, which holds only the escapes of characters an address may have
            email: Regex::new(&format!(
                r"((?:%(?:25)?[0-9A-Fa-f]{{2}})*?)((?:[A-Za-z0-9._+\-]|{LOCAL_ESCAPE})+){AT}([A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)*\.[A-Za-z]{{2,}})\b"
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
            // the key of a `key=value` or `"key": "value"` in text (group 1), up to its value ...
            field: Regex::new(r#"(?:^|[^A-Za-z0-9_\-])([A-Za-z][A-Za-z0-9_\-]*)["']?\s*[:=]\s*"#)?,
            // ... and the value: quoted (group 1, 2) or bare (3), up to the next delimiter
            field_value: Regex::new(r#"^(?:"([^"<>]*)"|'([^'<>]*)'|([^"'&#\r\n;,{}\[\]<>\s][^"'&#\r\n;,{}\[\]<>]*))"#)?,
            opaque_run: Regex::new(r"[A-Za-z0-9_\-]+")?,
            meeting: meeting::Patterns::new()?,
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
    /// The names whose variants are in [`Self::variants`], as compared ([`identity_text`]).
    learned: HashSet<String>,
    /// Every name value as it was found (before any decoding), with its label: what the export's check
    /// ([`check_names`]) looks for, apart from [`Self::variants`].
    found_names: HashMap<String, String>,
    names_regex: Option<Regex>,
    names_changed: bool,
    /// How many times a name was left inside a host that isn't a public one (`words.rs`), so as not to destroy that
    /// host ([`Sanitized::name_in_host`]).
    name_in_host: usize,
    /// How many times a learned passcode was replaced inside a host that isn't a public one (`meeting.rs`): a secret is
    /// redacted even there, and that host is gone ([`Sanitized::passcode_in_host`]).
    passcode_in_host: usize,
    /// Meetings' codes (`meeting.rs`).
    meetings: Table,
    /// The codes learned, as their digits.
    codes: HashSet<String>,
    /// The passcodes learned: lowercased, with the passcode as found.
    passcodes: HashMap<String, String>,
    /// Values that may be words (`Online`) that only an invitation's structure gave away (`meeting.rs`), lowercased:
    /// replaced where they follow a label, not everywhere.
    weak_passcodes: HashSet<String>,
    passcodes_regex: Option<Regex>,
    passcodes_changed: bool,
}

impl State {
    /// A secret's placeholder; the same secret in any encoding gets the same one.
    fn token(&mut self, value: &str) -> String {
        format!("<token:{}>", self.tokens.label(&decode_all(value)))
    }

    /// A whole name value's placeholder.
    fn name(&mut self, name: &str) -> String {
        let label = self.names.label(&identity_text(name));
        self.found_names.entry(name.to_owned()).or_insert_with(|| label.clone());
        format!("<name:{label}>")
    }

    fn pii(&mut self, value: &str) -> String {
        format!("<pii:{}>", self.pii.label(&identity_text(value)))
    }

    /// Learns a name value as it was found: as compared ([`identity_text`], where `+` stays `+`) and form-decoded (a
    /// `+` a space, `%2B` still a `+`), both under one label, each with its variants ([`name_variants`]).
    fn learn_name(&mut self, raw: &str) {
        let mut forms = vec![identity_text(raw), identity_text(&raw.replace('+', " "))];
        forms.dedup();
        forms.retain(|form| form.chars().count() >= 2);
        let Some(first) = forms.first() else {
            return;
        };
        let known = forms.iter().find_map(|form| self.names.labels.get(form).cloned());
        let label = known.unwrap_or_else(|| self.names.label(first));
        self.found_names.entry(raw.to_owned()).or_insert_with(|| label.clone());
        for form in forms {
            self.names.seed(form.clone(), &label);
            if !self.learned.insert(form.clone()) {
                continue;
            }
            for variant in name_variants(&form) {
                self.variants
                    .entry(variant.to_lowercase())
                    .or_insert_with(|| label.clone());
            }
            self.names_changed = true;
        }
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
    /// Under a meeting passcode's key: a secret, a number too, learned ([`meeting`]).
    Passcode,
    /// Under a meeting code's key: `<meeting:N>`, a number too, learned.
    Code,
    /// Under `meetingId`: a meeting's code when it is a number.
    NumericCode,
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

    /// Learns the names in one event (see the module docs), so that every event sanitized after replaces them. It reads
    /// the event the way [`Self::sanitize`] does: base64 bodies decoded, JSON inside strings parsed.
    pub fn learn(&mut self, event: &Value) {
        match event {
            Value::Object(map) => {
                let base64 = is_base64(map);
                for (key, value) in map {
                    self.learn_entry(key, value, base64);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| self.learn(item)),
            Value::String(text) => {
                if let Some(json) = parse_container(text) {
                    self.learn(&json);
                } else if let Some((_, json)) = json_suffix(text) {
                    self.learn(&json);
                } else {
                    self.learn_text(text);
                }
            }
            _ => {}
        }
    }

    /// The name fields in text (`displayName=…`, `"givenName":"…"`), and meetings' codes and passcodes (`meeting.rs`),
    /// at every layer of its encoding.
    fn learn_text(&mut self, text: &str) {
        let mut layered = Layered::new(text);
        for round in 0..=DECODE_ROUNDS {
            let plain = layered.plain();
            for (mode, range) in fields_in(&self.rules, &plain) {
                match mode {
                    Mode::Name => self.state.learn_name(&plain[range]),
                    Mode::Code | Mode::NumericCode => self.state.learn_code(&plain[range]),
                    _ => {}
                }
            }
            meeting::learn(&self.rules.meeting, &mut self.state, &plain);
            if round == DECODE_ROUNDS || !layered.decode() {
                break;
            }
        }
    }

    /// One entry of a map; `base64` when the map's body is base64.
    fn learn_entry(&mut self, key: &str, value: &Value, base64: bool) {
        match (key_mode(key), value) {
            (Mode::Name, _) => self.learn_names_in(value),
            (Mode::Code | Mode::NumericCode, Value::String(_) | Value::Number(_)) => {
                self.state.learn_code(&scalar_text(value));
            }
            (Mode::Passcode, Value::String(_) | Value::Number(_)) if meeting::names_passcode(key) => {
                self.state.learn_passcode(&scalar_text(value));
            }
            (_, Value::String(text)) if base64 && is_body_key(key) => {
                if let Some(text) = base64_text(text) {
                    self.learn(&Value::String(text));
                }
            }
            _ => self.learn(value),
        }
    }

    /// The value under a name key: a name, or a list of them (sanitizing makes each string in a list a `<name:N>`).
    fn learn_names_in(&mut self, value: &Value) {
        match value {
            Value::String(name) if !self.is_placeholder(name) => self.state.learn_name(name),
            Value::Array(items) => items.iter().for_each(|item| self.learn_names_in(item)),
            other => self.learn(other),
        }
    }

    /// One event, sanitized.
    ///
    /// # Errors
    /// The learned names don't compile into a pattern (too many of them).
    pub fn sanitize(&mut self, event: &Value) -> Result<Value, regex::Error> {
        self.state.rebuild_names()?;
        self.state.rebuild_passcodes()?;
        Ok(self.value(event, Mode::Plain))
    }

    fn is_placeholder(&self, text: &str) -> bool {
        self.rules.whole_placeholder.is_match(text)
    }

    /// Every name value found so far (learned, or replaced whole under a name key), as it was found, with its
    /// placeholder: what [`check_names`] looks for in the output.
    #[must_use]
    pub fn learned_names(&self) -> LearnedNames {
        LearnedNames::new(
            self.state
                .found_names
                .iter()
                .map(|(raw, label)| (raw.clone(), format!("<name:{label}>"))),
        )
    }

    fn value(&mut self, value: &Value, mode: Mode) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(text, mode)),
            Value::Array(items) => Value::Array(items.iter().map(|item| self.value(item, mode)).collect()),
            Value::Object(map) => Value::Object(self.object(map, mode)),
            Value::Number(number) => self.number(number, mode),
            other => other.clone(),
        }
    }

    /// A number: a meeting's code or passcode as one becomes its placeholder; any other stays.
    fn number(&mut self, number: &serde_json::Number, mode: Mode) -> Value {
        let text = number.to_string();
        match mode {
            Mode::Code => Value::String(self.state.meeting(&text)),
            Mode::NumericCode if meeting::is_numeric_code(&text) => Value::String(self.state.meeting(&text)),
            Mode::Passcode => Value::String(self.state.token(&text)),
            _ => Value::Number(number.clone()),
        }
    }

    fn object(&mut self, map: &Map<String, Value>, mode: Mode) -> Map<String, Value> {
        let base64 = is_base64(map);
        let mut out = Map::new();
        for (key, value) in map {
            let lower = key.to_lowercase();
            let child = match mode {
                Mode::Secret | Mode::Cookie | Mode::Passcode => Mode::Secret,
                _ => key_mode(key),
            };
            let sanitized = match value {
                Value::String(text) if base64 && is_body_key(key) => self.base64_body(text, child),
                Value::String(text) if is_binary(map, key) => {
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
        match base64_text(encoded) {
            Some(text) => Value::String(self.string(&text, mode)),
            None => binary(decoded_len(encoded)),
        }
    }

    fn key(&mut self, key: &str) -> String {
        layered(&self.rules, &mut self.state, &KEY_RULES, key)
    }

    fn string(&mut self, text: &str, mode: Mode) -> String {
        if self.is_placeholder(text) {
            return text.to_owned();
        }
        match mode {
            Mode::Secret | Mode::Passcode => self.credential(text),
            Mode::Cookie => self.cookie(text),
            Mode::Code if !text.trim().is_empty() => self.state.meeting(text),
            Mode::NumericCode if meeting::is_numeric_code(text) => self.state.meeting(text),
            Mode::Weak if text.len() >= WEAK_MIN => self.state.token(text),
            Mode::Name if !text.trim().is_empty() => self.state.name(text),
            Mode::Pii if !text.trim().is_empty() => self.state.pii(text),
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
        layered(&self.rules, &mut self.state, &TEXT_RULES, text)
    }
}

/// Sanitizes a whole capture with one [`Sanitizer`]: learns every event, then sanitizes each.
///
/// # Errors
/// As [`Sanitizer::new`] and [`Sanitizer::sanitize`].
pub fn sanitize_events(events: &[Value], identities: &Identities) -> Result<Vec<Value>, regex::Error> {
    sanitize_capture(events, identities).map(|sanitized| sanitized.events)
}

/// A capture sanitized, with the names found in it for [`check_names`].
pub struct Sanitized {
    pub events: Vec<Value>,
    pub names: LearnedNames,
    /// The events (from 1) where a word of a name was left inside a host other than a public protocol host
    /// (`https://jane.example.test/`): replacing it would have destroyed the host.
    pub name_in_host: Vec<usize>,
    /// The events (from 1) where a learned passcode was replaced inside a host other than a public protocol host: the
    /// secret is gone, and so is that host.
    pub passcode_in_host: Vec<usize>,
}

/// As [`sanitize_events`], also handing back the names found ([`Sanitizer::learned_names`]), where a name was left
/// inside a host and where a passcode took a host's place.
///
/// # Errors
/// As [`Sanitizer::new`] and [`Sanitizer::sanitize`].
pub fn sanitize_capture(events: &[Value], identities: &Identities) -> Result<Sanitized, regex::Error> {
    let mut sanitizer = Sanitizer::new(identities)?;
    for event in events {
        sanitizer.learn(event);
    }
    let mut out = Vec::with_capacity(events.len());
    let mut name_in_host = Vec::new();
    let mut passcode_in_host = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let before = (sanitizer.state.name_in_host, sanitizer.state.passcode_in_host);
        out.push(sanitizer.sanitize(event)?);
        if sanitizer.state.name_in_host > before.0 {
            name_in_host.push(index + 1);
        }
        if sanitizer.state.passcode_in_host > before.1 {
            passcode_in_host.push(index + 1);
        }
    }
    Ok(Sanitized {
        events: out,
        names: sanitizer.learned_names(),
        name_in_host,
        passcode_in_host,
    })
}

fn seed_mri(rules: &Rules, state: &mut State, mri: &str, label: &str) {
    let normalized = normalize_mri(mri);
    match rules.guid.find(&normalized) {
        Some(guid) => state.ids.seed(guid.as_str().to_lowercase(), label),
        None => state.ids.seed(format!("mri:{normalized}"), label),
    }
}

fn normalize_mri(mri: &str) -> String {
    decode_all(mri).to_lowercase()
}

/// Every text that stands for a name: the name and, for a "Last, First" name (one comma), "First Last", each also with
/// `%20`, `+` and `_` for its spaces; and its words ([`learnable_words`]: runs of letters and digits of 3 or more, any
/// script, so punctuation never sticks to one, `Doe, Jane` giving `Doe` and `Jane`; none from a part that is an
/// identifier, so `jane.doe@contoso.com` gives no `com`).
fn name_variants(name: &str) -> Vec<String> {
    let mut full = vec![name.to_owned()];
    if let Some((last, first)) = name.split_once(',').filter(|_| name.matches(',').count() == 1) {
        let (last, first) = (last.trim(), first.trim());
        if !last.is_empty() && !first.is_empty() {
            full.push(format!("{first} {last}"));
        }
    }
    let mut variants = Vec::new();
    for name in full {
        for space in ["%20", "+", "_"] {
            variants.push(name.replace(' ', space));
        }
        variants.push(name);
    }
    variants.extend(learnable_words(name).into_iter().map(str::to_owned));
    variants
}

/// A string's or a number's text.
fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// What an identity is compared by: decoded, without the spaces around it.
fn identity_text(text: &str) -> String {
    decode_all(text.trim()).trim().to_owned()
}

fn binary(len: usize) -> Value {
    Value::String(format!("<binary:{len}>"))
}

/// Whether a map holds a body in base64 (`"base64Encoded": true`, CDP's bodies and the recorder's).
fn is_base64(map: &Map<String, Value>) -> bool {
    map.get("base64Encoded") == Some(&Value::Bool(true))
}

/// The keys a base64 body sits under.
fn is_body_key(key: &str) -> bool {
    key == "body" || key == "postData"
}

/// Whether the string under `key` is binary content: a CDP frame's `payloadData` with opcode 2, the data of a frame
/// the recorder marked `"kind": "binary"` (a fence verdict on a binary WebSocket frame), or any `bytes`.
fn is_binary(map: &Map<String, Value>, key: &str) -> bool {
    match key {
        "payloadData" => map.get("opcode").and_then(Value::as_u64) == Some(2),
        "data" => map.get("kind").and_then(Value::as_str) == Some("binary"),
        "bytes" => true,
        _ => false,
    }
}

/// Base64 text decoded, when it is base64 of UTF-8 text.
fn base64_text(encoded: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded.trim()).ok()?;
    String::from_utf8(bytes).ok()
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
    let bare: String = lower.chars().filter(|&char| char != '_' && char != '-').collect();
    if let Some(mode) = meeting::key_mode(&bare).filter(|mode| *mode == Mode::Passcode) {
        return mode;
    }
    if SECRET_PARTS.iter().any(|part| lower.contains(part)) || SECRET_KEYS.contains(&lower.as_str()) {
        return Mode::Secret;
    }
    if WEAK_KEYS.contains(&lower.as_str()) {
        return Mode::Weak;
    }
    if let Some(mode) = meeting::key_mode(&bare) {
        return mode;
    }
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

/// Ranges of a text, each with the placeholder it becomes; in order, not overlapping.
type Edits = Vec<(Range<usize>, String)>;

type Rule = fn(&Rules, &mut State, &str) -> Edits;

/// The rules for free text, in order: secrets first (so a token is never taken apart; meetings' passcodes and codes
/// with them), then identifiers, then personal fields, names, and whatever still looks like a token.
const TEXT_RULES: [Rule; 17] = [
    bearer,
    secret_values,
    jwts,
    meetings,
    meetings_learned,
    mris,
    guids,
    threads,
    emails,
    tenant_domains,
    hosted_domains,
    ipv4s,
    ipv6s,
    ufrags,
    fields,
    names,
    opaque,
];

/// The rules for object keys: identifiers and learned names (a map may be keyed by MRI or by display name), never
/// secrets (a key is never a secret's name to redact).
const KEY_RULES: [Rule; 12] = [
    jwts,
    meetings_learned,
    mris,
    guids,
    threads,
    emails,
    tenant_domains,
    hosted_domains,
    ipv4s,
    ipv6s,
    names,
    opaque,
];

/// Runs `list` over `text` at every layer of its encoding (see the module docs): each rule in turn on the text as
/// written, then each again with one more layer of escapes decoded, until none is left.
fn layered(rules: &Rules, state: &mut State, list: &[Rule], text: &str) -> String {
    let mut layered = Layered::new(text);
    for round in 0..=DECODE_ROUNDS {
        for rule in list {
            let edits = rule_edits(rules, state, *rule, &layered);
            layered.replace(edits);
        }
        if round == DECODE_ROUNDS || !layered.decode() {
            break;
        }
    }
    layered.source()
}

/// What `rule` replaces in the text as the rules see it now, applied only between placeholders, so no rule ever
/// matches a placeholder or a part of one. Placeholder-shaped text that came from escapes (`%3Ctoken:1%3E`) is no
/// placeholder: it goes through the rules like any other text.
fn rule_edits(rules: &Rules, state: &mut State, rule: Rule, layered: &Layered) -> Edits {
    let plain = layered.plain();
    let mut edits = Vec::new();
    let mut gaps = Vec::new();
    let mut last = 0;
    let escaped = layered.escaped();
    let literal = |range: &Range<usize>| {
        let first = escaped.partition_point(|escape| escape.end <= range.start);
        escaped.get(first).is_none_or(|escape| escape.start >= range.end)
    };
    let placeholders = rules
        .placeholder
        .find_iter(&plain)
        .map(|found| found.range())
        .filter(|range| literal(range));
    for range in placeholders {
        gaps.push(last..range.start);
        last = range.end;
    }
    gaps.push(last..plain.len());
    for gap in gaps.into_iter().filter(|gap| !gap.is_empty()) {
        let found = rule(rules, state, &plain[gap.clone()]);
        edits.extend(
            found
                .into_iter()
                .map(|(range, with)| (range.start + gap.start..range.end + gap.start, with)),
        );
    }
    edits
}

/// One edit per match `edit` gives one for; a match it gives `None` stays as it is.
fn edits_each(
    regex: &Regex,
    text: &str,
    mut edit: impl FnMut(&Captures<'_>) -> Option<(Range<usize>, String)>,
) -> Edits {
    regex
        .captures_iter(text)
        .filter_map(|captures| edit(&captures))
        .collect()
}

fn bearer(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.bearer, text, |captures| {
        let token = captures.get(3)?;
        Some((token.range(), state.token(token.as_str())))
    })
}

fn secret_values(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.secret_value, text, |captures| {
        let (key, value) = (captures[2].to_lowercase(), captures.get(4)?);
        let weak = WEAK_KEYS.contains(&key.as_str());
        let plain = value.as_str();
        if SCHEMES.contains(&plain.to_lowercase().as_str()) || (weak && plain.len() < WEAK_MIN) {
            return None;
        }
        Some((value.range(), state.token(plain)))
    })
}

fn meetings(rules: &Rules, state: &mut State, text: &str) -> Edits {
    meeting::meetings(&rules.meeting, state, text)
}

fn meetings_learned(rules: &Rules, state: &mut State, text: &str) -> Edits {
    meeting::learned(&rules.meeting, state, text)
}

fn jwts(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.jwt, text, |captures| {
        let jwt = captures.get(0)?;
        Some((jwt.range(), state.token(jwt.as_str())))
    })
}

fn mris(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.mri, text, |captures| {
        let mri = captures.get(2)?;
        let normalized = normalize_mri(mri.as_str());
        let label = match rules.guid.find(&normalized) {
            Some(guid) => state.ids.label(&guid.as_str().to_lowercase()),
            None => state.ids.label(&format!("mri:{normalized}")),
        };
        Some((mri.range(), format!("<mri:{label}>")))
    })
}

fn guids(rules: &Rules, state: &mut State, text: &str) -> Edits {
    let bytes = text.as_bytes();
    let hex_at = |index: Option<usize>| {
        index
            .and_then(|index| bytes.get(index))
            .is_some_and(u8::is_ascii_hexdigit)
    };
    edits_each(&rules.guid, text, |captures| {
        let whole = captures.get(0)?;
        // a GUID-shaped piece of a longer hex run isn't one (one right after a percent escape is seen once the escape
        // is decoded)
        if hex_at(whole.start().checked_sub(1)) || hex_at(Some(whole.end())) {
            return None;
        }
        let label = state.ids.label(&whole.as_str().to_lowercase());
        Some((whole.range(), format!("<guid:{label}>")))
    })
}

fn threads(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.thread, text, |captures| {
        let id = captures.get(3)?;
        let label = state.threads.label(&decode_all(id.as_str()));
        Some((id.range(), format!("<thread:{label}>")))
    })
}

fn emails(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.email, text, |captures| {
        let (local, domain) = (captures.get(2)?, captures.get(3)?);
        let domain_name = domain.as_str().to_lowercase();
        let reserved = ["example.com", "example.org", "example.net"]
            .iter()
            .any(|reserved| domain_name == *reserved || domain_name.ends_with(&format!(".{reserved}")));
        if reserved {
            return None;
        }
        let address = decode_all(local.as_str()).to_lowercase();
        let label = state.users.label(&format!("{address}@{domain_name}"));
        Some((local.start()..domain.end(), format!("<user:{label}>")))
    })
}

fn tenant_domains(rules: &Rules, state: &mut State, text: &str) -> Edits {
    let Some(tenant) = &rules.tenant else {
        return Vec::new();
    };
    edits_each(tenant, text, |captures| {
        let domain = captures.get(0)?;
        let label = state.domains.label(&domain.as_str().to_lowercase());
        Some((domain.range(), format!("<domain:{label}>")))
    })
}

fn hosted_domains(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.hosted_domain, text, |captures| {
        // the `-my` left of a domain placeholder (`<domain:1>-my.sharepoint.com`) is no tenant
        if text[..captures.get(0)?.start()].ends_with('-') {
            return None;
        }
        let tenant = captures.get(1)?;
        let label = state.domains.label(&tenant.as_str().to_lowercase());
        Some((tenant.range(), format!("<domain:{label}>")))
    })
}

fn ipv4s(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.ipv4_run, text, |captures| {
        let run = captures.get(0)?;
        let canonical = run
            .as_str()
            .split('.')
            .all(|octet| octet == "0" || !octet.starts_with('0'));
        let address: Ipv4Addr = run.as_str().parse().ok().filter(|_| canonical)?;
        if address.is_loopback() || address.is_unspecified() {
            return None;
        }
        Some((run.range(), format!("<ip:{}>", state.ips.label(&address.to_string()))))
    })
}

fn ipv6s(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.ipv6_run, text, |captures| {
        let found = captures.get(0)?;
        let run = found.as_str();
        if run.matches(':').count() < 2 {
            return None;
        }
        let trimmed = run.trim_matches('.');
        // a `:` before the address that only the run took in (`port:2001:db8::1`) stays
        let lead = match trimmed.parse::<Ipv6Addr>() {
            Ok(_) => 0,
            Err(_) if trimmed.starts_with(':') && !trimmed.starts_with("::") => 1,
            Err(_) => return None,
        };
        let address: Ipv6Addr = trimmed[lead..].parse().ok()?;
        if address.is_loopback() || address.is_unspecified() {
            return None;
        }
        let start = found.start() + run.find(trimmed).unwrap_or_default() + lead;
        let end = found.start() + run.find(trimmed).unwrap_or_default() + trimmed.len();
        Some((start..end, format!("<ip:{}>", state.ips.label(&address.to_string()))))
    })
}

fn ufrags(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.ufrag, text, |captures| {
        let ufrag = captures.get(2)?;
        Some((ufrag.range(), state.token(ufrag.as_str())))
    })
}

/// The values of name, other personal and meeting code fields in text (`displayName=Jane`, `"jobTitle":"Engineer"`,
/// `meetingId=1234567890123`), with their mode ([`Mode::Name`], [`Mode::Pii`], [`Mode::Code`] or
/// [`Mode::NumericCode`]); in order, not overlapping.
fn fields_in(rules: &Rules, text: &str) -> Vec<(Mode, Range<usize>)> {
    let mut found = Vec::new();
    let mut done = 0;
    for captures in rules.field.captures_iter(text) {
        let (Some(key), Some(whole)) = (captures.get(1), captures.get(0)) else {
            continue;
        };
        let mode = key_mode(key.as_str());
        let wanted = matches!(mode, Mode::Name | Mode::Pii | Mode::Code | Mode::NumericCode);
        if key.start() < done || !wanted {
            continue;
        }
        let Some(range) = field_value(rules, text, whole.end()) else {
            continue;
        };
        done = range.end;
        found.push((mode, range));
    }
    found
}

/// The value of a field that starts at `at`, without the quotes and the spaces around it; `None` for an empty one or
/// `null`, `true`, `false`.
fn field_value(rules: &Rules, text: &str, at: usize) -> Option<Range<usize>> {
    let captures = rules.field_value.captures(&text[at..])?;
    let value = (1..=3).find_map(|group| captures.get(group))?;
    let raw = value.as_str();
    let trimmed = raw.trim();
    if trimmed.is_empty() || ["null", "true", "false"].contains(&trimmed) {
        return None;
    }
    let start = at + value.start() + (raw.len() - raw.trim_start().len());
    Some(start..start + trimmed.len())
}

fn fields(rules: &Rules, state: &mut State, text: &str) -> Edits {
    fields_in(rules, text)
        .into_iter()
        .filter_map(|(mode, range)| {
            let value = &text[range.clone()];
            let placeholder = match mode {
                Mode::Name => state.name(value),
                Mode::Code => state.meeting(value),
                Mode::NumericCode if meeting::is_numeric_code(value) => state.meeting(value),
                Mode::NumericCode => return None,
                _ => state.pii(value),
            };
            Some((range, placeholder))
        })
        .collect()
}

/// Learned names, in any case, as whole words: neither the character before a match nor the one after it may be a
/// letter or a digit (a name behind an escape, `Hello%20Jane`, is found once the escape is decoded). A match that isn't
/// bounded is looked at again as the single word it starts (`Jane Does` holds `Jane`, not `Jane Doe`), and the search
/// goes on from the next character, so nothing inside it is skipped.
///
/// A match inside a public protocol host (`teams.microsoft.com`, `words.rs`) is no name and stays. One inside any
/// other host stays too, so the host isn't destroyed, and is counted ([`State::name_in_host`]).
fn names(_: &Rules, state: &mut State, text: &str) -> Edits {
    let Some(regex) = &state.names_regex else {
        return Vec::new();
    };
    let hosts = hosts(text);
    let mut edits = Vec::new();
    let mut in_host = 0;
    let mut at = 0;
    while let Some(found) = regex.find_at(text, at) {
        let start = found.start();
        let next = start + text[start..].chars().next().map_or(1, char::len_utf8);
        let Some((end, label)) = name_at(state, text, start, found.end()) else {
            at = next;
            continue;
        };
        if let Some(public) = host_of(&hosts, &(start..end)) {
            in_host += usize::from(!public);
            at = next;
            continue;
        }
        edits.push((start..end, format!("<name:{label}>")));
        at = end;
    }
    state.name_in_host += in_host;
    edits
}

/// The learned name that starts at `start`, bounded on both sides: what the names pattern matched (up to `end`), or
/// else the single word there; where it ends, and its label.
fn name_at(state: &State, text: &str, start: usize, end: usize) -> Option<(usize, String)> {
    if text[..start].chars().next_back().is_some_and(char::is_alphanumeric) {
        return None;
    }
    let bounded = !text[end..].chars().next().is_some_and(char::is_alphanumeric);
    let matched = bounded
        .then(|| state.variants.get(&text[start..end].to_lowercase()))
        .flatten();
    if let Some(label) = matched {
        return Some((end, label.clone()));
    }
    let word = text[start..]
        .find(|char: char| !char.is_alphanumeric())
        .map_or(text.len(), |len| start + len);
    let label = state.variants.get(&text[start..word].to_lowercase())?;
    Some((word, label.clone()))
}

fn opaque(rules: &Rules, state: &mut State, text: &str) -> Edits {
    edits_each(&rules.opaque_run, text, |captures| {
        let run = captures.get(0)?;
        looks_random(run.as_str()).then(|| (run.range(), state.token(run.as_str())))
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
