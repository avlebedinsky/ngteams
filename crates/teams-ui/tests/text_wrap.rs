//! Text wrapping as GPUI does it with the vendored patch `build/patches/gpui-pre/0002`: no line ends right after
//! opening punctuation (UAX #14 LB14), as in Teams, in the drawn wrap and in the clamped label's truncation alike.

use gpui::{IndentAdjustment, LineFragment, TestAppContext, TruncateFrom, px};

const TEXT: &str = "Add it to an [[account]] entry: 彼は「こんにちは」と言った, 【【注意】】 （ｘ） „Zitat“ and \
    ((nested (brackets))) as ((((((((((((((((((((((((((((((((((((((((";

fn opening(c: char) -> bool {
    "([{“«„‚（［｛｟「『【〔〈《〖〘〚｢".contains(c)
}

/// Where each line of `text` ends when drawn `width` wide: shaped, then wrapped, as a text element does.
fn drawn_breaks(cx: &mut TestAppContext, text: &str, width: f32) -> gpui::Result<Vec<usize>> {
    let cx = cx.add_empty_window();
    cx.update(|window, _| {
        let run = window.text_style().to_run(text.len());
        let lines = window.text_system().shape_text(
            text.to_owned().into(),
            px(14.0),
            std::slice::from_ref(&run),
            Some(px(width)),
            None,
        )?;
        Ok(lines
            .iter()
            .flat_map(|line| {
                line.wrap_boundaries()
                    .iter()
                    .map(|at| line.unwrapped_layout.runs[at.run_ix].glyphs[at.glyph_ix].index)
                    .collect::<Vec<_>>()
            })
            .collect())
    })
}

#[gpui::test]
fn no_line_ends_on_an_opening_bracket_or_quote_cjk_ones_too(cx: &mut TestAppContext) {
    let run = TEXT.find(" ((((").unwrap() + 1;
    for width in (120..=720u16).step_by(8) {
        for at in drawn_breaks(cx, TEXT, f32::from(width)).unwrap() {
            let ends = TEXT[..at].chars().next_back().unwrap();
            // a run of brackets longer than the line breaks inside itself rather than overflow it
            let in_the_run = at > run + 4;
            assert!(
                !opening(ends) || in_the_run,
                "a line ends on {ends:?} at byte {at}, {width} px wide"
            );
        }
    }
    // the long run itself does break, somewhere, so it never runs off the line
    let breaks = drawn_breaks(cx, TEXT, 160.0).unwrap();
    assert!(
        breaks.iter().any(|&at| at > run + 4),
        "the bracket run is broken: {breaks:?}"
    );
}

#[gpui::test]
fn a_clamped_label_is_cut_where_its_lines_wrap(cx: &mut TestAppContext) {
    // `line_clamp` with an ellipsis: what the truncation keeps must wrap into the allowed lines, or its last line
    // overflows unmarked
    let font = gpui::font(".SystemUIFont");
    let mut wrapper = cx.text_system().line_wrapper(font, px(14.0));
    let run = gpui::TextRun {
        len: TEXT.len(),
        font: gpui::font(".SystemUIFont"),
        color: gpui::black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    for max_lines in 2..=3 {
        for width in (60..=400u16).step_by(2) {
            let width = px(f32::from(width));
            let (kept, _) = wrapper.truncate_wrapped_line(
                TEXT.into(),
                width,
                max_lines,
                "\u{2026}",
                std::slice::from_ref(&run),
                TruncateFrom::End,
            );
            let lines = 1 + wrapper
                .wrap_line(&[LineFragment::text(&kept)], width, IndentAdjustment::default())
                .count();
            assert!(
                lines <= max_lines,
                "'{kept}' wraps into {lines} lines at {width:?}, {max_lines} allowed"
            );
        }
    }
}
