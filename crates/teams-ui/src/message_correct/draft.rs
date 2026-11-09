//! A draft as the proofreading tool reads and writes it: its words between numbered markers ([`encode`]), and back
//! ([`Encoded::decode`]), its shape as it was.
//!
//! - **Blocks**: every block with words (a paragraph, a heading, a line of a quote, a list item) is `<xN>…</xN>`. A block
//!   of code, a picture, a rule and a blank line never go to the tool; they stay where they are.
//! - **Runs**: words with a format (a mark, a colour, a link) are `<xN>…</xN>` inside their block; what must come back
//!   exactly (a mention, inline code, a link that shows its own address, a line break) is `<xN/>`.
//! - **Strict to read**, unlike a translation's: the answer is used only when it has every marker exactly once, each in
//!   its own block, and no block lost its words. A correction the user may send must not lose a mention or a link.

use std::fmt::Write as _;

use teams_proto::chat::Marks;

use crate::chat_translate::codec::{Token, escape, tokens, unescape};
use crate::editor::doc::{Attrs, Block, Doc, Kind, Run};

/// What a marker stands for, and the block it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Slot {
    /// A block's words.
    Block(usize),
    /// Words with these attributes.
    Styled(usize, Attrs),
    /// A run put back as it was.
    Atom(usize, Run),
}

/// A draft encoded for the tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Encoded {
    /// What the tool reads.
    pub text: String,
    doc: Doc,
    slots: Vec<Slot>,
    words: bool,
}

/// `doc` as the tool reads it (see the module docs).
#[must_use]
pub fn encode(doc: &Doc) -> Encoded {
    let mut encoded = Encoded {
        text: String::new(),
        doc: doc.clone(),
        slots: Vec::new(),
        words: false,
    };
    for (index, block) in doc.blocks.iter().enumerate() {
        if has_words_to_read(block) {
            encoded.block(index, block);
        }
    }
    encoded
}

/// Whether `block` goes to the tool.
fn has_words_to_read(block: &Block) -> bool {
    !block.kind.is_atom() && block.kind != Kind::Code && !block.is_blank()
}

/// Whether `run` must come back exactly as it is.
fn kept_whole(run: &Run) -> bool {
    run.attrs.mention.is_some()
        || run.attrs.marks.contains(Marks::CODE)
        || run
            .attrs
            .link
            .as_deref()
            .is_some_and(|href| shows_address(&run.text, href))
}

/// Whether a link's text is its own address (with or without its scheme).
fn shows_address(text: &str, href: &str) -> bool {
    let text = text.trim();
    let bare = href.split_once("://").map_or(href, |(_, rest)| rest);
    text == href || text == bare
}

impl Encoded {
    /// The draft it was made from.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Whether it holds any word for the tool to read.
    #[must_use]
    pub fn has_words(&self) -> bool {
        self.words
    }

    fn add(&mut self, slot: Slot) -> usize {
        self.slots.push(slot);
        self.slots.len() - 1
    }

    fn block(&mut self, index: usize, block: &Block) {
        let n = self.add(Slot::Block(index));
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        let _ = write!(self.text, "<x{n}>");
        for run in &block.runs {
            self.run(index, run);
        }
        let _ = write!(self.text, "</x{n}>");
    }

    fn run(&mut self, block: usize, run: &Run) {
        if kept_whole(run) {
            return self.atom(block, run.clone());
        }
        for (at, line) in run.text.split('\n').enumerate() {
            if at > 0 {
                self.atom(block, Run::new("\n", run.attrs.clone()));
            }
            self.line(block, line, &run.attrs);
        }
    }

    fn atom(&mut self, block: usize, run: Run) {
        let n = self.add(Slot::Atom(block, run));
        let _ = write!(self.text, "<x{n}/>");
    }

    /// Words of one line of a run.
    fn line(&mut self, block: usize, line: &str, attrs: &Attrs) {
        if line.is_empty() {
            return;
        }
        self.words |= line.chars().any(char::is_alphanumeric);
        if *attrs == Attrs::default() {
            self.text.push_str(&escape(line));
            return;
        }
        let n = self.add(Slot::Styled(block, attrs.clone()));
        let _ = write!(self.text, "<x{n}>{}</x{n}>", escape(line));
    }

    /// The draft as `answer` corrects it; `None` when it doesn't keep every marker in its place (see the module docs).
    #[must_use]
    pub fn decode(&self, answer: &str) -> Option<Doc> {
        let mut reading = Reading {
            slots: &self.slots,
            seen: vec![false; self.slots.len()],
            block: None,
            styled: None,
            done: Vec::new(),
        };
        for token in tokens(answer) {
            reading.take(&token)?;
        }
        if reading.block.is_some() || reading.seen.iter().any(|seen| !seen) {
            return None;
        }
        let mut doc = self.doc.clone();
        for (index, runs) in reading.done {
            let block = doc.blocks.get(index)?;
            let corrected = Block::new(block.kind, block.indent, runs);
            if corrected.is_blank() {
                return None;
            }
            doc.blocks[index] = corrected;
        }
        Some(doc)
    }
}

/// A block being read from an answer: its marker, its index in the draft and its runs so far.
struct Open {
    marker: usize,
    index: usize,
    runs: Vec<Run>,
}

/// An answer being read.
struct Reading<'a> {
    slots: &'a [Slot],
    seen: Vec<bool>,
    block: Option<Open>,
    /// The styled words the reading is in: their marker and attributes.
    styled: Option<(usize, &'a Attrs)>,
    done: Vec<(usize, Vec<Run>)>,
}

impl Reading<'_> {
    fn take(&mut self, token: &Token<'_>) -> Option<()> {
        match token {
            Token::Text(text) => self.text(text),
            Token::Open(n) => self.open(*n),
            Token::Close(n) => self.close(*n),
            Token::Atom(n) => self.atom(*n),
        }
    }

    /// Marks marker `n` as met, the first time only.
    fn first_time(&mut self, n: usize) -> Option<()> {
        let seen = self.seen.get_mut(n)?;
        (!std::mem::replace(seen, true)).then_some(())
    }

    fn text(&mut self, text: &str) -> Option<()> {
        let Some(open) = &mut self.block else {
            return text.trim().is_empty().then_some(());
        };
        let attrs = self.styled.map_or_else(Attrs::default, |(_, attrs)| attrs.clone());
        open.runs.push(Run::new(unescape(text), attrs));
        Some(())
    }

    fn open(&mut self, n: usize) -> Option<()> {
        self.first_time(n)?;
        match (self.slots.get(n)?, &self.block, &self.styled) {
            (Slot::Block(index), None, _) => {
                self.block = Some(Open {
                    marker: n,
                    index: *index,
                    runs: Vec::new(),
                });
                Some(())
            }
            (Slot::Styled(block, attrs), Some(open), None) if open.index == *block => {
                self.styled = Some((n, attrs));
                Some(())
            }
            _ => None,
        }
    }

    fn close(&mut self, n: usize) -> Option<()> {
        if self.styled.is_some_and(|(marker, _)| marker == n) {
            self.styled = None;
            return Some(());
        }
        let open = self.block.take_if(|open| open.marker == n && self.styled.is_none())?;
        self.done.push((open.index, open.runs));
        Some(())
    }

    fn atom(&mut self, n: usize) -> Option<()> {
        self.first_time(n)?;
        let Slot::Atom(block, run) = self.slots.get(n)? else {
            return None;
        };
        let open = self
            .block
            .as_mut()
            .filter(|open| open.index == *block && self.styled.is_none())?;
        open.runs.push(run.clone());
        Some(())
    }
}
