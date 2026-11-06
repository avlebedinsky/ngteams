//! What the call window shows of a recording or a transcription someone runs, as the roster's recording bot tells it
//! (`docs/protocol/calling.md`, "More of a running call"), as Teams shows it:
//!
//! - **Notices** over the stage's top: "Recording and transcription" ("Audio recording and transcription" for the
//!   audio alone) or "Transcription has started" (a transcription alone), each with who started it — "Started by You.
//!   Let everyone know they're being included." to its starter, "Started by <name>. By attending this meeting, you
//!   agree to being included." to everyone else, with the privacy policy; then "Recording and transcription have
//!   stopped." or "Transcription has stopped" once they stop.
//! - **The header's mark** left of the duration: the red dot while it records, the microphone's while it records the
//!   audio alone, the transcript's while it only transcribes.
//! - **The Transcript pane** (Show transcript; it opens by itself for whoever started a transcription alone): its
//!   translation's language and Close, a bar offering live captions instead ("Switch to live captions"), "AI-generated
//!   content may be incorrect", and a row per start and stop of the transcription, as the roster told them. What is
//!   said isn't shown: no capture showed the transcript's speech reaching the call (the live captions show it).

use super::recording::{Stopped, TranscriptRow};
use super::*;
use crate::translate;

/// What the meeting's privacy policy link opens: Microsoft's privacy statement, as Teams links it when the
/// organization names none.
const PRIVACY_POLICY: &str = "https://go.microsoft.com/fwlink/?LinkId=521839";

impl CallStage {
    /// What the roster told since last seen of the recording and the transcription: a start or a stop shows its notice,
    /// the transcript gets its row, and the account's own transcription alone opens the transcript.
    pub(super) fn recorder_seen(&mut self, active: &ActiveCall) {
        let state = &active.meeting;
        let was_recording = self.recording_seen;
        let recording = state.recording.is_some();
        if recording && !was_recording {
            self.recording_notice = true;
            self.record.stopped = None;
            self.record.transcription_notice = None;
        }
        if !recording {
            self.recording_notice = false;
            if was_recording {
                // it stopped: whoever stopped it, the notice says so
                self.record.stopped = Some(Stopped {
                    by_me: std::mem::take(&mut self.record.stopping),
                    audio: self.record.audio_seen,
                });
                self.record.transcription_notice = None;
            }
        }
        self.recording_seen = recording;
        if let Some(recording) = &state.recording {
            self.record.audio_seen = recording.audio_only;
        }
        let transcribing = state.transcribing.clone();
        match (self.record.transcribing_seen.take(), &transcribing) {
            (None, Some(teams_core::Transcription { by })) => {
                self.record.transcript.push(TranscriptRow {
                    by: by.clone(),
                    started: true,
                });
                if !recording {
                    self.record.transcription_notice = Some(true);
                    self.record.stopped = None;
                    if by.as_ref().is_some_and(|by| by.as_str() == self.core.me()) {
                        self.pane = Pane::Transcript;
                        self.effects = None;
                    }
                }
            }
            (Some(teams_core::Transcription { by }), None) => {
                self.record.transcript.push(TranscriptRow { by, started: false });
                // with a recording, the recording's stop says it
                if !recording && !was_recording {
                    self.record.transcription_notice = Some(false);
                }
            }
            _ => {}
        }
        self.record.transcribing_seen = transcribing;
    }

    /// Opens or closes the Transcript pane, as Show transcript and Hide transcript do.
    pub fn show_transcript(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.pane = Pane::Transcript;
            self.effects = None;
        } else if self.pane == Pane::Transcript {
            self.pane = Pane::None;
        }
        cx.notify();
    }

    /// Whether the Transcript pane is open.
    #[must_use]
    pub fn transcript_shown(&self) -> bool {
        self.pane == Pane::Transcript
    }

    /// Who started a recording or a transcription, as its notice says it: its starter is asked to let everyone know.
    fn started_line(&self, active: &ActiveCall, by: Option<&Mri>, palette: Palette) -> AnyElement {
        let (text, selector) = if by.is_some_and(|by| by.as_str() == self.core.me()) {
            (
                SharedString::from("Started by You. Let everyone know they're being included."),
                "call-recording-privacy",
            )
        } else {
            let who = by.map_or_else(|| SharedString::from(SOMEONE), |mri| self.name_of(active, mri));
            (
                SharedString::from(format!(
                    "Started by {who}. By attending this meeting, you agree to being included."
                )),
                "call-recording-policy",
            )
        };
        meeting::text_with_link(selector, &text, "Privacy policy", PRIVACY_POLICY, palette)
    }

    /// A recording's notice while it shows: "Recording and transcription" (the audio alone: "Audio recording and
    /// transcription") and who started it.
    pub(super) fn recording_notice_view(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let recording = active.meeting.recording.as_ref().filter(|_| self.recording_notice)?;
        let (glyph, heading) = if recording.audio_only {
            (Glyph::MIC, "Audio recording and transcription")
        } else {
            (Glyph::RECORD, "Recording and transcription")
        };
        Some(meeting::notice(
            ("call-recording-notice", glyph, heading),
            self.started_line(active, recording.by.as_ref(), palette),
            palette,
            cx.listener(|this, _, _, cx| {
                this.recording_notice = false;
                cx.notify();
            }),
        ))
    }

    /// A transcription's notice while it shows: "Transcription has started" with who started it, or "Transcription has
    /// stopped".
    pub(super) fn transcription_notice(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let started = self.record.transcription_notice?;
        let close = cx.listener(|this, _, _, cx| {
            this.record.transcription_notice = None;
            cx.notify();
        });
        if !started {
            return Some(meeting::notice(
                (
                    "call-transcription-stopped",
                    Glyph::CALL_TRANSCRIPT,
                    "Transcription has stopped",
                ),
                SharedString::from("If you change your mind, you can start it again anytime."),
                palette,
                close,
            ));
        }
        let by = active
            .meeting
            .transcribing
            .as_ref()
            .and_then(|transcription| transcription.by.as_ref());
        Some(meeting::notice(
            (
                "call-transcription-notice",
                Glyph::CALL_TRANSCRIPT,
                "Transcription has started",
            ),
            self.started_line(active, by, palette),
            palette,
            close,
        ))
    }

    /// The Transcript pane while it is open (see the module docs).
    pub(super) fn transcript_pane(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pane != Pane::Transcript {
            return None;
        }
        let actions = div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.transcript_translate(cx))
            .child(selected_by(
                "call-transcript-close",
                icon_button("call-transcript-close", Glyph::X, "Close transcript")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.show_transcript(false, cx))),
            ));
        let rows = self.record.transcript.iter().map(|row| {
            let who = row
                .by
                .as_ref()
                .map_or_else(|| SharedString::from(SOMEONE), |mri| self.name_of(active, mri));
            let what = if row.started { "started" } else { "stopped" };
            transcript_line(
                Glyph::RECORD,
                SharedString::from(format!("{who} {what} transcription")),
                palette,
            )
            .debug_selector(|| "call-transcript-event".to_owned())
        });
        Some(
            people_pane::side_pane("call-transcript-pane", "Transcript", actions, palette)
                .children((!self.record.transcript_tip_closed).then(|| Self::transcript_tip(palette, cx)))
                .child(
                    div()
                        .id("call-transcript-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_4()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            transcript_line(
                                Glyph::INFO,
                                SharedString::from("AI-generated content may be incorrect"),
                                palette,
                            )
                            .text_color(rgb(palette.muted_foreground)),
                        )
                        .children(rows),
                )
                .into_any_element(),
        )
    }

    /// The pane's bar offering live captions instead: Switch to live captions shows them and closes the pane.
    fn transcript_tip(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let switch = Button::new("call-transcript-captions")
            .label("Switch to live captions")
            .outline()
            .small()
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_transcript(false, cx);
                this.ask(Ask::Captions(true), cx);
            }));
        let close = icon_button("call-transcript-tip-close", Glyph::X, "Close")
            .xsmall()
            .on_click(cx.listener(|this, _, _, cx| {
                this.record.transcript_tip_closed = true;
                cx.notify();
            }));
        div()
            .debug_selector(|| "call-transcript-tip".to_owned())
            .flex_none()
            .px_4()
            .py_2()
            .flex()
            .flex_col()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(Glyph::INFO.icon().size(px(16.0)).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .child("Follow the conversation better in real time by using live captions."),
                    )
                    .child(selected_by("call-transcript-tip-close", close)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .child(selected_by("call-transcript-captions", switch)),
            )
    }

    /// The pane's translation: the language the call's speech is translated into on this computer ("Translate to", as
    /// the live captions' settings have it), or Off.
    fn transcript_translate(&self, cx: &App) -> impl IntoElement + use<> {
        let settings = self.settings.clone();
        let current = settings.read(cx).settings().caption_translate_to;
        selected_by(
            "call-transcript-translate",
            icon_button("call-transcript-translate", Glyph::LANGUAGES, "Translate transcript")
                .small()
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                    let menu = menu.item(translate_item(&settings, (None, "Off"), current));
                    translate::TARGET_LANGUAGES
                        .iter()
                        .fold(menu, |menu, &(code, name)| {
                            menu.item(translate_item(&settings, (Some(code), name), current))
                        })
                        .scrollable(true)
                        .max_h(px(360.0))
                }),
        )
    }
}

/// The pane's translation's language `code` (Off: `None`), checked while it is `current`.
fn translate_item(
    settings: &Entity<SettingsModel>,
    (code, label): (Option<&'static str>, &'static str),
    current: Option<&'static str>,
) -> PopupMenuItem {
    let settings = settings.clone();
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("call-transcript-translate-{}", code.unwrap_or("off")))
            .child(label)
    })
    .checked(code == current)
    .on_click(move |_, _, cx| {
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.caption_translate_to = code, cx);
        });
    })
}

/// A line of the transcript: its mark and its text.
fn transcript_line(glyph: Glyph, text: SharedString, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .text_sm()
        .text_color(rgb(palette.foreground))
        .child(glyph.icon().size(px(14.0)).flex_none())
        .child(div().flex_1().min_w_0().child(text))
}

/// The mark Teams shows left of the call's duration while it records or transcribes: the red dot for a recording, the
/// microphone's for one of the audio alone, the transcript's for a transcription alone.
pub(super) fn indicator(active: &ActiveCall, palette: Palette) -> Option<AnyElement> {
    let state = &active.meeting;
    match (&state.recording, &state.transcribing) {
        (Some(recording), _) if recording.audio_only => Some(
            div()
                .debug_selector(|| "call-recording-audio".to_owned())
                .relative()
                .flex_none()
                .size(px(16.0))
                .child(Glyph::MIC.icon().size(px(14.0)))
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .bottom_0()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(rgb(palette.on_frame)),
                )
                .into_any_element(),
        ),
        (Some(_), _) => Some(
            div()
                .debug_selector(|| "call-recording-dot".to_owned())
                .flex_none()
                .size(px(10.0))
                .rounded_full()
                .bg(rgb(theme::DANGER))
                .into_any_element(),
        ),
        (None, Some(_)) => Some(
            div()
                .debug_selector(|| "call-transcribing".to_owned())
                .flex_none()
                .child(Glyph::TRANSCRIBE.icon().size(px(14.0)))
                .into_any_element(),
        ),
        (None, None) => None,
    }
}
