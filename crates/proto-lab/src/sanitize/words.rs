//! What a name value gives as words, and where a text holds a host: shared by the sanitizer's name rule and the
//! export's own check for names (`backstop.rs`), so both read a name the same way.
//!
//! A name value's words are its runs of letters and digits of [`MIN_WORD`] characters or more, apart from those of a
//! part that is an identifier: an email or UPN (`jane.doe@contoso.com`), an MRI (`8:orgid:…`, `28:…`, `tel:…`), a URL,
//! a GUID, or a domain (`lab.example.org`). The identifier rules replace those whole; their pieces (`com`, `contoso`,
//! `orgid`) are no person's name, and learning them would replace every host and path that holds them. A run of digits
//! alone is no word of a name either.
//!
//! A host is a run of letters, digits, `-` and `.` whose last label is a top-level domain ([`TLDS`] or any two
//! letters). One under [`PUBLIC_DOMAINS`] (`teams.microsoft.com`, `login.microsoftonline.com`) is a public protocol
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

/// Top-level domains a host may end in, besides every two-letter one; the reserved and local ones included.
const TLDS: [&str; 26] = [
    "com",
    "net",
    "org",
    "edu",
    "gov",
    "mil",
    "int",
    "info",
    "biz",
    "io",
    "ai",
    "app",
    "dev",
    "cloud",
    "microsoft",
    "office",
    "live",
    "skype",
    "azure",
    "test",
    "example",
    "invalid",
    "localhost",
    "local",
    "internal",
    "arpa",
];

/// The words of a name value (see the module docs).
pub(super) fn learnable_words(name: &str) -> Vec<&str> {
    name.split(is_part_break)
        .filter(|part| !is_identifier(part))
        .flat_map(|part| part.split(|char: char| !char.is_alphanumeric()))
        .filter(|word| word.chars().count() >= MIN_WORD && !word.chars().all(|char| char.is_ascii_digit()))
        .collect()
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

/// Whether a part of a name is an identifier: an email, an MRI, a URL, a GUID or a domain.
fn is_identifier(part: &str) -> bool {
    let part = part.trim_matches(|char: char| matches!(char, '.' | '\'' | '!' | '?' | ':'));
    let email = part
        .rsplit_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    let mri = part.split_once(':').is_some_and(|(prefix, rest)| {
        !rest.is_empty() && (prefix.eq_ignore_ascii_case("tel") || prefix.bytes().all(|byte| byte.is_ascii_digit()))
    });
    email || mri || part.contains("://") || has_guid(part) || is_domain(part)
}

/// Whether `text` holds a GUID (`8-4-4-4-12` hex digits).
fn has_guid(text: &str) -> bool {
    text.split(|char: char| !(char.is_ascii_hexdigit() || char == '-'))
        .any(|run| {
            let groups: Vec<usize> = run.split('-').map(str::len).collect();
            groups.windows(5).any(|window| window == [8, 4, 4, 4, 12])
        })
}

/// Whether `token` is a domain: labels of letters, digits and `-`, at least two, the last a top-level domain.
fn is_domain(token: &str) -> bool {
    let labels: Vec<&str> = token.split('.').collect();
    let well_formed = labels
        .iter()
        .all(|label| !label.is_empty() && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
    labels.len() >= 2 && well_formed && labels.last().is_some_and(|last| is_tld(last))
}

fn is_tld(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    lower.bytes().all(|byte| byte.is_ascii_lowercase()) && (lower.len() == 2 || TLDS.contains(&lower.as_str()))
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
