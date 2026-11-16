//! A chat message often ends a line, a paragraph or the whole text with no final dot. A correction keeps that style:
//! the instructions say so, and as a tool can ignore them, a dot that an answer only adds at such an end is taken out
//! here, after the answer is decoded.

use crate::editor::doc::{Block, Doc};

use super::draft::kept_whole_runs;

/// `corrected` with no final dot where `original`, the draft as written, ends a line or a block without one.
pub fn keep_missing(original: &Doc, corrected: &mut Doc) {
    for (old, new) in original.blocks.iter().zip(&mut corrected.blocks) {
        if old != new {
            keep_missing_in(old, new);
        }
    }
}

fn keep_missing_in(old: &Block, new: &mut Block) {
    let (old_text, new_text) = (old.text(), new.text());
    let old_lines: Vec<&str> = old_text.split('\n').collect();
    let new_lines = lines(&new_text);
    // Lines are matched only when the line breaks came back as they were; else only the block's end is.
    let matched = if old_lines.len() == new_lines.len() {
        old_lines.len()
    } else {
        1
    };
    let dots: Vec<usize> = old_lines
        .iter()
        .rev()
        .zip(new_lines.iter().rev())
        .take(matched)
        .filter_map(|(old, &(start, new))| added_dot(old, new).map(|at| start + at))
        .collect();
    if dots.is_empty() {
        return;
    }
    let whole = kept_whole_runs(&new.runs);
    let mut start = 0;
    let mut spans = Vec::with_capacity(new.runs.len());
    for run in &new.runs {
        spans.push(start..start + run.text.len());
        start += run.text.len();
    }
    for at in dots {
        let Some(index) = spans.iter().position(|span| span.contains(&at)) else {
            continue;
        };
        if !whole[index] {
            new.runs[index].text.remove(at - spans[index].start);
        }
    }
    new.normalize();
}

/// Each line of `text`, with where it starts.
fn lines(text: &str) -> Vec<(usize, &str)> {
    let mut start = 0;
    text.split('\n')
        .map(|line| {
            let at = start;
            start += line.len() + 1;
            (at, line)
        })
        .collect()
}

/// Where in `new` the dot is that it only adds at the end of `old`, a line with no final dot. A dot that takes the place
/// of a final `?`, `!`, `,`, `;` or `:` is a correction and stays, as an ellipsis does.
fn added_dot(old: &str, new: &str) -> Option<usize> {
    let before = new.trim_end().strip_suffix('.')?;
    let last = old.trim_end().chars().next_back()?;
    let replaces_mark = matches!(last, '?' | '!' | ',' | ';' | ':') && !before.ends_with(last);
    let kept = matches!(last, '.' | '…') || before.ends_with('.') || before.trim().is_empty();
    (!kept && !replaces_mark).then_some(before.len())
}
