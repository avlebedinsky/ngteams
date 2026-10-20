//! Teams meetings: a meeting's code (the numeric meeting id a join link and an invitation carry; a dial-in conference id
//! is one too) becomes `<meeting:N>`, its join passcode a `<token:N>`.
//!
//! Where they are found:
//! - **Join links**: `/meet/<code>?p=<passcode>`, also under `/v2/meet/`, `/lm/meet/` and after a `#`
//!   (`/v2/?meetingjoin=true#/meet/…`), on any host; a `p` in the query of those and of `/l/meetup-join/…` and
//!   `/l/meetnow-join` links. A `p` anywhere else is no passcode. The routes `/meet/meet-now` and the like hold no code.
//! - **Keys**: a code under `meetingCode`, `joinMeetingId`, `*conferenceId` (`teamsVtcConferenceId`), and under
//!   `meetingId` when it is a number; a passcode under `*passcode*` and `meetingPw`. Numbers too.
//! - **Invitations**, by their labels in every language Teams writes one in (`fixture_guard::labels`, the web client's
//!   own locale strings): `Meeting ID: 355 123 456 789`, `Besprechungs-ID: …`, `ID de la réunion : …`, and
//!   `Passcode: Ab3dEf`, `Code secret : …`, `Секретный код: …`, also with HTML tags, `&nbsp;` or a no-break space
//!   between the label and the value.
//! - **What an invitation's structure gives away**: in a text that holds a join link (any of the forms above, with or
//!   without a code or passcode in it), every value that follows a label of any wording (`Kennung: Ab3dEf`) is taken
//!   too: 6 digits or more as a code, or a single run of 4 to 64 letters and digits standing alone at the end of its
//!   element, line or string as a passcode. Teams renders an invitation from its fields, so a label it writes in a
//!   language no list has still sits in that structure.
//!
//! What is found is learned (as names are) and replaced wherever else it shows: a code in any grouping of its digits
//! (`355123456789`, `355 123 456 789`), a passcode, of letters only too, as a whole word in any case. A passcode is
//! never a literal or a setting (`true`, `required`, `fixture_guard::labels::NOT_A_PASSCODE`), and one is learned
//! from a key only when the key names the passcode itself (`passcode`, `meetingPasscode`, `meetingPw`; not
//! `passcodePolicy`). The one exception is a value that may be a word (no digit, no capital after its first letter:
//! `Online`) that only an invitation's structure gave away, under a label no list has: it is replaced wherever it
//! follows a label again, and not as a word everywhere. A learned passcode is never replaced inside a public protocol host, and
//! one replaced inside any other real host (`words.rs`; a secret is redacted even there) is counted
//! (`State::passcode_in_host`).

use std::ops::Range;

use fixture_guard::labels::{Label, Labelled, Labels, is_passcode_value};
use regex::Regex;

use super::layers::decode_all;
use super::words::{Host, hosts};
use super::{Edits, Mode, State};

/// Key fragments (lowercased, without `_` and `-`) whose values are always a meeting's code.
const CODE_PARTS: [&str; 3] = ["meetingcode", "joinmeetingid", "conferenceid"];
/// Whole keys (lowercased, without `_` and `-`) whose values are a meeting's code when they are a number; other meeting
/// ids (a thread id, base64) go through the other rules.
const NUMERIC_CODE_KEYS: [&str; 1] = ["meetingid"];
/// Key fragments (lowercased, without `_` and `-`) whose values are a meeting's passcode.
const PASSCODE_PARTS: [&str; 2] = ["passcode", "meetingpw"];

/// The fewest digits a learned code has.
const CODE_MIN: usize = 6;
/// The most digits a code has: digit groups are joined up to this many.
const CODE_MAX: usize = 24;
/// The longest label an invitation's structure is read from.
const LABEL_MAX: usize = 60;

/// The mode of a meeting key (`bare`: lowercased, without `_` and `-`), if it is one.
pub(super) fn key_mode(bare: &str) -> Option<Mode> {
    if PASSCODE_PARTS.iter().any(|part| bare.contains(part)) {
        Some(Mode::Passcode)
    } else if CODE_PARTS.iter().any(|part| bare.contains(part)) {
        Some(Mode::Code)
    } else if NUMERIC_CODE_KEYS.contains(&bare) {
        Some(Mode::NumericCode)
    } else {
        None
    }
}

/// Whether a key names a passcode itself (`passcode`, `meetingPasscode`, `meetingPw`), so its value is learned; a key
/// that only says something of one (`passcodePolicy`, `isPasscodeRequired`) is redacted, never learned.
pub(super) fn names_passcode(key: &str) -> bool {
    let bare: String = key
        .to_lowercase()
        .chars()
        .filter(|&char| char != '_' && char != '-')
        .collect();
    PASSCODE_PARTS.iter().any(|part| bare.ends_with(part))
}

/// Whether `value` is a code written as a number: digits (6 or more), spaces between groups, a `#` after.
pub(super) fn is_numeric_code(value: &str) -> bool {
    let value = value.trim().trim_end_matches('#');
    let digits = value.bytes().filter(u8::is_ascii_digit).count();
    digits >= CODE_MIN && value.chars().all(|char| char.is_ascii_digit() || is_group_space(char))
}

/// What may stand between a code's digit groups: a space, a no-break space, a form's `+`.
fn is_group_space(char: char) -> bool {
    matches!(char, ' ' | '\u{a0}' | '+')
}

/// What a code is known by: its letters and digits, lowercased.
fn code_key(code: &str) -> String {
    decode_all(code)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether a value can't be a word: it has a digit, or a capital after its first letter and a small letter too
/// (`ZqNBBm`; `Online`, `ONLINE` and `online` may be words).
fn is_strong(value: &str) -> bool {
    let digit = value.bytes().any(|byte| byte.is_ascii_digit());
    let inner_capital = value.bytes().skip(1).any(|byte| byte.is_ascii_uppercase());
    digit || (inner_capital && value.bytes().any(|byte| byte.is_ascii_lowercase()))
}

/// The patterns.
pub(super) struct Patterns {
    link: Regex,
    link_passcode: Regex,
    labels: Labels,
    digit_run: Regex,
}

impl Patterns {
    pub(super) fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            // a join path, with the code after `/meet/` (group 1)
            link: Regex::new(r"(?i)/(?:(?:v2/|lm/)?meet/([A-Za-z0-9]*)|l/meet(?:up|now)-join\b)")?,
            link_passcode: Regex::new(r#"(?i)[?&#;]p=([^&#;\s"'<>\\`]+)"#)?,
            labels: Labels::new()?,
            digit_run: Regex::new(r"[0-9]+(?:[ \x{a0}+][0-9]+)*")?,
        })
    }
}

/// A part of a meeting found in text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Code,
    Passcode,
    /// A value that may be a word (`Online`) after a label no list has, in an invitation: replaced, learned only as a
    /// label's value (see the module docs).
    Weak,
}

/// The codes and passcodes of the join links, the invitation labels and the invitations' structure in `text`; in
/// order, not overlapping. Without a join link in `text`, the structure gives only what was learned already.
fn parts(patterns: &Patterns, state: &State, text: &str) -> Vec<(Range<usize>, Part)> {
    let mut found = Vec::new();
    let linked = link_parts(patterns, text, &mut found);
    for value in patterns.labels.values(text) {
        if let Some(part) = labelled_part(state, text, &value, linked) {
            found.push((value.value, part));
        }
    }
    ordered(found)
}

/// The codes and passcodes of the join links in `text`; whether there is one.
fn link_parts(patterns: &Patterns, text: &str, found: &mut Vec<(Range<usize>, Part)>) -> bool {
    let mut linked = false;
    for captures in patterns.link.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        linked = true;
        if let Some(code) = captures
            .get(1)
            .filter(|code| is_link_code(text, code.as_str(), code.end()))
        {
            found.push((code.range(), Part::Code));
        }
        let tail = &text[whole.end()..];
        let tail = &tail[..tail.find(is_link_end).unwrap_or(tail.len())];
        let passcodes = patterns
            .link_passcode
            .captures_iter(tail)
            .filter_map(|captures| captures.get(1));
        found.extend(passcodes.map(|value| (whole.end() + value.start()..whole.end() + value.end(), Part::Passcode)));
    }
    linked
}

/// What the value after a label is, if anything: a code after a meeting id's label, a passcode after a passcode's, and
/// what the structure of an invitation gives after any other.
fn labelled_part(state: &State, text: &str, value: &Labelled, linked: bool) -> Option<Part> {
    let raw = &text[value.value.clone()];
    let whole = is_whole(text, value.value.end);
    match value.label {
        Label::MeetingId => whole.then_some(Part::Code),
        Label::Passcode => (whole && is_passcode_value(raw)).then_some(Part::Passcode),
        Label::Other => structural_part(state, text, value, linked),
    }
}

/// The value after a label no list has (see the module docs): any when `text` holds a join link (`linked`), else
/// only one learned.
fn structural_part(state: &State, text: &str, value: &Labelled, linked: bool) -> Option<Part> {
    if !value.colon || !is_label(&text[..value.label_end]) {
        return None;
    }
    let raw = &text[value.value.clone()];
    if value.is_code {
        let known = state.codes.contains(&code_key(raw));
        return (is_whole(text, value.value.end) && (linked || known)).then_some(Part::Code);
    }
    if !stands_alone(text, value.value.end) || !is_passcode_value(raw) {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    let known = state.passcodes.contains_key(&lower) || state.weak_passcodes.contains(&lower);
    (linked || known).then_some(if is_strong(raw) { Part::Passcode } else { Part::Weak })
}

/// Whether the text before a colon (`before` ends there) ends in a label as an invitation writes one: all its element,
/// line or string holds up to the colon, [`LABEL_MAX`] characters at most, with a letter, and no markup, quote, path or
/// code in it.
fn is_label(before: &str) -> bool {
    let window = &before[before.floor_char_boundary(before.len().saturating_sub(4 * LABEL_MAX))..];
    let start = match window.rfind(['>', '\n', '\r', '"']) {
        Some(at) => at + 1,
        None if window.len() == before.len() => 0,
        None => return false,
    };
    let label = &window[start..];
    let markup = ['<', '"', '/', '{', '}', ';', '=', ':', '\u{ff1a}', '\u{17d6}'];
    label.chars().count() <= LABEL_MAX && label.chars().any(char::is_alphabetic) && !label.contains(markup)
}

/// Whether the value that ends at `end` is a whole word: no letter or digit of any script after it.
fn is_whole(text: &str, end: usize) -> bool {
    !text[end..].starts_with(char::is_alphanumeric)
}

/// Whether the value that ends at `end` is all its element, line or string holds after its label: only spaces
/// (`&nbsp;`, a form's `+`) between it and a tag, a line's end (an escaped one too), a quote or the text's end.
fn stands_alone(text: &str, end: usize) -> bool {
    let mut rest = &text[end..];
    loop {
        rest = rest.trim_start_matches([' ', '\t', '\u{a0}', '+']);
        match rest.strip_prefix("&nbsp;") {
            Some(after) => rest = after,
            None => break,
        }
    }
    rest.is_empty() || rest.starts_with(['<', '\r', '\n', '"', '\\'])
}

/// Whether the text after `/meet/` up to `end` is a code: 6 characters or more with a digit, a whole word.
fn is_link_code(text: &str, code: &str, end: usize) -> bool {
    let whole = !text[end..].starts_with(['-', '_']);
    whole && code.len() >= CODE_MIN && code.bytes().any(|byte| byte.is_ascii_digit())
}

/// Where a link ends.
fn is_link_end(char: char) -> bool {
    char.is_whitespace() || matches!(char, '"' | '\'' | '<' | '>' | '\\' | '`')
}

/// Sorted by start, an item that overlaps one before it dropped.
fn ordered<T>(mut items: Vec<(Range<usize>, T)>) -> Vec<(Range<usize>, T)> {
    items.sort_by_key(|(range, _)| (range.start, range.end));
    let mut end = 0;
    items.retain(|(range, _)| {
        let keep = range.start >= end;
        if keep {
            end = range.end;
        }
        keep
    });
    items
}

/// Learns the codes and passcodes in `text` (one layer of it).
pub(super) fn learn(patterns: &Patterns, state: &mut State, text: &str) {
    for (range, part) in parts(patterns, state, text) {
        let value = &text[range];
        match part {
            Part::Code => state.learn_code(value),
            Part::Passcode => state.learn_passcode(value),
            Part::Weak => {
                state.weak_passcodes.insert(value.to_ascii_lowercase());
            }
        }
    }
}

/// The rule for join links and invitations: each code and passcode in place.
pub(super) fn meetings(patterns: &Patterns, state: &mut State, text: &str) -> Edits {
    parts(patterns, state, text)
        .into_iter()
        .map(|(range, part)| {
            let value = &text[range.clone()];
            let placeholder = match part {
                Part::Code => state.meeting(value),
                Part::Passcode | Part::Weak => state.token(value),
            };
            (range, placeholder)
        })
        .collect()
}

/// The rule for what was learned: every learned code and passcode wherever it shows (see the module docs).
pub(super) fn learned(patterns: &Patterns, state: &mut State, text: &str) -> Edits {
    let mut found = Vec::new();
    if !state.codes.is_empty() {
        for run in patterns.digit_run.find_iter(text) {
            found.extend(
                codes_in(state, text, run.range())
                    .into_iter()
                    .map(|range| (range, Part::Code)),
            );
        }
    }
    found.extend(
        learned_passcodes(state, text)
            .into_iter()
            .map(|range| (range, Part::Passcode)),
    );
    ordered(found)
        .into_iter()
        .map(|(range, part)| {
            let value = &text[range.clone()];
            let placeholder = match part {
                Part::Code => state.meeting(value),
                Part::Passcode | Part::Weak => {
                    let known = state.passcodes.get(&value.to_lowercase()).cloned();
                    state.token(&known.unwrap_or_else(|| value.to_owned()))
                }
            };
            (range, placeholder)
        })
        .collect()
}

/// The learned passcodes in `text`, as whole words, none inside a public protocol host; one inside any other real host
/// is counted ([`State::passcode_in_host`]).
fn learned_passcodes(state: &mut State, text: &str) -> Vec<Range<usize>> {
    let Some(regex) = &state.passcodes_regex else {
        return Vec::new();
    };
    let matches: Vec<Range<usize>> = regex
        .find_iter(text)
        .map(|found| found.range())
        .filter(|range| is_word(text, range))
        .collect();
    if matches.is_empty() {
        return matches;
    }
    let hosts = hosts(text);
    let mut kept = Vec::new();
    for range in matches {
        match state.host_at(&hosts, &range) {
            Some(Host::Public) => continue,
            Some(Host::Real) => state.passcode_in_host += 1,
            Some(Host::Shaped) | None => {}
        }
        kept.push(range);
    }
    kept
}

/// Whether `range` of `text` is a whole word: no letter or digit right before or after it.
fn is_word(text: &str, range: &Range<usize>) -> bool {
    let before = text[..range.start]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric);
    let after = text[range.end..].chars().next().is_some_and(char::is_alphanumeric);
    !before && !after
}

/// The learned codes in a run of digit groups (`run`): each the longest series of whole groups whose digits are one.
fn codes_in(state: &State, text: &str, run: Range<usize>) -> Vec<Range<usize>> {
    let mut groups = Vec::new();
    let mut at = run.start;
    for group in text[run.clone()].split(is_group_space) {
        groups.push(at..at + group.len());
        at += group.len() + text[at + group.len()..].chars().next().map_or(0, char::len_utf8);
    }
    let mut found = Vec::new();
    let mut first = 0;
    while first < groups.len() {
        let mut digits = String::new();
        let mut longest = None;
        for (last, group) in groups.iter().enumerate().skip(first) {
            digits.push_str(&text[group.clone()]);
            if digits.len() > CODE_MAX {
                break;
            }
            let range = groups[first].start..group.end;
            if state.codes.contains(&digits) && is_word(text, &range) {
                longest = Some((last, range));
            }
        }
        match longest {
            Some((last, range)) => {
                found.push(range);
                first = last + 1;
            }
            None => first += 1,
        }
    }
    found
}

impl State {
    /// A code's placeholder; the same code in any grouping gets the same one.
    pub(super) fn meeting(&mut self, code: &str) -> String {
        format!("<meeting:{}>", self.meetings.label(&code_key(code)))
    }

    /// Learns a code, to be replaced everywhere: only one of [`CODE_MIN`] digits or more.
    pub(super) fn learn_code(&mut self, code: &str) {
        let key = code_key(code);
        if key.len() >= CODE_MIN && key.len() <= CODE_MAX && key.bytes().all(|byte| byte.is_ascii_digit()) {
            self.codes.insert(key);
        }
    }

    /// Learns a passcode, to be replaced everywhere: 4 to 64 letters and digits of any mix, never a literal or a
    /// setting ([`is_passcode_value`]).
    pub(super) fn learn_passcode(&mut self, value: &str) {
        let value = decode_all(value.trim());
        if is_passcode_value(&value) && !self.passcodes.contains_key(&value.to_lowercase()) {
            self.passcodes.insert(value.to_lowercase(), value);
            self.passcodes_changed = true;
        }
    }

    /// The pattern of every learned passcode, when one was learned since the last time.
    pub(super) fn rebuild_passcodes(&mut self) -> Result<(), regex::Error> {
        if !self.passcodes_changed {
            return Ok(());
        }
        let mut passcodes: Vec<&String> = self.passcodes.keys().collect();
        passcodes.sort_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
        let alternation: Vec<String> = passcodes.iter().map(|passcode| regex::escape(passcode)).collect();
        self.passcodes_regex = Some(Regex::new(&format!("(?i)(?:{})", alternation.join("|")))?);
        self.passcodes_changed = false;
        Ok(())
    }
}
