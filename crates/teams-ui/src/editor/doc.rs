//! The editor's document: blocks of styled runs, a place in them ([`Pos`]) and every edit the editor makes, kept apart
//! from drawing so it is tested on its own.
//!
//! - A [`Block`] is a paragraph, a heading, a line of a quote, a list item (bulleted or numbered, nested by its
//!   `indent`), a block of code or a horizontal rule; a paragraph's `indent` is Teams' "Increase indent". Its text is
//!   its runs' text one after the other; a `\n` in it is a line break (`<br>`) inside the block.
//! - A [`Run`] is text and its [`Attrs`]: the marks (bold, italic, underline, strikethrough, inline code), the span's
//!   colour, highlight and size, a link, or a mention. A mention's run is one atom: it is never merged with the text
//!   beside it, no place falls inside it, and a deletion that reaches it takes it whole.
//! - A [`Pos`] is a block and a byte offset in its text, always on a character boundary and never inside a mention.
//!   The document always has one block at least.

use std::ops::Range;

use teams_proto::chat::{Marks, SpanStyle};

/// What a run of text carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Attrs {
    pub marks: Marks,
    pub span: SpanStyle,
    /// The link it is, by its address.
    pub link: Option<String>,
    /// The mention it is, by whom it names (a work account's MRI, or a group chat's id for everyone in it).
    pub mention: Option<String>,
}

impl Attrs {
    /// The attributes text typed after a run with these goes on with: the same, but never a mention.
    #[must_use]
    pub fn continued(&self) -> Self {
        Self {
            mention: None,
            ..self.clone()
        }
    }
}

/// A run of text with its attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub attrs: Attrs,
}

impl Run {
    #[must_use]
    pub fn new(text: impl Into<String>, attrs: Attrs) -> Self {
        Self {
            text: text.into(),
            attrs,
        }
    }

    fn is_mention(&self) -> bool {
        self.attrs.mention.is_some()
    }
}

/// What kind of block it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Kind {
    #[default]
    Paragraph,
    /// A heading, its level 1 to 6 (Teams' paragraph styles make 1 to 3; a message read may hold the others).
    Heading(u8),
    /// A line of a quote; quote lines one after the other are one quote.
    Quote,
    /// An item of a bulleted list.
    Bullet,
    /// An item of a numbered list.
    Numbered,
    /// A block of code: its lines one text, split by `\n`.
    Code,
    /// A horizontal rule: no text.
    Rule,
    /// A picture pasted into the box, by its id in the editor's pictures (`crate::editor::pictures`): no text.
    Picture(u64),
}

impl Kind {
    /// Whether it is a block with no text of its own, taken whole (a rule, a picture).
    #[must_use]
    pub const fn is_atom(self) -> bool {
        matches!(self, Self::Rule | Self::Picture(_))
    }

    /// Whether it is a list's item.
    #[must_use]
    pub const fn is_list(self) -> bool {
        matches!(self, Self::Bullet | Self::Numbered)
    }
}

/// The deepest a list nests or a paragraph is indented.
pub const MAX_INDENT: u8 = 8;

/// A block of the document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Block {
    pub kind: Kind,
    /// A list item's depth (0 the outermost list), or how many steps a paragraph is indented.
    pub indent: u8,
    pub runs: Vec<Run>,
}

impl Block {
    #[must_use]
    pub fn new(kind: Kind, indent: u8, runs: Vec<Run>) -> Self {
        let mut block = Self { kind, indent, runs };
        block.normalize();
        block
    }

    /// A paragraph of plain text.
    #[must_use]
    pub fn paragraph(text: &str) -> Self {
        Self::new(Kind::Paragraph, 0, vec![Run::new(text, Attrs::default())])
    }

    /// Its text.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// Its text's length, bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runs.iter().map(|run| run.text.len()).sum()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether it shows nothing: no text but whitespace and no mention (a rule shows itself).
    #[must_use]
    pub fn is_blank(&self) -> bool {
        !self.kind.is_atom()
            && self
                .runs
                .iter()
                .all(|run| !run.is_mention() && run.text.trim().is_empty())
    }

    /// Joins runs of the same attributes (never a mention's) and drops empty ones.
    pub fn normalize(&mut self) {
        let mut runs: Vec<Run> = Vec::with_capacity(self.runs.len());
        for run in std::mem::take(&mut self.runs) {
            if run.text.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some(last) if last.attrs == run.attrs && !last.is_mention() && !run.is_mention() => {
                    last.text.push_str(&run.text);
                }
                _ => runs.push(run),
            }
        }
        self.runs = runs;
    }

    /// Each run's range in the text.
    pub fn ranges(&self) -> impl Iterator<Item = (Range<usize>, &Run)> {
        let mut at = 0;
        self.runs.iter().map(move |run| {
            let start = at;
            at += run.text.len();
            (start..at, run)
        })
    }

    /// The mention atoms' ranges.
    #[must_use]
    pub fn mention_ranges(&self) -> Vec<Range<usize>> {
        self.ranges()
            .filter(|(_, run)| run.is_mention())
            .map(|(range, _)| range)
            .collect()
    }

    /// `offset` on a character boundary and outside a mention: moved to the mention's start, or its end with
    /// `forward`.
    #[must_use]
    pub fn snap(&self, offset: usize, forward: bool) -> usize {
        let text = self.text();
        let mut offset = text.floor_char_boundary(offset.min(text.len()));
        for range in self.mention_ranges() {
            if offset > range.start && offset < range.end {
                offset = if forward { range.end } else { range.start };
            }
        }
        offset
    }

    /// Splits the runs so that one starts at `offset` (outside a mention); the index of the run starting there.
    fn split_at(&mut self, offset: usize) -> usize {
        let mut at = 0;
        for index in 0..self.runs.len() {
            let len = self.runs[index].text.len();
            if offset == at {
                return index;
            }
            if offset < at + len {
                let tail = self.runs[index].text.split_off(offset - at);
                let attrs = self.runs[index].attrs.clone();
                self.runs.insert(index + 1, Run::new(tail, attrs));
                return index + 1;
            }
            at += len;
        }
        self.runs.len()
    }

    /// The runs of `range`, copied.
    #[must_use]
    pub fn slice(&self, range: Range<usize>) -> Vec<Run> {
        self.ranges()
            .filter_map(|(at, run)| {
                let start = at.start.max(range.start);
                let end = at.end.min(range.end);
                (start < end).then(|| Run::new(&run.text[start - at.start..end - at.start], run.attrs.clone()))
            })
            .collect()
    }

    /// Removes `range` (a mention it reaches goes whole).
    pub fn remove(&mut self, range: Range<usize>) {
        let start = self.snap(range.start, false);
        let end = self.snap(range.end, true);
        if start >= end {
            return;
        }
        let first = self.split_at(start);
        let last = self.split_at(end);
        self.runs.drain(first..last);
        self.normalize();
    }

    /// Puts `runs` at `offset`; the offset after them.
    pub fn insert_runs(&mut self, offset: usize, runs: Vec<Run>) -> usize {
        let offset = self.snap(offset, true);
        let len: usize = runs.iter().map(|run| run.text.len()).sum();
        let index = self.split_at(offset);
        self.runs.splice(index..index, runs);
        self.normalize();
        offset + len
    }

    /// Cuts the block at `offset`: what follows it, as runs.
    pub fn split_off(&mut self, offset: usize) -> Vec<Run> {
        let offset = self.snap(offset, true);
        let index = self.split_at(offset);
        let tail = self.runs.split_off(index);
        self.normalize();
        tail
    }

    /// The attributes of the character before `offset` (or after it at the start), as text typed there carries them:
    /// never a mention, and not a link at the link's end.
    #[must_use]
    pub fn attrs_at(&self, offset: usize) -> Attrs {
        let mut found = None;
        for (range, run) in self.ranges() {
            if range.start < offset && offset <= range.end {
                let mut attrs = run.attrs.continued();
                if offset == range.end {
                    attrs.link = None;
                }
                found = Some(attrs);
            } else if found.is_none() && offset == 0 && range.start == 0 {
                let mut attrs = run.attrs.continued();
                attrs.link = None;
                found = Some(attrs);
            }
        }
        found.unwrap_or_default()
    }

    /// Changes the attributes of the text in `range` with `change`.
    pub fn apply(&mut self, range: Range<usize>, change: &impl Fn(&mut Attrs)) {
        if range.start >= range.end {
            return;
        }
        let first = self.split_at(range.start);
        let last = self.split_at(range.end);
        for run in &mut self.runs[first..last] {
            change(&mut run.attrs);
        }
        self.normalize();
    }

    /// Whether every run in `range` (one at least) has what `test` asks.
    #[must_use]
    pub fn all(&self, range: Range<usize>, test: &impl Fn(&Attrs) -> bool) -> Option<bool> {
        let mut seen = false;
        for (at, run) in self.ranges() {
            if at.end <= range.start || at.start >= range.end {
                continue;
            }
            seen = true;
            if !test(&run.attrs) {
                return Some(false);
            }
        }
        seen.then_some(true)
    }
}

/// A place in the document: a block and a byte offset in its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub block: usize,
    pub offset: usize,
}

impl Pos {
    #[must_use]
    pub const fn new(block: usize, offset: usize) -> Self {
        Self { block, offset }
    }
}

/// The selection: where it started and where it ends (the cursor).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Pos,
    pub head: Pos,
}

impl Selection {
    #[must_use]
    pub const fn caret(at: Pos) -> Self {
        Self { anchor: at, head: at }
    }

    /// Its ends in order.
    #[must_use]
    pub fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// The document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub blocks: Vec<Block>,
}

impl Default for Doc {
    fn default() -> Self {
        Self {
            blocks: vec![Block::default()],
        }
    }
}

impl Doc {
    /// A document of `blocks`; one empty paragraph when there are none.
    #[must_use]
    pub fn new(blocks: Vec<Block>) -> Self {
        if blocks.is_empty() {
            Self::default()
        } else {
            Self { blocks }
        }
    }

    /// Plain text, a line per block.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.blocks.iter().map(Block::text).collect::<Vec<_>>().join("\n")
    }

    /// Whether it shows nothing.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.blocks.iter().all(Block::is_blank)
    }

    /// The end of the document.
    #[must_use]
    pub fn end(&self) -> Pos {
        let last = self.blocks.len() - 1;
        Pos::new(last, self.blocks[last].len())
    }

    /// `pos` inside the document, on a boundary and outside a mention (moved forward with `forward`).
    #[must_use]
    pub fn clamp(&self, pos: Pos, forward: bool) -> Pos {
        let block = pos.block.min(self.blocks.len() - 1);
        Pos::new(block, self.blocks[block].snap(pos.offset, forward))
    }

    /// The text between two places, a `\n` between blocks.
    #[must_use]
    pub fn text_between(&self, start: Pos, end: Pos) -> String {
        self.fragment(start, end)
            .iter()
            .map(Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The blocks between two places, cut at them.
    #[must_use]
    pub fn fragment(&self, start: Pos, end: Pos) -> Vec<Block> {
        (start.block..=end.block.min(self.blocks.len() - 1))
            .map(|index| {
                let block = &self.blocks[index];
                let from = if index == start.block { start.offset } else { 0 };
                let to = if index == end.block { end.offset } else { block.len() };
                Block {
                    kind: block.kind,
                    indent: block.indent,
                    runs: block.slice(from..to),
                }
            })
            .collect()
    }

    /// Deletes what is between two places (in order); where they meet.
    pub fn delete(&mut self, start: Pos, end: Pos) -> Pos {
        if start.block == end.block {
            let block = &mut self.blocks[start.block];
            let from = block.snap(start.offset, false);
            block.remove(from..end.offset);
            return Pos::new(start.block, from);
        }
        let tail_block = &mut self.blocks[end.block];
        let tail = tail_block.split_off(end.offset);
        let tail_kind = (tail_block.kind, tail_block.indent);
        let head = &mut self.blocks[start.block];
        let from = head.snap(start.offset, false);
        head.split_off(from);
        if head.kind.is_atom() {
            // a rule has no text to join: what follows takes its place
            (head.kind, head.indent) = tail_kind;
        }
        head.insert_runs(from, tail);
        self.blocks.drain(start.block + 1..=end.block);
        Pos::new(start.block, from)
    }

    /// Puts `blocks` (a fragment: [`Self::fragment`]) at `at`; the place after them. One block's runs go into the
    /// block at `at`; more split it, the first joining its start and the last its end.
    pub fn insert_fragment(&mut self, at: Pos, mut fragment: Vec<Block>) -> Pos {
        let at = self.clamp(at, true);
        if fragment.is_empty() {
            return at;
        }
        let host_kind = self.blocks[at.block].kind;
        if fragment.len() == 1 && !fragment[0].kind.is_atom() && !host_kind.is_atom() {
            let one = fragment.remove(0);
            let host = &mut self.blocks[at.block];
            if host.is_empty() && one.kind == Kind::Code {
                // code pasted on an empty line is a block of code; other text, copied within its block, takes the
                // line's kind (an empty list item stays one)
                (host.kind, host.indent) = (one.kind, one.indent);
            }
            let offset = host.insert_runs(at.offset, one.runs);
            return Pos::new(at.block, offset);
        }
        let mut head = self.blocks.remove(at.block);
        let tail = if head.kind.is_atom() {
            Block::default()
        } else {
            let runs = head.split_off(at.offset);
            Block::new(head.kind, head.indent, runs)
        };
        let first = fragment.remove(0);
        let mut out = Vec::with_capacity(fragment.len() + 3);
        if !head.kind.is_atom() && !first.kind.is_atom() {
            if head.is_empty() {
                (head.kind, head.indent) = (first.kind, first.indent);
            }
            let len = head.len();
            head.insert_runs(len, first.runs);
            out.push(head);
        } else {
            // an empty line before a rule pasted on it goes
            if head.kind.is_atom() || !head.is_empty() {
                out.push(head);
            }
            out.push(first);
        }
        out.extend(fragment);
        let caret = match out.last_mut() {
            Some(last) if !last.kind.is_atom() => {
                let end = last.len();
                last.insert_runs(end, tail.runs);
                Pos::new(out.len() - 1, end)
            }
            _ => {
                out.push(tail);
                Pos::new(out.len() - 1, 0)
            }
        };
        self.blocks.splice(at.block..at.block, out);
        Pos::new(at.block + caret.block, caret.offset)
    }

    /// Cuts the block at `at` in two (Enter); the start of the new one. A list item, a quote line and an indented
    /// paragraph go on as their kind; after a heading or a rule comes a paragraph.
    pub fn split(&mut self, at: Pos) -> Pos {
        let at = self.clamp(at, true);
        let block = &mut self.blocks[at.block];
        let tail = block.split_off(at.offset);
        let (kind, indent) = match block.kind {
            Kind::Heading(_) if tail.is_empty() => (Kind::Paragraph, 0),
            Kind::Rule | Kind::Picture(_) => (Kind::Paragraph, 0),
            kind => (kind, block.indent),
        };
        if at.offset == 0 && matches!(block.kind, Kind::Heading(_)) && !tail.is_empty() {
            // Enter at a heading's start pushes it down, an empty paragraph above it
            let heading = block.kind;
            block.kind = Kind::Paragraph;
            self.blocks.insert(at.block + 1, Block::new(heading, 0, tail));
            return Pos::new(at.block + 1, 0);
        }
        self.blocks.insert(at.block + 1, Block::new(kind, indent, tail));
        Pos::new(at.block + 1, 0)
    }

    /// Puts `text` (no `\n`, or line breaks within the block) at `at` with `attrs`; the place after it.
    pub fn insert_text(&mut self, at: Pos, text: &str, attrs: Attrs) -> Pos {
        let at = self.clamp(at, true);
        if self.blocks[at.block].kind.is_atom() {
            // typing on a rule types in a paragraph after it
            self.blocks.insert(at.block + 1, Block::default());
            return self.insert_text(Pos::new(at.block + 1, 0), text, attrs);
        }
        let offset = self.blocks[at.block].insert_runs(at.offset, vec![Run::new(text, attrs)]);
        Pos::new(at.block, offset)
    }

    /// The blocks a selection from `start` to `end` touches.
    #[must_use]
    pub fn blocks_of(&self, start: Pos, end: Pos) -> Range<usize> {
        start.block..(end.block + 1).min(self.blocks.len())
    }

    /// Changes the attributes of the text between `start` and `end` with `change`.
    pub fn apply(&mut self, start: Pos, end: Pos, change: &impl Fn(&mut Attrs)) {
        for index in self.blocks_of(start, end) {
            let block = &mut self.blocks[index];
            let from = if index == start.block { start.offset } else { 0 };
            let to = if index == end.block { end.offset } else { block.len() };
            block.apply(from..to, change);
        }
    }

    /// Whether all the text between `start` and `end` has what `test` asks; `None` when there is none.
    #[must_use]
    pub fn all(&self, start: Pos, end: Pos, test: &impl Fn(&Attrs) -> bool) -> Option<bool> {
        let mut seen = None;
        for index in self.blocks_of(start, end) {
            let block = &self.blocks[index];
            let from = if index == start.block { start.offset } else { 0 };
            let to = if index == end.block { end.offset } else { block.len() };
            match block.all(from..to, test) {
                Some(false) => return Some(false),
                Some(true) => seen = Some(true),
                None => {}
            }
        }
        seen
    }

    /// The place one character before `at` (across a block's start, over a mention whole); `None` at the start.
    #[must_use]
    pub fn before(&self, at: Pos) -> Option<Pos> {
        if at.offset == 0 {
            return at
                .block
                .checked_sub(1)
                .map(|block| Pos::new(block, self.blocks[block].len()));
        }
        let block = &self.blocks[at.block];
        let text = block.text();
        let previous = text[..at.offset].chars().next_back().map_or(0, char::len_utf8);
        Some(Pos::new(at.block, block.snap(at.offset - previous, false)))
    }

    /// The place one character after `at`; `None` at the end.
    #[must_use]
    pub fn after(&self, at: Pos) -> Option<Pos> {
        let block = &self.blocks[at.block];
        if at.offset >= block.len() {
            return (at.block + 1 < self.blocks.len()).then(|| Pos::new(at.block + 1, 0));
        }
        let text = block.text();
        let next = text[at.offset..].chars().next().map_or(0, char::len_utf8);
        Some(Pos::new(at.block, block.snap(at.offset + next, true)))
    }

    /// The start of the word before `at` (over spaces first), or of the block.
    #[must_use]
    pub fn word_before(&self, at: Pos) -> Pos {
        if at.offset == 0 {
            return self.before(at).unwrap_or(at);
        }
        let text = self.blocks[at.block].text();
        let head = &text[..at.offset];
        let trimmed = head.trim_end();
        let word_start = trimmed
            .char_indices()
            .rev()
            .take_while(|(_, c)| !c.is_whitespace())
            .last()
            .map_or(trimmed.len(), |(index, _)| index);
        Pos::new(at.block, self.blocks[at.block].snap(word_start, false))
    }

    /// The end of the word after `at` (over spaces first), or of the block.
    #[must_use]
    pub fn word_after(&self, at: Pos) -> Pos {
        let block = &self.blocks[at.block];
        if at.offset >= block.len() {
            return self.after(at).unwrap_or(at);
        }
        let text = block.text();
        let tail = &text[at.offset..];
        let spaces = tail.len() - tail.trim_start().len();
        let word = tail[spaces..]
            .char_indices()
            .find(|(_, c)| c.is_whitespace())
            .map_or(tail.len() - spaces, |(index, _)| index);
        Pos::new(at.block, block.snap(at.offset + spaces + word, true))
    }
}
