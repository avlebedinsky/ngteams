//! A video a message carries, played in its card as Teams plays one (`crate::files` draws the card's header): a black
//! box of the card's width holding the video's picture, with a seek bar and the controls over its bottom.
//!
//! - **Before it plays**: the file's thumbnail (Graph's, as the card shows it, `crate::inline::thumbnail`) with the
//!   controls over it. Play, a click on the picture or Space starts it: a [`Player`] (`teams-media`) on a thread of its
//!   own, reading the file part by part from the engine ([`Core::file_range`](crate::bridge::Core::file_range), through
//!   the card's [`PictureSource`]: no view builds a request), played by GStreamer, each frame shown here through
//!   GPUI's external surface, its sound on the speaker of the devices the app gave ([`set_playback`]).
//! - **The controls**, as Teams' player has them: play/pause, back 10 s, forward 10 s, mute, "More options" (open in
//!   the browser, download, copy link), full screen (the video over the whole window, the window full screen; Esc or
//!   the button leaves it) and "Open in browser" (Stream's player, where Teams pops the video out). The seek bar seeks
//!   where it is clicked or dragged to, its tooltip the time. While it plays the controls show only while the pointer
//!   is on the video; paused, always. Space plays or pauses, Left and Right go 10 s back or forward, M mutes.
//! - **Its states**: loading and buffering show a spinner over the picture; a video that can't be played here (a
//!   format no installed GStreamer plugin plays, a file the account can't read, the network) says so and offers the
//!   browser.
//! - The player lives as long as its card is drawn: a card scrolled away stops it. One plays at a time: starting one
//!   pauses the others, as in Teams.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Entity, ExternalSurface, FocusHandle, FontWeight, FrameCompletion,
    Global, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, ParentElement as _, PixelFrame, PixelRect, PixelSurface, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Task, WeakEntity, Window, anchored, canvas,
    deferred, div, hsla, img, point, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_core::EngineError;
use teams_media::devices::{PixelFormat, VideoFrame};
use teams_media::player::{
    ByteSource, Command, Failure, Player, PlayerOutput, PlayerSetup, SourceError, State, Status,
};
use teams_proto::chat::{FileKind, SharedFile};

use crate::files::{COPY_LINK, DOWNLOAD, OPEN_IN_BROWSER, PICTURE_HEIGHT};
use crate::icons::{Glyph, selected_by};
use crate::inline::{PictureSource, PictureState, thumbnail};
use crate::video::Picture;

/// How far back and forward the skip buttons go.
pub const SKIP: Duration = Duration::from_secs(10);

/// The video's controls' colour, on the black of the player in every theme, as Teams draws it.
const ON_VIDEO: u32 = 0x00ff_ffff;

/// What every video player is made with: the devices whose speaker plays the sound. The app sets it once
/// ([`set_playback`]); without it a video plays silently.
#[derive(Clone, Debug, Default)]
pub struct Playback(pub PlayerSetup);

impl Global for Playback {}

/// Sets what every video player is made with (see [`Playback`]).
pub fn set_playback(setup: PlayerSetup, cx: &mut App) {
    cx.set_global(Playback(setup));
}

/// Whether `file` is played in its card: a video with a sharing link (one whose format can't be played says so there).
#[must_use]
pub fn plays_inline(file: &SharedFile) -> bool {
    file.kind() == FileKind::Video && file.share_url.is_some()
}

/// A file's bytes as the engine reads them, for the player's threads.
struct EngineBytes {
    source: PictureSource,
    share_url: String,
}

impl ByteSource for EngineBytes {
    fn read(&self, offset: u64, len: u32) -> Result<(Vec<u8>, u64), SourceError> {
        let asked = self.source.engine().file_range(self.share_url.clone(), offset, len);
        match asked.blocking_wait() {
            Ok(range) => Ok((range.bytes, range.total)),
            Err(EngineError::FileUnavailable) => Err(SourceError::Unavailable),
            Err(EngineError::Stopped) => Err(SourceError::Stopped),
            Err(_) => Err(SourceError::Failed),
        }
    }
}

/// What the player's thread hands the view: its newest frame and status, and a wake.
struct Screen {
    picture: Mutex<Option<Arc<Picture>>>,
    status: Mutex<Option<Status>>,
    serial: AtomicU64,
    wake: async_channel::Sender<()>,
}

impl PlayerOutput for Screen {
    /// Takes a frame as the player hands it over: BGRA, its rows packed (anything else isn't drawn).
    fn frame(&self, frame: VideoFrame) {
        let (width, height) = (frame.width, frame.height);
        if frame.format != PixelFormat::Bgra || frame.stride != width * 4 {
            return;
        }
        let bgra = frame.data;
        let serial = self.serial.fetch_add(1, Ordering::Relaxed) + 1;
        *self.picture.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(Picture {
            width,
            height,
            bgra: Arc::new(bgra),
            serial,
            at: Instant::now(),
        }));
        // full means a wake is already waiting
        let _ = self.wake.try_send(());
    }

    fn status(&self, status: Status) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
        let _ = self.wake.try_send(());
    }
}

/// One card's player (see the module docs).
pub struct VideoPlayer {
    /// The card's id: the message's and the file's place.
    id: String,
    file: SharedFile,
    source: PictureSource,
    player: Option<Player>,
    screen: Arc<Screen>,
    status: Option<Status>,
    surface: Arc<PixelSurface>,
    drawn: Option<(u64, Arc<PixelFrame>)>,
    muted: bool,
    fullscreen: bool,
    /// The window was made full screen by this player (and is put back when it leaves).
    made_fullscreen: bool,
    /// The window is put back from full screen when this player goes (registered at its first full screen).
    release_hooked: bool,
    /// Where the seek bar was drawn, for a click's place on it.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The seek bar's place while it is dragged, as a fraction.
    dragging: Option<f32>,
    focus: FocusHandle,
    _woken: Task<()>,
}

impl VideoPlayer {
    /// The player of `file`, the card `id`'s, reading through `source`'s engine; nothing plays until asked.
    pub fn new(id: String, file: SharedFile, source: PictureSource, cx: &mut Context<Self>) -> Self {
        let me = cx.weak_entity();
        let shown = &mut cx.default_global::<Players>().0;
        shown.retain(|player| player.upgrade().is_some());
        shown.push(me);
        let (wake, woken) = async_channel::bounded(1);
        let task = cx.spawn(async move |this, cx| {
            while woken.recv().await.is_ok() {
                let alive = this.update(cx, |this, cx| {
                    this.status = *this.screen.status.lock().unwrap_or_else(PoisonError::into_inner);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        Self {
            id,
            file,
            source,
            player: None,
            screen: Arc::new(Screen {
                picture: Mutex::new(None),
                status: Mutex::new(None),
                serial: AtomicU64::new(0),
                wake,
            }),
            status: None,
            surface: PixelSurface::new(),
            drawn: None,
            muted: false,
            fullscreen: false,
            made_fullscreen: false,
            release_hooked: false,
            bar: Rc::new(Cell::new(None)),
            dragging: None,
            focus: cx.focus_handle(),
            _woken: task,
        }
    }

    /// The player's state as last told, `None` before it was started.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        self.status.map(|status| status.state)
    }

    /// Whether it plays, or will once it has loaded: what Pause stops and what another player's Play pauses.
    fn playing(&self) -> bool {
        self.player.is_some()
            && matches!(
                self.state(),
                None | Some(State::Loading | State::Playing | State::Buffering)
            )
    }

    /// Plays, or pauses while it plays; the first play opens the file.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if !self.playing() {
            pause_others(cx);
        }
        match &self.player {
            None => {
                let Some(share_url) = self.file.share_url.clone() else {
                    return;
                };
                let bytes = Arc::new(EngineBytes {
                    source: self.source.clone(),
                    share_url,
                });
                let setup = cx
                    .try_global::<Playback>()
                    .map(|playback| playback.0.clone())
                    .unwrap_or_default();
                let output: Arc<dyn PlayerOutput> = self.screen.clone();
                // a player whose thread couldn't be made told its failure to the screen already
                self.player = Player::open(bytes, setup, output, true);
                if self.muted
                    && let Some(player) = &self.player
                {
                    player.send(Command::Muted(true));
                }
            }
            Some(player) if self.playing() => player.send(Command::Pause),
            Some(player) => player.send(Command::Play),
        }
        cx.notify();
    }

    /// Goes `back` or forward by [`SKIP`].
    fn skip(&mut self, back: bool, cx: &mut Context<Self>) {
        let Some(status) = self.status else { return };
        let to = if back {
            status.position.saturating_sub(SKIP)
        } else {
            let ahead = status.position + SKIP;
            // a length not known yet holds nothing back
            if status.duration.is_zero() {
                ahead
            } else {
                ahead.min(status.duration)
            }
        };
        self.seek(to, cx);
    }

    fn seek(&mut self, to: Duration, cx: &mut Context<Self>) {
        if let Some(player) = &self.player {
            player.send(Command::Seek(to));
            if let Some(status) = self.status.as_mut() {
                status.position = to;
            }
            cx.notify();
        }
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        if let Some(player) = &self.player {
            player.send(Command::Muted(self.muted));
        }
        cx.notify();
    }

    fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fullscreen = !self.fullscreen;
        if self.fullscreen && !window.is_fullscreen() {
            window.toggle_fullscreen();
            self.made_fullscreen = true;
            if !self.release_hooked {
                // a card that goes while its video fills the screen (the chat switched, the list moved) puts the window
                // back as it found it
                self.release_hooked = true;
                cx.on_release_in(window, |this, window, _| {
                    if this.made_fullscreen && window.is_fullscreen() {
                        window.toggle_fullscreen();
                    }
                })
                .detach();
            }
        } else if !self.fullscreen && self.made_fullscreen {
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            self.made_fullscreen = false;
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// The seek bar's fraction under `position`, when the bar was drawn.
    fn fraction_at(&self, position: gpui::Point<Pixels>) -> Option<f32> {
        let bar = self.bar.get()?;
        let width = f32::from(bar.size.width).max(1.0);
        Some((f32::from(position.x - bar.origin.x) / width).clamp(0.0, 1.0))
    }

    fn seek_to_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let Some(status) = self.status else { return };
        self.seek(status.duration.mul_f32(fraction), cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "space" => self.toggle(cx),
            "left" => self.skip(true, cx),
            "right" => self.skip(false, cx),
            "m" => self.toggle_mute(cx),
            "escape" if self.fullscreen => self.toggle_fullscreen(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The frame to draw: the one made for the newest picture already, or a new one.
    fn frame(&mut self) -> Option<(Arc<PixelFrame>, (u32, u32))> {
        let picture = self
            .screen
            .picture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let shape = (picture.width, picture.height);
        if let Some((serial, frame)) = &self.drawn
            && *serial == picture.serial
        {
            return Some((Arc::clone(frame), shape));
        }
        let frame = Arc::new(PixelFrame {
            surface: Arc::clone(&self.surface),
            serial: picture.serial,
            width: picture.width,
            height: picture.height,
            pixels: Arc::clone(&picture.bgra),
            damage: vec![PixelRect {
                x: 0,
                y: 0,
                width: picture.width,
                height: picture.height,
            }],
            completion: FrameCompletion::new(|_| {}),
        });
        self.drawn = Some((picture.serial, Arc::clone(&frame)));
        Some((frame, shape))
    }
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.clone();
        let inline = if self.fullscreen {
            // the video is drawn over the whole window; the card keeps its place, black
            div().size_full().bg(gpui::black()).into_any_element()
        } else {
            self.screen_box(cx)
        };
        let overlay = self.fullscreen.then(|| {
            let viewport = window.viewport_size();
            let screen = self.screen_box(cx);
            deferred(
                anchored()
                    .position_mode(gpui::AnchoredPositionMode::Window)
                    .position(point(px(0.0), px(0.0)))
                    .child(div().w(viewport.width).h(viewport.height).child(screen)),
            )
            .with_priority(100)
        });
        div()
            .debug_selector(move || format!("video-player-{id}"))
            .w_full()
            .h(px(PICTURE_HEIGHT))
            .child(inline)
            .children(overlay)
    }
}

/// The parts of the player's box.
impl VideoPlayer {
    /// The black box with the picture, what is over it and the controls.
    fn screen_box(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let id = self.id.clone();
        let group = SharedString::from(format!("video-screen-{id}"));
        let picture = self.picture(cx);
        let middle = self.middle();
        let controls = self.controls(&group, cx);
        div()
            .id(SharedString::from(format!("video-screen-{id}")))
            .debug_selector(move || format!("video-screen-{id}"))
            .group(group)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(gpui::black())
            .child(
                div()
                    .id("video-picture")
                    .absolute()
                    .inset_0()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.toggle(cx);
                    }))
                    .child(picture),
            )
            .children(middle)
            .child(controls)
            .into_any_element()
    }

    /// The video's frame fitted into the box, else the file's thumbnail, else black.
    fn picture(&mut self, cx: &mut App) -> AnyElement {
        if let Some((frame, (width, height))) = self.frame() {
            return canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    window.paint_external_surface(fitted(bounds, width, height), ExternalSurface::Pixels(frame));
                },
            )
            .size_full()
            .into_any_element();
        }
        let poster = self
            .file
            .share_url
            .as_deref()
            .map_or(PictureState::Missing, |share_url| {
                thumbnail(share_url, &self.source, cx)
            });
        match poster {
            PictureState::Shown(image, _) => img(image).size_full().object_fit(ObjectFit::Contain).into_any_element(),
            PictureState::Reading | PictureState::Missing => div().size_full().into_any_element(),
        }
    }

    /// What shows in the middle: a spinner while it loads or waits, why it can't be played.
    fn middle(&self) -> Option<AnyElement> {
        let id = self.id.clone();
        match self.state()? {
            State::Loading | State::Buffering => Some(
                div()
                    .debug_selector(move || format!("video-loading-{id}"))
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Glyph::LOADING.icon().size(px(32.0)).text_color(gpui::rgb(ON_VIDEO)))
                    .into_any_element(),
            ),
            State::Failed(failure) => Some(self.failure(failure)),
            State::Paused | State::Playing | State::Ended => None,
        }
    }

    fn failure(&self, failure: Failure) -> AnyElement {
        let why = match failure {
            Failure::Unavailable => "This video can't be opened here.",
            Failure::Network => "The video couldn't be loaded. Check your connection.",
            Failure::Unsupported => "This video's format can't be played here.",
            Failure::NoCodec => "This video needs a codec that isn't installed (GStreamer's plugins for its format).",
            Failure::Internal => "The video couldn't be started.",
        };
        let id = self.id.clone();
        let open = self.file.open_link().map(str::to_owned);
        div()
            .debug_selector(move || format!("video-failed-{id}"))
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.75))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .text_sm()
            .text_color(gpui::rgb(ON_VIDEO))
            .child(div().text_center().child(why))
            .children(open.map(|link| {
                div()
                    .id("video-open-instead")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::rgb(ON_VIDEO))
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
                    .on_click(move |_, _, cx| cx.open_url(&link))
                    .child(OPEN_IN_BROWSER)
            }))
            .into_any_element()
    }

    /// The seek bar and the buttons over the box's bottom, on a shade.
    fn controls(&mut self, group: &SharedString, cx: &mut Context<Self>) -> AnyElement {
        let always = !self.playing() || self.dragging.is_some();
        let bar = self.seek_bar(cx);
        let buttons = self.buttons(cx);
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .px_3()
            .pb_1()
            .pt_6()
            .bg(gpui::linear_gradient(
                0.0,
                gpui::linear_color_stop(hsla(0.0, 0.0, 0.0, 0.7), 0.0),
                gpui::linear_color_stop(hsla(0.0, 0.0, 0.0, 0.0), 1.0),
            ))
            .when(!always, |controls| {
                controls
                    .opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
            })
            .flex()
            .flex_col()
            .gap_1()
            .child(bar)
            .child(buttons)
            .into_any_element()
    }

    fn seek_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (position, duration) = self.status.map_or((Duration::ZERO, Duration::ZERO), |status| {
            (status.position, status.duration)
        });
        let played = if duration.is_zero() {
            0.0
        } else {
            (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        let fraction = self.dragging.unwrap_or(played);
        let tip = SharedString::from(format!("{} / {}", clock(duration.mul_f32(fraction)), clock(duration)));
        let bar = Rc::clone(&self.bar);
        let id = self.id.clone();
        div()
            .id("video-seek")
            .debug_selector(move || format!("video-seek-{id}"))
            .relative()
            .h(px(14.0))
            .w_full()
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.dragging = this.fraction_at(event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.dragging.is_some() && event.pressed_button == Some(MouseButton::Left) {
                    this.dragging = this.fraction_at(event.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_drag))
            .child(
                canvas(move |bounds, _, _| bar.set(Some(bounds)), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(5.5))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 1.0, 0.35))
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(gpui::rgb(ON_VIDEO)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1.0))
                    .left(relative(fraction))
                    .ml(px(-6.0))
                    .size(px(12.0))
                    .rounded_full()
                    .bg(gpui::rgb(ON_VIDEO)),
            )
            .into_any_element()
    }

    fn end_drag(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            let fraction = self.fraction_at(event.position).unwrap_or(0.0);
            self.seek_to_fraction(fraction, cx);
        }
    }

    /// Play, back, forward and mute on the left; "More options", full screen and the browser on the right.
    fn buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let id = &self.id;
        let (play_glyph, play_tip) = if self.playing() {
            (Glyph::PAUSE, "Pause")
        } else {
            (Glyph::PLAY_MEDIA, "Play")
        };
        // a player that plays with no speaker says so on its sound's button
        let silent = self.status.is_some_and(|status| self.playing() && !status.sound);
        let (volume_glyph, volume_tip) = if silent {
            (Glyph::VOLUME_OFF, "No sound device")
        } else if self.muted {
            (Glyph::VOLUME_OFF, "Unmute")
        } else {
            (Glyph::VOLUME, "Mute")
        };
        let (screen_glyph, screen_tip) = if self.fullscreen {
            (Glyph::SHRINK, "Exit full screen")
        } else {
            (Glyph::FULLSCREEN, "Full screen")
        };
        let open = self.file.open_link().map(str::to_owned);
        let left = div()
            .flex()
            .items_center()
            .gap_1()
            .child(control(
                id,
                "play",
                play_glyph,
                play_tip,
                None,
                cx.listener(|this, _, _, cx| this.toggle(cx)),
            ))
            .child(control(
                id,
                "back",
                Glyph::SKIP_BACK,
                "Back 10 seconds",
                Some("10"),
                cx.listener(|this, _, _, cx| this.skip(true, cx)),
            ))
            .child(control(
                id,
                "forward",
                Glyph::SKIP_FORWARD,
                "Forward 10 seconds",
                Some("10"),
                cx.listener(|this, _, _, cx| this.skip(false, cx)),
            ))
            .child(control(
                id,
                "mute",
                volume_glyph,
                volume_tip,
                None,
                cx.listener(|this, _, _, cx| this.toggle_mute(cx)),
            ));
        let right = div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.more())
            .child(control(
                id,
                "fullscreen",
                screen_glyph,
                screen_tip,
                None,
                cx.listener(|this, _, window, cx| this.toggle_fullscreen(window, cx)),
            ))
            .children(open.map(|link| {
                control(
                    id,
                    "open",
                    Glyph::POP_OUT,
                    OPEN_IN_BROWSER,
                    None,
                    move |_, _, cx: &mut App| {
                        cx.open_url(&link);
                    },
                )
            }));
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(left)
            .child(right)
            .into_any_element()
    }

    /// "More options" on the video: the file's own actions, as the card's header offers them.
    fn more(&self) -> AnyElement {
        let id = self.id.clone();
        let open = self.file.open_link().map(str::to_owned);
        let download = self.file.download_link();
        let button = Button::new(SharedString::from(format!("video-more-button-{id}")))
            .ghost()
            .icon(Glyph::MORE.icon().size(px(18.0)).text_color(gpui::rgb(ON_VIDEO)))
            .tooltip("More options")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                if let Some(open) = open.clone() {
                    let copied = open.clone();
                    menu = menu
                        .item(
                            menu_item("open", Glyph::EXTERNAL_LINK, OPEN_IN_BROWSER).on_click(move |_, _, cx| {
                                cx.open_url(&open);
                            }),
                        )
                        .item(menu_item("copy", Glyph::LINK, COPY_LINK).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        }));
                }
                if let Some(download) = download.clone() {
                    menu = menu.item(
                        menu_item("download", Glyph::DOWNLOAD, DOWNLOAD)
                            .on_click(move |_, _, cx| cx.open_url(&download)),
                    );
                }
                menu
            });
        selected_by(format!("video-more-{}", self.id), button).into_any_element()
    }
}

/// A control on the video: its icon in white (with a small mark under it, the skip buttons' "10"), its tooltip, its
/// click; its test selector `video-<what>-<card>`.
fn control(
    card: &str,
    what: &'static str,
    glyph: Glyph,
    tip: &'static str,
    mark: Option<&'static str>,
    click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let selector = format!("video-{what}-{card}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .relative()
        .size(px(28.0))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            click(event, window, cx);
        })
        .child(glyph.icon().size(px(18.0)).text_color(gpui::rgb(ON_VIDEO)))
        .children(mark.map(|mark| {
            div()
                .absolute()
                .bottom(px(-1.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .text_size(px(8.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui::rgb(ON_VIDEO))
                .child(mark)
        }))
        .into_any_element()
}

/// A "More options" row: its icon and words.
fn menu_item(what: &'static str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("video-menu-{what}"))
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}

/// `bounds` narrowed to a picture of `width` by `height` fitted inside it, centred, its aspect ratio kept.
pub(crate) fn fitted(bounds: Bounds<Pixels>, width: u32, height: u32) -> Bounds<Pixels> {
    let side = |pixels: u32| f32::from(u16::try_from(pixels).unwrap_or(u16::MAX)).max(1.0);
    let (box_width, box_height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let scale = (box_width / side(width)).min(box_height / side(height));
    let (drawn_width, drawn_height) = (side(width) * scale, side(height) * scale);
    Bounds {
        origin: point(
            bounds.origin.x + px((box_width - drawn_width) / 2.0),
            bounds.origin.y + px((box_height - drawn_height) / 2.0),
        ),
        size: size(px(drawn_width), px(drawn_height)),
    }
}

/// A time as the player writes it: `m:ss`, or `h:mm:ss` from an hour.
#[must_use]
pub fn clock(at: Duration) -> String {
    let seconds = at.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Every player drawn this run (the ones whose card went are let go): one plays at a time, as in Teams.
#[derive(Default)]
struct Players(Vec<WeakEntity<VideoPlayer>>);

impl Global for Players {}

/// Pauses every player but the one being updated.
fn pause_others(cx: &mut Context<VideoPlayer>) {
    let me = cx.entity_id();
    for other in players(cx) {
        if other.entity_id() == me {
            continue;
        }
        other.update(cx, |other, cx| {
            if other.playing()
                && let Some(player) = &other.player
            {
                player.send(Command::Pause);
                cx.notify();
            }
        });
    }
}

/// The players drawn now, the first drawn first.
#[must_use]
pub fn players(cx: &App) -> Vec<Entity<VideoPlayer>> {
    cx.try_global::<Players>()
        .map(|players| players.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default()
}

/// The player of the card `id` showing `file`, kept while the card is drawn.
pub fn player_of(
    id: &str,
    file: &SharedFile,
    source: &PictureSource,
    window: &mut Window,
    cx: &mut App,
) -> Entity<VideoPlayer> {
    let (id, file, source) = (id.to_owned(), file.clone(), source.clone());
    window.use_keyed_state(SharedString::from(format!("video-player-{id}")), cx, move |_, cx| {
        VideoPlayer::new(id, file, source, cx)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_written_as_the_player_writes_them() {
        assert_eq!(clock(Duration::from_secs(5)), "0:05");
        assert_eq!(clock(Duration::from_secs(754)), "12:34");
        assert_eq!(clock(Duration::from_secs(3_723)), "1:02:03");
    }

    #[test]
    fn a_picture_is_fitted_inside_its_box() {
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: size(px(320.0), px(180.0)),
        };
        let fit = fitted(bounds, 640, 480);
        assert_eq!(fit.size, size(px(240.0), px(180.0)));
        assert_eq!(fit.origin, point(px(50.0), px(20.0)));
    }
}
