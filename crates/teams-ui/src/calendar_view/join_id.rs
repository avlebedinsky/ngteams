//! The calendar's "Join with an ID", as Teams' toolbar has it: a panel asking for the meeting's ID and passcode, and Join
//! meeting. The meeting is found among the account's calendar events by the ID and passcode its short join link
//! carries (`teams_core::MeetingCode`), then joined as its card's Join joins it: its pre-join, its organizer read from
//! its chat when its link names none. A meeting that isn't in the calendar is looked for among the account's meeting
//! chats by the link their threads name ([`CalendarViewEvent::JoinCode`], `Shell::join_meeting_code`).

use std::time::Duration;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, Window,
    div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use teams_core::{CalendarEvent, MeetingCode};

use super::{CalendarView, CalendarViewEvent};
use crate::calls::MeetingToJoin;
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::theme::Palette;
use crate::trace::TraceLine;

/// How far back and ahead of now the calendar is looked through for the meeting.
const LOOKED_THROUGH: Duration = Duration::from_hours(62 * 24);

/// What the panel says when the ID or the passcode isn't one.
pub const ID_INVALID: &str = "Enter the meeting ID and passcode from the invitation.";

/// The panel while it is open.
pub(super) struct JoinIdForm {
    id: Entity<InputState>,
    passcode: Entity<InputState>,
    problem: Option<&'static str>,
    /// The calendar is being looked through.
    finding: bool,
}

impl JoinIdForm {
    /// Its boxes.
    pub(super) fn inputs(&self) -> [&Entity<InputState>; 2] {
        [&self.id, &self.passcode]
    }
}

impl CalendarView {
    /// Join with an ID: the panel opens, the ID's box focused.
    pub fn open_join_id(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = cx.new(|cx| InputState::new(window, cx).placeholder("Type a meeting ID"));
        let passcode = cx.new(|cx| InputState::new(window, cx).placeholder("Type a meeting passcode"));
        id.update(cx, |input, cx| input.focus(window, cx));
        self.trace.record(&TraceLine::new("calendar", "join-id"));
        self.join_id = Some(JoinIdForm {
            id,
            passcode,
            problem: None,
            finding: false,
        });
        cx.notify();
    }

    /// The panel closes.
    pub fn close_join_id(&mut self, cx: &mut Context<Self>) {
        if self.join_id.take().is_some() {
            cx.notify();
        }
    }

    /// Join meeting: the ID and passcode read, the meeting found in the calendar and joined.
    pub fn join_by_id(&mut self, cx: &mut Context<Self>) {
        let Some(form) = &mut self.join_id else { return };
        if form.finding {
            return;
        }
        let (id, passcode) = (form.id.read(cx).value(), form.passcode.read(cx).value());
        let Some(code) = MeetingCode::typed(&id, &passcode) else {
            form.problem = Some(ID_INVALID);
            cx.notify();
            return;
        };
        form.finding = true;
        form.problem = None;
        cx.notify();
        let now = calendar_now_ms();
        let span = i64::try_from(LOOKED_THROUGH.as_millis()).unwrap_or(i64::MAX);
        let reply = self
            .core
            .stored_calendar(now.saturating_sub(span), now.saturating_add(span));
        cx.spawn(async move |this, cx| {
            let events = reply.await.unwrap_or_default();
            let found = events
                .into_iter()
                .find(|event| event.meeting_code.as_ref() == Some(&code));
            let _ = this.update(cx, |this, cx| this.found(found, code, cx));
        })
        .detach();
    }

    /// The meeting looked for: joined as its card joins it, or by its ID when the calendar doesn't have it.
    fn found(&mut self, event: Option<CalendarEvent>, code: MeetingCode, cx: &mut Context<Self>) {
        if self.join_id.take().is_none() {
            return;
        }
        let Some(event) = event else {
            cx.emit(CalendarViewEvent::JoinCode(code));
            cx.notify();
            return;
        };
        let title = if event.subject.is_empty() {
            "Meeting".to_owned()
        } else {
            event.subject.clone()
        };
        match (event.meeting_chat, event.meeting) {
            (Some(chat), Some(meeting)) => cx.emit(CalendarViewEvent::Join(MeetingToJoin { chat, meeting, title })),
            (Some(chat), None) => cx.emit(CalendarViewEvent::JoinChat {
                chat,
                title,
                link: event.join_url,
            }),
            _ => cx.emit(CalendarViewEvent::JoinCode(code)),
        }
        cx.notify();
    }

    /// The panel, while it is open: under the toolbar on the right, as Teams' flyout.
    pub(super) fn join_id_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let form = self.join_id.as_ref()?;
        let label = |text: &'static str| div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(text);
        let problem = form.problem.map(|problem| {
            div()
                .debug_selector(|| "calendar-join-id-problem".to_owned())
                .text_sm()
                .text_color(rgb(crate::theme::DANGER))
                .child(SharedString::from(problem))
        });
        Some(
            div()
                .debug_selector(|| "calendar-join-id-panel".to_owned())
                .id("calendar-join-id-panel")
                .absolute()
                .top(px(52.0))
                .right(px(16.0))
                .w(px(360.0))
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Join a meeting with an ID"),
                        )
                        .child(selected_by(
                            "calendar-join-id-close",
                            icon_button("calendar-join-id-close", Glyph::X, "Close")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.close_join_id(cx))),
                        )),
                )
                .child(label("Meeting ID"))
                .child(
                    div()
                        .debug_selector(|| "calendar-join-id-id".to_owned())
                        .child(Input::new(&form.id)),
                )
                .child(label("Meeting passcode"))
                .child(
                    div()
                        .debug_selector(|| "calendar-join-id-passcode".to_owned())
                        .child(Input::new(&form.passcode)),
                )
                .children(problem)
                .child(
                    div().flex().justify_end().child(selected_by(
                        "calendar-join-id-join",
                        Button::new("calendar-join-id-join")
                            .label("Join meeting")
                            .primary()
                            .spinning(form.finding)
                            .on_click(cx.listener(|this, _, _, cx| this.join_by_id(cx))),
                    )),
                ),
        )
    }
}

/// Now, in ms since the epoch.
fn calendar_now_ms() -> i64 {
    crate::calendar::now().timestamp_millis()
}
