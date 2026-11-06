//! A call transcribed and not recorded, in a 1:1 or group chat's Recap, as Teams' shows it (`docs/protocol/recap.md`,
//! "A 1:1 or group call's recording"): MCPS lists its transcript with nothing to read
//! ([`RecapPart::is_unread_transcript`]), so the Recap's sessions leave it out, as Teams' lists the recordings alone.
//! Its card's View recap (or Transcript chip) shows it:
//!
//! - **A call recorded too**: its session, the transcribed part beside the recording ("Part 1", "Part 2"), the
//!   transcribed one marked by the transcript's glyph and, while it shows, "This part of the meeting was transcribed
//!   only." under the top row.
//! - **A call transcribed alone**: its day and span as a plain line (no session menu: it isn't one the menu lists),
//!   Share and More options, the shared files and the panes across the whole width, its Transcript saying there is
//!   none, as Teams' does.
//!
//! When the recap's read doesn't list the call's transcript at all, the part its card stands for (its item, sharing
//! link, call and span, from the chat's messages) is shown in its place.

use gpui::{AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div, px, rgb};
use teams_proto::recap::RecapPart;

use super::{Contents, RecapView, ellipsized};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// The note over a part transcribed and not recorded, among a call's parts, as Teams words it.
pub const TRANSCRIBED_ONLY: &str = "This part of the meeting was transcribed only.";

/// The call transcribed alone the recap was asked to show.
#[derive(Default)]
pub(super) struct Transcribed {
    /// Its item's sharing link, once its card asked for it: its part joins the sessions.
    revealed: Option<String>,
    /// The part its card stands for, asked for before the parts were read.
    wanted: Option<RecapPart>,
    /// [`TRANSCRIBED_ONLY`] was put away for the part shown.
    pub(super) note_closed: bool,
}

impl RecapView {
    /// The part of the call transcribed alone that was asked for, by its place among the parts.
    pub(super) fn revealed(&self) -> Option<usize> {
        let link = self.transcribed.revealed.as_deref()?;
        self.parts()?
            .iter()
            .position(|part| part.is_unread_transcript() && part.sharing_link.as_deref() == Some(link))
    }

    /// Shows part `index`, a transcript with nothing to read, among the sessions.
    pub(super) fn reveal(&mut self, index: usize, cx: &mut Context<Self>) {
        self.transcribed.revealed = self
            .parts()
            .and_then(|parts| parts.get(index))
            .and_then(|part| part.sharing_link.clone());
        self.select_part(index, cx);
    }

    /// A call transcribed alone's card (View recap or its Transcript chip): the part it stands for, found among the
    /// recap's parts by its item or its sharing link (once they are read), or shown as `part` when the read lists none.
    pub fn show_transcribed(&mut self, part: RecapPart, cx: &mut Context<Self>) {
        let Some(parts) = self.parts() else {
            self.transcribed.wanted = Some(part);
            return;
        };
        let same = |known: &RecapPart| {
            (part.item_id.is_some() && known.item_id == part.item_id)
                || (part.sharing_link.is_some() && known.sharing_link == part.sharing_link)
        };
        let index = if let Some(index) = parts.iter().position(same) {
            index
        } else {
            let Contents::Read(parts) = &mut self.contents else {
                return;
            };
            parts.push(part.clone());
            parts.sort_by_key(|known| known.start_ms.unwrap_or(i64::MAX));
            let Some(index) = parts.iter().position(same) else {
                return;
            };
            index
        };
        if self
            .parts()
            .and_then(|parts| parts.get(index))
            .is_some_and(RecapPart::is_unread_transcript)
        {
            self.reveal(index, cx);
        } else {
            self.select_part(index, cx);
        }
    }

    /// The call transcribed alone asked for before the parts were read, shown now that they are.
    pub(super) fn show_wanted_transcribed(&mut self, cx: &mut Context<Self>) {
        if let Some(part) = self.transcribed.wanted.take() {
            self.show_transcribed(part, cx);
        }
    }

    /// The glyph of part `index`'s tab: the player's for a recording, the transcript's for a part transcribed only.
    pub(super) fn part_glyph(&self, index: usize) -> Glyph {
        let recorded = self
            .parts()
            .and_then(|parts| parts.get(index))
            .is_some_and(|part| part.recording.is_some());
        if recorded { Glyph::PLAY_MEDIA } else { Glyph::TRANSCRIPT }
    }

    /// [`TRANSCRIBED_ONLY`] under the top row, while a part transcribed only shows among its call's parts.
    pub(super) fn transcribed_note(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let part = self.part()?;
        if part.recording.is_some() || self.session().len() < 2 || self.transcribed.note_closed {
            return None;
        }
        let close =
            icon_button("recap-transcribed-close", Glyph::X, "Close").on_click(cx.listener(|this, _, _, cx| {
                this.transcribed.note_closed = true;
                cx.notify();
            }));
        Some(
            div()
                .debug_selector(|| "recap-transcribed-note".to_owned())
                .flex_none()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded_md()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_sm()
                .text_color(rgb(palette.foreground))
                .child(Glyph::TRANSCRIPT.icon().size(px(16.0)).flex_none())
                .child(div().flex_1().min_w_0().child(TRANSCRIBED_ONLY))
                .child(selected_by("recap-transcribed-close", close))
                .into_any_element(),
        )
    }
}

/// The shown session's line on its own, as Teams draws a call transcribed alone's: no box and no menu.
pub(super) fn session_line(line: String) -> AnyElement {
    div()
        .debug_selector(|| "recap-session-line".to_owned())
        .min_w_0()
        .flex_shrink(1.0)
        .h(px(32.0))
        .flex()
        .items_center()
        .child(ellipsized(line))
        .into_any_element()
}
