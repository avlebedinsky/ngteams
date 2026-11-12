//! The calendar's "Meet now", as Teams' toolbar has it: a split button whose face opens "Start a meeting now" (the
//! meeting's name, "Meeting with <first name>" until edited; Start meeting and Get a link to share) and whose menu has
//! "Join with an ID" ([`super::join_id`]). Both buttons make a meeting of the account's own
//! (`teams_core::calendar`'s Meet now: its chat and links, nobody invited and no event on the calendar). Start meeting
//! then opens its pre-join, as a meeting's Join does ([`CalendarViewEvent::JoinChat`]); Get a link to share shows the
//! meeting's short link with Copy, Start meeting and Share via email (a new mail in the desktop's mail app).

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};
use teams_proto::MeetNowMeeting;

use super::{CalendarView, CalendarViewEvent};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::theme::Palette;
use crate::trace::TraceLine;

/// What the dialog says when the meeting couldn't be made.
pub const MEET_NOW_FAILED: &str = "The meeting couldn't be started. Check the connection, then try again.";

/// What the dialog's buttons asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// Start meeting: its pre-join once it is made.
    Start,
    /// Get a link to share: its link once it is made.
    Link,
}

/// "Start a meeting now", while it is open.
pub(super) struct MeetNowForm {
    name: Entity<InputState>,
    /// The meeting being made, and for what.
    making: Option<Asked>,
    /// The meeting Get a link to share made: its link shown.
    made: Option<MeetNowMeeting>,
    /// Its name, as it was made.
    subject: String,
    copied: bool,
    problem: Option<&'static str>,
}

impl MeetNowForm {
    /// Its name's box.
    pub(super) fn input(&self) -> &Entity<InputState> {
        &self.name
    }
}

impl CalendarView {
    /// Meet now: "Start a meeting now" opens, its name "Meeting with <first name>" once the account's name is known.
    pub fn open_meet_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.core.read_only() {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).default_value("Meeting"));
        name.update(cx, |input, cx| input.focus(window, cx));
        self.trace.record(&TraceLine::new("calendar", "meet-now"));
        let reply = self.core.person(self.core.me().to_owned());
        let named = name.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(person) = reply.await else { return };
            let Some(first) = person.name.as_deref().and_then(|name| name.split_whitespace().next()) else {
                return;
            };
            let default = format!("Meeting with {first}");
            let _ = named.update_in(cx, |input, window, cx| {
                if input.value() == "Meeting" {
                    input.set_value(default, window, cx);
                }
            });
        })
        .detach();
        self.meet_now = Some(MeetNowForm {
            name,
            making: None,
            made: None,
            subject: String::new(),
            copied: false,
            problem: None,
        });
        cx.notify();
    }

    /// The dialog closes.
    pub fn close_meet_now(&mut self, cx: &mut Context<Self>) {
        if self.meet_now.take().is_some() {
            cx.notify();
        }
    }

    /// Start meeting (`start`) or Get a link to share: the meeting made, then its pre-join opened or its link shown.
    /// Start meeting once the link is shown joins the meeting made.
    pub fn meet(&mut self, start: bool, cx: &mut Context<Self>) {
        let Some(form) = &mut self.meet_now else { return };
        if form.making.is_some() {
            return;
        }
        if let (true, Some(made)) = (start, form.made.clone()) {
            let title = form.subject.clone();
            self.meet_now = None;
            return self.start_made(made, title, cx);
        }
        let subject = match form.name.read(cx).value().trim() {
            "" => "Meeting".to_owned(),
            name => name.to_owned(),
        };
        form.subject.clone_from(&subject);
        form.making = Some(if start { Asked::Start } else { Asked::Link });
        form.problem = None;
        let reply = self.core.meet_now(subject);
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.meeting_made(answer, cx));
        })
        .detach();
        cx.notify();
    }

    /// The meeting was made (or not): its pre-join opens, or its link is shown.
    fn meeting_made(&mut self, answer: Result<MeetNowMeeting, teams_core::EngineError>, cx: &mut Context<Self>) {
        let Some(form) = &mut self.meet_now else { return };
        let asked = form.making.take();
        match answer {
            Ok(made) if asked == Some(Asked::Start) => {
                let title = form.subject.clone();
                self.meet_now = None;
                self.start_made(made, title, cx);
            }
            Ok(made) => {
                self.trace.record(&TraceLine::new("calendar", "meet-now-link"));
                form.made = Some(made);
            }
            Err(teams_core::EngineError::Stopped) => {}
            Err(_) => form.problem = Some(MEET_NOW_FAILED),
        }
        cx.notify();
    }

    /// The meeting made opens in its pre-join, where Join now starts it.
    fn start_made(&mut self, made: MeetNowMeeting, title: String, cx: &mut Context<Self>) {
        self.trace.record(&TraceLine::new("calendar", "meet-now-start"));
        cx.emit(CalendarViewEvent::JoinChat {
            chat: made.thread,
            title,
            link: Some(made.join),
        });
        cx.notify();
    }

    /// The split Meet now: its face opens the dialog, its menu offers Join with an ID.
    pub(super) fn meet_now_button(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let view = cx.entity().downgrade();
        let read_only = self.core.read_only();
        let face = Button::new("calendar-meet-now")
            .ghost()
            .icon(Glyph::VIDEO.icon())
            .when(!self.compact_toolbar(), |button| button.label("Meet now"))
            .tooltip("Start a meeting now")
            .accessibility_label("Start a meeting now")
            .disabled(read_only)
            .on_click(cx.listener(|this, _, window, cx| this.open_meet_now(window, cx)));
        let more = Button::new("calendar-meet-now-more")
            .ghost()
            .px_1()
            .tooltip("More ways to meet")
            .child(
                Glyph::CHEVRON_DOWN
                    .icon()
                    .size(px(14.0))
                    .text_color(rgb(palette.muted_foreground)),
            )
            .dropdown_menu(move |menu, _, _| {
                let view = view.clone();
                menu.min_w(px(180.0)).item(
                    PopupMenuItem::element(|_, _| {
                        div()
                            .debug_selector(|| "calendar-join-id".to_owned())
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Glyph::HASH.icon().size(px(14.0)))
                            .child("Join with an ID")
                    })
                    .on_click(move |_, window, cx: &mut App| {
                        let _ = view.update(cx, |view, cx| view.open_join_id(window, cx));
                    }),
                )
            });
        div()
            .flex()
            .items_center()
            .child(selected_by("calendar-meet-now", face))
            .child(selected_by("calendar-meet-now-more", more))
    }

    /// "Start a meeting now" (or, once Get a link to share made the meeting, its link), over the calendar.
    pub(super) fn meet_now_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let form = self.meet_now.as_ref()?;
        let header = Self::meet_now_header(form, cx);
        let body = match &form.made {
            Some(made) => Self::meet_now_link(made, form.copied, palette, cx),
            None => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(div().text_sm().child("Meeting name"))
                .child(
                    div()
                        .debug_selector(|| "calendar-meet-now-name".to_owned())
                        .child(Input::new(&form.name)),
                ),
        };
        let problem = form.problem.map(|problem| {
            div()
                .debug_selector(|| "calendar-meet-now-problem".to_owned())
                .text_sm()
                .text_color(rgb(crate::theme::DANGER))
                .child(SharedString::from(problem))
        });
        let dialog = div()
            .debug_selector(|| "calendar-meet-now-dialog".to_owned())
            .id("calendar-meet-now-dialog")
            .occlude()
            .w(px(600.0))
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(header)
            .child(body)
            .children(problem)
            .child(Self::meet_now_actions(form, cx));
        Some(
            div()
                .id("calendar-meet-now-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .child(dialog),
        )
    }

    /// The dialog's title ("Start a meeting now", then the meeting's name) and its close.
    fn meet_now_header(form: &MeetNowForm, cx: &mut Context<Self>) -> gpui::Div {
        let title = match &form.made {
            Some(_) => SharedString::from(form.subject.clone()),
            None => SharedString::from("Start a meeting now"),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(selected_by(
                "calendar-meet-now-close",
                icon_button("calendar-meet-now-close", Glyph::X, "Close")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.close_meet_now(cx))),
            ))
    }

    /// The made meeting's link, with Copy.
    fn meet_now_link(made: &MeetNowMeeting, copied: bool, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let link = made.share.clone();
        div()
            .debug_selector(|| "calendar-meet-now-link".to_owned())
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .bg(rgb(palette.background))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(SharedString::from(link.clone())),
            )
            .child(selected_by(
                "calendar-meet-now-copy",
                icon_button(
                    "calendar-meet-now-copy",
                    if copied { Glyph::CHECK } else { Glyph::COPY },
                    if copied { "Copied" } else { "Copy link" },
                )
                .small()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                    if let Some(form) = &mut this.meet_now {
                        form.copied = true;
                    }
                    cx.notify();
                })),
            ))
    }

    /// Start meeting with Get a link to share, or, once the link shows, Share via email.
    fn meet_now_actions(form: &MeetNowForm, cx: &mut Context<Self>) -> gpui::Div {
        let start = Button::new("calendar-meet-now-start")
            .primary()
            .icon(Glyph::VIDEO.icon())
            .label("Start meeting")
            .spinning(form.making == Some(Asked::Start))
            .disabled(form.making.is_some())
            .on_click(cx.listener(|this, _, _, cx| this.meet(true, cx)));
        let second = match &form.made {
            Some(made) => {
                let mail = mail_link(&form.subject, &made.share);
                Button::new("calendar-meet-now-email")
                    .outline()
                    .icon(Glyph::SHARE.icon())
                    .label("Share via email")
                    .on_click(move |_, _, cx: &mut App| cx.open_url(&mail))
            }
            None => Button::new("calendar-meet-now-share")
                .outline()
                .icon(Glyph::LINK.icon())
                .label("Get a link to share")
                .spinning(form.making == Some(Asked::Link))
                .disabled(form.making.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.meet(false, cx))),
        };
        let second_id = if form.made.is_some() {
            "calendar-meet-now-email"
        } else {
            "calendar-meet-now-share"
        };
        div()
            .flex()
            .justify_end()
            .gap_2()
            .child(selected_by("calendar-meet-now-start", start))
            .child(selected_by(second_id, second))
    }
}

/// A new mail inviting to the meeting: its name as the subject, its link as the text.
#[must_use]
pub fn mail_link(subject: &str, link: &str) -> String {
    let escape = |text: &str| {
        text.bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => char::from(byte).to_string(),
                other => format!("%{other:02X}"),
            })
            .collect::<String>()
    };
    format!(
        "mailto:?subject={}&body={}",
        escape(subject),
        escape(&format!("Join the meeting: {link}"))
    )
}
