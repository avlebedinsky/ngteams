//! Captions over a meeting recording, as Teams' Recap player draws them from the recording's transcript: the words of
//! the cue spoken where it plays, centred over the bottom of the picture, in the style its caption settings chose
//! (size, colour, background). Whether they show and how they look is kept for the run ([`CaptionStyle`], a global):
//! a recap's other part, or another recap, shows them as last chosen, as Teams' player keeps them.

use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Global, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Styled as _, div, hsla,
    px, relative,
};
use teams_proto::transcript::Cue;

/// One caption: what is said and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caption {
    pub start: Duration,
    pub end: Duration,
    pub text: String,
}

/// A recording's captions: its transcript's cues, the earliest first, and their language (a BCP 47 tag) when known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Captions {
    cues: Arc<[Caption]>,
    language: Option<String>,
}

impl Captions {
    /// The captions of a transcript's `cues`, spoken in `language`.
    #[must_use]
    pub fn new(cues: &[Cue], language: Option<String>) -> Self {
        let mut captions: Vec<Caption> = cues
            .iter()
            .map(|cue| Caption {
                start: cue.start,
                end: cue.end,
                text: cue.text.clone(),
            })
            .collect();
        captions.sort_by_key(|caption| caption.start);
        Self {
            cues: captions.into(),
            language,
        }
    }

    /// The words spoken at `at`: the latest cue begun by then that hasn't ended.
    #[must_use]
    pub fn at(&self, at: Duration) -> Option<&str> {
        let begun = self.cues.partition_point(|caption| caption.start <= at);
        let caption = self.cues.get(begun.checked_sub(1)?)?;
        (at < caption.end).then_some(caption.text.as_str())
    }

    /// What the captions menu calls them: their language's name ("English (United States)"), its tag for a language
    /// the app doesn't name, "On" when it isn't known.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.language {
            Some(tag) => language_name(tag).map_or_else(|| tag.clone(), str::to_owned),
            None => "On".to_owned(),
        }
    }
}

/// The names of the languages Teams transcribes, by their tags, as its player's captions menu writes them.
const LANGUAGES: &[(&str, &str)] = &[
    ("ar-AE", "Arabic (United Arab Emirates)"),
    ("ar-SA", "Arabic (Saudi Arabia)"),
    ("bg-BG", "Bulgarian (Bulgaria)"),
    ("ca-ES", "Catalan (Spain)"),
    ("cs-CZ", "Czech (Czechia)"),
    ("cy-GB", "Welsh (United Kingdom)"),
    ("da-DK", "Danish (Denmark)"),
    ("de-CH", "German (Switzerland)"),
    ("de-DE", "German (Germany)"),
    ("el-GR", "Greek (Greece)"),
    ("en-AU", "English (Australia)"),
    ("en-CA", "English (Canada)"),
    ("en-GB", "English (United Kingdom)"),
    ("en-IN", "English (India)"),
    ("en-NZ", "English (New Zealand)"),
    ("en-US", "English (United States)"),
    ("es-ES", "Spanish (Spain)"),
    ("es-MX", "Spanish (Mexico)"),
    ("et-EE", "Estonian (Estonia)"),
    ("fi-FI", "Finnish (Finland)"),
    ("fr-CA", "French (Canada)"),
    ("fr-FR", "French (France)"),
    ("he-IL", "Hebrew (Israel)"),
    ("hi-IN", "Hindi (India)"),
    ("hr-HR", "Croatian (Croatia)"),
    ("hu-HU", "Hungarian (Hungary)"),
    ("it-IT", "Italian (Italy)"),
    ("ja-JP", "Japanese (Japan)"),
    ("ko-KR", "Korean (Korea)"),
    ("lt-LT", "Lithuanian (Lithuania)"),
    ("lv-LV", "Latvian (Latvia)"),
    ("nb-NO", "Norwegian Bokmål (Norway)"),
    ("nl-BE", "Dutch (Belgium)"),
    ("nl-NL", "Dutch (Netherlands)"),
    ("pl-PL", "Polish (Poland)"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("pt-PT", "Portuguese (Portugal)"),
    ("ro-RO", "Romanian (Romania)"),
    ("ru-RU", "Russian (Russia)"),
    ("sk-SK", "Slovak (Slovakia)"),
    ("sv-SE", "Swedish (Sweden)"),
    ("tr-TR", "Turkish (Türkiye)"),
    ("uk-UA", "Ukrainian (Ukraine)"),
    ("zh-CN", "Chinese (Simplified, China)"),
];

/// The name of the language `tag` names (`en-US`, any case), among those Teams transcribes.
#[must_use]
pub fn language_name(tag: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(tag))
        .map(|(_, name)| *name)
}

/// How big captions are drawn, as Teams' caption settings offer it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptionSize {
    #[default]
    Small,
    Medium,
    Large,
}

impl CaptionSize {
    pub const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
        }
    }

    /// Its test selector's key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
        }
    }

    /// The words' size, in pixels.
    fn text(self) -> f32 {
        match self {
            Self::Small => 14.0,
            Self::Medium => 20.0,
            Self::Large => 28.0,
        }
    }
}

/// The colours captions are drawn in, as Teams' caption settings offer them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptionColor {
    /// White on black.
    #[default]
    Standard,
    /// Black on white.
    StandardReverse,
    /// White on a dark purple.
    Purple,
    /// A dark purple on white.
    PurpleReverse,
}

/// The dark purple of Teams' purple captions.
const CAPTION_PURPLE: u32 = 0x0032_0022;

impl CaptionColor {
    pub const ALL: [Self; 4] = [Self::Standard, Self::StandardReverse, Self::Purple, Self::PurpleReverse];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::StandardReverse => "Standard reverse",
            Self::Purple => "Purple",
            Self::PurpleReverse => "Purple reverse",
        }
    }

    /// Its test selector's key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::StandardReverse => "standard-reverse",
            Self::Purple => "purple",
            Self::PurpleReverse => "purple-reverse",
        }
    }

    /// The words' colour and their background's.
    #[must_use]
    pub fn colors(self) -> (Hsla, Hsla) {
        let (white, black, purple) = (
            gpui::rgb(0x00ff_ffff).into(),
            gpui::rgb(0x0000_0000).into(),
            gpui::rgb(CAPTION_PURPLE).into(),
        );
        match self {
            Self::Standard => (white, black),
            Self::StandardReverse => (black, white),
            Self::Purple => (white, purple),
            Self::PurpleReverse => (purple, white),
        }
    }
}

/// Whether captions show, and how they look: kept for the run, for every player (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionStyle {
    pub on: bool,
    pub size: CaptionSize,
    pub color: CaptionColor,
    /// The words are drawn without their background.
    pub transparent: bool,
}

impl Global for CaptionStyle {}

impl CaptionStyle {
    /// The style as last chosen this run.
    #[must_use]
    pub fn current(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }

    /// Changes the style every player draws by.
    pub fn change(cx: &mut App, change: impl FnOnce(&mut Self)) {
        let mut style = Self::current(cx);
        change(&mut style);
        cx.set_global(style);
    }

    /// Back to Teams' defaults (small, standard, with a background), captions still on or off.
    pub fn revert(&mut self) {
        *self = Self {
            on: self.on,
            ..Self::default()
        };
    }
}

/// A caption's words over the bottom of the picture (above the controls), centred, in `style`.
pub(super) fn caption(card: &str, text: &str, style: CaptionStyle) -> AnyElement {
    let (words, background) = style.color.colors();
    let card = card.to_owned();
    div()
        .debug_selector(move || format!("video-caption-{card}"))
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(60.0))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w(relative(0.8))
                .px_1()
                .text_center()
                .text_size(px(style.size.text()))
                .line_height(relative(1.35))
                .text_color(words)
                .when_some((!style.transparent).then_some(background), |text, background| {
                    text.bg(background)
                })
                .child(text.to_owned()),
        )
        .into_any_element()
}

/// The swatch a colour's row shows: a disc of its background holding a dot of its words' colour.
pub(super) fn swatch(color: CaptionColor) -> AnyElement {
    let (words, background) = color.colors();
    div()
        .size(px(18.0))
        .rounded_full()
        .border_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.8))
        .bg(background)
        .flex()
        .items_center()
        .justify_center()
        .child(div().size(px(8.0)).rounded_full().bg(words))
        .into_any_element()
}
