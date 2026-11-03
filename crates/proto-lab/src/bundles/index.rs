//! The bundle index: URL templates, hosts, HTTP methods and route constants found in an unpacked bundle, with the
//! file, line and column of every place they occur. It helps identify captured requests and find the code to
//! read for the protocol notes; deeper analysis is rea's job (`analyze_javascript_application`,
//! `trace_javascript_semantics`).
//!
//! What counts as what:
//! - **url**: a string literal starting with `http://`, `https://`, `ws://` or `wss://`
//! - **host**: the host of such a URL (port and user info dropped), or a literal that is a bare lowercase domain
//!   name such as `notifications.skype.net` or `*.teams.cloud.microsoft` whose top-level domain is one of
//!   [`HOST_TLDS`] (so that property paths such as `window.location` are left out)
//! - **route**: a literal path such as `/v1/users/ME/conversations` (two segments at least), or a template that
//!   appends a path to a computed base, such as `` `${base}/threads/${id}/members` `` (indexed as
//!   `{}/threads/{}/members`)
//! - **method**: an HTTP verb given as `method: "POST"` (any quotes, any case, `httpMethod` too), paired with the
//!   nearest url or route literal within a few hundred bytes. The pairing is a guess from proximity, not data flow.
//!
//! A string that is none of these but holds quoted strings itself (JSON in `JSON.parse('…')`, HTML in a locale
//! string) is looked into as well; what is found there takes the site of the outer string.
//!
//! The index is stored as JSON Lines in `index.jsonl` beside the unpacked files, one [`Entry`] per line.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::fs;
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use super::literals::{Literal, literals, markup_literals};

/// The index file inside a bundle's directory.
pub const INDEX_FILE: &str = "index.jsonl";

/// How far (in bytes) a method looks for its url or route.
const METHOD_WINDOW: usize = 400;
/// How many levels deep the scan looks into strings that hold quoted strings themselves.
const NESTED_DEPTH: usize = 2;
/// Longer literals are data, not addresses.
const MAX_VALUE_LEN: usize = 2048;
const HTTP_METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
const URL_SCHEMES: [&str; 4] = ["https://", "http://", "wss://", "ws://"];
/// The top-level domains a bare host literal may end in; anything else (`window.location`) is a property path.
/// `microsoft` carries much of Teams (`teams.cloud.microsoft`, `*.teams.core.microsoft`), `scloud`, `fr` and `sg`
/// its sovereign clouds, `ai` Cortana.
pub const HOST_TLDS: [&str; 17] = [
    "com",
    "net",
    "org",
    "io",
    "ms",
    "us",
    "cn",
    "de",
    "gov",
    "mil",
    "cloud",
    "edu",
    "microsoft",
    "scloud",
    "fr",
    "sg",
    "ai",
];
const INDEXED_EXTENSIONS: [&str; 6] = ["js", "mjs", "cjs", "json", "html", "htm"];
/// How many sites [`render`] lists.
const SHOWN_SITES: usize = 3;

/// What an index entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Url,
    Host,
    Route,
    Method,
}

impl Kind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Host => "host",
            Self::Route => "route",
            Self::Method => "method",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One place a value occurs: the file relative to the bundle dir, its line and byte column (both from 1).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Site {
    pub file: String,
    pub line: usize,
    pub column: usize,
}

/// One distinct value and everywhere it occurs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub value: String,
    /// For a method: the url or route it was paired with, if one was near.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sites: Vec<Site>,
}

/// One value found in one source, by byte span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub kind: Kind,
    pub value: String,
    pub url: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// The findings of a JavaScript or JSON source.
#[must_use]
pub fn scan(source: &str) -> Vec<Finding> {
    findings(source, &literals(source))
}

/// The findings of an HTML source.
#[must_use]
pub fn scan_markup(source: &str) -> Vec<Finding> {
    findings(source, &markup_literals(source))
}

fn findings(source: &str, literals: &[Literal]) -> Vec<Finding> {
    let mut found = Vec::new();
    for literal in literals {
        classify_nested(&literal.text, literal, NESTED_DEPTH, &mut found);
    }
    let methods = methods(source, &found);
    found.extend(methods);
    found
}

/// Classifies `text`; when it is none of the kinds but holds quoted strings itself (JSON in `JSON.parse('…')`, HTML
/// in a locale string), classifies those too, up to `depth` levels. Findings inside take the span of `outer`.
fn classify_nested(text: &str, outer: &Literal, depth: usize, found: &mut Vec<Finding>) {
    if classify(text, outer, found) || depth == 0 || !text.contains(['"', '\'']) {
        return;
    }
    for inner in markup_literals(text) {
        classify_nested(&inner.text, outer, depth - 1, found);
    }
}

/// Pushes what `text` is, with the span of `outer`; returns whether it was anything.
fn classify(text: &str, outer: &Literal, found: &mut Vec<Finding>) -> bool {
    if text.len() > MAX_VALUE_LEN {
        return false;
    }
    let finding = |kind, value: &str| Finding {
        kind,
        value: value.to_owned(),
        url: None,
        start: outer.start,
        end: outer.end,
    };
    if is_url(text) {
        found.push(finding(Kind::Url, text));
        if let Some(host) = url_host(text) {
            found.push(finding(Kind::Host, &host));
        }
    } else if is_route(text) {
        found.push(finding(Kind::Route, text));
    } else if is_host(text) {
        found.push(finding(Kind::Host, text));
    } else {
        return false;
    }
    true
}

fn after_scheme(text: &str) -> Option<&str> {
    URL_SCHEMES.iter().find_map(|scheme| text.strip_prefix(scheme))
}

fn is_url(text: &str) -> bool {
    after_scheme(text).is_some_and(|rest| !rest.is_empty()) && text.bytes().all(|byte| byte > b' ' && byte != 0x7f)
}

fn url_host(url: &str) -> Option<String> {
    let rest = after_scheme(url)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port == "{}" || (!port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())) => name,
        _ => host,
    };
    let host = host.to_ascii_lowercase();
    let valid = host.contains('.')
        && !host.starts_with('.')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-.{}*".contains(&byte));
    valid.then_some(host)
}

fn is_host(text: &str) -> bool {
    let name = text
        .strip_prefix("*.")
        .or_else(|| text.strip_prefix("{}."))
        .unwrap_or(text);
    let labels: Vec<&str> = name.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
        && labels.last().is_some_and(|tld| HOST_TLDS.contains(tld))
}

fn is_route(text: &str) -> bool {
    let (path, query) = text.find(['?', '#']).map_or((text, ""), |at| text.split_at(at));
    let based = path.strip_prefix("{}");
    let absolute = based.unwrap_or(path);
    if !absolute.starts_with('/') || absolute.starts_with("//") {
        return false;
    }
    let path_ok = path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_.~{}$:;=,@%+-!*".contains(&byte));
    let query_ok = query.bytes().all(|byte| byte > b' ' && !b"\"'`<>\\".contains(&byte));
    let segments = absolute.split('/').filter(|segment| !segment.is_empty()).count();
    let has_letter = path.replace("{}", "").bytes().any(|byte| byte.is_ascii_alphabetic());
    path_ok && query_ok && has_letter && (segments >= 2 || based.is_some())
}

/// The `method: "VERB"` pairs of a source, each with the nearest url or route in `found`.
fn methods(source: &str, found: &[Finding]) -> Vec<Finding> {
    let bytes = source.as_bytes();
    source
        .match_indices("ethod")
        .filter(|&(at, _)| at > 0 && matches!(bytes[at - 1], b'm' | b'M'))
        .filter_map(|(at, _)| {
            let (verb, end) = method_value(bytes, at + "ethod".len())?;
            let start = at - 1;
            Some(Finding {
                kind: Kind::Method,
                value: verb,
                url: nearest_url(found, start, end),
                start,
                end,
            })
        })
        .collect()
}

/// Reads `: "VERB"` (after an optional closing quote of the key) at `pos`; returns the verb and the end.
fn method_value(bytes: &[u8], pos: usize) -> Option<(String, usize)> {
    let mut pos = pos;
    if matches!(bytes.get(pos), Some(b'"' | b'\'')) {
        pos += 1;
    }
    pos = skip_spaces(bytes, pos);
    if bytes.get(pos) != Some(&b':') {
        return None;
    }
    pos = skip_spaces(bytes, pos + 1);
    let quote = *bytes.get(pos)?;
    if !matches!(quote, b'"' | b'\'' | b'`') {
        return None;
    }
    let start = pos + 1;
    let len = bytes[start..]
        .iter()
        .take(8)
        .take_while(|byte| byte.is_ascii_alphabetic())
        .count();
    if bytes.get(start + len) != Some(&quote) {
        return None;
    }
    let verb = std::str::from_utf8(&bytes[start..start + len])
        .ok()?
        .to_ascii_uppercase();
    HTTP_METHODS.contains(&verb.as_str()).then_some((verb, start + len + 1))
}

fn skip_spaces(bytes: &[u8], mut pos: usize) -> usize {
    while bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        pos += 1;
    }
    pos
}

fn nearest_url(found: &[Finding], start: usize, end: usize) -> Option<String> {
    found
        .iter()
        .filter(|finding| matches!(finding.kind, Kind::Url | Kind::Route))
        .filter_map(|finding| {
            let distance = if finding.end <= start {
                start - finding.end
            } else if finding.start >= end {
                finding.start - end
            } else {
                return None;
            };
            (distance <= METHOD_WINDOW).then_some((distance, finding))
        })
        .min_by_key(|&(distance, finding)| (distance, finding.start))
        .map(|(_, finding)| finding.value.clone())
}

/// Line starts of a source, to turn byte offsets into lines and columns.
struct Lines(Vec<usize>);

impl Lines {
    fn new(source: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Self(starts)
    }

    fn locate(&self, offset: usize) -> (usize, usize) {
        let line = self.0.partition_point(|&start| start <= offset).max(1);
        (line, offset - self.0[line - 1] + 1)
    }
}

/// Collects findings from many sources into an [`Index`].
#[derive(Debug, Default)]
pub struct IndexBuilder {
    entries: BTreeMap<(Kind, String, Option<String>), Vec<Site>>,
}

impl IndexBuilder {
    /// Scans one source; `file` names it in the sites, and an `.html`/`.htm` name scans it as markup.
    pub fn add_source(&mut self, file: &str, source: &str) {
        let findings = if is_markup(file) {
            scan_markup(source)
        } else {
            scan(source)
        };
        let lines = Lines::new(source);
        for finding in findings {
            let (line, column) = lines.locate(finding.start);
            self.entries
                .entry((finding.kind, finding.value, finding.url))
                .or_default()
                .push(Site {
                    file: file.to_owned(),
                    line,
                    column,
                });
        }
    }

    #[must_use]
    pub fn finish(self) -> Index {
        let entries = self
            .entries
            .into_iter()
            .map(|((kind, value, url), mut sites)| {
                sites.sort();
                Entry {
                    kind,
                    value,
                    url,
                    sites,
                }
            })
            .collect();
        Index { entries }
    }
}

fn is_markup(file: &str) -> bool {
    Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm"))
}

/// Why an index couldn't be built, read or found.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{}:{line}: not an index entry: {source}", path.display())]
    Json {
        path: PathBuf,
        line: usize,
        source: serde_json::Error,
    },
    #[error("no bundle imported under {}; run `proto-lab bundles import <msix>` first", .0.display())]
    NoBundles(PathBuf),
    #[error("no imported bundle {version}; imported: {}", if available.is_empty() { "none".to_owned() } else { available.join(", ") })]
    UnknownVersion { version: String, available: Vec<String> },
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> IndexError {
    let path = path.to_path_buf();
    move |source| IndexError::Io { path, source }
}

/// The index of one bundle: entries sorted by kind, value and paired url.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    entries: Vec<Entry>,
}

impl Index {
    #[must_use]
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Indexes every `.js`, `.mjs`, `.cjs`, `.json`, `.html` and `.htm` file under `root` (symlinks are not
    /// followed). Sites name files relative to `root`.
    ///
    /// # Errors
    /// When a directory or file can't be read.
    pub fn build(root: &Path) -> Result<Self, IndexError> {
        let mut builder = IndexBuilder::default();
        for path in source_files(root)? {
            let bytes = fs::read(&path).map_err(io_at(&path))?;
            let relative = path.strip_prefix(root).unwrap_or(&path);
            builder.add_source(&relative.to_string_lossy(), &String::from_utf8_lossy(&bytes));
        }
        Ok(builder.finish())
    }

    /// Writes the index as JSON Lines.
    ///
    /// # Errors
    /// When the file can't be written.
    pub fn write(&self, path: &Path) -> Result<(), IndexError> {
        let file = fs::File::create(path).map_err(io_at(path))?;
        let mut writer = BufWriter::new(file);
        for entry in &self.entries {
            serde_json::to_writer(&mut writer, entry).map_err(|error| io_at(path)(error.into()))?;
            writer.write_all(b"\n").map_err(io_at(path))?;
        }
        writer.flush().map_err(io_at(path))
    }

    /// Reads an index written by [`Index::write`].
    ///
    /// # Errors
    /// When the file can't be read or a line isn't an entry.
    pub fn read(path: &Path) -> Result<Self, IndexError> {
        let file = fs::File::open(path).map_err(io_at(path))?;
        let mut entries = Vec::new();
        for (number, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(io_at(path))?;
            if line.trim().is_empty() {
                continue;
            }
            let entry = serde_json::from_str(&line).map_err(|source| IndexError::Json {
                path: path.to_path_buf(),
                line: number + 1,
                source,
            })?;
            entries.push(entry);
        }
        Ok(Self { entries })
    }

    /// The entries whose value or paired url matches `pattern`, of `kind` if one is given, in index order. Build the
    /// pattern with [`search_pattern`] to match the way `bundles search` does.
    pub fn search<'a>(&'a self, pattern: &'a Regex, kind: Option<Kind>) -> impl Iterator<Item = &'a Entry> + 'a {
        self.entries
            .iter()
            .filter(move |entry| kind.is_none_or(|kind| entry.kind == kind))
            .filter(|entry| {
                pattern.is_match(&entry.value) || entry.url.as_deref().is_some_and(|url| pattern.is_match(url))
            })
    }
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>, IndexError> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(io_at(&dir))? {
            let entry = entry.map_err(io_at(&dir))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(io_at(&path))?;
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && is_indexed(&path) {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn is_indexed(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| INDEXED_EXTENSIONS.contains(&extension))
}

/// Parses a bundle version: one to four dot-separated numbers, such as `26260.1717.5229.7049`.
#[must_use]
pub fn parse_version(version: &str) -> Option<Vec<u64>> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    parts
        .iter()
        .map(|part| {
            let digits = !part.is_empty() && part.len() <= 10 && part.bytes().all(|byte| byte.is_ascii_digit());
            digits.then(|| part.parse().ok()).flatten()
        })
        .collect()
}

/// The imported bundle versions under `bundles_dir`, oldest first. A version dir without an index is an unfinished
/// import and isn't listed.
///
/// # Errors
/// When `bundles_dir` exists but can't be read.
pub fn versions(bundles_dir: &Path) -> Result<Vec<String>, IndexError> {
    let entries = match fs::read_dir(bundles_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_at(bundles_dir)(error)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let name = entry.map_err(io_at(bundles_dir))?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(parsed) = parse_version(name)
            && bundles_dir.join(name).join(INDEX_FILE).is_file()
        {
            found.push((parsed, name.to_owned()));
        }
    }
    found.sort();
    Ok(found.into_iter().map(|(_, name)| name).collect())
}

/// The regex `bundles search` matches with: `pattern`, ignoring case.
///
/// # Errors
/// When `pattern` isn't a valid regex.
pub fn search_pattern(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).case_insensitive(true).build()
}

/// Loads the index of `version`, or of the newest imported bundle; returns the version with it.
///
/// # Errors
/// When nothing is imported, `version` isn't, or the index can't be read.
pub fn load(bundles_dir: &Path, version: Option<&str>) -> Result<(String, Index), IndexError> {
    let available = versions(bundles_dir)?;
    let chosen = match version {
        Some(version) => available
            .iter()
            .find(|candidate| *candidate == version)
            .cloned()
            .ok_or_else(|| IndexError::UnknownVersion {
                version: version.to_owned(),
                available: available.clone(),
            })?,
        None => available
            .last()
            .cloned()
            .ok_or_else(|| IndexError::NoBundles(bundles_dir.to_path_buf()))?,
    };
    let index = Index::read(&bundles_dir.join(&chosen).join(INDEX_FILE))?;
    Ok((chosen, index))
}

/// One entry for the terminal: kind, value and paired url, the site count and the first few sites.
#[must_use]
pub fn render(entry: &Entry) -> String {
    let mut out = format!("{:<6} {}", entry.kind.as_str(), entry.value);
    if let Some(url) = &entry.url {
        out.push(' ');
        out.push_str(url);
    }
    let count = entry.sites.len();
    // writing into a String can't fail
    let _ = write!(out, "  ({count} site{})", if count == 1 { "" } else { "s" });
    for site in entry.sites.iter().take(SHOWN_SITES) {
        let _ = write!(out, "\n    {}:{}:{}", site.file, site.line, site.column);
    }
    if count > SHOWN_SITES {
        let _ = write!(out, "\n    … and {} more", count - SHOWN_SITES);
    }
    out
}
