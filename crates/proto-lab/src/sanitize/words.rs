//! What a name value gives as words, and where a text holds a host: shared by the sanitizer's name rule and the
//! export's own check for names (`backstop.rs`), so both read a name the same way.
//!
//! A name value's words are its runs of letters and digits of [`MIN_WORD`] characters or more, apart from those of a
//! part that is an identifier: an email or UPN (`jane.doe@contoso.com`), an MRI (`8:orgid:…`, `28:…`, `tel:…`), a URL,
//! a GUID, or a host name (`lab.example.org`: three labels or more, lowercase, the last a top-level domain of the
//! Public Suffix List). The identifier rules replace those whole; their pieces (`com`, `contoso`, `orgid`) are no
//! person's name, and learning them would replace every host and path that holds them. A dotted part that may be a
//! person's (`jane.wu`, `Mei.Li`, `Doe.CA`) gives its words, but never a lowercase last label that is a top-level
//! domain (`contoso.com` gives `contoso`; the sanitizer leaves it inside a real host, and the export warns). A run of
//! digits alone is no word of a name either.
//!
//! A host in a text is a run of letters, digits, `-` and `.` whose last label is a top-level domain (of the Public
//! Suffix List, a reserved one, or any two letters), without the tail of an escape glued to its start (the second `2F`
//! of `%2F%2Fapi.example.test`). That shape is broad on purpose, and [`Host`] tells what such a run is:
//! - one under [`PUBLIC_DOMAINS`] (`teams.microsoft.com`, `login.microsoftonline.com`) is a public protocol host
//!   ([`Host::Public`]): its labels are never a name's (a group called "Microsoft Teams" doesn't make
//!   `teams.microsoft.com` a name), so the sanitizer leaves them and the check doesn't look in them. A lookalike that
//!   only holds one (`teams.microsoft.com.example.test`) isn't one;
//! - any other is a real host ([`Host::Real`]) only where it clearly is one: a URL's authority (after `://`, as written
//!   or encoded, `https%3A%2F%2F` or `https:\/\/`, or after a `//` that starts a value), the value of a `Host`-style
//!   header (`"host":"…"`, `":authority":"…"`, `Host: …` at the start of a line) or of a cookie's `Domain=`, or a
//!   host name by the rule a name value's parts are read by (`lab.example.org`: three labels or more, lowercase, the
//!   last a known top-level domain). The sanitizer leaves a word of a name inside one, so as not to destroy the host,
//!   and the export warns about it;
//! - anything else only looks like one ([`Host::Shaped`]): a dotted name (`Mei.Li`, `jane.wu`, `Jane.Doe.HR`), a file
//!   name (`notes-jane.md`), prose with no space after a full stop (`met Quill.So we`). A word of a name there is
//!   replaced as anywhere else.

use std::iter;
use std::ops::Range;

use super::layers::decode_all;

/// The shortest word of a name that counts.
pub(super) const MIN_WORD: usize = 3;

/// The registrable domains of Microsoft's public services: their hosts carry the protocol, never a tenant or a person.
/// Tenant-specific ones (`sharepoint.com`, `onmicrosoft.com`, `windows.net`, `azurewebsites.net`, `dynamics.com`) are
/// not here: the sanitizer redacts the tenant's part of those.
const PUBLIC_DOMAINS: [&str; 37] = [
    "microsoft.com",
    "microsoftonline.com",
    "microsoftonline-p.com",
    "microsoft.us",
    "cloud.microsoft",
    "static.microsoft",
    "live.com",
    "live.net",
    "skype.com",
    "skype.net",
    "skypeassets.com",
    "lync.com",
    "office.com",
    "office.net",
    "office365.com",
    "office365.us",
    "outlook.com",
    "sharepointonline.com",
    "onenote.com",
    "yammer.com",
    "microsoftstream.com",
    "msauth.net",
    "msftauth.net",
    "msauthimages.net",
    "msidentity.com",
    "msecnd.net",
    "msocdn.com",
    "sfbassets.com",
    "azureedge.net",
    "azure.com",
    "windows.com",
    "bing.com",
    "msn.com",
    "aka.ms",
    "gfx.ms",
    "svc.ms",
    "trouter.io",
];

/// Header keys (lowercased) whose value is a host: the request's own (`Host`, HTTP/2's `:authority`).
pub(super) const HOST_KEYS: [&str; 4] = ["host", ":authority", "authority", "x-forwarded-host"];

/// How far back from a host its context (`https%253A%252F%252F`, `"x-forwarded-host": "`) is looked for, in
/// characters.
const CONTEXT: usize = 48;

/// What a host-shaped run of a text is (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Host {
    /// Under [`PUBLIC_DOMAINS`]: no label of it is a name's.
    Public,
    /// Clearly a host: a URL's authority, a `Host`-style header's value, a cookie's domain, or a host name.
    Real,
    /// Only shaped like one: a dotted name, a file name, prose.
    Shaped,
}

/// Top-level domains of names reserved for tests, documentation and local networks, which the Public Suffix List
/// doesn't hold.
const RESERVED_TLDS: [&str; 6] = ["test", "example", "invalid", "localhost", "local", "internal"];

/// The words of a name value (see the module docs).
pub(super) fn learnable_words(name: &str) -> Vec<&str> {
    name.split(is_part_break)
        .filter(|part| !is_identifier(part))
        .flat_map(|part| without_host_tld(part).split(|char: char| !char.is_alphanumeric()))
        .filter(|word| word.chars().count() >= MIN_WORD && !word.chars().all(|char| char.is_ascii_digit()))
        .collect()
}

/// A dotted part of a name without its last label when that is a top-level domain written as a host writes one
/// (lowercase: `contoso.com` gives `contoso`), so a name never teaches `com` and rewrites every host that ends in it.
fn without_host_tld(part: &str) -> &str {
    match part.trim_end_matches(TRAILING).rsplit_once('.') {
        Some((rest, last)) if !last.bytes().any(|byte| byte.is_ascii_uppercase()) && is_known_tld(last) => rest,
        _ => part,
    }
}

/// What separates the parts of a name: spaces, brackets, quotes and list punctuation, never `.`, `@`, `:`, `/` or `-`,
/// which an identifier holds.
fn is_part_break(char: char) -> bool {
    char.is_whitespace()
        || matches!(
            char,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '"' | '|'
        )
}

/// Punctuation around a part of a name that is no part of an identifier in it.
const TRAILING: [char; 5] = ['.', '\'', '!', '?', ':'];

/// Whether a part of a name is an identifier: an email, an MRI, a URL, a GUID or a host name.
fn is_identifier(part: &str) -> bool {
    let part = part.trim_matches(TRAILING);
    let email = part
        .rsplit_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    let mri = part.split_once(':').is_some_and(|(prefix, rest)| {
        !rest.is_empty() && (prefix.eq_ignore_ascii_case("tel") || prefix.bytes().all(|byte| byte.is_ascii_digit()))
    });
    email || mri || part.contains("://") || has_guid(part) || is_host_name(part)
}

/// Whether a dotted part of a name is a host name and no person's dotted name: three labels or more, written as a
/// host writes them (lowercase), the last a known top-level domain (`lab.example.org`). A name may hold dots too, and
/// one of two labels (`jane.wu`, `mei.li`, `Doe.CA`) or one with capitals (`Jane.Doe.HR`) is a person's, so its
/// words are learned: when unsure, a word too many is replaced, never a name left.
fn is_host_name(part: &str) -> bool {
    let labels: Vec<&str> = part.split('.').collect();
    let lowercase = !part.bytes().any(|byte| byte.is_ascii_uppercase());
    labels.len() >= 3 && lowercase && is_domain(part) && labels.last().is_some_and(|last| is_known_tld(last))
}

/// Whether `text` holds a GUID (`8-4-4-4-12` hex digits).
fn has_guid(text: &str) -> bool {
    text.split(|char: char| !(char.is_ascii_hexdigit() || char == '-'))
        .any(|run| {
            let groups: Vec<usize> = run.split('-').map(str::len).collect();
            groups.windows(5).any(|window| window == [8, 4, 4, 4, 12])
        })
}

/// Whether `token` is shaped as a domain: labels of letters, digits and `-`, at least two, the last one of letters,
/// two of them or a known top-level domain. Broad on purpose: it finds the hosts in a text (see the module docs).
fn is_domain(token: &str) -> bool {
    let labels: Vec<&str> = token.split('.').collect();
    let well_formed = labels
        .iter()
        .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    labels.len() >= 2 && well_formed && labels.last().is_some_and(|last| is_tld(last))
}

fn is_tld(label: &str) -> bool {
    let letters = label.bytes().all(|byte| byte.is_ascii_alphabetic());
    letters && (label.len() == 2 || is_known_tld(label))
}

/// Whether `label` is a top-level domain of the Public Suffix List (ICANN's and the reserved ones above), in any case.
fn is_known_tld(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    let shaped = !lower.is_empty() && lower.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    let listed = || {
        psl::suffix(lower.as_bytes()).is_some_and(|suffix| suffix.is_known() && suffix.as_bytes() == lower.as_bytes())
    };
    shaped && (RESERVED_TLDS.contains(&lower.as_str()) || listed())
}

fn is_public(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    PUBLIC_DOMAINS
        .iter()
        .any(|domain| host == *domain || host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.')))
}

/// Every host-shaped run in `text`, in order, with what it is.
pub(super) fn hosts(text: &str) -> Vec<(Range<usize>, Host)> {
    let mut found = Vec::new();
    let mut start = None;
    for (index, char) in text.char_indices().chain(iter::once((text.len(), ' '))) {
        if char.is_ascii_alphanumeric() || char == '-' || char == '.' {
            start.get_or_insert(index);
            continue;
        }
        let Some(begin) = start.take() else {
            continue;
        };
        let begin = begin + escape_tail(&text[..begin], &text[begin..index]);
        let run = &text[begin..index];
        let host = run.trim_matches(['.', '-']);
        if is_domain(host) {
            let at = begin + (run.len() - run.trim_start_matches(['.', '-']).len());
            found.push((at..at + host.len(), kind(&text[..at], host)));
        }
    }
    found
}

/// What the host `host`, found after `before`, is. Its context is read without NULs, which the name check puts around a
/// decoded character (`backstop.rs`).
fn kind(before: &str, host: &str) -> Host {
    if is_public(host) {
        return Host::Public;
    }
    if is_host_name(host) {
        return Host::Real;
    }
    let before = context(before).replace('\0', "");
    if after_authority(&before) || after_host_key(&before) {
        Host::Real
    } else {
        Host::Shaped
    }
}

/// How many bytes at the start of `run` (letters, digits, `-` and `.`) are the tail of an escape `before` ends in: the
/// hex digits of a percent escape (`2F`, or `252F` for `%252F`, after a `%`), or of a `\uXXXX` or a `\xXX`.
fn escape_tail(before: &str, run: &str) -> usize {
    let bytes = run.as_bytes();
    let hex = |from: usize, len: usize| {
        bytes
            .get(from..from + len)
            .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
    };
    if before.ends_with('%') {
        let mut skip = 0;
        while bytes[skip..].starts_with(b"25") && hex(skip + 2, 2) {
            skip += 2;
        }
        return if hex(skip, 2) { skip + 2 } else { 0 };
    }
    if before.ends_with('\\') {
        return match bytes.first() {
            Some(b'u') if hex(1, 4) => 5,
            Some(b'x') if hex(1, 2) => 3,
            _ => 0,
        };
    }
    0
}

/// The last [`CONTEXT`] characters of `before`: as far back as a host's context is looked for.
fn context(before: &str) -> &str {
    let start = before
        .char_indices()
        .rev()
        .nth(CONTEXT - 1)
        .map_or(0, |(index, _)| index);
    &before[start..]
}

/// Whether a host found after `before` is a URL's authority: once every layer of escapes is decoded, `before` ends in
/// `://`, or in a `//` that starts a value (a protocol-relative URL).
fn after_authority(before: &str) -> bool {
    let decoded = decode_all(before);
    if decoded.ends_with("://") {
        return true;
    }
    decoded.strip_suffix("//").is_some_and(|rest| {
        !rest
            .chars()
            .next_back()
            .is_some_and(|char| char.is_alphanumeric() || matches!(char, '/' | '.' | '-' | '_' | '%' | '\\'))
    })
}

/// Whether a host found after `before` is the value of a `Host`-style header ([`HOST_KEYS`]: `"host":"` or
/// `":authority": "` in JSON, `Host: ` at the start of a line) or a cookie's domain (`; Domain=.`).
fn after_host_key(before: &str) -> bool {
    let value = before.trim_end_matches('.');
    let json = value
        .strip_suffix('"')
        .map(str::trim_end)
        .and_then(|rest| rest.strip_suffix(':'))
        .map(str::trim_end)
        .and_then(|rest| rest.strip_suffix('"'))
        .is_some_and(|key| ends_in_key(key, &HOST_KEYS, |before| before.ends_with('"')));
    // a line starts at the start of the text or after a line break, as written or escaped (`\r\n` in a JSON string)
    let line_start = |before: &str| {
        before.is_empty() || before.ends_with(['\n', '\r']) || before.ends_with("\\n") || before.ends_with("\\r")
    };
    let line = value
        .trim_end_matches([' ', '\t'])
        .strip_suffix(':')
        .is_some_and(|key| ends_in_key(key, &HOST_KEYS, line_start));
    let cookie = value.strip_suffix('=').is_some_and(|key| {
        ends_in_key(key, &["domain"], |before| {
            before.is_empty() || before.ends_with(|char: char| char.is_whitespace() || matches!(char, ';' | '"'))
        })
    });
    json || line || cookie
}

/// Whether `text` ends in one of `keys` (in any case), after a text that `opens` takes.
fn ends_in_key(text: &str, keys: &[&str], opens: impl Fn(&str) -> bool) -> bool {
    keys.iter().any(|key| {
        let Some(at) = text.len().checked_sub(key.len()) else {
            return false;
        };
        text.is_char_boundary(at) && text[at..].eq_ignore_ascii_case(key) && opens(&text[..at])
    })
}

/// Whether `range` of a text lies inside one of its `hosts`, and what that host is.
pub(super) fn host_of(hosts: &[(Range<usize>, Host)], range: &Range<usize>) -> Option<Host> {
    hosts
        .iter()
        .find(|(host, _)| host.start <= range.start && range.end <= host.end)
        .map(|(_, host)| *host)
}
