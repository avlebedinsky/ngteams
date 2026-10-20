//! What a name value gives as words, and where a text holds a host: shared by the sanitizer's name rule and the
//! export's own check for names (`backstop.rs`), so both read a name the same way.
//!
//! A name value's words are its runs of letters and digits of [`MIN_WORD`] characters or more, apart from those of a
//! part that is an identifier: an email or UPN (`jane.doe@contoso.com`), an MRI (`8:orgid:…`, `28:…`, `tel:…`), a URL,
//! a GUID, or a host name (`lab.example.org`: three labels or more, lowercase, the last a top-level domain of the
//! Public Suffix List). The identifier rules replace those whole; their pieces (`com`, `contoso`, `orgid`) are no
//! person's name, and learning them would replace every host and path that holds them. A dotted part that may be a
//! person's (`jane.wu`, `Mei.Li`, `Doe.CA`) gives its words, but never a lowercase last label that is a top-level
//! domain (`contoso.com` gives `contoso`; a host that holds it is then refused by the export, not left). A run of
//! digits alone is no word of a name either.
//!
//! A host in a text is a run of letters, digits, `-` and `.` whose last label is a top-level domain (of the Public
//! Suffix List, a reserved one, or any two letters). One under [`PUBLIC_DOMAINS`] (`teams.microsoft.com`, `login.microsoftonline.com`) is a public protocol
//! host: its labels are never a name's (a group called "Microsoft Teams" doesn't make `teams.microsoft.com` a name),
//! so the sanitizer leaves them and the check doesn't look in them. A lookalike that only holds one
//! (`teams.microsoft.com.example.test`) isn't one.

use std::iter;
use std::ops::Range;

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

/// Every host in `text`, in order, with whether it is a public protocol host.
pub(super) fn hosts(text: &str) -> Vec<(Range<usize>, bool)> {
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
        let run = &text[begin..index];
        let host = run.trim_matches(['.', '-']);
        if is_domain(host) {
            let at = begin + (run.len() - run.trim_start_matches(['.', '-']).len());
            found.push((at..at + host.len(), is_public(host)));
        }
    }
    found
}

/// Whether `range` of a text lies inside one of its `hosts`, and whether that host is public.
pub(super) fn host_of(hosts: &[(Range<usize>, bool)], range: &Range<usize>) -> Option<bool> {
    hosts
        .iter()
        .find(|(host, _)| host.start <= range.start && range.end <= host.end)
        .map(|(_, public)| *public)
}

/// `text` with every public protocol host made spaces.
pub(super) fn without_public_hosts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (host, _) in hosts(text).into_iter().filter(|(_, public)| *public) {
        out.push_str(&text[last..host.start]);
        out.push(' ');
        last = host.end;
    }
    out.push_str(&text[last..]);
    out
}
