//! Record and transcribe, as Teams' call window offers it in a call or a meeting (`docs/protocol/calling.md`, "More of
//! a running call"):
//!
//! - **More → Record and transcribe → Start recording** asks "Start recording and transcription" first: the spoken
//!   language (English (US) unless the captions named another) with Cancel and Confirm. Confirm starts it
//!   ([`Ask::Record`]): the recording bot is invited when it isn't in the call yet, and told to record. The roster's
//!   recording then shows the red dot and "Recording and transcription — Started by you" to the account, as anyone's
//!   does to everyone.
//! - **Stop recording**, while it records (whoever started it), asks "Stop recording and transcription?" first; Stop
//!   stops it ([`Ask::StopRecording`]), and "Recording and transcription have stopped." shows once the roster says so.
//! - A start or a stop that failed says why in Teams' words ([`teams_core::RecordingProblem`]): the policy's refusal,
//!   the call's, or that something went wrong.
//!
//! Teams' "More options → Choose what to record" (Audio only) isn't offered: no capture showed what it sends.

use super::*;
use teams_core::{RecordingPhase, RecordingProblem};
use teams_proto::captions::SPOKEN_LANGUAGES;

/// The spoken language the start asks in unless the captions named another, as Teams' dialog starts.
const DEFAULT_LANGUAGE: &str = "en-us";

/// What the meeting's privacy policy link opens: Microsoft's privacy statement, as Teams links it when the
/// organization names none.
const PRIVACY_POLICY: &str = "https://go.microsoft.com/fwlink/?LinkId=521839";

/// Record and transcribe's dialogs and notices in the call's window.
#[derive(Debug, Default)]
pub(super) struct RecordUi {
    /// "Start recording and transcription" is asked, with the spoken language picked.
    pub(super) dialog: Option<String>,
    /// "Stop recording and transcription?" is asked.
    pub(super) confirm_stop: bool,
    /// The account asked to stop the recording, until it stopped: its notice says "You stopped recording".
    pub(super) stopping: bool,
    /// "Recording and transcription have stopped." shows: whether the account stopped it.
    pub(super) stopped: Option<bool>,
    /// The account's start or stop that failed, while its notice shows; and the failure last seen.
    problem: Option<RecordingProblem>,
    problem_seen: Option<RecordingProblem>,
}

/// What More's Record and transcribe offers now.
#[derive(Clone, Copy)]
pub(super) struct RecordTools {
    /// It may be offered: the call is connected, not the test call, and the app may write.
    pub(super) available: bool,
    /// The call records now (the roster says so).
    pub(super) recording: bool,
    /// The account's own start or stop under way.
    pub(super) phase: RecordingPhase,
}

/// More's Record and transcribe, at its top: Start recording, or Stop recording while it records.
pub(super) fn record_menu(
    menu: PopupMenu,
    stage: &WeakEntity<CallStage>,
    tools: RecordTools,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if !tools.available {
        return menu;
    }
    let stage = stage.clone();
    // no icon: the kit draws a submenu's in the gutter, out of line with the items' own (Language and speech has none)
    menu.submenu("Record and transcribe", window, cx, move |menu, _, _| {
        let stage = stage.clone();
        if tools.recording {
            menu.item(
                toolbar::menu_item("call-record-stop", Some(Glyph::RECORD_STOP), "Stop recording")
                    .disabled(tools.phase == RecordingPhase::Stopping)
                    .on_click(move |_, _, cx| {
                        let _ = stage.update(cx, |stage, cx| stage.confirm_stop_recording(true, cx));
                    }),
            )
        } else {
            menu.item(
                toolbar::menu_item("call-record-start", Some(Glyph::RECORD), "Start recording")
                    .disabled(tools.phase == RecordingPhase::Starting)
                    .on_click(move |_, _, cx| {
                        let _ = stage.update(cx, CallStage::open_record_dialog);
                    }),
            )
        }
    })
    .separator()
}

impl CallStage {
    /// What More's Record and transcribe offers now.
    pub(super) fn record_tools(&self, cx: &App) -> RecordTools {
        let calls = self.calls.read(cx);
        let live = self.live(cx);
        RecordTools {
            available: !self.core.read_only()
                && live
                    .as_ref()
                    .is_some_and(|call| call.status == CallStatus::Connected && call.kind != CallKind::TestCall),
            recording: live.is_some_and(|call| call.meeting.recording.is_some()),
            phase: calls.recording().phase,
        }
    }

    /// Start recording: "Start recording and transcription" asks the spoken language first.
    pub fn open_record_dialog(&mut self, cx: &mut Context<Self>) {
        let captions = &self.calls.read(cx).captions().spoken_language;
        let language = if SPOKEN_LANGUAGES.iter().any(|(code, _)| code == captions) {
            captions.clone()
        } else {
            DEFAULT_LANGUAGE.to_owned()
        };
        self.record.dialog = Some(language);
        cx.notify();
    }

    /// The start dialog put away, as its Cancel and Close do.
    pub fn close_record_dialog(&mut self, cx: &mut Context<Self>) {
        self.record.dialog = None;
        cx.notify();
    }

    /// The start dialog's spoken language picked.
    fn pick_record_language(&mut self, code: &str, cx: &mut Context<Self>) {
        if self.record.dialog.is_some() {
            self.record.dialog = Some(code.to_owned());
            cx.notify();
        }
    }

    /// The start dialog's Confirm: the recording starts in the language picked, named as Teams names a call's
    /// ("Call with …") or a meeting's (its title).
    fn confirm_record(&mut self, cx: &mut Context<Self>) {
        let Some(language) = self.record.dialog.take() else {
            return;
        };
        let Some(active) = self.live(cx) else { return };
        let title = self.title(&active, cx);
        let meeting = meeting::is_meeting(&active);
        let (title, organizer) = if meeting {
            (
                title.to_string(),
                active.organizer.then(|| self.own_name.clone()).flatten(),
            )
        } else {
            (format!("Call with {title}"), None)
        };
        self.ask(
            Ask::Record {
                title,
                organizer: organizer.map(|name| name.to_string()),
                language,
            },
            cx,
        );
        cx.notify();
    }

    /// "Stop recording and transcription?" shown (`true`, as Stop recording asks first) or put away (Cancel).
    pub fn confirm_stop_recording(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.record.confirm_stop = shown;
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.record.confirm_stop = false;
        self.record.stopping = true;
        self.ask(Ask::StopRecording, cx);
        cx.notify();
    }

    /// A start or a stop that failed since last seen shows its notice.
    pub(super) fn record_problem_seen(&mut self, cx: &App) {
        let problem = self.calls.read(cx).recording().problem;
        if problem != self.record.problem_seen {
            self.record.problem = problem;
            self.record.problem_seen = problem;
            if problem.is_some() {
                self.record.stopping = false;
            }
        }
    }

    /// "Start recording and transcription" over the whole window while it is asked (see the module docs).
    pub(super) fn record_dialog_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let language = self.record.dialog.clone()?;
        let picker = Self::record_language_picker(&language, cx);
        let close = icon_button("call-record-close", Glyph::X, "Close")
            .on_click(cx.listener(|this, _, _, cx| this.close_record_dialog(cx)));
        let cancel = Button::new("call-record-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.close_record_dialog(cx)));
        let confirm = Button::new("call-record-confirm")
            .label("Confirm")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.confirm_record(cx)));
        let body = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_sm()
                    .child("Select the correct language to ensure the accuracy of features that process audio."),
            )
            .child(
                div()
                    .pt_1()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Spoken language"),
            )
            .child(selected_by("call-record-language", picker));
        let dialog = dialog_box("call-record-dialog", palette)
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::BOLD)
                            .child("Start recording and transcription"),
                    )
                    .child(selected_by("call-record-close", close)),
            )
            .child(body)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by("call-record-cancel", cancel))
                    .child(selected_by("call-record-confirm", confirm)),
            );
        Some(scrim("call-record-scrim", dialog))
    }

    /// The start dialog's Spoken language: the language picked, its dropdown listing every one.
    fn record_language_picker(language: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let language = language.to_owned();
        let label = SPOKEN_LANGUAGES
            .iter()
            .find(|(code, _)| *code == language)
            .map_or("English (US)", |(_, label)| *label);
        let stage = cx.weak_entity();
        Button::new("call-record-language")
            .outline()
            .w_full()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label)
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(14.0))),
            )
            .dropdown_menu_with_anchor(Anchor::TopLeft, move |mut menu: PopupMenu, _, _| {
                for &(code, label) in SPOKEN_LANGUAGES {
                    let stage = stage.clone();
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .debug_selector(move || format!("call-record-language-{code}"))
                                .child(label)
                        })
                        .checked(code == language)
                        .on_click(move |_, _, cx| {
                            let _ = stage.update(cx, |stage, cx| stage.pick_record_language(code, cx));
                        }),
                    );
                }
                menu.scrollable(true).max_h(px(360.0))
            })
    }

    /// "Stop recording and transcription?" over the whole window while it is asked: Stop stops it, Cancel leaves it.
    pub(super) fn stop_recording_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.record.confirm_stop {
            return None;
        }
        let cancel = Button::new("call-record-stop-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| this.confirm_stop_recording(false, cx)));
        let stop = Button::new("call-record-stop-confirm")
            .label("Stop")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| this.stop_recording(cx)));
        let dialog = dialog_box("call-record-stop-dialog", palette)
            .w(px(400.0))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Stop recording and transcription?"),
            )
            .child(
                div()
                    .pt_4()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by("call-record-stop-cancel", cancel))
                    .child(selected_by("call-record-stop-confirm", stop)),
            );
        Some(scrim("call-record-stop-scrim", dialog))
    }

    /// The recording's notices besides the one of its start: that it stopped, and a start or stop that failed.
    pub(super) fn recording_notices(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let stopped = self.record.stopped.map(|by_me| {
            let line = if by_me {
                "You stopped recording. If you change your mind, you can start them again anytime."
            } else {
                "The recording will be saved to the cloud."
            };
            meeting::notice(
                (
                    "call-recording-stopped",
                    Glyph::RECORD,
                    "Recording and transcription have stopped.",
                ),
                SharedString::from(line),
                palette,
                cx.listener(|this, _, _, cx| {
                    this.record.stopped = None;
                    cx.notify();
                }),
            )
        });
        let problem = self.record.problem.map(|RecordingProblem { heading, detail }| {
            meeting::notice(
                ("call-recording-problem", Glyph::CIRCLE_ALERT, heading),
                SharedString::from(detail),
                palette,
                cx.listener(|this, _, _, cx| {
                    this.record.problem = None;
                    cx.notify();
                }),
            )
        });
        problem.into_iter().chain(stopped).collect()
    }

    /// The start's notice line for the account that started it: Teams' "Started by you", with its privacy policy.
    pub(super) fn started_by_you(palette: Palette) -> AnyElement {
        div()
            .flex()
            .flex_wrap()
            .gap_x_1()
            .child("Started by you. Let everyone know they're being included.")
            .child(meeting::link_text(
                "call-recording-privacy",
                SharedString::from("Privacy policy"),
                PRIVACY_POLICY.to_owned(),
                palette,
            ))
            .into_any_element()
    }
}

/// A dialog's box, as Teams draws one: a raised, rounded surface.
fn dialog_box(selector: &'static str, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .occlude()
        .w(px(520.0))
        .max_w_full()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .shadow_lg()
}

/// `dialog` centred over the whole window, which it dims.
fn scrim(id: &'static str, dialog: gpui::Div) -> AnyElement {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .p_6()
        .bg(gpui::black().opacity(0.4))
        .occlude()
        .child(dialog)
        .into_any_element()
}
