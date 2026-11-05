//! A meeting's transcript as Teams' recap shows it, read from the `WebVTT` file its transcript is downloaded as
//! (`docs/protocol/recap.md`): each cue's times, its speaker (`<v Name>`) and its words, then the cues grouped into
//! the entries the recap's transcript pane draws: one per turn of a speaker, under their name and the time the turn
//! began, each utterance a paragraph. Teams names each cue `<utterance>-<n>` (`…/5-0`, `…/5-1`): the cues of one
//! utterance are one paragraph; a cue with no such identifier is a paragraph of its own.
//!
//! Only what a cue says is read: a style, a region or a note block is skipped, a cue whose times don't read is left out,
//! and a file that holds no cue at all reads as no transcript.

use std::time::Duration;

/// The longest transcript read, in bytes: the most the transport reads of a site's answer ([`crate::MAX_BODY`]),
/// hours of talk.
pub const MAX_TRANSCRIPT: usize = crate::MAX_BODY;

/// One cue of a transcript: a stretch of one speaker's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    /// Its identifier, when it has one (Teams': `<utterance>-<n>`).
    pub id: Option<String>,
    pub start: Duration,
    pub end: Duration,
    /// Who spoke (`<v Name>`), when the cue names them.
    pub speaker: Option<String>,
    pub text: String,
}

/// A turn of one speaker, as the recap's transcript draws it: their name and the time it began over its paragraphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turn {
    pub speaker: Option<String>,
    pub start: Duration,
    /// Its paragraphs, one per utterance (the cues of one utterance joined, a cue without one a paragraph of its own):
    /// where each starts and what it says.
    pub paragraphs: Vec<Paragraph>,
}

/// One paragraph of a [`Turn`]: an utterance (its cues' words, one after another).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

impl Turn {
    /// When its last paragraph ends.
    #[must_use]
    pub fn end(&self) -> Duration {
        self.paragraphs.last().map_or(self.start, |paragraph| paragraph.end)
    }
}

/// The cues of the `WebVTT` file `text`; `None` when it isn't one (no `WEBVTT` line first) or holds no cue.
#[must_use]
pub fn read_vtt(text: &str) -> Option<Vec<Cue>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.len() > MAX_TRANSCRIPT {
        return None;
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut blocks = normalized
        .split("\n\n")
        .map(|block| block.trim_matches('\n'))
        .filter(|block| !block.is_empty());
    let header = blocks.next()?;
    let first = header.lines().next().unwrap_or_default();
    if !(first == "WEBVTT" || first.starts_with("WEBVTT ") || first.starts_with("WEBVTT\t")) {
        return None;
    }
    let cues: Vec<Cue> = blocks.filter_map(cue).collect();
    (!cues.is_empty()).then_some(cues)
}

/// One block's cue; `None` for a block that isn't one (`NOTE`, `STYLE`, `REGION`, times that don't read).
fn cue(block: &str) -> Option<Cue> {
    if block.starts_with("NOTE") || block.starts_with("STYLE") || block.starts_with("REGION") {
        return None;
    }
    let mut lines = block.lines();
    let mut timing = lines.next()?;
    let mut id = None;
    if !timing.contains("-->") {
        // the cue's identifier comes first
        id = Some(timing.trim().to_owned()).filter(|id| !id.is_empty());
        timing = lines.next()?;
    }
    let (start, rest) = timing.split_once("-->")?;
    // cue settings may follow the end time
    let end = rest.split_whitespace().next()?;
    let (start, end) = (timestamp(start.trim())?, timestamp(end)?);
    let body: Vec<&str> = lines.collect();
    let (speaker, text) = voiced(&body.join("\n"));
    let text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    Some(Cue {
        id,
        start,
        end: end.max(start),
        speaker,
        text,
    })
}

/// A `WebVTT` timestamp, `[hh:]mm:ss.ttt`.
fn timestamp(text: &str) -> Option<Duration> {
    let (clock, millis) = text.split_once('.')?;
    let parts: Vec<&str> = clock.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => ("0", *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return None,
    };
    let number = |digits: &str| -> Option<u64> {
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| digits.parse().ok())
            .flatten()
    };
    let (hours, minutes, seconds) = (number(hours)?, number(minutes)?, number(seconds)?);
    if minutes >= 60 || seconds >= 60 || millis.len() != 3 {
        return None;
    }
    let millis = number(millis)?;
    // an hour count too large for the arithmetic is no time a meeting has
    let total = hours
        .checked_mul(60)?
        .checked_add(minutes)?
        .checked_mul(60)?
        .checked_add(seconds)?
        .checked_mul(1_000)?
        .checked_add(millis)?;
    Some(Duration::from_millis(total))
}

/// A cue's speaker (its first `<v Name>`) and its words, every tag taken out and the entities read.
fn voiced(body: &str) -> (Option<String>, String) {
    let mut speaker = None;
    let mut text = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            text.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let tag = &rest[open + 1..open + close];
        if speaker.is_none()
            && let Some(name) = tag.strip_prefix('v').filter(|name| name.starts_with([' ', '\t', '.']))
        {
            // `<v.class Name>`: the classes before the first space
            let name = name.split_once([' ', '\t']).map_or("", |(_, name)| name).trim();
            if !name.is_empty() {
                speaker = Some(entities(name));
            }
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let lines: Vec<String> = text.lines().map(|line| entities(line.trim())).collect();
    (speaker, lines.join(" ").trim().to_owned())
}

/// `WebVTT`'s character references: the named ones it has and numeric ones.
fn entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        let Some(end) = after.find(';').filter(|end| *end <= 10) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let name = &after[1..end];
        let read = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "lrm" => Some('\u{200e}'),
            "rlm" => Some('\u{200f}'),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|decimal| decimal.parse().ok()))
                .and_then(char::from_u32),
        };
        if let Some(character) = read {
            out.push(character);
            rest = &after[end + 1..];
        } else {
            out.push('&');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    out
}

/// The utterance a cue is part of: its identifier but the last `-<n>`.
fn utterance(cue: &Cue) -> Option<&str> {
    let (utterance, part) = cue.id.as_deref()?.rsplit_once('-')?;
    (!utterance.is_empty() && !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())).then_some(utterance)
}

/// The cues grouped into turns: a speaker's cues in a row are one turn, an utterance's cues one paragraph of it.
#[must_use]
pub fn turns(cues: &[Cue]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut last_utterance: Option<&str> = None;
    for cue in cues {
        let this = utterance(cue);
        let paragraph = Paragraph {
            start: cue.start,
            end: cue.end,
            text: cue.text.clone(),
        };
        match turns.last_mut() {
            Some(turn) if turn.speaker == cue.speaker => match turn.paragraphs.last_mut() {
                Some(last) if this.is_some() && this == last_utterance => {
                    last.text.push(' ');
                    last.text.push_str(&cue.text);
                    last.end = last.end.max(cue.end);
                }
                _ => turn.paragraphs.push(paragraph),
            },
            _ => turns.push(Turn {
                speaker: cue.speaker.clone(),
                start: cue.start,
                paragraphs: vec![paragraph],
            }),
        }
        last_utterance = this;
    }
    turns
}

/// The paragraph (turn, paragraph) being spoken at `at`: the last that began by then; `None` before the first.
#[must_use]
pub fn paragraph_at(turns: &[Turn], at: Duration) -> Option<(usize, usize)> {
    let turn = turn_at(turns, at)?;
    let paragraphs = &turns.get(turn)?.paragraphs;
    let after = paragraphs.partition_point(|paragraph| paragraph.start <= at);
    Some((turn, after.checked_sub(1)?))
}

/// The turn (its index) being spoken at `at`: the last that began by then; `None` before the first.
#[must_use]
pub fn turn_at(turns: &[Turn], at: Duration) -> Option<usize> {
    let after = turns.partition_point(|turn| turn.start <= at);
    after.checked_sub(1)
}

/// The paragraphs that hold `query` (case and accents as written: matched case-insensitively), as (turn, paragraph)
/// indexes in order; none for a blank query.
#[must_use]
pub fn find(turns: &[Turn], query: &str) -> Vec<(usize, usize)> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    turns
        .iter()
        .enumerate()
        .flat_map(|(at, turn)| {
            let query = query.clone();
            turn.paragraphs
                .iter()
                .enumerate()
                .filter(move |(_, paragraph)| paragraph.text.to_lowercase().contains(&query))
                .map(move |(index, _)| (at, index))
        })
        .collect()
}
