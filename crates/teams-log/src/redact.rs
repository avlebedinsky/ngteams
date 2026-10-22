//! Redaction: what the log never writes, taken out by the logger itself, never left to a call site.
//!
//! - **By name**: a header whose name is a credential's ([`secret_field`]: `Authorization`, `Cookie`, `Set-Cookie`,
//!   `X-Skypetoken`, `Authentication`, any name holding `token`); a JSON field of the same names (`access_token`,
//!   `refresh_token`, `id_token`, `skypetoken`, `token`, …), at any depth; a form or query field of those names and
//!   of sign-in's (`code`, `code_verifier`, `client_secret`, …: [`secret_param`]); the arguments of Trouter's
//!   `user.authenticate`.
//! - **What can't be read is redacted whole**: a body that says it is JSON and isn't, a frame whose data isn't JSON,
//!   are never written raw ([`UNREADABLE`]). A binary body (a picture) is written as its type and size only.
//! - **The last pass** ([`line`]): every line, any record's, has its JWT-shaped strings, `skypetoken=…` and
//!   `Bearer …` values, and long opaque runs (a refresh token, an authorization code) replaced, whatever put them
//!   there (a panic message, an error's text).

use std::fmt::Write as _;

use serde_json::Value;

/// What a redacted value is written as.
pub const REDACTED: &str = "[redacted]";

/// What a body or frame the redactor can't read is written as (with its size).
pub const UNREADABLE: &str = "[unreadable, redacted whole]";

/// The shortest run of token characters ([`opaque`]) taken as a credential by the last pass: longer than any id the
/// client logs (an MRI, a chat id, a message id), shorter than a refresh token or an authorization code.
pub const OPAQUE_RUN: usize = 100;

/// Header and JSON field names that always hold a credential, besides any name holding `token`.
const SECRET_NAMES: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "authentication",
    "password",
];

/// Form and query field names of sign-in and renewal that hold a credential, besides [`SECRET_NAMES`].
const SECRET_PARAMS: [&str; 5] = [
    "code",
    "code_verifier",
    "client_secret",
    "client_assertion",
    "assertion",
];

/// Whether a header or JSON field named `name` holds a credential: one of [`SECRET_NAMES`] or any name holding
/// `token`, in any case (`X-Skypetoken`, `skypeToken`, `refresh_token`).
#[must_use]
pub fn secret_field(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name.contains("token") || SECRET_NAMES.contains(&name.as_str())
}

/// Whether a form or query field named `name` holds a credential: [`secret_field`]'s names and sign-in's own
/// ([`SECRET_PARAMS`]: the authorization code, the PKCE verifier, …).
#[must_use]
pub fn secret_param(name: &str) -> bool {
    secret_field(name) || SECRET_PARAMS.contains(&name.trim().to_ascii_lowercase().as_str())
}

/// A header's value as the log writes it: [`REDACTED`] for a credential's header.
#[must_use]
pub fn header_value<'a>(name: &str, value: &'a str) -> &'a str {
    if secret_field(name) { REDACTED } else { value }
}

/// A URL as the log writes it: its query's and fragment's credential fields ([`secret_param`]) redacted, the rest as
/// it is.
#[must_use]
pub fn url(url: &str) -> String {
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

/// `name=value&…` text (a form body, a query) with its credential fields' values ([`secret_param`]) redacted.
#[must_use]
pub fn form(text: &str) -> String {
    text.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if secret_param(&percent_decoded(name)) => format!("{name}={REDACTED}"),
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&")
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

/// Redacts a JSON value in place: every field named as a credential ([`secret_field`]), at any depth, and the
/// arguments of a `user.authenticate` event (`{"name": "user.authenticate", "args": …}`).
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
                    if secret_field(name) || (authenticate && name == "args") {
                        *field = Value::String(REDACTED.to_owned());
                    } else {
                        pending.push(field);
                    }
                }
            }
            Value::Array(items) => pending.extend(items.iter_mut()),
            Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
        }
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
/// its credential fields redacted ([`form`]), text as it is (the last pass still runs over it), anything else (a
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
        Kind::Text => text.to_owned(),
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

/// Whether `rest` (a run of [`opaque`] bytes) is JWT-shaped: `eyJ`, then three dot-separated parts.
fn jwt_shaped(run: &str) -> bool {
    run.starts_with("eyJ") && run.split('.').filter(|part| !part.is_empty()).count() >= 2 && run.len() >= 20
}

/// The last pass over a whole line (see the module docs): JWT-shaped strings, `skypetoken=` and `Bearer ` values,
/// and runs of [`OPAQUE_RUN`] or more token characters become [`REDACTED`].
#[must_use]
pub fn line(text: &str) -> String {
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
