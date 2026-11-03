//! Exchange-scoped credential values, including short values echoed outside credential-named fields.

use std::fmt;

use super::{Request, Response, header};
use crate::redact::{self, REDACTED};

/// A wire record formatted with its normal redaction plus the known credentials of this exchange. Raw values,
/// decoded JSON strings (including nested JSON), and URL/form percent escapes in either hex case are supported;
/// form `+` and `%20` both match a space. Other transformations are not inferred. This context is borrowed only
/// for formatting, never kept by the logger.
pub struct WithSecrets<'a, T> {
    record: T,
    secrets: Secrets<'a>,
}

impl Request<'_> {
    /// Also redacts these exchange-scoped values wherever a request echoes them, before its normal redaction.
    #[must_use]
    pub fn with_secrets<'a>(self, secrets: &'a [&'a str]) -> WithSecrets<'a, Self> {
        WithSecrets {
            record: self,
            secrets: Secrets(secrets),
        }
    }
}

impl Response<'_> {
    /// Also redacts these exchange-scoped values wherever a response echoes them, before its normal redaction.
    #[must_use]
    pub fn with_secrets<'a>(self, secrets: &'a [&'a str]) -> WithSecrets<'a, Self> {
        WithSecrets {
            record: self,
            secrets: Secrets(secrets),
        }
    }
}

impl fmt::Display for WithSecrets<'_, Request<'_>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let request = &self.record;
        write!(
            f,
            "request {} {} headers {}",
            self.secrets.text(request.method),
            redact::url(&self.secrets.text(request.url)),
            Headers {
                entries: request.headers,
                secrets: self.secrets
            }
        )?;
        if let Some(body) = request.body {
            write!(
                f,
                " body {}",
                self.secrets.body(header(request.headers, "content-type"), body)
            )?;
        }
        Ok(())
    }
}

impl fmt::Display for WithSecrets<'_, Response<'_>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let response = &self.record;
        write!(
            f,
            "response {} {} -> {} in {} ms headers {} body {}",
            self.secrets.text(response.method),
            redact::url(&self.secrets.text(response.url)),
            response.status,
            response.elapsed.as_millis(),
            Headers {
                entries: response.headers,
                secrets: self.secrets
            },
            self.secrets
                .body(header(response.headers, "content-type"), response.body)
        )
    }
}

struct Headers<'a, 's> {
    entries: &'a [(String, String)],
    secrets: Secrets<'s>,
}

impl fmt::Display for Headers<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (at, (name, value)) in self.entries.iter().enumerate() {
            if at > 0 {
                f.write_str(", ")?;
            }
            let value = self.secrets.text(value);
            write!(f, "{}: {}", self.secrets.text(name), redact::header_value(name, &value))?;
        }
        f.write_str("}")
    }
}

#[derive(Clone, Copy)]
struct Secrets<'a>(&'a [&'a str]);

impl Secrets<'_> {
    fn text(self, text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut at = 0;
        while at < text.len() {
            let longest = self
                .0
                .iter()
                .filter(|secret| !secret.is_empty())
                .filter_map(|secret| match_end(&text.as_bytes()[at..], secret.as_bytes()))
                .max();
            if let Some(length) = longest {
                result.push_str(REDACTED);
                at += length;
            } else if let Some(character) = text[at..].chars().next() {
                result.push(character);
                at += character.len_utf8();
            }
        }
        result
    }

    fn body(self, content_type: Option<&str>, bytes: &[u8]) -> String {
        redact::body_with(content_type, bytes, &|text| self.text(text))
    }
}

/// A set of possible input offsets makes literal `%` values and percent escapes unambiguous without exponential
/// backtracking. The longest complete representation wins over shorter, overlapping credentials.
fn match_end(text: &[u8], secret: &[u8]) -> Option<usize> {
    let mut positions = vec![0];
    for wanted in secret {
        let mut next = Vec::new();
        for at in positions {
            extend_matches(&mut next, text, at, *wanted);
        }
        if next.is_empty() {
            return None;
        }
        next.sort_unstable();
        next.dedup();
        positions = next;
    }
    positions.into_iter().max()
}

fn extend_matches(next: &mut Vec<usize>, text: &[u8], at: usize, wanted: u8) {
    let tail = &text[at..];
    if tail.first() == Some(&wanted) || (wanted == b' ' && tail.first() == Some(&b'+')) {
        next.push(at + 1);
    }
    if percent_byte(tail) == Some(wanted) {
        next.push(at + 3);
    }
}

fn percent_byte(text: &[u8]) -> Option<u8> {
    let [b'%', high, low, ..] = text else {
        return None;
    };
    let high = char::from(*high).to_digit(16)?;
    let low = char::from(*low).to_digit(16)?;
    u8::try_from((high << 4) | low).ok()
}
