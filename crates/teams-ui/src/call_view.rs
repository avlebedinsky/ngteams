//! The calls in the main window (M3 Task 17), at the top of the shell's right side ([`CallBar`]). The call itself has
//! a window of its own ([`crate::call_window`], [`crate::call_stage`]), as Teams' call window is: the chats here stay
//! usable while it runs.
//!
//! - **The incoming call's banner** while an invitation the engine offered rings: who calls (the caller's name as the
//!   invitation gives it, "Someone" without one; a group call's chat by its row's name), Answer in Teams' green and
//!   Decline in its red. The ringtone and the notification are the desktop's ([`crate::call_alert`]).
//! - **The in-call strip** while a call is up or being set up, the way back to its window as Teams' call monitor is:
//!   who it is with (a chat's members other than the account, by their names; an invitation's caller; the test
//!   call's echo, "Echo", as a 1:1 call's other member), its state (calling, ringing, connecting, connected with its
//!   duration, reconnecting, taken by voicemail), "Return to call" (the call window brought to the front:
//!   [`crate::call_window::raise`]), the microphone and the camera (a build that shows video) as icons whose tooltips
//!   say Mute or Unmute and Turn camera on or off, Leave in Teams' red, and why an ask went nowhere while it is up.
//!   Once a call ended, how (or why a later ask went nowhere) until the next call or Dismiss (an ✕); a test call's end
//!   is its results, which its window shows ([`crate::call_stage`]), so the strip says nothing of it.
//!
//! A build that shows no calls draws none of it.
//!
//! Every button asks through [`calls::ask`], as the shell's keys and the switcher's commands do. What is shown is
//! traced (`calling registered`/`unregistered` as the calling endpoint goes, `call-banner shown`/`hidden` with the
//! call's number, its kind, whether the caller is named, a group call's chat and the caller's MRI; `in-call shown`
//! with the call's number, its kind, state, end, mute, camera and voicemail, its chat and its participants' MRIs;
//! `in-call hidden`; `test-call results` once a test call ended, a kind per check), never a name.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::Selectable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::tooltip::Tooltip;
use teams_core::{CallId, CallStatus, IncomingCall};
use teams_fence::Mri;

use crate::bridge::Core;
use crate::calls::{self, ActiveCall, Ask, CallKind, Calls, Ended, Refused, Verdict};
use crate::chat_list::{self, ChatList};
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::notify::SOMEONE;
use crate::people::Shown;
use crate::shell::{AnswerCall, DeclineCall, HangUpCall, ToggleCameraCall, ToggleMuteCall};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};

/// What the test call's echo service is called in a call, as Teams' call window names it (its roster gives it no
/// name). Its chat's row takes the app's own "Test call" instead ([`chat_list::ECHO`]).
pub const ECHO_IN_CALL: &str = "Echo";

/// How often the duration of a connected call is drawn again.
pub(crate) const TICK: Duration = Duration::from_secs(1);

/// What the in-call strip was last traced showing.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BarShown {
    call: CallId,
    kind: &'static str,
    status: &'static str,
    end: Option<&'static str>,
    /// The step a failed call failed at (a name fixed in the code).
    failed: Option<&'static str>,
    muted: bool,
    camera: bool,
    voicemail: bool,
    chat: Option<String>,
    people: Vec<String>,
}

/// Who a call is with besides the account, `(MRI, name)`, read once per call: an invitation's caller as it named
/// itself; a chat's members other than the account, by the names the engine has (or their MRIs); the echo service for
/// the test call, as Teams names it.
#[derive(Default)]
pub(crate) struct CallPeople {
    read: Option<(CallId, Vec<(String, SharedString)>)>,
    reading: Option<Task<()>>,
}

impl CallPeople {
    /// Who `call` is with, once read (nothing for another call).
    pub(crate) fn of(&self, call: CallId) -> &[(String, SharedString)] {
        self.read
            .as_ref()
            .filter(|(read, _)| *read == call)
            .map_or(&[], |(_, people)| people.as_slice())
    }

    /// Who the call read last is with.
    pub(crate) fn last(&self) -> &[(String, SharedString)] {
        self.read.as_ref().map_or(&[], |(_, people)| people.as_slice())
    }

    /// Reads who `active` is with, unless it was read already; `slot` finds the people in the view `cx` is, and
    /// `done` runs on it once the names came.
    pub(crate) fn follow<V: 'static>(
        &mut self,
        active: &ActiveCall,
        (core, echo): (&Arc<dyn Core>, Option<&Mri>),
        cx: &mut Context<V>,
        (slot, done): (fn(&mut V) -> &mut Self, fn(&mut V, &mut Context<V>)),
    ) {
        let call = active.id;
        if self.read.as_ref().is_some_and(|(read, _)| *read == call) {
            return;
        }
        self.read = Some((call, Vec::new()));
        self.reading = None;
        if active.kind == CallKind::TestCall {
            let echo = echo.map(|echo| (echo.as_str().to_owned(), SharedString::from(ECHO_IN_CALL)));
            self.read = Some((call, echo.into_iter().collect()));
            return;
        }
        if let Some(caller) = &active.caller {
            let name = active.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
            self.read = Some((call, vec![(caller.as_str().to_owned(), name.into())]));
            if active.chat.is_none() {
                return;
            }
        }
        let Some(chat) = active.chat.clone() else {
            return;
        };
        let core = Arc::clone(core);
        self.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let people = members_named(&core, chat).await;
            let _ = this.update(cx, |view, cx| {
                let slot = slot(view);
                if slot.read.as_ref().is_some_and(|(read, _)| *read == call) {
                    slot.read = Some((call, people));
                    done(view, cx);
                }
            });
        }));
    }
}

/// The members of `chat` other than the account, `(MRI, name)`, each by the name the engine has (or its MRI); a 1:1
/// chat whose members aren't stored, its other member as its id names them.
async fn members_named(core: &Arc<dyn Core>, chat: String) -> Vec<(String, SharedString)> {
    let me = core.me().to_owned();
    let members = core.members(chat.clone()).await.unwrap_or_default();
    let mut others: Vec<String> = members
        .into_iter()
        .map(|member| member.mri)
        .filter(|mri| *mri != me)
        .collect();
    if others.is_empty()
        && let Some(partner) = chat_list::one_to_one_partner(&chat, &me)
    {
        others.push(partner);
    }
    let mut people = Vec::with_capacity(others.len());
    for mri in others {
        let name = match core.person(mri.clone()).await {
            Ok(person) => Shown::of(person).name,
            Err(_) => None,
        };
        let name = name.unwrap_or_else(|| SharedString::from(mri.clone()));
        people.push((mri, name));
    }
    people
}

/// Who a call is with, as its title: the echo for the test call; an invitation's caller (in its group chat, named as
/// `chat_name` names it); a chat's name.
pub(crate) fn call_title(active: &ActiveCall, chat_name: impl Fn(&str) -> SharedString) -> SharedString {
    match active.kind {
        CallKind::TestCall => ECHO_IN_CALL.into(),
        CallKind::Incoming => {
            let caller = active.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
            match &active.chat {
                Some(chat) => format!("{caller} in {}", chat_name(chat)).into(),
                None => caller.into(),
            }
        }
        CallKind::Chat | CallKind::Join => active
            .chat
            .as_deref()
            .map_or_else(|| SharedString::from("Call"), chat_name),
    }
}

/// A call's state as the strip and the window say it, with its duration once connected.
pub(crate) fn status_with_duration(active: &ActiveCall) -> String {
    let status = calls::status_text(active);
    match (&active.status, active.connected_at, active.voicemail) {
        (CallStatus::Connected, Some(at), false) => {
            format!("{status} · {}", calls::duration_text(at.elapsed().as_secs()))
        }
        _ => status,
    }
}

/// A task that draws `cx`'s view again every [`TICK`] while it lives: a connected call's duration.
pub(crate) fn ticking<V: 'static>(cx: &mut Context<V>) -> Task<()> {
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        let mut shown = true;
        while shown {
            cx.background_executor().timer(TICK).await;
            shown = this.update(cx, |_, cx| cx.notify()).is_ok();
        }
    })
}

/// The banner and the in-call strip (see the module docs). A GPUI entity the shell holds.
pub struct CallBar {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    list: Entity<ChatList>,
    /// The last test call whose results were traced.
    results_traced: Option<CallId>,
    trace: Arc<ViewTrace>,
    banner_traced: Option<CallId>,
    bar_traced: Option<BarShown>,
    /// Whether the calling endpoint was last traced registered.
    registered_traced: bool,
    people: CallPeople,
    ticking: Option<Task<()>>,
    _observing: [Subscription; 2],
}

impl CallBar {
    /// The strip over `calls`, a chat named as `list` names it.
    pub fn new(
        core: Arc<dyn Core>,
        calls: Entity<Calls>,
        list: Entity<ChatList>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = [
            cx.observe(&calls, |this: &mut Self, _, cx| this.changed(cx)),
            // a chat's name read again: the banner and the strip name it as its row does
            cx.observe(&list, |_, _, cx| cx.notify()),
        ];
        let mut bar = Self {
            core,
            calls,
            list,
            results_traced: None,
            trace,
            banner_traced: None,
            bar_traced: None,
            registered_traced: false,
            people: CallPeople::default(),
            ticking: None,
            _observing: observing,
        };
        bar.changed(cx);
        bar
    }

    /// The call's participants other than the account, `(MRI, name)`, once read.
    #[must_use]
    pub fn participants(&self) -> &[(String, SharedString)] {
        self.people.last()
    }

    /// The calls changed: the participants read for a new call, the duration ticking while connected, and what is
    /// shown traced.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let model = self.calls.read(cx);
        let active = model.call().cloned();
        let echo = model.echo().cloned();
        if let Some(active) = &active {
            let core = Arc::clone(&self.core);
            self.people.follow(
                active,
                (&core, echo.as_ref()),
                cx,
                (
                    |bar| &mut bar.people,
                    |bar, cx| {
                        bar.trace_now(cx);
                        cx.notify();
                    },
                ),
            );
        }
        let connected = active
            .as_ref()
            .is_some_and(|active| active.status == CallStatus::Connected);
        if connected && self.ticking.is_none() {
            self.ticking = Some(ticking(cx));
        } else if !connected {
            self.ticking = None;
        }
        self.trace_now(cx);
        cx.notify();
    }

    /// Traces the banner and the strip when what they show changed.
    fn trace_now(&mut self, cx: &mut Context<Self>) {
        let model = self.calls.read(cx);
        if model.registered() != self.registered_traced {
            self.registered_traced = model.registered();
            let change = if self.registered_traced {
                "registered"
            } else {
                "unregistered"
            };
            self.trace.record(&TraceLine::new("calling", change));
        }
        let ringing = model.ringing().cloned();
        let shown = model.call().map(|active| self.bar_of(active));
        let ended = model.ended().cloned();
        self.trace_banner(ringing.as_ref());
        match (shown, ended) {
            (Some(shown), _) => {
                if self.bar_traced.as_ref() != Some(&shown) {
                    record_bar(&self.trace, &shown);
                    self.bar_traced = Some(shown);
                }
            }
            (None, Some(ended)) => self.trace_ended(&ended),
            (None, None) => {
                if self.bar_traced.take().is_some() {
                    self.trace.record(&TraceLine::new("in-call", "hidden"));
                }
            }
        }
    }

    /// Traces how the last call ended, once, and a test call's results.
    fn trace_ended(&mut self, ended: &Ended) {
        let shown = BarShown {
            call: ended.id,
            kind: ended.kind.name(),
            status: "ended",
            end: Some(calls::end_name(&ended.end)),
            failed: match ended.end {
                teams_core::End::Failed(step) => Some(step),
                _ => None,
            },
            muted: false,
            camera: false,
            voicemail: ended.voicemail,
            chat: ended.chat.clone(),
            people: Vec::new(),
        };
        self.trace_results(ended);
        if self.bar_traced.as_ref().is_some_and(|traced| traced.call == ended.id) {
            if self.bar_traced.as_ref().is_none_or(|traced| traced.end.is_none()) {
                record_bar(&self.trace, &shown);
                self.bar_traced = Some(shown);
            }
        } else if self.bar_traced.is_none() {
            // ended before it was ever shown here (an invitation declined): its end, once
            record_bar(&self.trace, &shown);
            self.bar_traced = Some(shown);
        }
    }

    /// A test call's results seen for the first time: traced once, a kind per check.
    fn trace_results(&mut self, ended: &Ended) {
        let Some(check) = ended.check else { return };
        if self.results_traced == Some(ended.id) {
            return;
        }
        self.results_traced = Some(ended.id);
        let line = TraceLine::new("test-call", "results")
            .call(ended.id.get())
            .kind(match (check.connected, check.failed) {
                (false, _) => "not-connected",
                (true, true) => "dropped",
                (true, false) => "connected",
            })
            .kind(match (check.microphone_lost, check.heard) {
                (true, _) => "mic-lost",
                (false, true) => "mic-heard",
                (false, false) => "mic-silent",
            })
            .kind(match (check.speaker_lost, check.played) {
                (true, _) => "speaker-lost",
                (false, true) => "speaker-played",
                (false, false) => "speaker-silent",
            })
            .kind(match check.camera() {
                (Verdict::Problem, _) => "camera-failed",
                (Verdict::Fine, _) => "camera-on",
                (Verdict::Untried, _) => "camera-untried",
            });
        self.trace.record(&line);
    }

    fn trace_banner(&mut self, ringing: Option<&IncomingCall>) {
        let call = ringing.map(|ringing| ringing.call);
        if self.banner_traced == call {
            return;
        }
        if let Some(old) = self.banner_traced.take() {
            self.trace
                .record(&TraceLine::new("call-banner", "hidden").call(old.get()));
        }
        if let Some(ringing) = ringing {
            let mut line = TraceLine::new("call-banner", "shown")
                .call(ringing.call.get())
                .kind(if ringing.group_chat.is_some() {
                    "group"
                } else {
                    "one-to-one"
                })
                .kind(if ringing.caller_name.is_some() {
                    "named"
                } else {
                    "unnamed"
                })
                .people([ringing.caller.as_str()]);
            if let Some(chat) = &ringing.group_chat {
                line = line.conversation(chat);
            }
            self.trace.record(&line);
            self.banner_traced = Some(ringing.call);
        }
    }

    fn bar_of(&self, active: &ActiveCall) -> BarShown {
        BarShown {
            call: active.id,
            kind: active.kind.name(),
            status: calls::status_name(&active.status),
            end: None,
            failed: None,
            muted: active.muted,
            camera: active.camera,
            voicemail: active.voicemail,
            chat: active.chat.clone(),
            people: self.people.of(active.id).iter().map(|(mri, _)| mri.clone()).collect(),
        }
    }

    /// Asks `ask` of the calls, as a button does.
    pub fn ask(&self, ask: Ask, cx: &mut Context<Self>) {
        calls::ask(&self.calls, &self.core, &self.trace, ask, cx);
    }

    /// A chat's name as its row has it.
    fn chat_name(&self, chat: &str, cx: &App) -> SharedString {
        chat_name_in(&self.list, chat, cx)
    }

    fn banner(&self, ringing: &IncomingCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let caller = ringing.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
        let text: SharedString = match &ringing.group_chat {
            Some(chat) => format!("{caller} is calling in {}", self.chat_name(chat, cx)).into(),
            None => format!("{caller} is calling").into(),
        };
        strip(palette)
            .debug_selector(|| "call-banner".to_owned())
            .child(call_mark(palette))
            .child(div().flex_1().font_weight(gpui::FontWeight::SEMIBOLD).child(text))
            .child(control(
                "call-answer",
                Control::Labelled(Glyph::PHONE, "Answer"),
                ("Answer", &AnswerCall),
                Look::Success,
                cx,
                |this, cx| this.ask(Ask::Answer, cx),
            ))
            .child(control(
                "call-decline",
                Control::Labelled(Glyph::PHONE_OFF, "Decline"),
                ("Decline", &DeclineCall),
                Look::Danger,
                cx,
                |this, cx| this.ask(Ask::Decline, cx),
            ))
            .into_any_element()
    }

    /// The in-call strip: the call's mark, who it is with and its state, and its controls.
    fn in_call(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let people = self.people.of(active.id);
        let with = (!people.is_empty()).then(|| {
            let names: Vec<&str> = people.iter().map(|(_, name)| name.as_ref()).collect();
            div()
                .debug_selector(|| "in-call-participants".to_owned())
                .truncate()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(format!("With {}", names.join(", "))))
        });
        // an ask that went nowhere while the call is up (a second start, a mute the engine refused) is told here
        let refused = self.calls.read(cx).refused().map(|refused| {
            div()
                .debug_selector(|| "in-call-refused".to_owned())
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(refusal_text(refused)))
        });
        let title = call_title(active, |chat| self.chat_name(chat, cx));
        strip(palette)
            .debug_selector(|| "in-call".to_owned())
            .bg(rgb(palette.selected))
            .child(call_mark(palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().truncate().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                            .child(
                                div()
                                    .debug_selector(|| "in-call-status".to_owned())
                                    .flex_none()
                                    .text_xs()
                                    .text_color(rgb(palette.muted_foreground))
                                    .child(SharedString::from(status_with_duration(active))),
                            ),
                    )
                    .children(with)
                    .children(refused),
            )
            .child(self.controls(active, palette, cx))
            .into_any_element()
    }

    /// The strip's controls: "Return to call", the microphone, the camera (a build that shows video) and Leave, in
    /// Teams' red.
    fn controls(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (mic, mute) = if active.muted {
            (Glyph::MIC_OFF, "Unmute")
        } else {
            (Glyph::MIC, "Mute")
        };
        let camera = self.calls.read(cx).video().is_some().then(|| {
            let (glyph, tip) = if active.camera {
                (Glyph::VIDEO, "Turn camera off")
            } else {
                (Glyph::VIDEO_OFF, "Turn camera on")
            };
            control(
                "call-camera",
                Control::Icon(glyph),
                (tip, &ToggleCameraCall),
                Look::Toggle(!active.camera),
                cx,
                |this, cx| this.ask(Ask::ToggleCamera, cx),
            )
        });
        let back = selected_by(
            "call-return",
            Button::new("call-return")
                .icon(Glyph::POP_OUT.icon())
                .label("Return to call")
                .primary()
                .small()
                .on_click(cx.listener(|_, _, _, cx| crate::call_window::raise(cx))),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(back)
            .child(control(
                "call-mute",
                Control::Icon(mic),
                (mute, &ToggleMuteCall),
                Look::Toggle(active.muted),
                cx,
                |this, cx| this.ask(Ask::ToggleMute, cx),
            ))
            .children(camera)
            .child(div().w(px(1.0)).h(px(24.0)).mx_1().bg(rgb(palette.border)))
            .child(control(
                "call-hang-up",
                Control::Labelled(Glyph::PHONE_OFF, "Leave"),
                ("Leave call", &HangUpCall),
                Look::Danger,
                cx,
                |this, cx| this.ask(Ask::HangUp, cx),
            ))
    }

    /// How the last call ended, or why the last ask went nowhere, with Dismiss; nothing for a test call's end, whose
    /// results its window shows.
    fn after(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.calls.read(cx);
        let refused = model.refused().map(refusal_text);
        let ended = model.ended().filter(|ended| ended.check.is_none());
        let text = refused.or_else(|| ended.map(|ended| calls::end_text(&ended.end, ended.voicemail)))?;
        Some(
            strip(palette)
                .debug_selector(|| "call-after".to_owned())
                .child(div().flex_1().child(SharedString::from(text)))
                .child(self.dismiss(cx))
                .into_any_element(),
        )
    }

    /// Dismiss (an ✕): how the last call ended, or the refusal, goes.
    fn dismiss(&self, cx: &mut Context<Self>) -> gpui::Div {
        selected_by(
            "call-dismiss",
            icon_button("call-dismiss", Glyph::X, "Dismiss")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.calls.update(cx, Calls::dismiss))),
        )
    }
}

/// `chat`'s name as `list`'s row has it.
pub(crate) fn chat_name_in(list: &Entity<ChatList>, chat: &str, cx: &App) -> SharedString {
    list.read(cx)
        .row(chat)
        .map_or_else(|| SharedString::from("a group chat"), |row| row.name.clone())
}

fn record_bar(trace: &ViewTrace, shown: &BarShown) {
    let mut line = TraceLine::new("in-call", "shown")
        .call(shown.call.get())
        .kind(shown.kind)
        .kind(shown.status);
    for kind in [shown.end, shown.failed].into_iter().flatten() {
        line = line.kind(kind);
    }
    line = line.kind(if shown.muted { "muted" } else { "unmuted" });
    line = line.kind(if shown.camera { "camera-on" } else { "camera-off" });
    if shown.voicemail {
        line = line.kind("voicemail");
    }
    if let Some(chat) = &shown.chat {
        line = line.conversation(chat);
    }
    trace.record(&line.people(shown.people.iter().map(String::as_str)));
}

/// What the user is told of an ask that went nowhere.
pub(crate) fn refusal_text(refused: &Refused) -> String {
    format!("Couldn't {}: {}", ask_label(refused.ask), refused.text)
}

/// What a refused ask's message calls it.
fn ask_label(ask: &str) -> &'static str {
    match ask {
        "call" => "call",
        "test-call" => "start the test call",
        "join" => "join",
        "answer" => "answer",
        "decline" => "decline",
        "hang-up" => "leave the call",
        "camera" => "turn the camera on or off",
        _ => "do that",
    }
}

/// A call's mark: Teams' phone in a tinted circle.
fn call_mark(palette: Palette) -> gpui::Div {
    div()
        .size(px(32.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(palette.background))
        .text_color(rgb(palette.brand))
        .child(Glyph::PHONE.icon().size(px(16.0)))
}

/// A strip across the top of the right side.
fn strip(palette: Palette) -> gpui::Div {
    div()
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .bg(rgb(palette.surface))
        .border_b_1()
        .border_color(rgb(palette.border))
}

/// What a call control shows: an icon alone (named by its tooltip), or an icon and a word.
#[derive(Clone, Copy)]
pub(crate) enum Control {
    Icon(Glyph),
    Labelled(Glyph, &'static str),
}

/// How a call control looks: quiet, marked while its state is on (muted, camera off), or Teams' red or green.
#[derive(Clone, Copy)]
pub(crate) enum Look {
    Toggle(bool),
    Danger,
    Success,
}

/// A call control whose debug selector is its id, its tooltip naming it and the key bound to `action`.
pub(crate) fn control<V: 'static>(
    id: &'static str,
    shows: Control,
    (tip, action): (&'static str, &dyn gpui::Action),
    look: Look,
    cx: &mut Context<V>,
    on: fn(&mut V, &mut Context<V>),
) -> AnyElement {
    let (fill, hover) = match look {
        Look::Toggle(toggled) => {
            let glyph = match shows {
                Control::Icon(glyph) | Control::Labelled(glyph, _) => glyph,
            };
            return selected_by(
                id,
                icon_button_with_key(id, glyph, tip, action)
                    .selected(toggled)
                    .on_click(cx.listener(move |this, _, _, cx| on(this, cx))),
            )
            .into_any_element();
        }
        Look::Danger => (theme::DANGER, theme::DANGER_HOVER),
        Look::Success => (theme::SUCCESS, theme::SUCCESS_HOVER),
    };
    // Teams' red and green are solid: the kit's own coloured buttons are only tinted
    let (glyph, label) = match shows {
        Control::Icon(glyph) => (glyph, None),
        Control::Labelled(glyph, label) => (glyph, Some(label)),
    };
    let action = action.boxed_clone();
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .h(px(32.0))
        .px_3()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(fill))
        .text_color(rgb(0x00ff_ffff))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .hover(move |button| button.bg(rgb(hover)))
        .tooltip(move |window, cx| Tooltip::new(tip).action(action.as_ref(), None).build(window, cx))
        .on_click(cx.listener(move |this, _, _, cx| on(this, cx)))
        .child(glyph.icon().size(px(16.0)))
        .children(label)
        .into_any_element()
}

impl Render for CallBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let model = self.calls.read(cx);
        let (shows, ringing, active) = (model.shows(), model.ringing().cloned(), model.call().cloned());
        let banner = ringing
            .filter(|_| shows)
            .map(|ringing| self.banner(&ringing, palette, cx));
        let bar = match active {
            _ if !shows => None,
            Some(active) => Some(self.in_call(&active, palette, cx)),
            None => self.after(palette, cx),
        };
        div()
            .debug_selector(|| "call-bar".to_owned())
            .w_full()
            .flex()
            .flex_col()
            .children(banner)
            .children(bar)
            .min_h(px(0.0))
    }
}
