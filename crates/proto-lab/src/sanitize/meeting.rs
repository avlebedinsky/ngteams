//! Teams meetings: a meeting's code (the numeric meeting id a join link and an invitation carry; a dial-in conference id
//! is one too) becomes `<meeting:N>`, its join passcode a `<token:N>`.
//!
//! Where they are found:
//! - **Join links**: `/meet/<code>?p=<passcode>`, also under `/v2/meet/`, `/lm/meet/` and after a `#`
//!   (`/v2/?meetingjoin=true#/meet/…`), on any host; a `p` in the query of those and of `/l/meetup-join/…` and
//!   `/l/meetnow-join` links. A `p` anywhere else is no passcode. The routes `/meet/meet-now` and the like hold no code.
//! - **Keys**: a code under `meetingCode`, `joinMeetingId`, `*conferenceId` (`teamsVtcConferenceId`), and under
//!   `meetingId` when it is a number; a passcode under `*passcode*` and `meetingPw`. Numbers too.
//! - **Invitations**: `Meeting ID: 355 123 456 789` and `Passcode: Ab3dEf`, also with HTML tags and `&nbsp;` between
//!   the label and the value.
//!
//! What is found anywhere in a capture is learned (as names are), and replaced wherever else it shows: a code in any
//! grouping of its digits (`355123456789`, `355 123 456 789`), a passcode as a whole word in any case. Only a code of 6
//! digits or more and a passcode of 4 to 64 letters and digits with a digit in it are learned, so a short number or
//! a word under a passcode key (`true`) is never replaced everywhere.

use std::ops::Range;

use regex::Regex;

use super::layers::decode_all;
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

/// An HTML tag between a label and its value (`</span><span class="x">`), never a placeholder.
const TAG: &str = r"</?[A-Za-z][A-Za-z0-9]*(?:\s[^<>]*)?/?>";

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

/// Whether `value` is a code written as a number: digits (6 or more), spaces between groups, a `#` after.
pub(super) fn is_numeric_code(value: &str) -> bool {
    let value = value.trim().trim_end_matches('#');
    let digits = value.bytes().filter(u8::is_ascii_digit).count();
    digits >= CODE_MIN && value.chars().all(|char| char.is_ascii_digit() || is_group_space(char))
}

fn is_group_space(char: char) -> bool {
    char == ' ' || char == '\u{a0}'
}

/// What a code is known by: its letters and digits, lowercased.
fn code_key(code: &str) -> String {
    decode_all(code)
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// The patterns.
pub(super) struct Patterns {
    link: Regex,
    link_passcode: Regex,
    id_label: Regex,
    passcode_label: Regex,
    digit_run: Regex,
}

impl Patterns {
    pub(super) fn new() -> Result<Self, regex::Error> {
        Ok(Self {
            // a join path, with the code after `/meet/` (group 1)
            link: Regex::new(r"(?i)/(?:(?:v2/|lm/)?meet/([A-Za-z0-9]*)|l/meet(?:up|now)-join\b)")?,
            link_passcode: Regex::new(r#"(?i)[?&#;]p=([^&#;\s"'<>\\`]+)"#)?,
            id_label: Regex::new(&format!(
                r"(?i)\b(?:meeting|conference)\s*id(?:\s|{TAG})*:(?:\s|{TAG}|&nbsp;)*([0-9](?:[ \x{{a0}}]?[0-9]){{5,}})"
            ))?,
            passcode_label: Regex::new(&format!(
                r#"(?i)(?:passcode|meetingpw)["']?(?:\s|{TAG})*[:=](?:\s|["']|{TAG}|&nbsp;)*([A-Za-z0-9]{{4,64}})\b"#
            ))?,
            digit_run: Regex::new(r"[0-9]+(?:[ \x{a0}][0-9]+)*")?,
        })
    }
}

/// A part of a meeting found in text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Code,
    Passcode,
}

/// The codes and passcodes of the join links and the invitation labels in `text`; in order, not overlapping.
fn parts(patterns: &Patterns, text: &str) -> Vec<(Range<usize>, Part)> {
    let mut found = Vec::new();
    for captures in patterns.link.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
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
    for (regex, part) in [
        (&patterns.id_label, Part::Code),
        (&patterns.passcode_label, Part::Passcode),
    ] {
        let values = regex.captures_iter(text).filter_map(|captures| captures.get(1));
        found.extend(
            values
                .filter(|value| !is_literal(value.as_str()))
                .map(|value| (value.range(), part)),
        );
    }
    ordered(found)
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

/// A JSON literal, which a label's value never is.
fn is_literal(value: &str) -> bool {
    ["true", "false", "null"].contains(&value.to_ascii_lowercase().as_str())
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
    for (range, part) in parts(patterns, text) {
        match part {
            Part::Code => state.learn_code(&text[range]),
            Part::Passcode => state.learn_passcode(&text[range]),
        }
    }
}

/// The rule for join links and invitations: each code and passcode in place.
pub(super) fn meetings(patterns: &Patterns, state: &mut State, text: &str) -> Edits {
    parts(patterns, text)
        .into_iter()
        .map(|(range, part)| {
            let value = &text[range.clone()];
            let placeholder = match part {
                Part::Code => state.meeting(value),
                Part::Passcode => state.token(value),
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
    if let Some(regex) = &state.passcodes_regex {
        let bounded = regex.find_iter(text).filter(|found| is_word(text, &found.range()));
        found.extend(bounded.map(|found| (found.range(), Part::Passcode)));
    }
    ordered(found)
        .into_iter()
        .map(|(range, part)| {
            let value = &text[range.clone()];
            let placeholder = match part {
                Part::Code => state.meeting(value),
                Part::Passcode => {
                    let known = state.passcodes.get(&value.to_lowercase()).cloned();
                    state.token(&known.unwrap_or_else(|| value.to_owned()))
                }
            };
            (range, placeholder)
        })
        .collect()
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

    /// Learns a passcode, to be replaced everywhere: only one of 4 to 64 letters and digits, a digit among them.
    pub(super) fn learn_passcode(&mut self, value: &str) {
        let value = decode_all(value.trim());
        let shaped = (4..=64).contains(&value.len())
            && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
            && value.bytes().any(|byte| byte.is_ascii_digit());
        if shaped && !self.passcodes.contains_key(&value.to_lowercase()) {
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
