//! Text as layers of encoding, so the sanitizer's rules see every layer: a captured URL holds its query values
//! percent-encoded once, a redirect's target twice (`%253D`), a JSON string inside a string backslash-escaped, a
//! message body HTML entities.
//!
//! A [`Layered`] text is a list of pieces, each with what the rules see (`plain`) and what is written back (`source`):
//! equal for text as it came, different for an escape decoded (`plain` "é", `source` "%C3%A9"). Rules replace ranges
//! of the plain text ([`Layered::replace`]), and [`Layered::decode`] peels off one more layer of escapes, so the next
//! round of rules sees `sig=abc+def=` where the capture had `sig%3Dabc%252Bdef%253D`. What no rule touched is written
//! back exactly as it came ([`Layered::source`]).

use std::iter::Peekable;
use std::mem;
use std::ops::Range;

/// How many layers of escapes are decoded at most (`%25253D` → `%253D` → `%3D` → `=`).
pub(super) const DECODE_ROUNDS: usize = 4;

/// A stretch of text: `plain` as the rules see it; `source`, when it differs, as written back.
#[derive(Debug)]
struct Piece {
    plain: String,
    /// `None` for text as it came (written back as `plain`); `Some` for one decoded escape, whose `plain` is one
    /// character.
    source: Option<String>,
}

impl Piece {
    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(&self.plain)
    }

    /// The part between two offsets of `plain`. An escape is one character, never cut: any part of it is all of it.
    fn slice(&self, start: usize, end: usize) -> Option<Self> {
        if start >= end {
            return None;
        }
        match &self.source {
            None => Some(Self {
                plain: self.plain[start..end].to_owned(),
                source: None,
            }),
            Some(source) => (start == 0).then(|| Self {
                plain: self.plain.clone(),
                source: Some(source.clone()),
            }),
        }
    }
}

/// What a range of the plain text becomes.
enum Splice {
    /// Text written as is: a placeholder.
    Replace(String),
    /// A decoded escape: the character the rules see, written back as the source it covers.
    Decode(char),
}

/// A text as pieces; see the module docs.
#[derive(Debug)]
pub(super) struct Layered {
    pieces: Vec<Piece>,
}

impl Layered {
    pub(super) fn new(text: &str) -> Self {
        Self {
            pieces: vec![Piece {
                plain: text.to_owned(),
                source: None,
            }],
        }
    }

    /// The text as the rules see it now.
    pub(super) fn plain(&self) -> String {
        self.pieces.iter().map(|piece| piece.plain.as_str()).collect()
    }

    /// The text as it is written back.
    pub(super) fn source(&self) -> String {
        self.pieces.iter().map(Piece::source).collect()
    }

    /// Where the plain text holds decoded escapes, in order: everything else is written back as the rules see it.
    pub(super) fn escaped(&self) -> Vec<Range<usize>> {
        let mut offset = 0;
        let mut escaped = Vec::new();
        for piece in &self.pieces {
            let end = offset + piece.plain.len();
            if piece.source.is_some() {
                escaped.push(offset..end);
            }
            offset = end;
        }
        escaped
    }

    /// Replaces ranges of the plain text (in order, not overlapping) with text written as is.
    pub(super) fn replace(&mut self, edits: Vec<(Range<usize>, String)>) {
        if !edits.is_empty() {
            self.splice(edits.into_iter().map(|(range, with)| (range, Splice::Replace(with))));
        }
    }

    /// Decodes one layer of escapes in the plain text; `false` when there was none.
    pub(super) fn decode(&mut self) -> bool {
        let escapes = escapes(&self.plain());
        if escapes.is_empty() {
            return false;
        }
        self.splice(escapes.into_iter().map(|(range, char)| (range, Splice::Decode(char))));
        true
    }

    /// Cuts ranges of the plain text (in order, not overlapping) out of the pieces and puts what each becomes in its
    /// place.
    fn splice(&mut self, cuts: impl Iterator<Item = (Range<usize>, Splice)>) {
        let mut splicer = Splicer {
            cuts: cuts.peekable(),
            out: Vec::with_capacity(self.pieces.len()),
            open: None,
        };
        let mut start = 0;
        for piece in mem::take(&mut self.pieces) {
            let end = start + piece.plain.len();
            let mut at = start;
            while at < end {
                at = splicer.step(&piece, start, at);
            }
            start = end;
        }
        splicer.close();
        self.pieces = splicer.out;
    }
}

/// The state of [`Layered::splice`].
struct Splicer<I: Iterator<Item = (Range<usize>, Splice)>> {
    cuts: Peekable<I>,
    out: Vec<Piece>,
    /// The cut being covered: where it ends, what it becomes, the source it covered so far.
    open: Option<(usize, Splice, String)>,
}

impl<I: Iterator<Item = (Range<usize>, Splice)>> Splicer<I> {
    /// Takes the next part of `piece` (which starts at `start` of the plain text) from `at` on: into the open cut, or
    /// as it is up to the next cut. Returns where the part ends.
    fn step(&mut self, piece: &Piece, start: usize, at: usize) -> usize {
        let end = start + piece.plain.len();
        if let Some((until, _, covered)) = &mut self.open {
            let stop = (*until).min(end);
            if let Some(part) = piece.slice(at - start, stop - start) {
                covered.push_str(part.source());
            }
            if stop == *until {
                self.close();
            }
            return stop;
        }
        let Some((range, splice)) = self.cuts.next_if(|(range, _)| range.start < end) else {
            push(&mut self.out, piece.slice(at - start, end - start));
            return end;
        };
        let cut = range.start.max(at);
        push(&mut self.out, piece.slice(at - start, cut - start));
        self.open = Some((range.end.max(cut), splice, String::new()));
        cut
    }

    /// Ends the open cut: a replacement as text written as is, a decoded escape as a piece of its own.
    fn close(&mut self) {
        match self.open.take() {
            Some((_, Splice::Replace(with), _)) => push(
                &mut self.out,
                Some(Piece {
                    plain: with,
                    source: None,
                }),
            ),
            Some((_, Splice::Decode(char), covered)) => self.out.push(Piece {
                plain: char.to_string(),
                source: Some(covered),
            }),
            None => {}
        }
    }
}

/// Appends a piece, joining text as it came to the text before it.
fn push(out: &mut Vec<Piece>, piece: Option<Piece>) {
    let Some(piece) = piece else {
        return;
    };
    match out.last_mut() {
        Some(last) if last.source.is_none() && piece.source.is_none() => last.plain.push_str(&piece.plain),
        _ => out.push(piece),
    }
}

/// `text` with every layer of escapes decoded: what an identity is compared by.
pub(super) fn decode_all(text: &str) -> String {
    if !text.contains(['%', '\\', '&']) {
        return text.to_owned();
    }
    let mut layered = Layered::new(text);
    for _ in 0..DECODE_ROUNDS {
        if !layered.decode() {
            break;
        }
    }
    layered.plain()
}

/// `text` with one layer of escapes decoded, twice: as is, and with a NUL on both sides of every decoded character
/// (so a word that only joins an escaped letter, `%41Jane`, is seen apart from it too); `None` when there is no escape.
pub(super) fn decode_once(text: &str) -> Option<(String, String)> {
    let escapes = escapes(text);
    if escapes.is_empty() {
        return None;
    }
    let mut plain = String::with_capacity(text.len());
    let mut apart = String::with_capacity(text.len() + escapes.len() * 2);
    let mut at = 0;
    for (range, char) in escapes {
        plain.push_str(&text[at..range.start]);
        plain.push(char);
        apart.push_str(&text[at..range.start]);
        apart.extend(['\0', char, '\0']);
        at = range.end;
    }
    plain.push_str(&text[at..]);
    apart.push_str(&text[at..]);
    Some((plain, apart))
}

/// Every escape in `text`, with the character it stands for, in order: percent escapes (a run of them read as UTF-8,
/// a byte that isn't as Latin-1), backslash escapes (`\n`, `\"`, `\\`, `\uXXXX` and surrogate pairs, `\xXX`), and HTML
/// character references (`&#233;`, `&#xE9;`, `&amp;`, `&quot;`, `&apos;`, `&lt;`, `&gt;`, `&nbsp;`).
fn escapes(text: &str) -> Vec<(Range<usize>, char)> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let next = match bytes[at] {
            b'%' => percent_run(bytes, at, &mut found),
            b'\\' => backslash(bytes, at, &mut found),
            b'&' => entity(text, at, &mut found),
            _ => None,
        };
        at = next.unwrap_or(at + 1);
    }
    found
}

/// The value of the two hex digits at `at`.
fn hex_byte(bytes: &[u8], at: usize) -> Option<u8> {
    let digits = bytes.get(at..at + 2)?;
    u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

/// A run of percent escapes from `at`, decoded; where the run ends, or `None` when there is no escape at `at`.
fn percent_run(bytes: &[u8], at: usize, found: &mut Vec<(Range<usize>, char)>) -> Option<usize> {
    let mut run = Vec::new();
    let mut end = at;
    while bytes.get(end) == Some(&b'%') {
        let Some(byte) = hex_byte(bytes, end + 1) else {
            break;
        };
        run.push(byte);
        end += 3;
    }
    if run.is_empty() {
        return None;
    }
    let mut index = 0;
    while index < run.len() {
        let (char, len) = utf8_char(&run[index..]).unwrap_or((char::from(run[index]), 1));
        let start = at + index * 3;
        found.push((start..start + len * 3, char));
        index += len;
    }
    Some(end)
}

/// The UTF-8 character `bytes` start with, and its length.
fn utf8_char(bytes: &[u8]) -> Option<(char, usize)> {
    let len = match bytes.first()? {
        0x00..=0x7F => 1,
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    let text = std::str::from_utf8(bytes.get(..len)?).ok()?;
    Some((text.chars().next()?, len))
}

/// A backslash escape at `at`, decoded; where it ends, or `None` when it is none we read.
fn backslash(bytes: &[u8], at: usize, found: &mut Vec<(Range<usize>, char)>) -> Option<usize> {
    let (char, len) = match *bytes.get(at + 1)? {
        b'n' => ('\n', 2),
        b'r' => ('\r', 2),
        b't' => ('\t', 2),
        b'b' => ('\u{8}', 2),
        b'f' => ('\u{c}', 2),
        b'v' => ('\u{b}', 2),
        quoted @ (b'"' | b'\'' | b'/' | b'\\') => (char::from(quoted), 2),
        b'x' => (char::from(hex_byte(bytes, at + 2)?), 4),
        b'u' => unicode_escape(bytes, at)?,
        _ => return None,
    };
    found.push((at..at + len, char));
    Some(at + len)
}

/// `\uXXXX` at `at`, or a surrogate pair `😀`; a lone surrogate is none.
fn unicode_escape(bytes: &[u8], at: usize) -> Option<(char, usize)> {
    let unit = |at: usize| -> Option<u32> {
        if bytes.get(at..at + 2)? != b"\\u" {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(bytes.get(at + 2..at + 6)?).ok()?, 16).ok()
    };
    let high = unit(at)?;
    if !(0xD800..0xDC00).contains(&high) {
        return Some((char::from_u32(high)?, 6));
    }
    let low = unit(at + 6).filter(|low| (0xDC00..0xE000).contains(low))?;
    Some((char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00))?, 12))
}

/// An HTML character reference at `at`, decoded; where it ends, or `None` when it is none we read.
fn entity(text: &str, at: usize, found: &mut Vec<(Range<usize>, char)>) -> Option<usize> {
    let rest = &text[at..];
    // the longest we read is `&#x10FFFF;`
    let (end, _) = rest.char_indices().take(12).find(|&(_, char)| char == ';')?;
    let name = &rest[1..end];
    let char = match name {
        "amp" => '&',
        "quot" => '"',
        "apos" => '\'',
        "lt" => '<',
        "gt" => '>',
        // a space to the rules, so a name split by one is still the name
        "nbsp" => ' ',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    found.push((at..at + end + 1, char));
    Some(at + end + 1)
}
