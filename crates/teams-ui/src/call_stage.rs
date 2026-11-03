//! What a call's own window shows ([`CallStage`]), as Teams' call window does: the main window's chats stay usable
//! beside it ([`crate::call_window`] opens and closes it).
//!
//! - **The header**: who the call is with (a chat's name, an invitation's caller, "Echo" for the test call) and its
//!   state (calling, ringing, connecting, connected with its duration, reconnecting, taken by voicemail); on its right
//!   Teams' call controls: People (the participants' pane), More (Video effects, in a build that shows video; Device
//!   settings: the main window's settings on their Calls page), the camera (a build that shows video) and the microphone, each with a menu of the devices the
//!   settings list (the camera's; the microphone's and the speaker's) when they are listed, and Leave in Teams' red.
//!   Each control's tooltip names it and its key (Ctrl+Shift+O, Ctrl+Shift+M, Ctrl+Shift+H, the main window's keys).
//! - **The stage**: before anyone is in the call, the callee's avatar with the call's state, as Teams shows it while it
//!   rings; once the call's roster told who is in it, a tile per participant in a grid (its video when a line shows
//!   it, else its initials; its name; a border in the brand colour while it speaks); before the roster, a tile per
//!   person the call is with (a 1:1 call's far end's video on its tile). The account's own tile (its self view, a
//!   border while it speaks) floats at the stage's bottom right, as Teams' self view does.
//! - **The participants' pane** (People): who is in the call, the account first, and who speaks.
//! - **The video effects' pane** (More's or the camera menu's Video effects; one pane at a time, as Teams has it):
//!   the self-preview over the background effects ([`crate::effects::EffectsPicker`]): what the call sends while the
//!   camera is on, else a preview only the user's Preview turns on. A background chosen applies to the call's camera
//!   at once and is kept for the next calls.
//! - **How a call ended**, when the user didn't end it (the far end hung up, it dropped, never connected, went to
//!   voicemail): who it was with (their avatar and name), why it ended in plain words ([`calls::Ended::reason`]: "Lab
//!   Bee ended the call", "No one else joined the call", never a code the service sent), how long it lasted, and
//!   Call back (a 1:1 or a group chat's call, Ctrl+Shift+C) or Rejoin (a meeting's call, or a group call that runs on
//!   without the account, Ctrl+Shift+J), each a new start or join the user asks for, and Close. Left alone it closes by itself
//!   ([`AUTO_CLOSE`]) unless Call back or Rejoin was asked.
//! - **A test call's results** once it ended, as Teams shows them in its call window: the microphone, the speaker and
//!   the camera (each naming the device the call used) and the network, each checked by what the call showed
//!   ([`calls::DeviceCheck`]), with "Device settings" and Close.
//!
//! Every control asks through [`calls::ask`], as the main window's do; a device chosen goes to the settings, as the
//! settings' own pickers do it. What it shows is traced (`call-participant shown` per grid cell when the grid changed,
//! with the call's number, the participant's MRI and kinds for its camera, its line and whether it speaks, and
//! `call-participant self` for the account's own; the tiles' `video-tile` lines; `call-window ended` with the call's
//! number, why it ended and what it offers: `call-back`, `rejoin` or `none`), never a name.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::Size;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_core::{CallId, CallParticipant, CallStatus, CallView};
use teams_media::devices::catalog::DeviceKind;

use crate::bridge::Core;
use crate::call_view::{self, CallPeople, Control, Look, control};
use crate::calls::{self, ActiveCall, Ask, CallKind, Calls, DeviceCheck, EndReason, Ended, Verdict};
use crate::effects::{EffectsPicker, PickerPlace};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::notify::SOMEONE;
use crate::people;
use crate::settings::SettingsModel;
use crate::settings_view::{self, DeviceChoice, device_label};
use crate::shell::{CallChat, HangUpCall, JoinCall, ToggleCameraCall, ToggleMuteCall};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::video::{Tile, Tiles, VideoFeed};

/// The call window's key context.
pub const CONTEXT: &str = "CallStage";

/// The header's height, px.
const HEADER_HEIGHT: f32 = 56.0;
/// The self view's size, px.
const SELF_VIEW: (f32, f32) = (224.0, 126.0);
/// The participants' pane's width, px.
const ROSTER_WIDTH: f32 = 280.0;
/// The avatar of the one the call rings (or ended with) and of a tile without video: the kit's large one (80 px),
/// whose initials it sizes and centres (a custom size leaves them small, at its top left).
const BIG_AVATAR: Size = Size::Large;
/// How long a call's end stays shown when the user does nothing.
pub const AUTO_CLOSE: Duration = Duration::from_secs(8);

/// What names a group chat's call in its window: the main window's chat list's row for it, when the list is there.
pub type ChatNamer = Rc<dyn Fn(&str, &App) -> Option<SharedString>>;

/// What the call window asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStageEvent {
    /// More's (or a test call's results') "Device settings": the main window's settings on their Calls page.
    DeviceSettings,
    /// Close on a call's end or a test call's results, or the end left alone for [`AUTO_CLOSE`].
    Close,
}

/// What a call's end offers: a new start to its chat, or a join of its group call that runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FollowUp {
    ask: Ask,
    rejoin: bool,
}

/// What the stage is made of: the account's engine (its request side), its calls, the app's settings, the view
/// trace, and what names a group chat.
pub struct StageParts {
    pub core: Arc<dyn Core>,
    pub calls: Entity<Calls>,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
    pub namer: Option<ChatNamer>,
    /// The app's other accounts' calls: Call back and Rejoin wait while one of them is busy (one call at a time across
    /// the accounts, as the shell's call buttons do).
    pub others: Vec<Entity<Calls>>,
}

/// One tile of the stage: its test selector, its name, whether it speaks, and the video line showing it.
struct StageTile {
    selector: String,
    name: SharedString,
    speaking: bool,
    video: Option<Tile>,
}

/// What the call window shows (see the module docs). A GPUI entity, the window's root view.
pub struct CallStage {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    namer: Option<ChatNamer>,
    /// The other accounts' calls ([`StageParts::others`]).
    others: Vec<Entity<Calls>>,
    /// The call the window is for.
    call: CallId,
    people: CallPeople,
    /// The video tiles, in a build that shows video.
    tiles: Option<Tiles>,
    /// The participants' pane is open.
    roster: bool,
    /// The video effects' pane, while it is open.
    effects: Option<Entity<EffectsPicker>>,
    /// The devices a test call used, named as it ended.
    results: Option<[String; 3]>,
    /// The grid as last traced.
    grid_traced: Option<CallView>,
    /// The window's title as last set.
    titled: Option<SharedString>,
    /// Who the call was with, as last said while it was up: its end still names them.
    with: Option<SharedString>,
    /// The call's end was shown (and traced).
    end_shown: bool,
    /// The end closing by itself, while it is due.
    closing: Option<Task<()>>,
    focus: FocusHandle,
    ticking: Option<Task<()>>,
    _waking: Option<Task<()>>,
    _observing: [Subscription; 3],
}

impl EventEmitter<CallStageEvent> for CallStage {}

impl CallStage {
    /// The window of call `call` over `parts`.
    pub fn new(parts: StageParts, call: CallId, cx: &mut Context<Self>) -> Self {
        let StageParts {
            core,
            calls,
            settings,
            trace,
            namer,
            others,
        } = parts;
        let observing = [
            cx.observe(&calls, |this: &mut Self, _, cx| this.changed(cx)),
            // the devices listed or chosen: their menus follow
            cx.observe(&settings, |_, _, cx| cx.notify()),
            // the window gone (its call ended, or closed): the call's frames go with it
            cx.on_release(|this: &mut Self, _| {
                if let Some(tiles) = &mut this.tiles {
                    tiles.clear(this.call.get());
                }
            }),
        ];
        let feed = calls.read(cx).video().cloned();
        let waking = feed.as_ref().map(|feed| wake_on_frames(feed, cx));
        let tiles = feed.map(|feed| Tiles::new(feed, Arc::clone(&trace)));
        let mut stage = Self {
            core,
            calls,
            settings,
            trace,
            namer,
            others,
            call,
            people: CallPeople::default(),
            tiles,
            roster: false,
            effects: None,
            results: None,
            grid_traced: None,
            titled: None,
            with: None,
            end_shown: false,
            closing: None,
            focus: cx.focus_handle(),
            ticking: None,
            _waking: waking,
            _observing: observing,
        };
        stage.changed(cx);
        stage
    }

    /// Opens or closes the participants' pane, as People does.
    pub fn show_roster(&mut self, open: bool, cx: &mut Context<Self>) {
        self.roster = open;
        if open {
            self.effects = None;
        }
        cx.notify();
    }

    /// Opens or closes the video effects' pane, as More's Video effects does (a build that shows video).
    pub fn show_effects(&mut self, open: bool, cx: &mut Context<Self>) {
        if !open {
            self.effects = None;
        } else if self.effects.is_none()
            && let Some(feed) = self.calls.read(cx).video().cloned()
        {
            let (settings, trace, call) = (self.settings.clone(), Arc::clone(&self.trace), self.call.get());
            self.effects =
                Some(cx.new(|cx| EffectsPicker::new(settings, trace, PickerPlace::CallPane, cx).in_call(feed, call)));
            self.roster = false;
        }
        cx.notify();
    }

    /// Whether the video effects' pane is open.
    #[must_use]
    pub fn effects_shown(&self) -> bool {
        self.effects.is_some()
    }

    /// What takes the window's keyboard, so the call keys reach the window's controls.
    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The call while it is up, if it is this window's.
    fn live(&self, cx: &App) -> Option<ActiveCall> {
        self.calls
            .read(cx)
            .call()
            .filter(|active| active.id == self.call)
            .cloned()
    }

    /// How this window's call ended, once it did.
    fn ended(&self, cx: &App) -> Option<Ended> {
        self.calls
            .read(cx)
            .ended()
            .filter(|ended| ended.id == self.call)
            .cloned()
    }

    /// The calls changed: the participants read, the duration ticking while connected; once the call ended its frames
    /// go and a test call's devices are named as they were.
    fn changed(&mut self, cx: &mut Context<Self>) {
        let live = self.live(cx);
        if let Some(active) = &live {
            let (core, echo) = (Arc::clone(&self.core), self.calls.read(cx).echo().cloned());
            self.people.follow(
                active,
                (&core, echo.as_ref()),
                cx,
                (|stage| &mut stage.people, |_, cx| cx.notify()),
            );
        } else {
            if let Some(tiles) = &mut self.tiles {
                tiles.clear(self.call.get());
            }
            if self.results.is_none() && self.ended(cx).is_some_and(|ended| ended.check.is_some()) {
                self.results = Some(self.devices_in_use(cx));
            }
            self.end_seen(cx);
        }
        if let Some(ask) = self.calls.update(cx, Calls::next_choice) {
            self.ask(ask, cx);
        }
        let connected = live
            .as_ref()
            .is_some_and(|active| active.status == CallStatus::Connected);
        if connected && self.ticking.is_none() {
            self.ticking = Some(call_view::ticking(cx));
        } else if !connected {
            self.ticking = None;
        }
        cx.notify();
    }

    /// The call's end, shown for the first time (one the user didn't choose, not a test call's): traced, and due to
    /// close by itself.
    fn end_seen(&mut self, cx: &mut Context<Self>) {
        if self.end_shown {
            return;
        }
        let Some(ended) = self.end_card(cx) else { return };
        self.end_shown = true;
        let offers = match self.follow_up(&ended, cx) {
            Some(FollowUp { rejoin: true, .. }) => "rejoin",
            Some(_) => "call-back",
            None => "none",
        };
        self.trace.record(
            &TraceLine::new("call-window", "ended")
                .call(self.call.get())
                .kind(ended.reason(self.core.me()).name())
                .kind(offers),
        );
        self.close_later(cx);
    }

    /// The end closes by itself after [`AUTO_CLOSE`].
    fn close_later(&mut self, cx: &mut Context<Self>) {
        self.closing = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(AUTO_CLOSE).await;
            // a stage that is gone has nothing left to close
            let _ = this.update(cx, |this, cx| {
                this.closing = None;
                cx.emit(CallStageEvent::Close);
            });
        }));
    }

    /// How this window's call ended, when the window shows it as its end: one the user didn't choose, not a test
    /// call's (whose results it shows).
    fn end_card(&self, cx: &App) -> Option<Ended> {
        self.ended(cx)
            .filter(|ended| ended.check.is_none() && ended.stays_in_window())
    }

    /// What the end offers: Rejoin for a meeting's call, whatever ended it (a meeting never offers Call back), and for
    /// a group call that runs on without the account (it dropped, never got in, or the chat's call was running already)
    /// and that an invitation named; else Call back to its chat, or for a 1:1 invitation to the caller's chat; nothing
    /// in the read-only mode or for a test call.
    fn follow_up(&self, ended: &Ended, cx: &App) -> Option<FollowUp> {
        if self.core.read_only() || ended.kind == CallKind::TestCall {
            return None;
        }
        let reason = ended.reason(self.core.me());
        if reason == EndReason::Own {
            return None;
        }
        if ended.kind == CallKind::Meeting {
            // a meeting can be gone into at any time, whatever became of its call
            let ask = self.calls.read(cx).rejoin_of(ended)?;
            return Some(FollowUp { ask, rejoin: true });
        }
        if let Some(chat) = ended.group_chat() {
            let runs_on = matches!(
                reason,
                EndReason::Dropped | EndReason::NotConnected | EndReason::ChatHasCall
            );
            if runs_on && self.calls.read(cx).joinable(chat) {
                let ask = Ask::Join { chat: chat.to_owned() };
                return Some(FollowUp { ask, rejoin: true });
            }
            if reason == EndReason::ChatHasCall {
                return None;
            }
        }
        let chat = ended.call_back_chat(self.core.me())?;
        Some(FollowUp {
            ask: Ask::Call { chat },
            rejoin: false,
        })
    }

    /// Call back or Rejoin, as the end offers it (its button or its key); nothing while the call is up or another
    /// rings or sets up.
    fn act_on_end(&mut self, rejoin: bool, cx: &mut Context<Self>) {
        if self.calls.read(cx).busy() || self.others.iter().any(|calls| calls.read(cx).busy()) {
            return;
        }
        let Some(ended) = self.end_card(cx) else { return };
        let Some(follow_up) = self
            .follow_up(&ended, cx)
            .filter(|follow_up| follow_up.rejoin == rejoin)
        else {
            return;
        };
        // the user acted: the end no longer closes by itself
        self.closing = None;
        self.ask(follow_up.ask, cx);
    }

    /// The names of the devices the calls use now, in [`RESULT_DEVICES`]' order.
    fn devices_in_use(&self, cx: &App) -> [String; 3] {
        let model = self.settings.read(cx);
        RESULT_DEVICES.map(|kind| settings_view::device_in_use(kind, model).unwrap_or_else(|| SYSTEM_DEVICE.to_owned()))
    }

    /// Asks `ask` of the calls, as a button does.
    fn ask(&self, ask: Ask, cx: &mut Context<Self>) {
        calls::ask(&self.calls, &self.core, &self.trace, ask, cx);
    }

    /// Asks `ask` of the calls from a part of the stage drawn elsewhere (the lobby's card).
    pub(crate) fn ask_call(&self, ask: Ask, cx: &mut Context<Self>) {
        self.ask(ask, cx);
    }

    /// Who the call is with, as the window's title says it.
    fn title(&self, active: &ActiveCall, cx: &App) -> SharedString {
        let names = call_view::names_in_call(active, self.people.of(active.id));
        let joined = || {
            let names: Vec<&str> = names.iter().map(SharedString::as_ref).collect();
            if names.is_empty() {
                SharedString::from("Group call")
            } else {
                SharedString::from(names.join(", "))
            }
        };
        call_view::call_title(active, |chat| {
            self.namer
                .as_ref()
                .and_then(|namer| namer(chat, cx))
                .unwrap_or_else(joined)
        })
    }

    /// Sets the window's title to `title`, when it changed.
    fn retitle(&mut self, title: &SharedString, window: &mut Window) {
        if self.titled.as_ref() != Some(title) {
            window.set_window_title(title.as_ref());
            self.titled = Some(title.clone());
        }
    }

    /// The header: who the call is with and its state, and the controls while it is up.
    fn header(
        &self,
        live: Option<&ActiveCall>,
        title: SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let state: SharedString = match (live, self.ended(cx)) {
            (Some(active), _) => call_view::status_with_duration(active).into(),
            (None, Some(ended)) if ended.check.is_none() => {
                let heading = ended.reason(self.core.me()).heading();
                match ended.duration {
                    Some(lasted) => format!("{heading} · {}", calls::duration_text(lasted.as_secs())).into(),
                    None => heading.into(),
                }
            }
            (None, Some(ended)) => calls::end_text(&ended.end, ended.voicemail).into(),
            (None, None) => "Call ended".into(),
        };
        div()
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .bg(rgb(palette.surface))
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().truncate().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
                    .child(
                        div()
                            .debug_selector(|| "call-window-status".to_owned())
                            .truncate()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(state),
                    ),
            )
            .children(live.map(|active| self.toolbar(active, palette, cx)))
    }

    /// Teams' call controls: People, More, the camera and the microphone with their devices, and Leave.
    fn toolbar(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let people_tip = if self.roster {
            "Hide participants"
        } else {
            "Show participants"
        };
        let roster = tool(
            ("call-roster", Glyph::USERS, "People"),
            (people_tip, None),
            self.roster,
            palette,
            cx,
            |this, cx| {
                let open = !this.roster;
                this.show_roster(open, cx);
            },
        );
        let camera = self
            .calls
            .read(cx)
            .video()
            .is_some()
            .then(|| self.camera(active, palette, cx));
        let (mic, mute) = if active.muted {
            (Glyph::MIC_OFF, "Unmute")
        } else {
            (Glyph::MIC, "Mute")
        };
        let microphone = tool(
            ("call-mute", mic, "Mic"),
            (mute, Some(&ToggleMuteCall)),
            active.muted,
            palette,
            cx,
            |this, cx| this.ask(Ask::ToggleMute, cx),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(roster)
            .child(self.more(palette, cx))
            .child(div().w(px(1.0)).h(px(32.0)).mx_1().bg(rgb(palette.border)))
            .children(camera)
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(microphone)
                    .children(self.devices_menu("call-audio-devices", AUDIO, "Choose microphone and speaker", cx)),
            )
            .child(div().w_2())
            .child(control(
                "call-hang-up",
                Control::Labelled(Glyph::PHONE_OFF, "Leave"),
                ("Leave call", &HangUpCall),
                Look::Danger,
                cx,
                |this, cx| this.ask(Ask::HangUp, cx),
            ))
    }

    /// The camera's control, with its devices.
    fn camera(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let (glyph, tip) = if active.camera {
            (Glyph::VIDEO, "Turn camera off")
        } else {
            (Glyph::VIDEO_OFF, "Turn camera on")
        };
        div()
            .flex()
            .items_center()
            .child(tool(
                ("call-camera", glyph, "Camera"),
                (tip, Some(&ToggleCameraCall)),
                !active.camera,
                palette,
                cx,
                |this, cx| this.ask(Ask::ToggleCamera, cx),
            ))
            .children(self.devices_menu("call-camera-devices", CAMERAS, "Choose camera and video effects", cx))
    }

    /// More: Video effects (a build that shows video) and Device settings.
    fn more(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let stage = cx.weak_entity();
        let video = self.calls.read(cx).video().is_some();
        let face = tool_face(Glyph::MORE, "More", palette);
        selected_by(
            "call-more",
            Button::new("call-more")
                .ghost()
                .h(px(48.0))
                .child(face)
                .tooltip("More actions")
                .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                    let menu = if video {
                        menu.item(effects_item("call-more-video-effects", stage.clone()))
                    } else {
                        menu
                    };
                    let stage = stage.clone();
                    menu.min_w(px(200.0)).item(
                        PopupMenuItem::element(|_, _| {
                            div()
                                .debug_selector(|| "call-more-device-settings".to_owned())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(Glyph::SETTINGS.icon().size(px(14.0)))
                                .child("Device settings")
                        })
                        .on_click(move |_, _, cx| emit(&stage, CallStageEvent::DeviceSettings, cx)),
                    )
                }),
        )
        .into_any_element()
    }

    /// The menu of the devices of `kinds` (each under its name), chosen as the settings choose them; none while the
    /// devices aren't listed.
    fn devices_menu(
        &self,
        id: &'static str,
        kinds: &'static [DeviceKind],
        tip: &'static str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        self.settings.read(cx).devices()?;
        let model = self.settings.clone();
        let stage = cx.weak_entity();
        let effects = kinds == CAMERAS;
        Some(
            selected_by(
                id,
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(Glyph::CHEVRON_DOWN.icon())
                    .tooltip(tip)
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, cx| {
                        let menu = device_items(menu, kinds, &model, cx);
                        if effects {
                            menu.separator()
                                .item(effects_item("call-camera-video-effects", stage.clone()))
                        } else {
                            menu
                        }
                    }),
            )
            .into_any_element(),
        )
    }
}

impl CallStage {
    /// The stage of the call up: who rings while nobody is in it, else the grid, with the self view over it.
    fn stage(
        &mut self,
        active: &ActiveCall,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // waiting in the lobby, the account sees nobody: only that someone should let it in
        let tiles = if active.status == CallStatus::Lobby {
            Vec::new()
        } else {
            self.tiles_of(active)
        };
        let call = active.id.get();
        let lobby = crate::call_lobby::card(active, palette, cx);
        let body = if tiles.is_empty() {
            let waiting = active.kind == CallKind::Meeting && active.status == CallStatus::Connected;
            let state = if waiting {
                "Waiting for others to join".to_owned()
            } else {
                calls::status_text(active)
            };
            calling(title, &state, palette)
        } else {
            let selector = if active.view.participants.is_empty() {
                "call-people"
            } else {
                "call-grid"
            };
            let drawn: Vec<AnyElement> = tiles
                .into_iter()
                .map(|tile| {
                    let video = tile.video.and_then(|line| self.tiles.as_mut()?.fill(line, call));
                    tile_view(tile, video, palette)
                })
                .collect();
            grid(selector, drawn)
        };
        let own = self.tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Own, call));
        div()
            .debug_selector(|| "call-stage".to_owned())
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .p_3()
            .bg(rgb(palette.background))
            .child(body)
            .child(self_view(active.view.speaking, own, palette))
            .children(lobby)
            .into_any_element()
    }

    /// The stage's tiles: the roster's participants once it told who is in the call (the grid, traced when it
    /// changed); before that, once the media connected, the people the call is with (a 1:1 call's far end on line
    /// 0); none while it rings.
    fn tiles_of(&mut self, active: &ActiveCall) -> Vec<StageTile> {
        let view = &active.view;
        if !view.participants.is_empty() {
            let people = self.people.of(active.id);
            let tiles = view
                .participants
                .iter()
                .enumerate()
                .map(|(index, participant)| StageTile {
                    selector: format!("call-participant-{index}"),
                    name: participant_name(participant, people),
                    speaking: participant.speaking,
                    video: participant.line.map(Tile::Line),
                })
                .collect();
            self.trace_grid(active.id, view);
            return tiles;
        }
        if !matches!(active.status, CallStatus::Connected | CallStatus::Reconnecting) {
            return Vec::new();
        }
        let people = self.people.of(active.id);
        let one = people.len() == 1;
        people
            .iter()
            .enumerate()
            .map(|(index, (_, name))| StageTile {
                selector: format!("call-person-{index}"),
                name: name.clone(),
                speaking: false,
                video: one.then_some(Tile::REMOTE),
            })
            .collect()
    }

    /// Traces the grid's cells when the grid of `call` changed: each participant's MRI, camera, line and speaking, and
    /// the account's own speaking and whether its microphone hears anything (a closing `self` line).
    fn trace_grid(&mut self, call: CallId, view: &CallView) {
        if self.grid_traced.as_ref() == Some(view) {
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
        self.grid_traced = Some(view.clone());
    }

    /// The participants' pane: who is in the call, the account first, and who speaks.
    fn roster_pane(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let people = self.people.of(active.id);
        let mut rows = vec![self_row(active.view.speaking, palette)];
        if active.view.participants.is_empty() {
            rows.extend(
                people
                    .iter()
                    .enumerate()
                    .map(|(index, (_, name))| roster_row(format!("call-roster-{index}"), name.clone(), false, palette)),
            );
        } else {
            rows.extend(active.view.participants.iter().enumerate().map(|(index, participant)| {
                let name = participant_name(participant, people);
                roster_row(format!("call-roster-{index}"), name, participant.speaking, palette)
            }));
        }
        let count = rows.len();
        let close = selected_by(
            "call-roster-close",
            icon_button("call-roster-close", Glyph::X, "Close participants")
                .small()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.roster = false;
                    cx.notify();
                })),
        );
        div()
            .debug_selector(|| "call-roster-pane".to_owned())
            .w(px(ROSTER_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.surface))
            .border_l_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .h(px(48.0))
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Participants"),
                    )
                    .child(close),
            )
            .child(
                div()
                    .px_4()
                    .py_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.muted_foreground))
                    .child(SharedString::from(format!("In this call ({count})"))),
            )
            .children(rows)
            .into_any_element()
    }

    /// The video effects' pane while it is open: its title and Close over the picker, which learns whether the call's
    /// camera is on.
    fn effects_pane(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.effects.clone()?;
        picker.update(cx, |picker, cx| picker.set_camera_on(active.camera, cx));
        let close = selected_by(
            "call-effects-close",
            icon_button("call-effects-close", Glyph::X, "Close video effects")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_effects(false, cx))),
        );
        Some(
            div()
                .debug_selector(|| "call-effects-pane".to_owned())
                .w(px(ROSTER_WIDTH))
                .flex_none()
                .h_full()
                .flex()
                .flex_col()
                .bg(rgb(palette.surface))
                .border_l_1()
                .border_color(rgb(palette.border))
                .child(
                    div()
                        .h(px(48.0))
                        .px_4()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Video effects"),
                        )
                        .child(close),
                )
                .child(
                    div()
                        .id("call-effects-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_3()
                        .child(picker),
                )
                .into_any_element(),
        )
    }

    /// A test call's results, as Teams shows them in its call window once it ended: a row per check (the microphone,
    /// the speaker and the camera, each naming the device the call used, and the network), "Device settings" and
    /// Close.
    fn results_card(&self, ended: &Ended, check: DeviceCheck, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        // the devices as they were when the call ended: a device chosen since wasn't checked
        let named = self.results.clone().unwrap_or_else(|| self.devices_in_use(cx));
        let verdicts = [check.microphone(), check.speaker(), check.camera()];
        let mut checks: Vec<AnyElement> = [Glyph::MIC, Glyph::VOLUME, Glyph::VIDEO]
            .into_iter()
            .zip(RESULT_DEVICES.into_iter().zip(named))
            .zip(verdicts)
            .map(|((glyph, (kind, device)), verdict)| check_row((glyph, device_label(kind), device), verdict, palette))
            .collect();
        checks.push(check_row(
            (Glyph::WIFI, "Network", "Your connection".to_owned()),
            check.network(),
            palette,
        ));
        let passed = check.passed();
        let summary = if passed {
            "Your devices are working.".to_owned()
        } else {
            calls::end_text(&ended.end, ended.voicemail)
        };
        let verdict = if passed { Verdict::Fine } else { Verdict::Problem };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(rgb(palette.background))
            .child(
                div()
                    .w(px(RESULTS_WIDTH))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .child(results_header(verdict, summary, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(palette.border))
                            .children(checks),
                    )
                    .child(Self::results_buttons(cx)),
            )
            .into_any_element()
    }

    /// The results' "Device settings" and Close.
    fn results_buttons(cx: &mut Context<Self>) -> gpui::Div {
        let device_settings = selected_by(
            "test-call-device-settings",
            Button::new("test-call-device-settings")
                .label("Device settings")
                .outline()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::DeviceSettings))),
        );
        let close = selected_by(
            "test-call-close",
            Button::new("test-call-close")
                .label("Close")
                .primary()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::Close))),
        );
        div().flex().justify_end().gap_2().child(device_settings).child(close)
    }

    /// Why the last ask went nowhere while the call is up.
    fn refused(&self, palette: Palette, cx: &App) -> Option<gpui::Div> {
        let refused = self.calls.read(cx).refused()?;
        Some(
            div()
                .debug_selector(|| "call-window-refused".to_owned())
                .flex_none()
                .px_4()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .bg(rgb(palette.surface))
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)).text_color(rgb(theme::DANGER)))
                .child(SharedString::from(call_view::refusal_text(refused))),
        )
    }

    /// What the window shows under its header: the stage (and the participants' pane), a test call's results, or that
    /// the call ended.
    fn body(
        &mut self,
        (live, ended): (Option<&ActiveCall>, Option<&Ended>),
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(active) = live {
            let stage = self.stage(active, title, palette, cx);
            let roster = self.roster.then(|| self.roster_pane(active, palette, cx));
            let effects = self.effects_pane(active, palette, cx);
            return div()
                .size_full()
                .flex()
                .child(stage)
                .children(roster)
                .children(effects)
                .into_any_element();
        }
        match (ended, ended.and_then(|ended| ended.check)) {
            (Some(ended), Some(check)) => self.results_card(ended, check, palette, cx),
            (Some(ended), None) => self.end_view(ended, title, palette, cx),
            (None, _) => calling(title, "Call ended", palette),
        }
    }

    /// How the call ended, as the window shows it once it did: who it was with, why it ended, how long it lasted,
    /// Call back or Rejoin, and Close.
    fn end_view(&self, ended: &Ended, title: &SharedString, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let group = ended.group_chat().is_some();
        let reason = match ended.reason(self.core.me()) {
            // a 1:1 call's far end goes by the name the app shows them by (the window's title), not the one its
            // client sent with the end
            EndReason::EndedBy(_) if !group => EndReason::EndedBy(None),
            reason => reason,
        };
        // a group call's end that names nobody isn't the chat's doing
        let other = if group { SOMEONE } else { title.as_ref() };
        let why = reason.text(other).map(|why| {
            div()
                .debug_selector(|| "call-end-reason".to_owned())
                .text_color(rgb(palette.foreground))
                .child(SharedString::from(why))
        });
        let lasted = ended.duration.map(|lasted| {
            div()
                .debug_selector(|| "call-end-duration".to_owned())
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(format!(
                    "Duration {}",
                    calls::duration_text(lasted.as_secs())
                )))
        });
        let follow_up = self
            .follow_up(ended, cx)
            .filter(|_| !self.calls.read(cx).busy())
            .map(|follow_up| follow_up_button(follow_up.rejoin, cx));
        let close = selected_by(
            "call-end-close",
            Button::new("call-end-close")
                .label("Close")
                .outline()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::Close))),
        );
        div()
            .debug_selector(|| "call-end".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .bg(rgb(palette.background))
            .child(ended_avatar(title, palette))
            .child(
                div()
                    .mt_2()
                    .max_w_full()
                    .truncate()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "call-end-heading".to_owned())
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(reason.heading()),
            )
            .children(why)
            .children(lasted)
            .child(
                div()
                    .mt_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(follow_up)
                    .child(close),
            )
            .into_any_element()
    }

    /// Who the call was with, as its end names them: as last said while it was up, else as the end knows them.
    fn ended_title(&self, ended: &Ended, cx: &App) -> SharedString {
        if let Some(with) = &self.with {
            return with.clone();
        }
        let chat_name = |chat: &str| self.namer.as_ref().and_then(|namer| namer(chat, cx));
        match (ended.kind, &ended.chat) {
            (CallKind::Incoming, None) => ended.caller_name.clone().map(SharedString::from),
            (_, Some(chat)) => chat_name(chat),
            _ => None,
        }
        .or_else(|| {
            let names: Vec<&str> = self.people.last().iter().map(|(_, name)| name.as_ref()).collect();
            (!names.is_empty()).then(|| SharedString::from(names.join(", ")))
        })
        .unwrap_or_else(|| SharedString::from("Call"))
    }
}

impl Render for CallStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let live = self.live(cx);
        let ended = self.ended(cx);
        let title = match (&live, &ended) {
            (Some(active), _) => {
                let title = self.title(active, cx);
                self.with = Some(title.clone());
                title
            }
            (None, Some(ended)) if ended.check.is_some() => SharedString::from("Test call"),
            (None, Some(ended)) => self.ended_title(ended, cx),
            (None, None) => SharedString::from("Call"),
        };
        self.retitle(&title, window);
        let body = self.body((live.as_ref(), ended.as_ref()), &title, palette, cx);
        let header = self.header(live.as_ref(), title, palette, cx);
        // a refusal while the call is up, or of a call back from its end
        let refused = (live.is_some() || ended.as_ref().is_some_and(|ended| ended.check.is_none()))
            .then(|| self.refused(palette, cx))
            .flatten();
        div()
            .debug_selector(|| "call-window".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleMuteCall, _, cx| this.ask(Ask::ToggleMute, cx)))
            .on_action(cx.listener(|this, _: &ToggleCameraCall, _, cx| this.ask(Ask::ToggleCamera, cx)))
            .on_action(cx.listener(|this, _: &HangUpCall, _, cx| this.ask(Ask::HangUp, cx)))
            .on_action(cx.listener(|this, _: &CallChat, _, cx| this.act_on_end(false, cx)))
            .on_action(cx.listener(|this, _: &JoinCall, _, cx| this.act_on_end(true, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(header)
            .children(refused)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// What a test call's results name a device by while the devices aren't listed (or the system has no default).
const SYSTEM_DEVICE: &str = "The system's default";

/// The devices a test call's results name, in their rows' order.
const RESULT_DEVICES: [DeviceKind; 3] = [DeviceKind::Microphone, DeviceKind::Speaker, DeviceKind::Camera];

/// The results' card's width, px.
const RESULTS_WIDTH: f32 = 480.0;

/// The microphone's menu: the microphones, then the speakers.
const AUDIO: &[DeviceKind] = &[DeviceKind::Microphone, DeviceKind::Speaker];

/// The camera's menu.
const CAMERAS: &[DeviceKind] = &[DeviceKind::Camera];

/// A new frame wakes the stage, which draws the newest one.
fn wake_on_frames(feed: &VideoFeed, cx: &mut Context<CallStage>) -> Task<()> {
    let woken = feed.woken();
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        while woken.recv().await.is_ok() {
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
}

/// The menus' Video effects: opens the video effects' pane.
fn effects_item(selector: &'static str, stage: WeakEntity<CallStage>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .child(Glyph::SPARKLES.icon().size(px(14.0)))
            .child("Video effects")
    })
    .on_click(move |_, _, cx| {
        if let Some(stage) = stage.upgrade() {
            stage.update(cx, |stage, cx| stage.show_effects(true, cx));
        }
    })
}

/// Tells the stage's listeners `event`, while the stage is there.
fn emit(stage: &WeakEntity<CallStage>, event: CallStageEvent, cx: &mut App) {
    let _ = stage.update(cx, |_, cx| cx.emit(event));
}

/// The menu's rows of the devices of `kinds`: each kind's name, then its choices (a check on the one chosen), each
/// row's test selector `call-<kind>-<key>`; a choice goes to the settings as their own pickers set it.
pub(crate) fn device_items(
    mut menu: PopupMenu,
    kinds: &[DeviceKind],
    model: &Entity<SettingsModel>,
    cx: &App,
) -> PopupMenu {
    let settings = model.read(cx);
    let Some(list) = settings.devices() else {
        return menu;
    };
    for (index, kind) in kinds.iter().copied().enumerate() {
        if index > 0 {
            menu = menu.separator();
        }
        menu = menu.label(device_label(kind));
        for choice in settings_view::device_choices(kind, settings.settings(), list) {
            menu = menu.item(device_item(kind, choice, model));
        }
    }
    menu.scrollable(true).max_h(px(MENU_HEIGHT)).min_w(px(260.0))
}

/// The tallest a device menu gets before it scrolls, px.
const MENU_HEIGHT: f32 = 360.0;

/// One device's row of a menu, its test selector `call-<kind>-<key>`.
fn device_item(kind: DeviceKind, choice: DeviceChoice, model: &Entity<SettingsModel>) -> PopupMenuItem {
    let selector = format!("call-{}-{}", kind.key(), choice.key);
    let DeviceChoice {
        label, chosen, name, ..
    } = choice;
    let model = model.clone();
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div().debug_selector(move || selector.clone()).child(label.clone())
    })
    .checked(chosen)
    .on_click(move |_, _, cx| {
        let name = name.clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.set_device(kind, name.clone()), cx);
        });
    })
}

/// A control of the toolbar, Teams' way: its icon over its caption, its tooltip naming it (with the key bound to its
/// action, when it has one), marked while `marked` (muted, the camera off, the pane open).
fn tool(
    (id, glyph, caption): (&'static str, Glyph, &'static str),
    (tip, action): (&'static str, Option<&dyn gpui::Action>),
    marked: bool,
    palette: Palette,
    cx: &mut Context<CallStage>,
    on: fn(&mut CallStage, &mut Context<CallStage>),
) -> AnyElement {
    let action = action.map(gpui::Action::boxed_clone);
    div()
        .debug_selector(move || id.to_owned())
        .id(id)
        .h(px(48.0))
        .min_w(px(56.0))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .when(marked, |tool| tool.bg(rgb(palette.selected)))
        .hover(move |tool| tool.bg(rgb(palette.hover)))
        .tooltip(move |window, cx| {
            let tooltip = Tooltip::new(tip);
            match &action {
                Some(action) => tooltip.action(action.as_ref(), None).build(window, cx),
                None => tooltip.build(window, cx),
            }
        })
        .on_click(cx.listener(move |this, _, _, cx| on(this, cx)))
        .child(tool_face(glyph, caption, palette))
        .into_any_element()
}

/// A toolbar control's face: its icon over its caption.
fn tool_face(glyph: Glyph, caption: &'static str, palette: Palette) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap_0p5()
        .child(glyph.icon().size(px(20.0)))
        .child(div().text_xs().text_color(rgb(palette.foreground)).child(caption))
}

/// The screen while the call rings (or once it ended): its avatar, who it is with and its state, centred.
fn calling(title: &SharedString, state: &str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "call-calling".to_owned())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .child(people::avatar(Some(title), None, BIG_AVATAR))
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.clone()),
        )
        .child(
            div()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(state.to_owned())),
        )
        .into_any_element()
}

/// The avatar of who an ended call was with, a red hung-up mark on it.
fn ended_avatar(title: &SharedString, palette: Palette) -> gpui::Div {
    div()
        .relative()
        .child(people::avatar(Some(title), None, BIG_AVATAR))
        .child(
            div()
                .absolute()
                .bottom_0()
                .right_0()
                .size(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(rgb(palette.background))
                .bg(rgb(theme::DANGER))
                .text_color(rgb(0x00ff_ffff))
                .child(Glyph::PHONE_OFF.icon().size(px(16.0))),
        )
}

/// Call back (Teams' green) or Rejoin, with its key.
fn follow_up_button(rejoin: bool, cx: &mut Context<CallStage>) -> AnyElement {
    if rejoin {
        control(
            "call-end-rejoin",
            Control::Labelled(Glyph::PHONE, "Rejoin"),
            ("Rejoin the call", &JoinCall),
            Look::Success,
            cx,
            |this, cx| this.act_on_end(true, cx),
        )
    } else {
        control(
            "call-end-call-back",
            Control::Labelled(Glyph::PHONE, "Call back"),
            ("Call back", &CallChat),
            Look::Success,
            cx,
            |this, cx| this.act_on_end(false, cx),
        )
    }
}

/// The tiles in a grid of near-square rows, each tile as wide as a column, a short last row centred: Teams' gallery.
fn grid(selector: &'static str, tiles: Vec<AnyElement>) -> AnyElement {
    let count = tiles.len();
    let columns = (1..=count).find(|columns| columns * columns >= count).unwrap_or(1);
    let width = 1.0 / f32::from(u16::try_from(columns).unwrap_or(1));
    let mut rows: Vec<Vec<AnyElement>> = Vec::new();
    for tile in tiles {
        match rows.last_mut() {
            Some(row) if row.len() < columns => row.push(tile),
            _ => rows.push(vec![tile]),
        }
    }
    div()
        .debug_selector(move || selector.to_owned())
        .size_full()
        .flex()
        .flex_col()
        .children(rows.into_iter().map(|row| {
            div().flex_1().min_h_0().w_full().flex().justify_center().children(
                row.into_iter()
                    .map(|tile| div().w(relative(width)).h_full().p_1().child(tile)),
            )
        }))
        .into_any_element()
}

/// One tile: its video when a line shows it, else its initials; its name; a border in the brand colour while it
/// speaks.
fn tile_view(tile: StageTile, video: Option<AnyElement>, palette: Palette) -> AnyElement {
    let StageTile {
        selector,
        name,
        speaking,
        ..
    } = tile;
    let face = video.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(people::avatar(Some(&name), None, BIG_AVATAR))
            .into_any_element()
    });
    div()
        .debug_selector(move || selector.clone())
        .relative()
        .size_full()
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(palette.rail))
        .border_2()
        .border_color(rgb(if speaking { palette.brand } else { palette.rail }))
        .child(face)
        .child(name_tag(name, speaking, palette))
        .into_any_element()
}

/// A tile's name at its bottom left, with a speaker while it speaks.
fn name_tag(name: SharedString, speaking: bool, palette: Palette) -> gpui::Div {
    div()
        .absolute()
        .bottom_2()
        .left_2()
        .max_w(relative(0.9))
        .px_2()
        .py_0p5()
        .flex()
        .items_center()
        .gap_1()
        .rounded_md()
        .bg(rgb(palette.surface))
        .text_xs()
        .when(speaking, |tag| {
            tag.child(Glyph::VOLUME.icon().size(px(12.0)).text_color(rgb(palette.brand)))
        })
        .child(div().truncate().child(name))
}

/// The account's own tile, floating at the stage's bottom right: its self view while the camera sends, else its mark;
/// a border while it speaks.
fn self_view(speaking: bool, video: Option<AnyElement>, palette: Palette) -> AnyElement {
    let face = video.unwrap_or_else(|| {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::USER.icon().size(px(32.0)))
            .into_any_element()
    });
    div()
        .debug_selector(|| "call-participant-self".to_owned())
        .absolute()
        .bottom_5()
        .right_5()
        .w(px(SELF_VIEW.0))
        .h(px(SELF_VIEW.1))
        .rounded_lg()
        .overflow_hidden()
        .bg(rgb(palette.surface))
        .border_2()
        .border_color(rgb(if speaking { palette.brand } else { palette.border }))
        .shadow_md()
        .child(face)
        .child(name_tag(SharedString::from("You"), speaking, palette))
        .into_any_element()
}

/// One row of the participants' pane: the person's initials, name, and a speaker while they speak.
fn roster_row(selector: impl Into<String>, name: SharedString, speaking: bool, palette: Palette) -> AnyElement {
    let selector = selector.into();
    div()
        .debug_selector(move || selector.clone())
        .h(px(44.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(people::avatar(Some(&name), None, Size::Small))
        .child(div().flex_1().min_w_0().truncate().child(name))
        .when(speaking, |row| {
            row.child(Glyph::VOLUME.icon().size(px(16.0)).text_color(rgb(palette.brand)))
        })
        .into_any_element()
}

/// The account's own row of the participants' pane: its mark (not the initials of "You") and "You".
fn self_row(speaking: bool, palette: Palette) -> AnyElement {
    div()
        .debug_selector(|| "call-roster-self".to_owned())
        .h(px(44.0))
        .px_4()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(24.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(palette.selected))
                .text_color(rgb(palette.brand))
                .child(Glyph::USER.icon().size(px(14.0))),
        )
        .child(div().flex_1().child("You"))
        .when(speaking, |row| {
            row.child(Glyph::VOLUME.icon().size(px(16.0)).text_color(rgb(palette.brand)))
        })
        .into_any_element()
}

/// A participant's name: the roster's, else the one the call's people give its MRI, else "Someone".
fn participant_name(participant: &CallParticipant, people: &[(String, SharedString)]) -> SharedString {
    participant
        .name
        .clone()
        .map(SharedString::from)
        .or_else(|| {
            people
                .iter()
                .find(|(mri, _)| *mri == participant.mri)
                .map(|(_, name)| name.clone())
        })
        .unwrap_or_else(|| SharedString::from(SOMEONE))
}

/// The results' heading: the verdict's mark, "Test call results" and what they say.
fn results_header(verdict: Verdict, summary: String, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "test-call-results".to_owned())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(verdict_mark(verdict, 24.0, palette))
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Test call results"),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(summary)),
        )
}

/// A verdict's mark: Teams' green check, its red alert, or a muted dash.
fn verdict_mark(verdict: Verdict, size: f32, palette: Palette) -> AnyElement {
    let (glyph, color) = match verdict {
        Verdict::Fine => (Glyph::CIRCLE_CHECK, theme::SUCCESS),
        Verdict::Problem => (Glyph::CIRCLE_ALERT, theme::DANGER),
        Verdict::Untried => (Glyph::CIRCLE_MINUS, palette.muted_foreground),
    };
    glyph.icon().size(px(size)).text_color(rgb(color)).into_any_element()
}

/// One check of a test call's results, its test selector `test-call-check-<name>`: its device's glyph, its name and
/// the device, and on its right the verdict with its words.
fn check_row(
    (glyph, name, device): (Glyph, &'static str, String),
    (verdict, said): (Verdict, &'static str),
    palette: Palette,
) -> AnyElement {
    let selector = format!("test-call-check-{}", name.to_lowercase());
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .border_b_1()
        .border_color(rgb(palette.border))
        .child(glyph.icon().size(px(18.0)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(SharedString::from(device)),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .child(verdict_mark(verdict, 14.0, palette))
                .child(said),
        )
        .into_any_element()
}
