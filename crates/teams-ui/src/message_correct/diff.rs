//! What a correction changed in a draft, word by word, as the box shows it: one text with the words taken out and those
//! put in marked, and the text as it would be. The structure is the draft's (a correction never changes it), so each
//! block is compared with itself.

use std::ops::Range;
use std::time::{Duration, Instant};

use similar::{Algorithm, DiffOp, capture_diff_slices_deadline};

use crate::editor::doc::{Block, Doc, Kind};

/// The longest a block's comparison may take before the rest counts as changed.
const DIFF_BUDGET: Duration = Duration::from_millis(500);

/// What a marked stretch of a preview is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Words the correction takes out.
    Removed,
    /// Words the correction puts in.
    Added,
}

/// What a correction changes, as the box shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preview {
    /// The draft with the words taken out and those put in.
    pub changes: String,
    /// The marked stretches of [`Self::changes`], in order.
    pub marks: Vec<(Range<usize>, Change)>,
    /// The text as it would be.
    pub result: String,
}

impl Preview {
    fn push(&mut self, text: &str, change: Option<Change>) {
        let start = self.changes.len();
        self.changes.push_str(text);
        if let Some(change) = change {
            self.marks.push((start..self.changes.len(), change));
        }
        if change != Some(Change::Removed) {
            self.result.push_str(text);
        }
    }

    fn line_break(&mut self) {
        self.changes.push('\n');
        self.result.push('\n');
    }

    /// `old` compared with `new`, word by word.
    fn compare(&mut self, old: &str, new: &str) {
        let (old, new) = (words(old), words(new));
        let deadline = Instant::now() + DIFF_BUDGET;
        for operation in capture_diff_slices_deadline(Algorithm::Myers, &old, &new, Some(deadline)) {
            match operation {
                DiffOp::Equal { old_index, len, .. } => self.push(&old[old_index..old_index + len].concat(), None),
                DiffOp::Delete { old_index, old_len, .. } => {
                    self.push(&old[old_index..old_index + old_len].concat(), Some(Change::Removed));
                }
                DiffOp::Insert { new_index, new_len, .. } => {
                    self.push(&new[new_index..new_index + new_len].concat(), Some(Change::Added));
                }
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    self.push(&old[old_index..old_index + old_len].concat(), Some(Change::Removed));
                    self.push(&new[new_index..new_index + new_len].concat(), Some(Change::Added));
                }
            }
        }
    }
}

/// What `original` and `corrected`, the same draft's before and after a correction, show (blank lines at either end left
/// out, as a message's are).
#[must_use]
pub fn preview(original: &Doc, corrected: &Doc) -> Preview {
    let mut preview = Preview::default();
    let shown = |block: &Block| !block.is_blank();
    let first = corrected.blocks.iter().position(shown).unwrap_or(0);
    let last = corrected.blocks.iter().rposition(shown).unwrap_or(0);
    let pairs = original.blocks.iter().zip(&corrected.blocks).enumerate();
    for (index, (old, new)) in pairs.filter(|(index, _)| (first..=last).contains(index)) {
        if index > first {
            preview.line_break();
        }
        preview.push(lead_of(new), None);
        match new.kind {
            Kind::Rule => preview.push("———", None),
            Kind::Picture(_) => preview.push("[Picture]", None),
            _ => preview.compare(&old.text(), &new.text()),
        }
    }
    preview
}

/// What a block starts with in the preview, so a list or a quote reads as one.
fn lead_of(block: &Block) -> &'static str {
    match block.kind {
        Kind::Bullet | Kind::Numbered => "• ",
        Kind::Quote => "│ ",
        _ => "",
    }
}

/// `text` cut into what is compared: words, runs of whitespace, and each other character (punctuation, and each
/// character of a script written without spaces).
fn words(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut class = None;
    for (at, character) in text.char_indices() {
        let now = class_of(character);
        if class != Some(now) || now == Class::Single {
            if at > start {
                found.push(&text[start..at]);
            }
            start = at;
            class = Some(now);
        }
    }
    if start < text.len() {
        found.push(&text[start..]);
    }
    found
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Single,
}

fn class_of(character: char) -> Class {
    if character.is_whitespace() {
        Class::Space
    } else if character.is_alphanumeric() && !written_without_spaces(character) {
        Class::Word
    } else {
        Class::Single
    }
}

/// Kana and Chinese characters: a run of them is many words.
fn written_without_spaces(character: char) -> bool {
    matches!(character, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}
