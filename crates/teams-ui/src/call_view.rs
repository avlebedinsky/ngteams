//! The calls in the window (M3 Task 17), at the top of the shell's right side ([`CallBar`]):
//!
//! - **The incoming call's banner** while an invitation the engine offered rings: who calls (the caller's name as the
//!   invitation gives it, "Someone" without one; a group call's chat by its row's name), Answer and Decline. The
//!   ringtone and the notification are the desktop's ([`crate::call_alert`]).
//! - **The in-call bar** while a call is up or being set up: who it is with (a chat's members other than the account,
//!   by their names; an invitation's caller; the test call's echo), its state (calling, ringing, connecting,
//!   connected with its duration, reconnecting, taken by voicemail), Mute or Unmute, Camera on or off (a build that
//!   shows video), Hang up, and why an ask went nowhere while it is up; under it, once the call's roster told who is in
//!   it, **the participant grid**: a cell per participant (its name as the roster gives it, its video when a line shows
//!   it, a mark while it speaks) and the account's own (its self view, a mark while it speaks); before that, the video
//!   tiles, the far end's picture and the self view ([`crate::video::Tiles`]). Once it ended, how (or why a later ask
//!   went nowhere) until the next call or Dismiss.
//!
//! A build that shows no calls draws none of it.
//!
//! Every button asks through [`calls::ask`], as the shell's keys and the switcher's commands do. What is shown is
//! traced (`calling registered`/`unregistered` as the calling endpoint goes, `call-banner shown`/`hidden` with the call's number, its kind, whether the caller is named, a group call's
//! chat and the caller's MRI; `in-call shown` with the call's number, its kind, state, end, mute, camera and voicemail,
//! its chat and its participants' MRIs; `in-call hidden`; `call-participant shown` per grid cell when the grid changed,
//! with the call's number, the participant's MRI and kinds for its camera, its line and whether it speaks, and
//! `call-participant self` for the account's own; the tiles' `video-tile` lines), never a name.

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use teams_core::{CallId, CallStatus, CallView, IncomingCall};

use crate::bridge::Core;
use crate::calls::{self, ActiveCall, Ask, CallKind, Calls, Refused};
use crate::chat_list::{self, ChatList};
use crate::notify::SOMEONE;
use crate::people::Shown;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::video::{Tile, Tiles, VideoFeed};

/// How often the duration of a connected call is drawn again.
const TICK: Duration = Duration::from_secs(1);

/// What the in-call bar was last traced showing.
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

/// The banner and the in-call bar (see the module docs). A GPUI entity the shell holds.
pub struct CallBar {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    list: Entity<ChatList>,
    trace: Arc<ViewTrace>,
    banner_traced: Option<CallId>,
    bar_traced: Option<BarShown>,
    /// Whether the calling endpoint was last traced registered.
    registered_traced: bool,
    /// The call's participants other than the account, `(MRI, name)`, once read.
    participants: Option<(CallId, Vec<(String, SharedString)>)>,
    reading: Option<Task<()>>,
    ticking: Option<Task<()>>,
    /// The video tiles, in a build that shows video, with the call they last drew.
    tiles: Option<Tiles>,
    tiles_call: Option<CallId>,
    /// The participant grid as last traced, with its call.
    grid_traced: Option<(CallId, CallView)>,
    _waking: Option<Task<()>>,
    _observing: [Subscription; 2],
}

impl CallBar {
    pub fn new(
        core: Arc<dyn Core>,
        calls: Entity<Calls>,
        list: Entity<ChatList>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = [
            cx.observe(&calls, |this: &mut Self, _, cx| this.changed(cx)),
            // a chat's name read again: the banner and the bar name it as its row does
            cx.observe(&list, |_, _, cx| cx.notify()),
        ];
        let feed = calls.read(cx).video().cloned();
        let waking = feed.as_ref().map(|feed| Self::wake_on_frames(feed, cx));
        let tiles = feed.map(|feed| Tiles::new(feed, Arc::clone(&trace)));
        let mut bar = Self {
            core,
            calls,
            list,
            trace,
            banner_traced: None,
            bar_traced: None,
            registered_traced: false,
            participants: None,
            reading: None,
            ticking: None,
            tiles,
            tiles_call: None,
            grid_traced: None,
            _waking: waking,
            _observing: observing,
        };
        bar.changed(cx);
        bar
    }

    /// A new frame wakes the bar, which draws the newest one.
    fn wake_on_frames(feed: &VideoFeed, cx: &mut Context<Self>) -> Task<()> {
        let woken = feed.woken();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            while woken.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        })
    }

    /// The call's participants other than the account, `(MRI, name)`, once read.
    #[must_use]
    pub fn participants(&self) -> &[(String, SharedString)] {
        self.participants.as_ref().map_or(&[], |(_, people)| people.as_slice())
    }

    /// The calls changed: the participants read for a new call, the duration ticking while connected, and what is
    /// shown traced.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let active = self.calls.read(cx).call().cloned();
        if let Some(active) = &active
            && self.participants.as_ref().is_none_or(|(call, _)| *call != active.id)
        {
            self.read_participants(active, cx);
        }
        let up = active.as_ref().map(|active| active.id);
        if let Some(gone) = self.tiles_call.filter(|call| Some(*call) != up)
            && let Some(tiles) = &mut self.tiles
        {
            // the call ended (or another began): its frames go
            tiles.clear(gone.get());
        }
        self.tiles_call = up;
        let connected = active
            .as_ref()
            .is_some_and(|active| active.status == CallStatus::Connected);
        if connected && self.ticking.is_none() {
            self.ticking = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
                let mut shown = true;
                while shown {
                    cx.background_executor().timer(TICK).await;
                    shown = this.update(cx, |_, cx| cx.notify()).is_ok();
                }
            }));
        } else if !connected {
            self.ticking = None;
        }
        self.trace_now(cx);
        cx.notify();
    }

    /// Reads who `active` is with: an invitation's caller as it named itself; a chat's members other than the
    /// account, by the names the engine has (or their MRIs); nobody for the test call.
    fn read_participants(&mut self, active: &ActiveCall, cx: &mut Context<Self>) {
        let call = active.id;
        self.participants = Some((call, Vec::new()));
        if let Some(caller) = &active.caller {
            let name = active.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
            self.participants = Some((call, vec![(caller.as_str().to_owned(), name.into())]));
            if active.chat.is_none() {
                return;
            }
        }
        let Some(chat) = active.chat.clone() else {
            return;
        };
        let core = Arc::clone(&self.core);
        self.reading = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
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
            let _ = this.update(cx, |this, cx| {
                if this.participants.as_ref().is_some_and(|(read, _)| *read == call) {
                    this.participants = Some((call, people));
                    this.trace_now(cx);
                    cx.notify();
                }
            });
        }));
    }

    /// Traces the banner and the bar when what they show changed.
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
                    self.record_bar(&shown);
                    self.bar_traced = Some(shown);
                }
            }
            (None, Some(ended)) => {
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
                if self.bar_traced.as_ref().is_some_and(|traced| traced.call == ended.id) {
                    if self.bar_traced.as_ref().is_none_or(|traced| traced.end.is_none()) {
                        self.record_bar(&shown);
                        self.bar_traced = Some(shown);
                    }
                } else if self.bar_traced.is_none() {
                    // ended before it was ever shown here (an invitation declined): its end, once
                    self.record_bar(&shown);
                    self.bar_traced = Some(shown);
                }
            }
            (None, None) => {
                if self.bar_traced.take().is_some() {
                    self.trace.record(&TraceLine::new("in-call", "hidden"));
                }
            }
        }
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
            people: self
                .participants
                .as_ref()
                .filter(|(call, _)| *call == active.id)
                .map(|(_, people)| people.iter().map(|(mri, _)| mri.clone()).collect())
                .unwrap_or_default(),
        }
    }

    fn record_bar(&self, shown: &BarShown) {
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
        self.trace.record(&line.people(shown.people.iter().map(String::as_str)));
    }

    /// Asks `ask` of the calls, as a button does.
    pub fn ask(&self, ask: Ask, cx: &mut Context<Self>) {
        calls::ask(&self.calls, &self.core, &self.trace, ask, cx);
    }

    /// A chat's name as its row has it.
    fn chat_name(&self, chat: &str, cx: &Context<Self>) -> SharedString {
        self.list
            .read(cx)
            .row(chat)
            .map_or_else(|| SharedString::from("a group chat"), |row| row.name.clone())
    }

    /// Who the call is with, as its title.
    fn title(&self, active: &ActiveCall, cx: &Context<Self>) -> SharedString {
        match active.kind {
            CallKind::TestCall => "Test call".into(),
            CallKind::Incoming => {
                let caller = active.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
                match &active.chat {
                    Some(chat) => format!("{caller} in {}", self.chat_name(chat, cx)).into(),
                    None => caller.into(),
                }
            }
            CallKind::Chat | CallKind::Join => active
                .chat
                .as_deref()
                .map_or_else(|| SharedString::from("Call"), |chat| self.chat_name(chat, cx)),
        }
    }

    fn banner(&self, ringing: &IncomingCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let caller = ringing.caller_name.clone().unwrap_or_else(|| SOMEONE.to_owned());
        let text: SharedString = match &ringing.group_chat {
            Some(chat) => format!("{caller} is calling in {}", self.chat_name(chat, cx)).into(),
            None => format!("{caller} is calling").into(),
        };
        strip(palette)
            .debug_selector(|| "call-banner".to_owned())
            .child(div().flex_1().font_weight(gpui::FontWeight::BOLD).child(text))
            .child(button("call-answer", "Answer", true, cx, |this, cx| {
                this.ask(Ask::Answer, cx);
            }))
            .child(button("call-decline", "Decline", false, cx, |this, cx| {
                this.ask(Ask::Decline, cx);
            }))
            .into_any_element()
    }

    fn in_call(&mut self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let mut status = calls::status_text(active);
        if let (CallStatus::Connected, Some(at), false) = (&active.status, active.connected_at, active.voicemail) {
            status = format!("{status} · {}", calls::duration_text(at.elapsed().as_secs()));
        }
        let with = self
            .participants
            .as_ref()
            .filter(|(call, people)| *call == active.id && !people.is_empty())
            .map(|(_, people)| {
                let names: Vec<&str> = people.iter().map(|(_, name)| name.as_ref()).collect();
                div()
                    .debug_selector(|| "in-call-participants".to_owned())
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
        let mute = if active.muted { "Unmute" } else { "Mute" };
        let camera = self.tiles.is_some().then(|| {
            let label = if active.camera { "Camera off" } else { "Camera on" };
            button("call-camera", label, false, cx, |this, cx| {
                this.ask(Ask::ToggleCamera, cx);
            })
        });
        let grid = self.grid(active, palette);
        let tiles = if grid.is_none() {
            self.video_tiles(active, palette)
        } else {
            None
        };
        let bar = strip(palette)
            .debug_selector(|| "in-call".to_owned())
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().font_weight(gpui::FontWeight::BOLD).child(self.title(active, cx)))
                    .children(with)
                    .child(
                        div()
                            .debug_selector(|| "in-call-status".to_owned())
                            .text_xs()
                            .child(SharedString::from(status)),
                    )
                    .children(refused),
            )
            .child(button("call-mute", mute, false, cx, |this, cx| {
                this.ask(Ask::ToggleMute, cx);
            }))
            .children(camera)
            .child(button("call-hang-up", "Hang up", true, cx, |this, cx| {
                this.ask(Ask::HangUp, cx);
            }));
        div()
            .w_full()
            .flex()
            .flex_col()
            .child(bar)
            .children(grid)
            .children(tiles)
            .into_any_element()
    }

    /// A 1:1 call's video tiles (the far end's and the account's own), when there are any.
    fn video_tiles(&mut self, active: &ActiveCall, palette: Palette) -> Option<gpui::Div> {
        let tiles = self.tiles.as_mut()?.elements(active.id.get());
        (!tiles.is_empty()).then(|| {
            div()
                .debug_selector(|| "video-tiles".to_owned())
                .w_full()
                .flex()
                .items_end()
                .gap_2()
                .px_3()
                .py_2()
                .bg(rgb(palette.surface))
                .children(tiles)
        })
    }

    /// The participant grid of `active`, once its roster told who is in it: a cell per participant and the account's
    /// own (see the module docs); traced when it changed.
    fn grid(&mut self, active: &ActiveCall, palette: Palette) -> Option<AnyElement> {
        let view = &active.view;
        if view.participants.is_empty() {
            return None;
        }
        self.trace_grid(active.id, view);
        let call = active.id.get();
        let names = self
            .participants
            .as_ref()
            .filter(|(read, _)| *read == active.id)
            .map(|(_, people)| people.clone())
            .unwrap_or_default();
        let mut cells = Vec::with_capacity(view.participants.len() + 1);
        for (index, participant) in view.participants.iter().enumerate() {
            let tile = participant
                .line
                .and_then(|line| self.tiles.as_mut()?.element(Tile::Line(line), call));
            let name = participant
                .name
                .clone()
                .map(SharedString::from)
                .or_else(|| {
                    names
                        .iter()
                        .find(|(mri, _)| *mri == participant.mri)
                        .map(|(_, name)| name.clone())
                })
                .unwrap_or_else(|| SharedString::from(SOMEONE));
            cells.push(cell(
                format!("call-participant-{index}"),
                name,
                participant.speaking,
                tile,
                palette,
            ));
        }
        let own = self.tiles.as_mut().and_then(|tiles| tiles.element(Tile::Own, call));
        cells.push(cell(
            "call-participant-self".to_owned(),
            SharedString::from("You"),
            view.speaking,
            own,
            palette,
        ));
        Some(
            div()
                .debug_selector(|| "call-grid".to_owned())
                .w_full()
                .flex()
                .flex_wrap()
                .items_end()
                .gap_2()
                .px_3()
                .py_2()
                .bg(rgb(palette.surface))
                .children(cells)
                .into_any_element(),
        )
    }

    /// Traces the grid's cells when the grid of `call` changed: each participant's MRI, camera, line and speaking, and
    /// the account's own speaking and whether its microphone hears anything (a closing `self` line).
    fn trace_grid(&mut self, call: CallId, view: &CallView) {
        if self
            .grid_traced
            .as_ref()
            .is_some_and(|(traced, shown)| *traced == call && shown == view)
        {
            return;
        }
        for participant in &view.participants {
            let line = TraceLine::new("call-participant", "shown")
                .call(call.get())
                .people([participant.mri.as_str()])
                .kind(if participant.video_on {
                    "camera-on"
                } else {
                    "camera-off"
                })
                .kind(participant.line.map_or("no-line", |line| Tile::Line(line).name()))
                .kind(if participant.speaking { "speaking" } else { "quiet" });
            self.trace.record(&line);
        }
        self.trace.record(
            &TraceLine::new("call-participant", "self")
                .call(call.get())
                .kind(if view.speaking { "speaking" } else { "quiet" })
                .kind(if view.hearing { "mic-sound" } else { "mic-silent" }),
        );
        self.grid_traced = Some((call, view.clone()));
    }

    /// How the last call ended, or why the last ask went nowhere, with Dismiss.
    fn after(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = self.calls.read(cx);
        let refused = model.refused().map(refusal_text);
        let ended = model.ended().map(|ended| calls::end_text(&ended.end, ended.voicemail));
        let text = refused.or(ended)?;
        Some(
            strip(palette)
                .debug_selector(|| "call-after".to_owned())
                .child(div().flex_1().child(SharedString::from(text)))
                .child(button("call-dismiss", "Dismiss", false, cx, |this, cx| {
                    this.calls.update(cx, Calls::dismiss);
                }))
                .into_any_element(),
        )
    }
}

/// What the user is told of an ask that went nowhere.
fn refusal_text(refused: &Refused) -> String {
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
        "hang-up" => "hang up",
        "camera" => "turn the camera on or off",
        _ => "do that",
    }
}

/// One cell of the participant grid: its tile (when its video shows), its name, and a border in the brand colour while
/// it speaks.
fn cell(
    selector: String,
    name: SharedString,
    speaking: bool,
    tile: Option<AnyElement>,
    palette: Palette,
) -> AnyElement {
    let border = if speaking { palette.brand } else { palette.border };
    let marked = if speaking {
        SharedString::from(format!("{name} · speaking"))
    } else {
        name
    };
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap_1()
        .p_1()
        .rounded_md()
        .border_2()
        .border_color(rgb(border))
        .children(tile)
        .child(div().text_xs().child(marked))
        .into_any_element()
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

/// A button whose debug selector is its id.
fn button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    cx: &mut Context<CallBar>,
    on: fn(&mut CallBar, &mut Context<CallBar>),
) -> AnyElement {
    let button = Button::new(id)
        .label(label)
        .compact()
        .on_click(cx.listener(move |this, _, _, cx| on(this, cx)));
    let button = if primary { button.primary() } else { button.ghost() };
    div()
        .debug_selector(move || id.to_owned())
        .child(button)
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
