//! The camera's background effects as Teams offers them (its "Background effects"): a self-preview over a grid of
//! tiles, None, Blur, "Add new" (a picture of the user's), the app's own pictures, and the ones the user added (each
//! with Remove on hover). A tile chosen is the settings' [`VideoEffect`], kept between calls and applied by the media
//! to every frame the camera sends from the next one on, a call's camera on now included ([`crate::video::VideoFeed`]).
//!
//! - **Where**: the settings' Calls page (the device setup, under the camera) and the call window's Video effects pane
//!   (More, or the camera's menu), each an [`EffectsPicker`].
//! - **The self-preview**: in a call with the camera on, what the call sends (its self view). Otherwise the camera is
//!   off, and Preview (the user's click, never by itself) turns it on for the preview only ([`CameraPreview`]: on a
//!   thread of its own, the effect applied as a call's camera applies it, nothing sent) until Stop preview, the pane or
//!   page closing, the call's camera turning on, or the picker going unseen for [`UNSEEN`] while frames come (another
//!   page, another account, the window hidden: a camera is never left on out of sight).
//! - **Pictures the user adds** are copied (scaled down to fit 1080p, as JPEGs) into the backgrounds' directory beside
//!   the settings file ([`SettingsModel::backgrounds_dir`]); Remove deletes the copy.
//!
//! The trace has `video-effect chosen` (the kind: none, blur, stock, added) and `video-effect preview` (on, off), never
//! a picture's name.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, PathPromptOptions, Render, RenderImage, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, Task, Window, div, img, prelude::FluentBuilder as _, px, relative,
    rgb, rgba,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use image::Frame;
use image::imageops::FilterType;
use teams_media::devices::Devices;
use teams_media::video::convert::{MaxSize, to_i420};
use teams_media::video::effects::{self, Background, BackgroundChoice, Effects, STOCK};

use crate::icons::{Glyph, icon_button, selected_by};
use crate::settings::{SettingsModel, VideoEffect};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::video::{Tile, Tiles, VideoFeed};

/// The preview's largest picture: it is drawn small, so a smaller frame than a call's saves the effects' work.
const PREVIEW_SIZE: MaxSize = MaxSize {
    width: 640,
    height: 360,
};
/// How long the preview's thread waits for a frame before it checks whether to stop.
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// A tile's thumbnail, px (16:9).
const THUMB: (u32, u32) = (240, 135);
/// The stock picture the Blur tile shows blurred.
const BLUR_TILE: usize = 0;
/// How long the preview runs on while its picker isn't drawn though frames come: then it stops.
pub const UNSEEN: Duration = Duration::from_secs(3);

/// The camera turned on for the effects' preview (see the module docs): its frames, the effect applied, on a feed of
/// its own. Cloned for each view; one preview runs at a time.
#[derive(Clone)]
pub struct CameraPreview {
    inner: Arc<PreviewInner>,
}

/// A running preview's number, its stop flag and its thread.
type Running = (u64, Arc<AtomicBool>, JoinHandle<()>);

struct PreviewInner {
    devices: Arc<dyn Devices + Send + Sync>,
    feed: VideoFeed,
    background: BackgroundChoice,
    /// The preview's thread while one runs (or is stopping), and its number.
    running: Mutex<Option<Running>>,
    /// Why the last preview stopped by itself (the camera couldn't be opened or went away).
    failure: Arc<Mutex<Option<String>>>,
    /// When a picker last drew the running preview.
    seen: Arc<Mutex<Instant>>,
    next: Mutex<u64>,
}

impl std::fmt::Debug for CameraPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CameraPreview").finish_non_exhaustive()
    }
}

/// A preview running: it stops when this is dropped.
pub struct PreviewSession {
    preview: CameraPreview,
    number: u64,
}

impl PreviewSession {
    /// The picker drew the preview: it runs on.
    pub fn seen(&self) {
        *lock(&self.preview.inner.seen) = Instant::now();
    }

    /// Whether the preview still runs (it stops by itself when the camera fails or nothing draws it).
    #[must_use]
    pub fn running(&self) -> bool {
        lock(&self.preview.inner.running)
            .as_ref()
            .is_some_and(|(number, _, handle)| *number == self.number && !handle.is_finished())
    }
}

impl Drop for PreviewSession {
    fn drop(&mut self) {
        self.preview.stop(self.number);
    }
}

impl CameraPreview {
    /// A preview of the camera `devices` open (the one the settings choose).
    #[must_use]
    pub fn new(devices: Arc<dyn Devices + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(PreviewInner {
                devices,
                feed: VideoFeed::new(),
                background: BackgroundChoice::default(),
                running: Mutex::new(None),
                failure: Arc::new(Mutex::new(None)),
                seen: Arc::new(Mutex::new(Instant::now())),
                next: Mutex::new(0),
            }),
        }
    }

    /// The feed the preview's frames are shown on (its [`Tile::Own`]).
    #[must_use]
    pub fn feed(&self) -> &VideoFeed {
        &self.inner.feed
    }

    /// Applies `background` from the preview's next frame on.
    pub fn set_background(&self, background: Background) {
        self.inner.background.set(background);
    }

    /// Why the last preview stopped by itself, if it did.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        lock(&self.inner.failure).clone()
    }

    /// Turns the camera on for the preview (the user asked), until the session returned is dropped. A preview running
    /// for another view stops first.
    #[must_use]
    pub fn start(&self) -> PreviewSession {
        let number = {
            let mut next = lock(&self.inner.next);
            *next += 1;
            *next
        };
        *lock(&self.inner.failure) = None;
        *lock(&self.inner.seen) = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let previous = lock(&self.inner.running).take();
        let run = PreviewRun {
            devices: Arc::clone(&self.inner.devices),
            feed: self.inner.feed.clone(),
            background: self.inner.background.clone(),
            failure: Arc::clone(&self.inner.failure),
            seen: Arc::clone(&self.inner.seen),
            stop: Arc::clone(&stop),
        };
        let spawned = thread::Builder::new().name("teams-ui-preview".into()).spawn(move || {
            if let Some((_, stop, handle)) = previous {
                stop.store(true, Ordering::Relaxed);
                let _ = handle.join();
            }
            run.run();
        });
        match spawned {
            Ok(handle) => *lock(&self.inner.running) = Some((number, stop, handle)),
            Err(error) => *lock(&self.inner.failure) = Some(error.to_string()),
        }
        PreviewSession {
            preview: self.clone(),
            number,
        }
    }

    /// Stops preview `number` if it is the one running; its thread ends by itself (no wait on a window's thread).
    fn stop(&self, number: u64) {
        if let Some((running, stop, _)) = lock(&self.inner.running).as_ref()
            && *running == number
        {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// One preview's thread.
struct PreviewRun {
    devices: Arc<dyn Devices + Send + Sync>,
    feed: VideoFeed,
    background: BackgroundChoice,
    failure: Arc<Mutex<Option<String>>>,
    seen: Arc<Mutex<Instant>>,
    stop: Arc<AtomicBool>,
}

impl PreviewRun {
    fn run(self) {
        let mut camera = match self.devices.camera() {
            Ok(camera) => camera,
            Err(error) => return self.fail(&format!("the camera can't be opened: {error}")),
        };
        let Some(mut sink) = teams_core::VideoOutput::preview(&self.feed) else {
            return;
        };
        let mut effects = Effects::new();
        let mut first_shown: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            match camera.next_frame(FRAME_WAIT) {
                Ok(Some(frame)) => {
                    let Some(mut shown) = to_i420(&frame, PREVIEW_SIZE) else {
                        continue;
                    };
                    effects.apply(&mut shown, &self.background.get());
                    let _ = sink.show(&shown);
                    let first = *first_shown.get_or_insert_with(Instant::now);
                    if first.elapsed() > UNSEEN && lock(&self.seen).elapsed() > UNSEEN {
                        log::info!("the camera's preview stopped: nothing draws it");
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => return self.fail(&format!("the camera stopped: {error}")),
            }
        }
        self.feed.clear();
        self.feed.wake();
    }

    fn fail(&self, why: &str) {
        log::warn!("the camera's preview: {why}");
        *lock(&self.failure) = Some(why.to_owned());
        self.feed.clear();
        self.feed.wake();
    }
}

/// Where a picker is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerPlace {
    /// The settings' Calls page: wider tiles.
    Settings,
    /// The call window's pane: three tiles a row.
    CallPane,
}

impl PickerPlace {
    /// A tile's width, px.
    fn tile_width(self) -> f32 {
        match self {
            Self::Settings => 120.0,
            Self::CallPane => 80.0,
        }
    }

    /// The preview's width, px.
    fn preview_width(self) -> f32 {
        match self {
            Self::Settings => 400.0,
            Self::CallPane => 256.0,
        }
    }
}

/// The video effects' picker (see the module docs). A GPUI entity, a child of the settings view or the call stage.
pub struct EffectsPicker {
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    place: PickerPlace,
    /// The call's video and whether its camera is on (a call window's picker).
    call: Option<(VideoFeed, bool, u64)>,
    /// The tiles drawing the preview's feed, and the call's.
    preview_tiles: Option<Tiles>,
    call_tiles: Option<Tiles>,
    session: Option<PreviewSession>,
    /// Each tile's thumbnail by its key, once decoded; the keys being decoded.
    thumbs: HashMap<String, Arc<RenderImage>>,
    decoding: HashSet<String>,
    /// The pictures the user added, the oldest first.
    added: Vec<PathBuf>,
    /// A picture being added, and what went wrong with the last one.
    adding: Option<Task<()>>,
    problem: Option<SharedString>,
    _waking: Option<Task<()>>,
    _observing: Subscription,
}

impl EffectsPicker {
    /// A picker over the settings' choice, shown at `place`.
    pub fn new(
        settings: Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        place: PickerPlace,
        cx: &mut Context<Self>,
    ) -> Self {
        let observing = cx.observe(&settings, |_, _, cx| cx.notify());
        let preview = settings.read(cx).camera_preview().cloned();
        let waking = preview.as_ref().map(|preview| wake_on(preview.feed(), cx));
        // the tiles' own lines stay out of the trace: the stage's self view already tells the call's
        let preview_tiles = preview.map(|preview| Tiles::new(preview.feed().clone(), Arc::new(ViewTrace::off())));
        let mut picker = Self {
            settings,
            trace,
            place,
            call: None,
            preview_tiles,
            call_tiles: None,
            session: None,
            thumbs: HashMap::new(),
            decoding: HashSet::new(),
            added: Vec::new(),
            adding: None,
            problem: None,
            _waking: waking,
            _observing: observing,
        };
        picker.reload_added(cx);
        picker
    }

    /// The picker of a call window: the call's video (`feed`), so the preview shows what the call sends while its
    /// camera is on.
    #[must_use]
    pub fn in_call(mut self, feed: VideoFeed, call: u64) -> Self {
        self.call_tiles = Some(Tiles::new(feed.clone(), Arc::new(ViewTrace::off())));
        self.call = Some((feed, false, call));
        self
    }

    /// Tells the picker whether the call's camera is on: while it is, the preview is what the call sends, and a
    /// preview of the picker's own stops.
    pub fn set_camera_on(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some((_, camera_on, _)) = &mut self.call
            && *camera_on != on
        {
            *camera_on = on;
            if on {
                self.stop_preview();
            }
            cx.notify();
        }
    }

    /// Whether the picker's own preview runs.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.session.is_some()
    }

    /// Preview: turns the camera on for the preview (the user's click).
    pub fn start_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.settings.read(cx).camera_preview().cloned() else {
            return;
        };
        self.session = Some(preview.start());
        self.trace.record(&TraceLine::new("video-effect", "preview").kind("on"));
        cx.notify();
    }

    fn stop_preview(&mut self) {
        if self.session.take().is_some() {
            self.trace
                .record(&TraceLine::new("video-effect", "preview").kind("off"));
        }
    }

    /// Chooses `effect`: the settings keep it, and the camera sends with it from its next frame.
    fn choose(&mut self, effect: VideoEffect, cx: &mut Context<Self>) {
        let kind = match &effect {
            VideoEffect::None => "none",
            VideoEffect::Blur => "blur",
            VideoEffect::Stock(_) => "stock",
            VideoEffect::Added(_) => "added",
        };
        self.trace.record(&TraceLine::new("video-effect", "chosen").kind(kind));
        self.settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.video_effect = effect, cx);
        });
    }

    /// Reads the pictures the user added again.
    fn reload_added(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.settings.read(cx).backgrounds_dir() else {
            return;
        };
        let listing = cx.background_spawn(async move { effects::added(&dir) });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let added = listing.await;
            let _ = this.update(cx, |picker, cx| {
                picker.added = added;
                cx.notify();
            });
        })
        .detach();
    }

    /// "Add new": the user picks a picture; it is copied into the backgrounds' directory and chosen.
    fn add_picture(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.settings.read(cx).backgrounds_dir() else {
            return;
        };
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add".into()),
        });
        self.adding = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            let Some(source) = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next())
            else {
                // cancelled (or the chooser failed): Add new works again
                let _ = this.update(cx, |picker, cx| {
                    picker.adding = None;
                    cx.notify();
                });
                return;
            };
            let imported = cx.background_spawn(async move { effects::import(&source, &dir) }).await;
            let _ = this.update(cx, |picker, cx| picker.added_picture(imported, cx));
        }));
    }

    /// A picture added, or why it couldn't be.
    fn added_picture(&mut self, imported: Result<PathBuf, String>, cx: &mut Context<Self>) {
        self.adding = None;
        match imported {
            Ok(path) => {
                self.problem = None;
                let name = path.file_name().map(|name| name.to_string_lossy().into_owned());
                if let Some(name) = name {
                    self.choose(VideoEffect::Added(name), cx);
                }
                self.reload_added(cx);
            }
            Err(error) => {
                log::warn!("a background picture can't be added: {error}");
                self.problem = Some("That picture can't be used. Choose a JPEG, PNG, GIF or WebP picture.".into());
                cx.notify();
            }
        }
    }

    /// Remove on an added picture's tile: its copy deleted (no longer chosen if it was).
    fn remove_picture(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned());
        if name.is_some_and(|name| self.settings.read(cx).settings().video_effect == VideoEffect::Added(name)) {
            self.choose(VideoEffect::None, cx);
        }
        self.thumbs.remove(&thumb_key_of_file(&path));
        let removing = cx.background_spawn(async move { std::fs::remove_file(&path) });
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            if let Err(error) = removing.await {
                log::warn!("a background picture can't be removed: {error}");
            }
            let _ = this.update(cx, EffectsPicker::reload_added);
        })
        .detach();
    }

    /// The thumbnail of `key` once decoded; its decoding started now unless it is under way.
    fn thumb(&mut self, key: &str, source: ThumbSource, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        if let Some(thumb) = self.thumbs.get(key) {
            return Some(Arc::clone(thumb));
        }
        if self.decoding.insert(key.to_owned()) {
            let key = key.to_owned();
            let decoding = cx.background_spawn(async move { source.decode() });
            cx.spawn(async move |this, cx: &mut AsyncApp| {
                let decoded = decoding.await;
                let _ = this.update(cx, |picker, cx| picker.decoded(key, decoded, cx));
            })
            .detach();
        }
        None
    }

    /// Keeps `key`'s thumbnail once decoded (none when it can't be).
    fn decoded(&mut self, key: String, decoded: Option<RenderImage>, cx: &mut Context<Self>) {
        self.decoding.remove(&key);
        if let Some(decoded) = decoded {
            self.thumbs.insert(key, Arc::new(decoded));
            cx.notify();
        }
    }
}

/// Where a thumbnail comes from.
enum ThumbSource {
    Stock(&'static [u8]),
    StockBlurred(&'static [u8]),
    File(PathBuf),
}

impl ThumbSource {
    /// The thumbnail ([`THUMB`], cropped to fill it) in the BGRA GPUI draws; `None` when the picture doesn't decode.
    fn decode(self) -> Option<RenderImage> {
        let picture = match &self {
            Self::Stock(bytes) | Self::StockBlurred(bytes) => image::load_from_memory(bytes).ok()?,
            Self::File(path) => image::open(path).ok()?,
        };
        let mut thumb = picture
            .resize_to_fill(THUMB.0, THUMB.1, FilterType::Triangle)
            .into_rgba8();
        if matches!(self, Self::StockBlurred(_)) {
            thumb = image::imageops::blur(&thumb, 6.0);
        }
        for pixel in thumb.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Some(RenderImage::new(vec![Frame::new(thumb)]))
    }
}

fn thumb_key_of_file(path: &Path) -> String {
    format!("file:{}", path.display())
}

impl EffectsPicker {
    /// The self-preview: what the call sends while its camera is on, else the picker's own preview, else the camera
    /// off with Preview.
    fn preview(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.session.as_ref().is_some_and(|session| !session.running()) {
            self.stop_preview();
        }
        let width = self.place.preview_width();
        let camera_on = self.call.as_ref().is_some_and(|(_, on, _)| *on);
        let call_number = self.call.as_ref().map_or(0, |(_, _, call)| *call);
        let video = if camera_on {
            self.call_tiles
                .as_mut()
                .and_then(|tiles| tiles.fill(Tile::Own, call_number))
        } else if let Some(session) = &self.session {
            session.seen();
            self.preview_tiles.as_mut().and_then(|tiles| tiles.fill(Tile::Own, 0))
        } else {
            None
        };
        let has_video = video.is_some();
        let face = video.unwrap_or_else(|| self.camera_off(camera_on, palette, cx));
        let stop = (self.session.is_some() && !camera_on).then(|| {
            selected_by(
                "effects-stop-preview",
                icon_button("effects-stop-preview", Glyph::VIDEO_OFF, "Stop preview")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.stop_preview();
                        cx.notify();
                    })),
            )
            .absolute()
            .top_2()
            .right_2()
            .rounded_md()
            .bg(rgb(palette.surface))
        });
        div()
            .debug_selector(|| "effects-preview".to_owned())
            .relative()
            .w(px(width))
            .max_w_full()
            .h(px(width * 9.0 / 16.0))
            .flex_none()
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(if has_video { 0x0020_2020 } else { palette.surface }))
            .border_1()
            .border_color(rgb(palette.border))
            .child(face)
            .children(stop)
            .into_any_element()
    }

    /// The preview while no picture shows: the camera off, and Preview where it can start one (or why it stopped).
    fn camera_off(&self, camera_on: bool, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let preview = self.settings.read(cx).camera_preview().cloned();
        let failure = preview.as_ref().and_then(CameraPreview::failure);
        let starting = self.session.is_some() || camera_on;
        let words: SharedString = if let Some(failure) = &failure {
            format!("The preview stopped: {failure}").into()
        } else if starting {
            "Turning the camera on…".into()
        } else if preview.is_some() {
            "Your camera is off".into()
        } else {
            "There's no camera to preview".into()
        };
        let button = (preview.is_some() && !starting).then(|| {
            selected_by(
                "effects-preview-start",
                Button::new("effects-preview-start")
                    .icon(Glyph::VIDEO.icon())
                    .label("Preview")
                    .outline()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| this.start_preview(cx))),
            )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_3()
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::VIDEO_OFF.icon().size(px(24.0)))
            .child(div().text_sm().text_center().child(words))
            .children(button)
            .into_any_element()
    }

    /// The tiles: None, Blur, Add new, the app's pictures, the user's.
    fn tiles(&mut self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let chosen = self.settings.read(cx).settings().video_effect.clone();
        let width = self.place.tile_width();
        let mut tiles = vec![
            self.plain_tile(
                ("effect-none", "No background effect", VideoEffect::None),
                Glyph::BAN,
                &chosen,
                palette,
                cx,
            ),
            self.blur_tile(&chosen, palette, cx),
        ];
        if self.settings.read(cx).backgrounds_dir().is_some() {
            tiles.push(self.add_tile(palette, cx));
        }
        for stock in &STOCK {
            let thumb = self.thumb(&format!("stock:{}", stock.key), ThumbSource::Stock(stock.jpeg), cx);
            let effect = VideoEffect::Stock(stock.key);
            let selected = chosen == effect;
            let face = thumb_face(thumb, palette);
            tiles.push(tile(
                (format!("effect-stock-{}", stock.key), stock.name.into()),
                (selected, width),
                face,
                palette,
                cx.listener(move |this, _, _, cx| this.choose(effect.clone(), cx)),
            ));
        }
        let added = self.added.clone();
        for (index, path) in added.into_iter().enumerate() {
            tiles.push(self.added_tile(index, path, &chosen, palette, cx));
        }
        tiles
    }

    fn plain_tile(
        &mut self,
        (selector, tip, effect): (&'static str, &'static str, VideoEffect),
        glyph: Glyph,
        chosen: &VideoEffect,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let face = div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(glyph.icon().size(px(20.0)))
            .into_any_element();
        let selected = *chosen == effect;
        tile(
            (selector.to_owned(), tip.into()),
            (selected, self.place.tile_width()),
            face,
            palette,
            cx.listener(move |this, _, _, cx| this.choose(effect.clone(), cx)),
        )
    }

    fn blur_tile(&mut self, chosen: &VideoEffect, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let source = ThumbSource::StockBlurred(STOCK[BLUR_TILE].jpeg);
        let thumb = self.thumb("blur", source, cx);
        let face = div()
            .relative()
            .size_full()
            .child(thumb_face(thumb, palette))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0x00ff_ffff))
                    .child(Glyph::USER.icon().size(px(22.0))),
            )
            .into_any_element();
        tile(
            ("effect-blur".to_owned(), "Blur".into()),
            (*chosen == VideoEffect::Blur, self.place.tile_width()),
            face,
            palette,
            cx.listener(|this, _, _, cx| this.choose(VideoEffect::Blur, cx)),
        )
    }

    fn add_tile(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.adding.is_some();
        let face = div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.brand))
            .child(
                if busy { Glyph::LOADING } else { Glyph::IMAGE_PLUS }
                    .icon()
                    .size(px(18.0)),
            )
            .child(div().text_xs().child("Add new"))
            .into_any_element();
        tile(
            ("effect-add".to_owned(), "Add a picture of your own".into()),
            (false, self.place.tile_width()),
            face,
            palette,
            cx.listener(|this, _, _, cx| {
                if this.adding.is_none() {
                    this.add_picture(cx);
                }
            }),
        )
    }

    fn added_tile(
        &mut self,
        index: usize,
        path: PathBuf,
        chosen: &VideoEffect,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let thumb = self.thumb(&thumb_key_of_file(&path), ThumbSource::File(path.clone()), cx);
        let effect = VideoEffect::Added(name);
        let selected = *chosen == effect;
        let group: SharedString = format!("effect-added-{index}").into();
        let remove = selected_by(
            format!("effect-added-{index}-remove"),
            icon_button(("effect-remove", index), Glyph::X, "Remove background")
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.remove_picture(path.clone(), cx);
                })),
        )
        .absolute()
        .top_1()
        .right_1()
        .rounded_md()
        .bg(rgb(palette.surface))
        .invisible()
        .group_hover(group.clone(), gpui::Styled::visible);
        let face = div()
            .relative()
            .size_full()
            .child(thumb_face(thumb, palette))
            .child(remove)
            .into_any_element();
        div()
            .group(group.clone())
            .child(tile(
                (group.to_string(), "Your picture".into()),
                (selected, self.place.tile_width()),
                face,
                palette,
                cx.listener(move |this, _, _, cx| this.choose(effect.clone(), cx)),
            ))
            .into_any_element()
    }
}

/// A thumbnail filling its tile, or the tile's surface while it decodes.
fn thumb_face(thumb: Option<Arc<RenderImage>>, palette: Palette) -> AnyElement {
    match thumb {
        Some(thumb) => img(thumb).size_full().object_fit(ObjectFit::Cover).into_any_element(),
        None => div().size_full().bg(rgb(palette.background)).into_any_element(),
    }
}

/// One tile: its face in a 16:9 box `width` wide, a ring in the brand colour when it is the one chosen, its name as
/// its tooltip, `selector` for tests.
fn tile(
    (selector, tip): (String, SharedString),
    (chosen, width): (bool, f32),
    face: AnyElement,
    palette: Palette,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let id = SharedString::from(selector.clone());
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .w(px(width))
        .h(px(width * 9.0 / 16.0))
        .flex_none()
        .rounded_md()
        .overflow_hidden()
        .border_2()
        .border_color(if chosen { rgb(palette.brand) } else { rgba(0x0000_0000) })
        .when(!chosen, |tile| {
            tile.hover(|style| style.border_color(rgb(palette.border)))
        })
        .cursor_pointer()
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(on_click)
        .child(face)
        .into_any_element()
}

impl Render for EffectsPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let preview = self.preview(palette, cx);
        let tiles = self.tiles(palette, cx);
        div()
            .debug_selector(|| "effects-picker".to_owned())
            .flex()
            .flex_col()
            .gap_3()
            .child(preview)
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child("Background effects"),
            )
            .child(
                div()
                    .debug_selector(|| "effects-tiles".to_owned())
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .w(relative(1.0))
                    .children(tiles),
            )
            .children(
                self.problem
                    .clone()
                    .map(|problem| div().text_xs().text_color(rgb(theme::DANGER)).child(problem)),
            )
    }
}

/// Redraws the picker whenever `feed` brings a frame.
fn wake_on(feed: &VideoFeed, cx: &mut Context<EffectsPicker>) -> Task<()> {
    let woken = feed.woken();
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        while woken.recv().await.is_ok() {
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
