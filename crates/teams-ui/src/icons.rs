//! The app's icons (Lucide's, ISC: `assets/icons/LICENSE-LUCIDE`), compiled in as SVG, and the icon-only button the
//! toolbars use: one icon on a quiet square, named by its tooltip (with its key, when it has one).

use gpui::{
    Action, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};
use gpui_component::Icon;
use gpui_component::button::{Button, ButtonVariants as _};

/// One of the app's icons.
#[derive(Clone, Copy)]
pub struct Glyph(&'static [u8]);

impl Glyph {
    pub const ARROW_DOWN: Self = Self(include_bytes!("../assets/icons/arrow-down.svg"));
    pub const BELL: Self = Self(include_bytes!("../assets/icons/bell.svg"));
    pub const BOT: Self = Self(include_bytes!("../assets/icons/bot.svg"));
    pub const BUG: Self = Self(include_bytes!("../assets/icons/bug.svg"));
    pub const CALENDAR: Self = Self(include_bytes!("../assets/icons/calendar.svg"));
    pub const CHECK: Self = Self(include_bytes!("../assets/icons/check.svg"));
    pub const CHEVRON_DOWN: Self = Self(include_bytes!("../assets/icons/chevron-down.svg"));
    pub const CLOCK: Self = Self(include_bytes!("../assets/icons/clock.svg"));
    pub const CODE: Self = Self(include_bytes!("../assets/icons/square-code.svg"));
    pub const COPY: Self = Self(include_bytes!("../assets/icons/copy.svg"));
    pub const DOWNLOAD: Self = Self(include_bytes!("../assets/icons/download.svg"));
    pub const EMOJI: Self = Self(include_bytes!("../assets/icons/face-slightly-smiling.svg"));
    pub const EXTERNAL_LINK: Self = Self(include_bytes!("../assets/icons/external-link.svg"));
    pub const FULLSCREEN: Self = Self(include_bytes!("../assets/icons/scan.svg"));
    pub const FILE: Self = Self(include_bytes!("../assets/icons/file.svg"));
    pub const FILE_ARCHIVE: Self = Self(include_bytes!("../assets/icons/file-archive.svg"));
    pub const FILE_AUDIO: Self = Self(include_bytes!("../assets/icons/file-headphone.svg"));
    pub const FILE_CHART: Self = Self(include_bytes!("../assets/icons/file-chart-column.svg"));
    pub const FILE_IMAGE: Self = Self(include_bytes!("../assets/icons/file-image.svg"));
    pub const FILE_SPREADSHEET: Self = Self(include_bytes!("../assets/icons/file-spreadsheet.svg"));
    pub const FILE_VIDEO: Self = Self(include_bytes!("../assets/icons/file-play.svg"));
    pub const FORWARD: Self = Self(include_bytes!("../assets/icons/forward.svg"));
    pub const HEADPHONES: Self = Self(include_bytes!("../assets/icons/headphones.svg"));
    pub const INFO: Self = Self(include_bytes!("../assets/icons/info.svg"));
    pub const LINK: Self = Self(include_bytes!("../assets/icons/link.svg"));
    pub const LOADING: Self = Self(include_bytes!("../assets/icons/loader-circle.svg"));
    pub const LOG_OUT: Self = Self(include_bytes!("../assets/icons/log-out.svg"));
    pub const MESSAGE: Self = Self(include_bytes!("../assets/icons/message-square-text.svg"));
    pub const MIC: Self = Self(include_bytes!("../assets/icons/mic.svg"));
    pub const MIC_OFF: Self = Self(include_bytes!("../assets/icons/mic-off.svg"));
    pub const MINUS: Self = Self(include_bytes!("../assets/icons/minus.svg"));
    pub const MORE: Self = Self(include_bytes!("../assets/icons/ellipsis.svg"));
    pub const PALETTE: Self = Self(include_bytes!("../assets/icons/palette.svg"));
    pub const PAUSE: Self = Self(include_bytes!("../assets/icons/pause.svg"));
    pub const PENCIL: Self = Self(include_bytes!("../assets/icons/pencil.svg"));
    pub const PHONE: Self = Self(include_bytes!("../assets/icons/phone.svg"));
    pub const PHONE_OFF: Self = Self(include_bytes!("../assets/icons/phone-off.svg"));
    pub const PLAY: Self = Self(include_bytes!("../assets/icons/circle-play.svg"));
    pub const PLAY_MEDIA: Self = Self(include_bytes!("../assets/icons/play.svg"));
    pub const PLUS: Self = Self(include_bytes!("../assets/icons/plus.svg"));
    pub const POP_OUT: Self = Self(include_bytes!("../assets/icons/square-arrow-out-up-right.svg"));
    pub const QUOTE: Self = Self(include_bytes!("../assets/icons/quote.svg"));
    pub const REACT: Self = Self(include_bytes!("../assets/icons/smile-plus.svg"));
    pub const REFRESH: Self = Self(include_bytes!("../assets/icons/refresh-cw.svg"));
    pub const REPLY: Self = Self(include_bytes!("../assets/icons/reply.svg"));
    pub const RESET: Self = Self(include_bytes!("../assets/icons/rotate-ccw.svg"));
    pub const SEND: Self = Self(include_bytes!("../assets/icons/send-horizontal.svg"));
    pub const SHARE: Self = Self(include_bytes!("../assets/icons/share.svg"));
    pub const SHIELD: Self = Self(include_bytes!("../assets/icons/shield.svg"));
    pub const SHRINK: Self = Self(include_bytes!("../assets/icons/shrink.svg"));
    pub const SKIP_BACK: Self = Self(include_bytes!("../assets/icons/rotate-ccw.svg"));
    pub const SKIP_FORWARD: Self = Self(include_bytes!("../assets/icons/rotate-cw.svg"));
    pub const TRANSCRIPT: Self = Self(include_bytes!("../assets/icons/file-text.svg"));
    pub const TRASH: Self = Self(include_bytes!("../assets/icons/trash.svg"));
    pub const USER: Self = Self(include_bytes!("../assets/icons/user-round.svg"));
    /// Everyone in a chat (the mention picker's Everyone).
    pub const USERS: Self = Self(include_bytes!("../assets/icons/users-round.svg"));
    pub const VIDEO: Self = Self(include_bytes!("../assets/icons/video.svg"));
    pub const VIDEO_OFF: Self = Self(include_bytes!("../assets/icons/video-off.svg"));
    pub const VOLUME: Self = Self(include_bytes!("../assets/icons/volume-2.svg"));
    pub const VOLUME_OFF: Self = Self(include_bytes!("../assets/icons/volume-x.svg"));
    pub const X: Self = Self(include_bytes!("../assets/icons/x.svg"));

    /// The icon, in the text colour around it.
    #[must_use]
    pub fn icon(self) -> Icon {
        Icon::default().data(self.0)
    }
}

/// An icon-only button: `glyph` on a ghost square, `tip` its tooltip (and what it is called). The caller adds its
/// click, its variant when it isn't a quiet one, and its size.
#[must_use]
pub fn icon_button(id: impl Into<ElementId>, glyph: Glyph, tip: impl Into<SharedString>) -> Button {
    let tip = tip.into();
    Button::new(id)
        .icon(glyph.icon())
        .ghost()
        .accessibility_label(tip.clone())
        .tooltip(tip)
}

/// [`icon_button`] whose tooltip also shows the key bound to `action`.
#[must_use]
pub fn icon_button_with_key(
    id: impl Into<ElementId>,
    glyph: Glyph,
    tip: impl Into<SharedString>,
    action: &dyn Action,
) -> Button {
    let tip = tip.into();
    Button::new(id)
        .icon(glyph.icon())
        .ghost()
        .accessibility_label(tip.clone())
        .tooltip_with_action(tip, action, None)
}

/// `element` with `selector` as its test selector: the kit's buttons take none of their own.
pub fn selected_by(selector: impl Into<String>, element: impl IntoElement) -> gpui::Div {
    let selector = selector.into();
    div()
        .debug_selector(move || selector.clone())
        .flex_none()
        .child(element)
}
