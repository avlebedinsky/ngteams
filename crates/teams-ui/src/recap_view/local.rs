//! The recap's parts of the chat's calls' captions saved on this computer ([`crate::call_transcript`]): merged with
//! Teams' own parts (Teams' transcript of the same time wins), read from the store, marked "Saved on this computer",
//! read again as the recorder keeps more of them, and deleted from More options after a confirmation (their kept
//! summary with them). Their transcript pane says they are the user's own captions instead of Teams' AI notice.

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, Subscription, div,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::DropdownMenu as _;
use teams_proto::recap::RecapPart;
use teams_proto::transcript::read_vtt;
use teams_store::CallTranscript;

use super::{Contents, Read, RecapView, event_row, menu_row, shown_key};
use crate::call_transcript::{self, SAVED_HERE, TranscriptsChanged};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::recap::Transcript;
use crate::recap_summary::{self, cache_key};
use crate::theme::Palette;

/// What a saved transcript's pane says first.
pub const SAVED_NOTICE: &str = "Live captions saved on this computer. Nobody else in the call sees them.";
/// What Delete transcript asks.
pub const DELETE_ASK: &str = "Delete this transcript? It's kept only on this computer, so it can't be brought back.";
/// The note once a transcript was deleted.
pub const DELETED: &str = "Deleted the transcript.";

/// The saved transcripts the recap shows, and Teams' parts they were merged with.
#[derive(Default)]
pub(super) struct Local {
    /// Teams' parts as last read; `None` before, or when the read failed.
    teams: Option<Vec<RecapPart>>,
    saved: Vec<CallTranscript>,
    /// The saved transcript asked for (its chat's Transcript link) before the parts were read.
    wanted: Option<i64>,
    /// Delete transcript was asked: its confirmation shows.
    confirming: bool,
    _changed: Option<Subscription>,
}

impl Local {
    /// Follows the saved transcripts' changes: the recap's own chat's are read again.
    pub(super) fn follow(cx: &mut Context<RecapView>) -> Self {
        let saved = call_transcript::saved(cx);
        let changed = cx.subscribe(&saved, |this: &mut RecapView, _, changed: &TranscriptsChanged, cx| {
            if changed.account == this.core.me() && changed.conversation == this.conversation {
                this.read_saved_again(cx);
            }
        });
        Self {
            _changed: Some(changed),
            ..Self::default()
        }
    }

    /// The parts to show of Teams' (`None`: unread) and the saved transcripts of account `me`.
    pub(super) fn merge(&mut self, teams: Option<Vec<RecapPart>>, saved: Vec<CallTranscript>, me: &str) -> Contents {
        let ours: Vec<RecapPart> = saved
            .iter()
            .map(|transcript| call_transcript::part(me, transcript))
            .collect();
        self.teams.clone_from(&teams);
        self.saved = saved;
        match teams {
            Some(parts) => Contents::Read(call_transcript::merged(parts, ours)),
            None if !ours.is_empty() => Contents::Read(ours),
            None => Contents::Failed,
        }
    }

    fn started_at(&self, id: i64) -> Option<i64> {
        self.saved
            .iter()
            .find(|transcript| transcript.id == id)
            .map(|transcript| transcript.started_at)
    }
}

impl RecapView {
    /// Whether the part shown is a saved transcript.
    #[must_use]
    pub fn shows_saved(&self) -> bool {
        self.saved_shown().is_some()
    }

    /// The saved transcript the part shown is, by its id.
    fn saved_shown(&self) -> Option<i64> {
        call_transcript::local_id(self.part()?.transcript.as_deref()?)
    }

    /// Shows the saved transcript `id` (its chat's Transcript link), its transcript from the top; once the parts are
    /// read when they aren't yet.
    pub fn show_saved(&mut self, id: i64, cx: &mut gpui::Context<Self>) {
        let Some(parts) = self.parts() else {
            self.local.wanted = Some(id);
            return;
        };
        let at = parts.iter().position(|part| {
            part.transcript
                .as_deref()
                .and_then(call_transcript::local_id)
                .is_some_and(|shown| shown == id)
        });
        if let Some(at) = at {
            self.select_part(at, cx);
            self.show_transcript(cx);
        }
    }

    /// The saved transcript asked for before the parts were read, shown now that they are.
    pub(super) fn show_wanted_saved(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(id) = self.local.wanted.take() {
            self.show_saved(id, cx);
        }
    }

    /// Reads saved transcript `id`'s lines (at `location`), as a `WebVTT` transcript timed from its call's start.
    pub(super) fn read_saved(&mut self, location: String, id: i64, cx: &mut gpui::Context<Self>) {
        let started_at = self
            .local
            .started_at(id)
            .or_else(|| self.part().and_then(|part| part.start_ms))
            .unwrap_or_default();
        let reply = self.core.transcript_lines(id);
        self.languages.insert(location.clone(), None);
        cx.spawn(async move |this, cx| {
            let lines = reply.await.unwrap_or_default();
            let vtt = call_transcript::vtt(&call_transcript::cues(started_at, &lines));
            let read = Transcript::of(&vtt)
                .zip(read_vtt(&vtt))
                .map_or(Read::Failed, |(transcript, cues)| Read::Shown {
                    vtt: vtt.into(),
                    transcript,
                    cues: cues.into(),
                });
            let _ = this.update(cx, |this, cx| this.transcript_read(location, read, cx));
        })
        .detach();
    }

    /// The chat's saved transcripts changed (the call goes on, or one was deleted): they are read again, merged with
    /// Teams' parts as last read, and a transcript that grew is read again.
    fn read_saved_again(&mut self, cx: &mut gpui::Context<Self>) {
        let reply = self.core.call_transcripts(self.conversation.clone());
        cx.spawn(async move |this, cx| {
            let saved = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                if matches!(this.contents, Contents::Reading) {
                    return;
                }
                let before = std::mem::take(&mut this.local.saved);
                let grown: Vec<String> = saved
                    .iter()
                    .filter(|now| {
                        before
                            .iter()
                            .find(|was| was.id == now.id)
                            .is_none_or(|was| (was.lines, was.ended_at) != (now.lines, now.ended_at))
                    })
                    .map(|transcript| call_transcript::location(this.core.me(), transcript.id))
                    .collect();
                let shown = this.part().and_then(shown_key);
                let teams = this.local.teams.clone();
                this.contents = this.local.merge(teams, saved, this.core.me());
                this.settle_parts(shown.as_deref(), cx);
                this.transcripts
                    .retain(|location, read| matches!(read, Read::Reading) || !grown.contains(location));
                this.read_transcript(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// More options of a saved transcript: Delete transcript and Refresh (it has no link at Teams to share or open).
    pub(super) fn saved_actions(cx: &mut gpui::Context<Self>) -> Vec<AnyElement> {
        let me = cx.entity().downgrade();
        let more = icon_button("recap-more", Glyph::MORE, "More options").dropdown_menu(move |menu, _, _| {
            let (deleted, again) = (me.clone(), me.clone());
            menu.item(
                menu_row("recap-more-delete", Glyph::TRASH, "Delete transcript").on_click(move |_, _, cx| {
                    let _ = deleted.update(cx, Self::ask_delete);
                }),
            )
            .item(
                menu_row("recap-more-refresh", Glyph::REFRESH, "Refresh").on_click(move |_, _, cx| {
                    let _ = again.update(cx, Self::read);
                }),
            )
        });
        vec![selected_by("recap-more", more).into_any_element()]
    }

    /// Delete transcript: its confirmation shows.
    pub fn ask_delete(&mut self, cx: &mut gpui::Context<Self>) {
        if self.shows_saved() {
            self.local.confirming = true;
            cx.notify();
        }
    }

    /// The confirmation's Cancel.
    pub fn cancel_delete(&mut self, cx: &mut gpui::Context<Self>) {
        self.local.confirming = false;
        cx.notify();
    }

    /// The confirmation's Delete: the saved transcript shown deleted with its lines, and its summary forgotten.
    pub fn delete_saved(&mut self, cx: &mut gpui::Context<Self>) {
        self.local.confirming = false;
        let Some(id) = self.saved_shown() else { return };
        if let Some(key) = self.part().and_then(cache_key) {
            let dir = self
                .settings
                .as_ref()
                .and_then(|settings| settings.read(cx).summaries_dir());
            recap_summary::forget(&key, dir.as_deref(), cx);
            self.summaries.remove(&key);
        }
        call_transcript::delete(&self.core, self.conversation.clone(), id, cx);
        self.note = Some(DELETED.into());
        cx.notify();
    }

    /// The confirmation of Delete transcript, while it is asked.
    pub(super) fn delete_confirmation(&self, palette: Palette, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        if !self.local.confirming || !self.shows_saved() {
            return None;
        }
        let delete = Button::new("recap-delete-confirm")
            .label("Delete")
            .danger()
            .on_click(cx.listener(|this, _, _, cx| this.delete_saved(cx)));
        let cancel = Button::new("recap-delete-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.cancel_delete(cx)));
        Some(
            div()
                .debug_selector(|| "recap-delete".to_owned())
                .flex_none()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .border_1()
                .border_color(gpui::rgb(palette.border))
                .bg(gpui::rgb(palette.surface))
                .child(
                    div()
                        .text_color(gpui::rgb(crate::theme::DANGER))
                        .child(Glyph::TRASH.icon().size(gpui::px(16.0))),
                )
                .child(div().flex_1().min_w_0().text_sm().child(DELETE_ASK))
                .child(selected_by("recap-delete-confirm", delete))
                .child(selected_by("recap-delete-cancel", cancel))
                .into_any_element(),
        )
    }
}

/// The first rows of a saved transcript: whose captions they are.
pub(super) fn notice_rows(palette: Palette) -> Vec<AnyElement> {
    vec![event_row(
        "recap-saved-notice",
        Glyph::MONITOR,
        SAVED_NOTICE.to_owned(),
        palette,
    )]
}

/// "Saved on this computer", beside the parts of a saved transcript.
pub(super) fn saved_mark(palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "recap-saved-here".to_owned())
        .flex_none()
        .flex()
        .items_center()
        .gap_1p5()
        .text_sm()
        .text_color(gpui::rgb(palette.muted_foreground))
        .child(Glyph::MONITOR.icon().size(gpui::px(14.0)))
        .child(SAVED_HERE)
        .into_any_element()
}
