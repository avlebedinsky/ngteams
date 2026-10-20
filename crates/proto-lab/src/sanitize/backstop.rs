//! The export's last check for names ([`check_names`]): whether any word of a name the sanitizer found is still in its
//! output, at any layer of encoding.
//!
//! It works from the name values as they were found in the capture ([`LearnedNames`], kept before any decoding), not
//! from the sanitizer's own list of what to replace, so a mistake in how that list is made doesn't hide its own leak.
//! Each value is decoded (fully, and with form decoding where a `+` is a space and `%2B` a plus) and split into words
//! the way the sanitizer reads a name (`words.rs`: runs of letters and digits, any script, of 3 characters or more,
//! none from a part that is an identifier such as `jane.doe@contoso.com`, no digits alone); each, lowercased, is looked
//! for, anywhere but in a public protocol host (`teams.microsoft.com`), whose labels are never a name's. Every line
//! of the output is checked as written and again after each layer of escapes is decoded (percent escapes, backslash
//! escapes, HTML character references, a `+` being a separator like any other character that isn't a letter or a
//! digit), up to [`CHECK_ROUNDS`] layers; a line that still holds escapes after that can't be checked and is refused
//! as such. Placeholders written as is (`<name:1>`) are no words; one that only appears once escapes are decoded is.
//!
//! What it covers, honestly: names the sanitizer found under a name field (or replaced whole there). A word shorter
//! than 3 characters, and a name never seen under a name field (only ever in free prose), are outside it. A word of a
//! name that is also an ordinary word ("Mark", a channel called "General") left anywhere fails the check too: the export
//! is refused rather than guessed about.

use std::collections::HashMap;
use std::fmt;

use super::KINDS;
use super::layers::decode_once;
use super::words::{MIN_WORD, learnable_words, without_public_hosts};

/// How many layers of escapes [`check_names`] decodes in a line (more than the sanitizer's rules do, so it also sees
/// what they never reached).
pub const CHECK_ROUNDS: usize = 8;

/// The name values found in a capture, as they were found, each with its placeholder (`<name:1>`). Never printed: its
/// `Debug` shows only the placeholders.
#[derive(Clone, Default)]
pub struct LearnedNames {
    names: Vec<(String, String)>,
}

impl LearnedNames {
    /// Names as found (`raw`), each with the placeholder it became.
    pub fn new(names: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut names: Vec<(String, String)> = names.into_iter().collect();
        names.sort();
        Self { names }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every word looked for, lowercased, with the placeholder of the name it belongs to (the first one, when two names
    /// share a word).
    fn words(&self) -> HashMap<String, String> {
        let mut words = HashMap::new();
        for (raw, label) in &self.names {
            for decoded in [decoded(raw), decoded(&raw.replace('+', " "))] {
                for word in learnable_words(&decoded) {
                    words.entry(word.to_lowercase()).or_insert_with(|| label.clone());
                }
            }
        }
        words
    }
}

impl fmt::Debug for LearnedNames {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let labels: Vec<&str> = self.names.iter().map(|(_, label)| label.as_str()).collect();
        formatter.debug_tuple("LearnedNames").field(&labels).finish()
    }
}

/// What [`check_names`] found: where, never the name itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameFinding {
    /// A word of the name `label` (`<name:1>`) on `line` (from 1), seen once `layer` layers of escapes were decoded
    /// (0: as written).
    Name { line: usize, layer: usize, label: String },
    /// `line` still holds escapes after [`CHECK_ROUNDS`] layers: it couldn't be checked.
    TooDeep { line: usize },
}

impl fmt::Display for NameFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name { line, layer, label } => write!(formatter, "line {line}: {label} (layer {layer})"),
            Self::TooDeep { line } => write!(
                formatter,
                "line {line}: still encoded after {CHECK_ROUNDS} layers, can't be checked"
            ),
        }
    }
}

/// Every place in `text` (a sanitized capture as it would be written) that still holds a word of one of `names`; see
/// the module docs. Empty when there is nothing to look for.
#[must_use]
pub fn check_names(names: &LearnedNames, text: &str) -> Vec<NameFinding> {
    let words = names.words();
    if words.is_empty() {
        return Vec::new();
    }
    let longest = words.keys().map(|word| word.chars().count()).max().unwrap_or_default();
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        check_line(&words, longest, index + 1, line, &mut found);
    }
    found
}

/// One line, at every layer; a name is reported once per line, at the first layer it shows in.
fn check_line(words: &HashMap<String, String>, longest: usize, line: usize, text: &str, found: &mut Vec<NameFinding>) {
    let mut seen: Vec<&str> = Vec::new();
    let mut report = |layer: usize, text: &str| {
        for label in hits(words, longest, text) {
            if !seen.contains(&label) {
                seen.push(label);
                found.push(NameFinding::Name {
                    line,
                    layer,
                    label: label.to_owned(),
                });
            }
        }
    };
    let mut current = without_placeholders(text);
    report(0, &current);
    for layer in 1..=CHECK_ROUNDS {
        let Some((plain, apart)) = decode_once(&current) else {
            return;
        };
        report(layer, &plain);
        report(layer, &apart);
        current = plain;
    }
    if decode_once(&current).is_some() {
        found.push(NameFinding::TooDeep { line });
    }
}

/// The labels of the names whose words `text` holds, outside its public protocol hosts.
fn hits<'a>(words: &'a HashMap<String, String>, longest: usize, text: &str) -> Vec<&'a str> {
    let mut labels = Vec::new();
    let text = without_public_hosts(text);
    for word in name_words(&text) {
        if word.chars().count() > longest {
            continue;
        }
        if let Some(label) = words.get(&word.to_lowercase()) {
            labels.push(label.as_str());
        }
    }
    labels
}

/// The words of 3 characters or more: runs of letters and digits.
fn name_words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|char: char| !char.is_alphanumeric())
        .filter(|word| word.chars().count() >= MIN_WORD)
}

/// `text` decoded as far as [`check_names`] looks.
fn decoded(text: &str) -> String {
    let mut text = text.to_owned();
    for _ in 0..CHECK_ROUNDS {
        match decode_once(&text) {
            Some((plain, _)) => text = plain,
            None => break,
        }
    }
    text
}

/// `text` with every placeholder written as is (`<name:1>`, `<user:a>`) made a space, so its kind is no word.
fn without_placeholders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        if let Some(len) = placeholder_len(after) {
            out.push(' ');
            rest = &after[len..];
        } else {
            out.push('<');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The length of the placeholder `text` starts with, if it starts with one.
fn placeholder_len(text: &str) -> Option<usize> {
    let inner = text.strip_prefix('<')?;
    let kind_len = inner.bytes().take_while(u8::is_ascii_lowercase).count();
    let kind = &inner[..kind_len];
    let label = inner[kind_len..].strip_prefix(':')?;
    if !KINDS.split('|').any(|known| known == kind) {
        return None;
    }
    let label_len = label.bytes().take_while(u8::is_ascii_alphanumeric).count();
    (label_len > 0 && label.as_bytes().get(label_len) == Some(&b'>')).then_some(1 + kind_len + 1 + label_len + 1)
}
