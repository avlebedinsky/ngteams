//! Endpoint-only URL and path shapes for diagnostic records.

use percent_encoding::percent_decode_str;
use teams_log::redact::secret_param;
use url::Url;

/// What [`log_url`] writes for a path segment, a segment's value or a query key that looks like a credential.
pub const REDACTED: &str = "[redacted]";

/// What [`log_url`] writes for a URL it can't parse, or one without a path to keep (`data:`, `blob:`, `about:`).
pub const UNREADABLE_URL: &str = "[unreadable url]";

/// The shortest path segment or query key [`log_url`] takes as an opaque credential when it holds only token
/// characters: longer than any id a path carries (a 1:1 chat id is 91 characters and holds `:` and `@`), shorter than
/// any token.
const OPAQUE_RUN: usize = 100;

/// A URL for payload-free diagnostics: what names the endpoint, and nothing a credential travels in. It keeps the scheme, the
/// host, an explicit port, the path and the query's keys (`?i&e&skypeToken`), never a query value, the fragment or
/// the user info, so a credential nobody listed yet stays out wherever a query or a fragment carries it. In the path,
/// a segment that looks like a credential is [`REDACTED`]: a JWT or base64 JSON (`eyJ…`), a run of 100 or more token
/// characters, the segment after one named as a credential ([`secret_param`]: `/skypetoken/<token>`), and the
/// value of any `name=value` segment (`;sig=…`), whatever its name; a query key that looks like a token goes the same
/// way. An empty URL (an unknown socket's) and a bare `host:port` (a `CONNECT`'s target) stay as they are; one that
/// can't be parsed, or has no path to keep, is [`UNREADABLE_URL`].
#[must_use]
pub fn log_url(url: &str) -> String {
    if url.starts_with(['?', '#']) {
        // Nothing before the query or fragment: no endpoint to name.
        return String::new();
    }
    if url.is_empty() || authority_form(url) {
        return url.to_owned();
    }
    let Ok(parsed) = Url::parse(url) else {
        return UNREADABLE_URL.to_owned();
    };
    let Some(segments) = parsed.path_segments() else {
        return UNREADABLE_URL.to_owned();
    };
    let mut out = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or_default());
    if let Some(port) = parsed.port() {
        out = format!("{out}:{port}");
    }
    let mut after_credential_word = false;
    for segment in segments {
        out.push('/');
        out.push_str(&log_segment(segment, after_credential_word));
        after_credential_word = credential_name(segment);
    }
    if let Some(query) = parsed.query() {
        out.push('?');
        out.push_str(&query_keys(query));
    }
    out
}

/// Whether `target` is a bare `host:port` (a `CONNECT`'s target): kept as it is, having nowhere to carry a credential.
fn authority_form(target: &str) -> bool {
    !target.contains("://")
        && target.contains(':')
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']'))
        && target.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// One path segment as [`log_url`] writes it, `after_credential_name` when the segment before it names a credential.
fn log_segment(segment: &str, after_credential_name: bool) -> String {
    if after_credential_name || opaque(segment) {
        return REDACTED.to_owned();
    }
    match segment.split_once('=') {
        Some((name, _)) => format!("{name}={REDACTED}"),
        None => segment.to_owned(),
    }
}

/// A query's keys, without their values, in order: `i=1&e=2&skypeToken=…` is `i&e&skypeToken`.
fn query_keys(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .map(|key| if opaque(key) { REDACTED } else { key })
        .collect::<Vec<_>>()
        .join("&")
}

/// Whether a path segment or a query key (percent-encoded, as the URL holds it) looks like a credential by its form: a
/// JWT or base64 JSON (`eyJ…`), or a run of [`OPAQUE_RUN`] or more token characters.
fn opaque(text: &str) -> bool {
    let decoded = percent_decode_str(text).decode_utf8_lossy();
    decoded.starts_with("eyJ")
        || (decoded.len() >= OPAQUE_RUN
            && decoded.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
            }))
}

/// Whether a path segment names a credential, using the diagnostic logger's shared names.
fn credential_name(segment: &str) -> bool {
    let name = percent_decode_str(segment).decode_utf8_lossy().to_ascii_lowercase();
    secret_param(&name)
}

/// The longest path segment [`path_shape`] keeps as it is.
const MAX_WORD: usize = 32;

/// The shape of a URL path (as [`Url::path`] gives it, percent-encoded): each segment percent-decoded and, when it is
/// an id, replaced by its kind, so that the shape names an endpoint and not whom it concerns:
/// - `<chat>`: a conversation id (`19:…`, a chat's, a channel's or a meeting's);
/// - `<mri>`: any other MRI (`8:orgid:…`, `28:…`), but a named `48:` stream (`48:notifications`) stays as it is;
/// - `<guid>`: a GUID;
/// - `<message>`: a number of 6 digits or more (a message id, a timestamp);
/// - `<id>`: anything else that isn't a short word: longer than 32 characters, holding a character other than ASCII
///   letters, digits and `. _ ~ $ -`, or a run of 6 digits or more (an object id, an endpoint id, a token).
///
/// Every other segment is a word of the API and stays.
#[must_use]
pub fn path_shape(path: &str) -> String {
    path.split('/')
        .map(|segment| shape_segment(&percent_decode_str(segment).decode_utf8_lossy()))
        .collect::<Vec<_>>()
        .join("/")
}

fn shape_segment(segment: &str) -> String {
    if segment.is_empty() {
        return String::new();
    }
    let kind = if segment.starts_with("19:") {
        "<chat>"
    } else if let Some((prefix, rest)) = segment.split_once(':')
        && !prefix.is_empty()
        && prefix.bytes().all(|byte| byte.is_ascii_digit())
    {
        if prefix == "48" && !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_lowercase()) {
            return segment.to_owned();
        }
        "<mri>"
    } else if is_guid_text(segment) {
        "<guid>"
    } else if segment.len() >= 6 && segment.bytes().all(|byte| byte.is_ascii_digit()) {
        "<message>"
    } else if is_word(segment) {
        return segment.to_owned();
    } else {
        "<id>"
    };
    kind.to_owned()
}

/// A short word of an API: at most [`MAX_WORD`] ASCII letters, digits and `. _ ~ $ -`, without a run of 6 digits.
fn is_word(segment: &str) -> bool {
    segment.len() <= MAX_WORD
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'$' | b'-'))
        && !segment
            .as_bytes()
            .windows(6)
            .any(|run| run.iter().all(u8::is_ascii_digit))
}

/// A GUID, `8-4-4-4-12` hex digits, in braces or not.
fn is_guid_text(segment: &str) -> bool {
    let bare = segment
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(segment);
    let groups: Vec<&str> = bare.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
