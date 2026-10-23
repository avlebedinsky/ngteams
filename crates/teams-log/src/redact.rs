//! Redaction: what the log never writes, taken out by the logger itself, never left to a call site.
//!
//! - **By name**: a header whose name is a credential's ([`secret_field`]: `Authorization`, `Cookie`, `Set-Cookie`,
//!   `X-Skypetoken`, `Authentication`, any name holding `token`, `secret`, `password`, `signature` or `credential`);
//!   a JSON, form or query field of those names (`access_token`, `refresh_token`, `id_token`, `skypeToken`, `token`,
//!   Trouter's `connectparams` `sig`, the registrar's `aesKey`, …) and of sign-in's (`code`, `code_verifier`,
//!   `client_secret`, …: [`secret_param`]), JSON's at any depth (a string that is itself JSON, a Trouter delivery's
//!   `body`, read as JSON too); the arguments of Trouter's `user.authenticate`; a URL's query and fragment wherever
//!   one is written (a request's, a header's value, a string in JSON).
//! - **`<name>=<value>` anywhere** ([`params`]): a credential's name followed by `=` in any text, after any separator
//!   in any of its escaped forms (`&`, `&amp;`, `&#38;`, `&#x26;`, `%26`, `%2526`, `\u0026`, a space, a quote, the
//!   start), its `=` in any of its forms (`=`, `%3D`, `\u003d`, `&#61;`), has its value redacted: a SAS link in a
//!   message's HTML (`href="…?sv=…&amp;sig=…"`), percent-encoded inside another link, or in an error's text. Every
//!   line passes it ([`line`]), so it holds wherever the text came from; a message's own `code=1` goes too.
//! - **What can't be read is redacted whole**: a body that says it is JSON and isn't, a frame whose data isn't JSON,
//!   are never written raw ([`UNREADABLE`]). A binary body (a picture) is written as its type and size only.
//! - **The last pass** ([`line`]): every line, any record's, has its JWT-shaped strings, `skypetoken=…` and
//!   `Bearer …` values, and long opaque runs (a refresh token, an authorization code) replaced, whatever put them
//!   there (a panic message, an error's text).

use std::borrow::Cow;
use std::fmt::Write as _;

use serde_json::Value;

/// What a redacted value is written as.
pub const REDACTED: &str = "[redacted]";

/// What a body or frame the redactor can't read is written as (with its size).
pub const UNREADABLE: &str = "[unreadable, redacted whole]";

/// The shortest run of token characters ([`opaque`]) taken as a credential by the last pass: longer than any id the
/// client logs (an MRI, a chat id, a message id), shorter than a refresh token or an authorization code.
pub const OPAQUE_RUN: usize = 100;

/// Header, JSON and form field names that always hold a credential, besides any name holding one of
/// [`SECRET_WORDS`]: the bearer and cookie headers, Trouter's `connectparams` signature (`sig`, valid for days: a
/// reconnect's credential), the registrar's push encryption key (`aesKey`), a SAS link's `sig`, a meeting's
/// `passcode`.
const SECRET_NAMES: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "authentication",
    "sig",
    "aeskey",
    "passcode",
];

/// Words a credential's name holds, wherever in the name (`X-Skypetoken`, `skypeToken`, `refresh_token`,
/// `client_secret`, `password`, `x-ms-signature`).
const SECRET_WORDS: [&str; 5] = ["token", "secret", "password", "signature", "credential"];

/// Field names of sign-in and renewal that hold a credential, besides [`SECRET_NAMES`]: the authorization code, the
/// PKCE verifier, a client's secret or assertion.
const SECRET_PARAMS: [&str; 5] = [
    "code",
    "code_verifier",
    "client_secret",
    "client_assertion",
    "assertion",
];

/// Whether a header named `name` holds a credential: one of [`SECRET_NAMES`] or a name holding one of
/// [`SECRET_WORDS`], in any case.
#[must_use]
pub fn secret_field(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    SECRET_WORDS.iter().any(|word| name.contains(word)) || SECRET_NAMES.contains(&name.as_str())
}

/// Whether a JSON, form or query field named `name` holds a credential: [`secret_field`]'s names and sign-in's own
/// ([`SECRET_PARAMS`]: the authorization code, the PKCE verifier, …). A JSON error's `code` is redacted with them:
/// over-redaction, never under.
#[must_use]
pub fn secret_param(name: &str) -> bool {
    secret_field(name) || SECRET_PARAMS.contains(&name.trim().to_ascii_lowercase().as_str())
}

/// A header's value as the log writes it: [`REDACTED`] for a credential's header; any other with its credential
/// fields redacted ([`url`]: a `Location`, a link, a `name=value` list).
#[must_use]
pub fn header_value<'a>(name: &str, value: &'a str) -> Cow<'a, str> {
    if secret_field(name) {
        Cow::Borrowed(REDACTED)
    } else {
        Cow::Owned(url(value))
    }
}

/// A URL as the log writes it: its query's and fragment's credential fields ([`secret_param`]) redacted, and every
/// credential's `<name>=` in it, in any escaped form ([`params`]: a link nested in its query); the rest as it is.
#[must_use]
pub fn url(url: &str) -> String {
    params(&query_and_fragment(url))
}

/// `url` with its query's and fragment's credential fields redacted ([`form`]).
fn query_and_fragment(url: &str) -> String {
    let (before_fragment, fragment) = match url.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (url, None),
    };
    let mut out = match before_fragment.split_once('?') {
        Some((base, query)) => format!("{base}?{}", form(query)),
        None => before_fragment.to_owned(),
    };
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(&form(fragment));
    }
    out
}

/// `name=value&…` text (a form body, a query) with its credential fields' values ([`secret_param`]) redacted, a name
/// read with its `%XX` escapes decoded, and every credential's `<name>=` in any escaped form ([`params`]).
#[must_use]
pub fn form(text: &str) -> String {
    let pairs = text
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if secret_param(&percent_decoded(name)) => format!("{name}={REDACTED}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&");
    params(&pairs)
}

/// Text with the value of every credential's `<name>=<value>` ([`secret_param`]) redacted, wherever it is: the name
/// a whole word (after the start, any byte that can't be in a name, or a `%XX`, `%25XX`, `\uXXXX` or `\xXX` escape:
/// `?`, `&`, `&amp;`, `&#38;`, `&#x26;`, `%26`, `%2526`, `\u0026`, a space, a quote), its `=` in any form (`=`,
/// `%3D`, `%253D`, `\u003d`, `\x3d`, `&#61;`, `&amp;#61;`, `&#x3d;`, `&equals;`), its value everything up to a
/// space, a control character, a quote, `&`, `<`, `>`, `\`, `#` or `` ` ``. It can't tell a link from prose, so
/// a message's own `code=1` goes too: redacting more, never less.
///
/// One pass, linear in the text: every name that starts inside a run of name bytes ends where the run does, so a run's
/// `=` and value are read once for all of them, and whether a name can start at a byte ([`word_start`]) is read from
/// the escapes carried forward ([`percent_escapes`]), never by walking back.
#[must_use]
pub fn params(text: &str) -> String {
    let bytes = text.as_bytes();
    let escapes = percent_escapes(bytes);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    // `at` always sits on a char boundary: names, `=` forms and value ends are ASCII
    while at < bytes.len() {
        if !name_byte(bytes[at]) {
            let ch = text[at..].chars().next().unwrap_or_default();
            out.push(ch);
            at += ch.len_utf8();
            continue;
        }
        let end = at + bytes[at..].iter().take_while(|byte| name_byte(**byte)).count();
        if let Some((equals, value)) = secret_in_run(bytes, &escapes, at, end) {
            out.push_str(&text[at..end + equals]);
            out.push_str(REDACTED);
            at = end + equals + value;
        } else {
            out.push_str(&text[at..end]);
            at = end;
        }
    }
    out
}

/// Whether `byte` can be part of a parameter's name.
fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// For each `at` in `0..=bytes.len()`, whether `bytes[..at]` ends with a `%` and any number of `25`s: the start of a
/// percent escape, encoded again or not. Carried forward, so no prefix is walked back.
fn percent_escapes(bytes: &[u8]) -> Vec<bool> {
    let mut escapes = vec![false; bytes.len() + 1];
    for at in 1..=bytes.len() {
        escapes[at] = bytes[at - 1] == b'%' || (at >= 2 && bytes[at - 2..at] == *b"25" && escapes[at - 2]);
    }
    escapes
}

/// The lengths of the `=` and the (non-empty) value after the run of name bytes `bytes[start..end]`, if a
/// credential's name ([`secret_param`]) starts somewhere in the run, at a byte a name can start at ([`word_start`]).
/// Every such name ends at `end`: the `=` and the value are read once for the run, the value's length only for a
/// credential's.
fn secret_in_run(bytes: &[u8], escapes: &[bool], start: usize, end: usize) -> Option<(usize, usize)> {
    let equals = equals_len(&bytes[end..])?;
    if bytes.get(end + equals).is_none_or(|byte| value_end(*byte)) {
        return None;
    }
    let names = SecretSuffixes::new(&bytes[start..end]);
    (start..end).find(|&from| word_start(bytes, escapes, from) && names.secret(from - start))?;
    let value = bytes[end + equals..]
        .iter()
        .take_while(|byte| !value_end(**byte))
        .count();
    Some((equals, value))
}

/// Which suffixes of a run of name bytes are a credential's name ([`secret_param`]), each answered without reading
/// the suffix again: one holding one of [`SECRET_WORDS`] starts at or before the run's last such word, and one of
/// [`SECRET_NAMES`] or [`SECRET_PARAMS`] is compared whole (a name of another length is unequal at once).
struct SecretSuffixes {
    lower: Vec<u8>,
    last_word: Option<usize>,
}

impl SecretSuffixes {
    fn new(run: &[u8]) -> Self {
        let lower = run.to_ascii_lowercase();
        // a run of `name_byte`s: ASCII
        let last_word = std::str::from_utf8(&lower)
            .ok()
            .and_then(|text| SECRET_WORDS.iter().filter_map(|word| text.rfind(word)).max());
        Self { lower, last_word }
    }

    /// Whether the run's suffix from `from` is a credential's name: [`secret_param`] of it.
    fn secret(&self, from: usize) -> bool {
        let name = &self.lower[from..];
        self.last_word.is_some_and(|word| from <= word)
            || SECRET_NAMES
                .iter()
                .chain(SECRET_PARAMS.iter())
                .any(|secret| secret.as_bytes() == name)
    }
}

/// Whether a name can start at `at` in `bytes`: at the start, after a byte that can't be in a name, or after an escape
/// (`%XX`, `%25XX`, …, `\uXXXX`, `\xXX`) whose hex digits would otherwise read as the name's start. `percents` is
/// [`percent_escapes`] of `bytes`.
fn word_start(bytes: &[u8], percents: &[bool], at: usize) -> bool {
    let before = &bytes[..at];
    let Some(&last) = before.last() else {
        return true;
    };
    if !name_byte(last) {
        return true;
    }
    let hex_tail =
        |digits: usize| before.len() >= digits && before[before.len() - digits..].iter().all(u8::is_ascii_hexdigit);
    let escaped = |prefix: &[u8], digits: usize| {
        hex_tail(digits)
            && before.len() >= digits + prefix.len()
            && before[before.len() - digits - prefix.len()..before.len() - digits].eq_ignore_ascii_case(prefix)
    };
    escaped(b"\\u", 4) || escaped(b"\\x", 2) || (hex_tail(2) && percents[at - 2])
}

/// The length of an `=` in any of its forms at the start of `rest`: `=`; `%3D` (encoded again, `%253D`, …);
/// `\u003d` or `\x3d` (after any number of backslashes); an HTML character reference (`&#61;`, `&#x3d;`, `&equals;`).
fn equals_len(rest: &[u8]) -> Option<usize> {
    match rest.first()? {
        b'=' => Some(1),
        b'%' => {
            let mut at = 1;
            while rest[at..].starts_with(b"25") {
                at += 2;
            }
            rest.get(at..at + 2)
                .filter(|digits| digits.eq_ignore_ascii_case(b"3d"))
                .map(|_| at + 2)
        }
        b'\\' => {
            let slashes = rest.iter().take_while(|byte| **byte == b'\\').count();
            let escape = &rest[slashes..];
            [b"u003d".as_slice(), b"x3d".as_slice()]
                .iter()
                .find(|form| starts_with_ignore_case(escape, form))
                .map(|form| slashes + form.len())
        }
        b'&' => html_equals(rest),
        _ => None,
    }
}

/// The length of an HTML character reference to `=` at the start of `rest` (`&#61;`, `&#061;`, `&#x3d;`,
/// `&equals;`, in any case), its `&` escaped again any number of times (`&amp;#61;`: an `&#61;` typed in a message).
fn html_equals(rest: &[u8]) -> Option<usize> {
    let mut at = 1;
    while starts_with_ignore_case(&rest[at..], b"amp;") {
        at += 4;
    }
    if starts_with_ignore_case(&rest[at..], b"equals;") {
        return Some(at + 7);
    }
    let reference = rest[at..].strip_prefix(b"#")?;
    let (radix, digits) = match reference.first() {
        Some(b'x' | b'X') => (16, &reference[1..]),
        _ => (10, reference),
    };
    let count = digits.iter().take_while(|byte| byte.is_ascii_hexdigit()).count();
    if count == 0 || count > 8 || digits.get(count) != Some(&b';') {
        return None;
    }
    let number = std::str::from_utf8(&digits[..count]).ok()?;
    let prefix = rest.len() - digits.len();
    (u32::from_str_radix(number, radix).ok()? == u32::from(b'=')).then_some(prefix + count + 1)
}

/// Whether `text` starts with `prefix`, in any ASCII case.
fn starts_with_ignore_case(text: &[u8], prefix: &[u8]) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Whether `byte` ends a parameter's value: a space or a control character, a quote, `&`, `<`, `>`, `\`, `#`, `` ` ``.
fn value_end(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || byte.is_ascii_control()
        || matches!(byte, b'"' | b'\'' | b'&' | b'<' | b'>' | b'\\' | b'#' | b'`')
}

/// A form field's name with its `%XX` escapes and `+` decoded, as far as they make text (names are ASCII).
fn percent_decoded(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|digits| std::str::from_utf8(digits).ok())
            .and_then(|digits| u8::from_str_radix(digits, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                at += 1;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Redacts a JSON value in place: every field named as a credential ([`secret_param`]: the token, cookie, secret and
/// signature names and sign-in's), at any depth, the arguments of a `user.authenticate` event (`{"name":
/// "user.authenticate", "args": …}`), a string that is itself a JSON object or array (a Trouter delivery's `body`)
/// redacted as JSON and written back, and every other string's credential fields: a URL's query or fragment (sign-in's
/// redirect, `{"redirect": "…?code=…"}`; [`url`]) and every credential's `<name>=` in any escaped form (a SAS link in
/// a message's HTML; [`params`]).
pub fn json(value: &mut Value) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Value::Object(fields) => {
                let authenticate = fields
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == "user.authenticate");
                for (name, field) in fields.iter_mut() {
                    if secret_param(name) || (authenticate && name == "args") {
                        *field = Value::String(REDACTED.to_owned());
                    } else {
                        pending.push(field);
                    }
                }
            }
            Value::Array(items) => pending.extend(items.iter_mut()),
            Value::String(text) => string(text),
            Value::Number(_) | Value::Bool(_) | Value::Null => {}
        }
    }
}

/// A JSON string's text redacted in place ([`json`]): as JSON when it is a JSON object or array, otherwise as a URL
/// ([`url`]: its query's and fragment's credential fields and every credential's `<name>=`).
fn string(text: &mut String) {
    if text.trim_start().starts_with(['{', '['])
        && let Some(redacted) = json_text(text)
    {
        *text = redacted;
        return;
    }
    let redacted = url(text);
    if redacted != *text {
        *text = redacted;
    }
}

/// JSON text redacted ([`json`]); `None` when it isn't JSON.
#[must_use]
pub fn json_text(text: &str) -> Option<String> {
    let mut value: Value = serde_json::from_str(text).ok()?;
    json(&mut value);
    Some(value.to_string())
}

/// What kind of body a content type names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Json,
    Form,
    Text,
    Binary,
    Unknown,
}

fn kind(content_type: Option<&str>) -> Kind {
    let Some(content_type) = content_type else {
        return Kind::Unknown;
    };
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if essence.contains("json") {
        Kind::Json
    } else if essence == "application/x-www-form-urlencoded" {
        Kind::Form
    } else if essence.starts_with("text/") || essence.contains("xml") || essence.contains("javascript") {
        Kind::Text
    } else if essence.is_empty() {
        Kind::Unknown
    } else {
        Kind::Binary
    }
}

/// A body as the log writes it, by its content type: JSON with its credential fields redacted ([`json`]), a form with
/// its credential fields redacted ([`form`]), text with every credential's `<name>=` redacted ([`params`]: an HTML
/// page's links), anything else (a
/// picture, an image) as its type and size only. A body that says it is JSON and isn't, or whose type isn't given and
/// that is neither JSON nor text, is [`UNREADABLE`].
#[must_use]
pub fn body(content_type: Option<&str>, bytes: &[u8]) -> String {
    let size = bytes.len();
    if bytes.is_empty() {
        return "(empty)".to_owned();
    }
    let binary = || format!("[{}, {size} bytes]", content_type.unwrap_or("no content type"));
    let kind = kind(content_type);
    if kind == Kind::Binary {
        return binary();
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return binary();
    };
    let unreadable = || format!("{UNREADABLE} ({size} bytes)");
    match kind {
        Kind::Json => json_text(text).unwrap_or_else(unreadable),
        Kind::Form => form(text.trim()),
        Kind::Text => params(text),
        Kind::Unknown | Kind::Binary => json_text(text).unwrap_or_else(|| {
            let looks_like_form = !text.contains(char::is_whitespace) && text.contains('=');
            if looks_like_form { form(text) } else { unreadable() }
        }),
    }
}

/// A socket.io 0.9 frame (`<type>:<id>:<endpoint>:<data>`) as the log writes it: its JSON data redacted ([`json`]:
/// `user.authenticate`'s arguments and every credential field); a frame with no data as it is; a frame whose data
/// should be JSON and isn't, or that isn't a frame at all, [`UNREADABLE`].
#[must_use]
pub fn frame(text: &str) -> String {
    let unreadable = || format!("{UNREADABLE} ({} bytes)", text.len());
    let mut parts = text.splitn(4, ':');
    let (Some(kind), Some(id), Some(endpoint)) = (parts.next(), parts.next(), parts.next()) else {
        return unreadable();
    };
    let Some(data) = parts.next() else {
        // `2::`: a frame without its data separator carries none
        return match kind {
            "0" | "1" | "2" | "8" if id.len() + endpoint.len() < OPAQUE_RUN => text.to_owned(),
            _ => unreadable(),
        };
    };
    let head = format!("{kind}:{id}:{endpoint}:");
    match kind {
        // disconnect, connect, heartbeat, noop: no data of ours; an error's reason and advice
        "0" | "1" | "2" | "7" | "8" if data.len() < OPAQUE_RUN => format!("{head}{data}"),
        "3" | "4" | "5" if data.is_empty() => head,
        "3" | "4" | "5" => json_text(data).map_or_else(unreadable, |data| format!("{head}{data}")),
        // an ack: its id, then `+` and its JSON arguments, if any
        "6" => match data.split_once('+') {
            None if data.bytes().all(|byte| byte.is_ascii_digit()) => format!("{head}{data}"),
            Some((acked, arguments)) if acked.bytes().all(|byte| byte.is_ascii_digit()) => {
                json_text(arguments).map_or_else(unreadable, |arguments| format!("{head}{acked}+{arguments}"))
            }
            _ => unreadable(),
        },
        _ => unreadable(),
    }
}

/// Whether `byte` can be part of a token: base64url, base64 and the dots and tildes of JWTs and Entra's tokens.
fn opaque(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'=')
}

/// Whether `run` (a run of [`opaque`] bytes) is JWT-shaped: `eyJ`, at least two non-empty dot-separated parts, 20
/// characters or more.
fn jwt_shaped(run: &str) -> bool {
    run.starts_with("eyJ") && run.split('.').filter(|part| !part.is_empty()).count() >= 2 && run.len() >= 20
}

/// The last pass over a whole line (see the module docs): every credential's `<name>=<value>` ([`params`]), then
/// JWT-shaped strings, `skypetoken=` and `Bearer ` values, and runs of [`OPAQUE_RUN`] or more token characters become
/// [`REDACTED`].
#[must_use]
pub fn line(text: &str) -> String {
    let text = params(text);
    let text = text.as_str();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    // `at` always sits on a char boundary: runs and prefixes are ASCII
    while at < bytes.len() {
        if let Some(prefix) = credential_prefix(&text[at..]) {
            out.push_str(&text[at..at + prefix]);
            at += prefix;
            let value = text[at..]
                .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | '&' | '}' | ']' | ')'))
                .unwrap_or(text.len() - at);
            if value > 0 {
                out.push_str(REDACTED);
            }
            at += value;
            continue;
        }
        if opaque(bytes[at]) && (at == 0 || !opaque(bytes[at - 1])) {
            let run = bytes[at..].iter().take_while(|byte| opaque(**byte)).count();
            let candidate = &text[at..at + run];
            if run >= OPAQUE_RUN || jwt_shaped(candidate) {
                out.push_str(REDACTED);
            } else {
                out.push_str(candidate);
            }
            at += run;
            continue;
        }
        let ch = text[at..].chars().next().unwrap_or_default();
        out.push(ch);
        at += ch.len_utf8();
    }
    out
}

/// The length of a credential's prefix at the start of `rest` (`Bearer `, `skypetoken=`, `skypetoken: `, in any case),
/// if one is there and is a word of its own.
fn credential_prefix(rest: &str) -> Option<usize> {
    const PREFIXES: [&str; 3] = ["bearer ", "skypetoken=", "skypetoken: "];
    let head: String = rest.chars().take(12).collect::<String>().to_ascii_lowercase();
    PREFIXES
        .iter()
        .find(|prefix| head.starts_with(**prefix))
        .map(|prefix| prefix.len())
}

/// Text with every control character but none written visibly (`\n` as `\\n`, others as `\u{…}`), so a record is one
/// line of the file.
#[must_use]
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, equals_len, name_byte, params, secret_param, value_end};

    /// Round 2's `params`, which walked back through every escape and read a run's name, `=` and value again at
    /// every byte of it: the reference the linear scan must agree with on every text.
    fn reference(text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        while at < bytes.len() {
            if let Some((name, equals, value)) = reference_at(bytes, at) {
                out.push_str(&text[at..at + name + equals]);
                out.push_str(REDACTED);
                at += name + equals + value;
                continue;
            }
            let ch = text[at..].chars().next().unwrap_or_default();
            out.push(ch);
            at += ch.len_utf8();
        }
        out
    }

    fn reference_at(bytes: &[u8], at: usize) -> Option<(usize, usize, usize)> {
        if !reference_word_start(&bytes[..at]) {
            return None;
        }
        let rest = &bytes[at..];
        let name = rest.iter().take_while(|byte| name_byte(**byte)).count();
        if name == 0 {
            return None;
        }
        let equals = equals_len(&rest[name..])?;
        let value = rest[name + equals..]
            .iter()
            .take_while(|byte| !value_end(**byte))
            .count();
        let name_text = std::str::from_utf8(&rest[..name]).ok()?;
        (value > 0 && secret_param(name_text)).then_some((name, equals, value))
    }

    fn reference_word_start(before: &[u8]) -> bool {
        let Some(&last) = before.last() else {
            return true;
        };
        if !name_byte(last) {
            return true;
        }
        let hex_tail =
            |digits: usize| before.len() >= digits && before[before.len() - digits..].iter().all(u8::is_ascii_hexdigit);
        let escaped = |prefix: &[u8], digits: usize| {
            hex_tail(digits)
                && before.len() >= digits + prefix.len()
                && before[before.len() - digits - prefix.len()..before.len() - digits].eq_ignore_ascii_case(prefix)
        };
        escaped(b"\\u", 4) || escaped(b"\\x", 2) || (hex_tail(2) && reference_escape(&before[..before.len() - 2]))
    }

    fn reference_escape(mut before: &[u8]) -> bool {
        loop {
            match before {
                [.., b'%'] => return true,
                [rest @ .., b'2', b'5'] => before = rest,
                _ => return false,
            }
        }
    }

    /// Pieces a text is made of: names (credentials' and others', in any case, run together or not), every form of a
    /// separator, an escape and an `=`, value bytes and value ends, and a non-ASCII character.
    const PIECES: [&str; 44] = [
        "sig",
        "SIG",
        "code",
        "Code_Verifier",
        "client_secret",
        "assertion",
        "proxy-authorization",
        "set-cookie",
        "skypeToken",
        "x-ms-signature",
        "password1",
        "aeskey",
        "passcode",
        "sv",
        "a",
        "x",
        "q.",
        "_",
        "-",
        "41",
        "2",
        "5",
        "25",
        "3D",
        "u003d",
        "x3d",
        "0026",
        "%",
        "=",
        "&",
        "amp;",
        "#61;",
        "#x3d;",
        "equals;",
        "#",
        "\\",
        "\\u",
        "?",
        " ",
        "\"",
        ";",
        "<",
        "é",
        "value",
    ];

    /// A small xorshift generator: the same texts on every run.
    struct Texts(u64);

    impl Texts {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn piece(&mut self) -> &'static str {
            let at = self.next() % PIECES.len() as u64;
            PIECES[usize::try_from(at).unwrap_or_default()]
        }

        fn text(&mut self) -> String {
            let pieces = 1 + self.next() % 24;
            (0..pieces).map(|_| self.piece()).collect()
        }
    }

    #[test]
    fn the_linear_scan_redacts_exactly_what_the_reference_does() {
        let mut texts = Texts(0x9e37_79b9_7f4a_7c15);
        let mut redacted = 0;
        for _ in 0..200_000 {
            let text = texts.text();
            let expected = reference(&text);
            assert_eq!(params(&text), expected, "{text:?}");
            redacted += usize::from(expected != text);
        }
        // the texts reach the redaction often, not only the pass-through
        assert!(redacted > 10_000, "{redacted}");
    }

    #[test]
    fn crafted_runs_redact_as_the_reference_does() {
        let runs = [
            format!("%{}", "25".repeat(300)),
            format!("%{}%253Dvalue", "25".repeat(300)),
            format!("%{}sig=1", "25".repeat(300)),
            format!("%{}41sig=1 {}x_token=2", "25".repeat(300), "a".repeat(300)),
            format!("{}sig=1 \\u0041{}code=2", "b".repeat(300), "c".repeat(300)),
            "a=".repeat(300),
            "sig=".repeat(300),
            format!("x&#{}", "1".repeat(300)),
        ];
        for text in runs {
            assert_eq!(params(&text), reference(&text), "{text:?}");
        }
    }
}
