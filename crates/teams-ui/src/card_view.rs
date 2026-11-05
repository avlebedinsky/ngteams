//! A card a bot or a workflow posted (`RichText/Media_Card`, read by `teams_proto::chat::card`), drawn natively as
//! Teams' web client draws it in a chat and in a channel.
//!
//! - **The card** is its own surface: [`CARD_WIDTH`] wide at most, padded, bordered and rounded, its body's elements
//!   one under another with the spacing Teams' host config gives each (`spacing`, a line for `separator`), its buttons
//!   under them. Several cards of one message stand one under another.
//! - **Text** is the card's markdown ([`card::markdown_html`]) read as any message's HTML is (`parse_html`, then
//!   [`rich_text::layout`]), its date and time functions written in the local time zone on the app's clock, in the
//!   look its size, weight, colour and `isSubtle` give it; one that doesn't wrap is one line, cut.
//! - **Pictures** are asked of the engine by the async gateway's thumbnail of their link
//!   (`teams_proto::LinkPreviewPicture::card_picture_src`), as a link preview's are: the reader's client never reaches
//!   the picture's site. While one is read its place is held; one there is none of is left out.
//! - **Containers, columns, facts, tables and action sets** are laid out as Teams lays them out: a styled container on
//!   its style's surface, padded; columns side by side by their widths; facts as a column of titles beside a column of
//!   values.
//! - **Actions**: `Action.OpenUrl` opens its link (GPUI's `open_url`); `Action.ShowCard` shows its card under the
//!   buttons, and hides it again; `Action.ToggleVisibility` shows, hides or flips the elements it names. Both change
//!   only what this view shows ([`CardState`], held by the view per message). `Action.Submit`/`Execute` send to the
//!   bot through Teams' bot path, which hands the account's token to the app's backend and is never sent natively
//!   (`AGENTS.md`'s hard rules): such a button opens the message in Teams on the web, where it runs, and says so in
//!   its tooltip. A `selectAction` does what its action does on a click anywhere on its element.
//! - **What it can't show** (an input, an element or action of a type no capture showed, an attachment that isn't a
//!   card) is never guessed at: the card says the message has content that can't be shown here
//!   (`chat_view::UNSHOWN_CONTENT`), under what it does show.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use chrono::{DateTime, Local};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, FontWeight, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, StyledImage as _, Window, div, img, px, relative, rgb,
};
use gpui_component::button::Button;
use gpui_component::{Selectable as _, Sizable as _};
use teams_proto::LinkPreviewPicture;
use teams_proto::chat::card::{
    self, Action, ActionKind, Card, CardMessage, Column, ColumnSet, Container, ContainerStyle, DateFormat, Element,
    Frame, HAlign, Image, ImageSize, Kind, RichTextBlock, Spacing, Table, TextBlock, TextColor, TextLook, TextPart,
    TextSize, TextWeight, VAlign, Width,
};
use teams_proto::chat::parse_html;

use crate::clock::HourCycle;
use crate::icons::Glyph;
use crate::inline::{PictureSource, PictureState, picture};
use crate::rich_text::{self, Mentions, Rendered, tint};
use crate::theme::{self, Palette};

/// The widest a card is drawn, px: Teams' card in a chat and in a channel.
pub const CARD_WIDTH: f32 = 448.0;
/// A card's padding, px (Teams' host config `spacing.padding`).
pub const PADDING: f32 = 16.0;
/// What a card's preview line in the chat list says, as Teams' does.
pub const SENT_A_CARD: &str = "Sent a card";
/// The tooltip of a button that sends to the bot: it opens the message in Teams on the web.
pub const RUNS_IN_TEAMS: &str = "Opens in Teams on the web, where this button runs";
/// The warning colour of a card's text and surfaces (Teams' host config `warning`).
const WARNING: u32 = 0x00c1_9c00;
/// How many laid-out texts are kept between frames before the cache starts over.
const TEXTS_KEPT: usize = 2048;

/// A message's cards, as a row holds them: compared by their content.
#[derive(Clone, Debug)]
pub struct ShownCards(pub Arc<CardMessage>);

impl PartialEq for ShownCards {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || *self.0 == *other.0
    }
}

// a card's numbers are finite (`teams_proto::chat::card` keeps no NaN), so its equality is an equivalence
impl Eq for ShownCards {}

impl ShownCards {
    /// The cards of a card message's `content`; `None` when it carries none this build draws (only attachments of
    /// another kind: the message is drawn as one the app doesn't show, with Teams on the web to see it).
    #[must_use]
    pub fn of(content: &str) -> Option<Self> {
        card::card_message(content)
            .filter(|message| !message.cards.is_empty())
            .map(|message| Self(Arc::new(message)))
    }

    /// It has a part this build can't show.
    #[must_use]
    pub fn unshown(&self) -> bool {
        self.0.unshown || self.0.cards.iter().any(|card| card.unshown)
    }
}

/// What a message's cards show besides what they say: the elements a toggle showed or hid, and the cards a
/// `ShowCard` opened. Held by the view per message; it changes nothing outside the view.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardState {
    visibility: HashMap<String, bool>,
    open: HashSet<String>,
}

impl CardState {
    /// Whether the element `id` (visible to begin with when `visible`) is shown.
    #[must_use]
    pub fn shown(&self, id: Option<&str>, visible: bool) -> bool {
        id.and_then(|id| self.visibility.get(id)).copied().unwrap_or(visible)
    }

    /// Whether the card of the `ShowCard` at `key` is open.
    #[must_use]
    pub fn is_open(&self, key: &str) -> bool {
        self.open.contains(key)
    }

    /// The `ShowCard` at `key` opened, or closed when it was open; the other cards of its set closed.
    #[must_use]
    pub fn flip_card(&self, key: &str, set: &[String]) -> Self {
        let mut next = self.clone();
        let was_open = next.open.contains(key);
        for other in set {
            next.open.remove(other);
        }
        if !was_open {
            next.open.insert(key.to_owned());
        }
        next
    }

    /// The elements `targets` name shown, hidden or flipped (`None`); `base` is each element's own visibility.
    #[must_use]
    pub fn toggle(&self, targets: &[(String, Option<bool>)], base: &HashMap<String, bool>) -> Self {
        let mut next = self.clone();
        for (id, to) in targets {
            let now = next.shown(Some(id), base.get(id).copied().unwrap_or(true));
            next.visibility.insert(id.clone(), to.unwrap_or(!now));
        }
        next
    }
}

/// What a click on a button or an element does.
type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// What a card tells its view: its state as a click left it.
pub type OnState = Rc<dyn Fn(CardState, &mut Window, &mut App)>;

/// A message's cards, drawn (see the module docs). `id` keeps its elements apart from another message's.
#[derive(IntoElement)]
pub struct CardsElement {
    id: SharedString,
    cards: ShownCards,
    state: CardState,
    palette: Palette,
    cycle: HourCycle,
    source: Option<PictureSource>,
    on_state: Option<OnState>,
    in_teams: Option<String>,
}

impl CardsElement {
    #[must_use]
    pub fn new(id: impl Into<SharedString>, cards: ShownCards, palette: Palette, cycle: HourCycle) -> Self {
        Self {
            id: id.into(),
            cards,
            state: CardState::default(),
            palette,
            cycle,
            source: None,
            on_state: None,
            in_teams: None,
        }
    }

    /// What its toggles and shown cards stand at, and who is told when a click changes it.
    #[must_use]
    pub fn state(mut self, state: CardState, on_state: OnState) -> Self {
        self.state = state;
        self.on_state = Some(on_state);
        self
    }

    /// Where its pictures are asked for.
    #[must_use]
    pub fn source(mut self, source: Option<PictureSource>) -> Self {
        self.source = source;
        self
    }

    /// The clock its times are written on.
    #[must_use]
    pub fn clock(mut self, cycle: HourCycle) -> Self {
        self.cycle = cycle;
        self
    }

    /// The message in Teams on the web: where a button that sends to the bot opens.
    #[must_use]
    pub fn in_teams(mut self, link: Option<String>) -> Self {
        self.in_teams = link;
        self
    }
}

impl RenderOnce for CardsElement {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut base = HashMap::new();
        for card in &self.cards.0.cards {
            card_visibility(card, &mut base);
        }
        let draw = Draw {
            id: &self.id,
            palette: self.palette,
            cycle: self.cycle,
            source: self.source.as_ref(),
            state: &self.state,
            on_state: self.on_state.as_ref(),
            in_teams: self.in_teams.as_deref(),
            base: &base,
        };
        let id = self.id.clone();
        // an attachment beside the cards that isn't one (`CardMessage::unshown`) is said under them
        let unshown = self.cards.0.unshown.then(|| {
            let selector = draw.selector("unshown");
            div().debug_selector(move || selector).child(unshown_line(draw.palette))
        });
        div()
            .debug_selector(move || format!("cards-{id}"))
            .flex()
            .flex_col()
            .gap_2()
            .children(
                self.cards
                    .0
                    .cards
                    .iter()
                    .enumerate()
                    .map(|(at, shown)| card_surface(&draw, &format!("c{at}"), shown, cx)),
            )
            .children(unshown)
    }
}

/// What every element of one message's cards is drawn with.
struct Draw<'a> {
    id: &'a str,
    palette: Palette,
    cycle: HourCycle,
    source: Option<&'a PictureSource>,
    state: &'a CardState,
    on_state: Option<&'a OnState>,
    in_teams: Option<&'a str>,
    base: &'a HashMap<String, bool>,
}

impl Draw<'_> {
    fn selector(&self, path: &str) -> String {
        format!("card-{}-{path}", self.id)
    }
}

/// The ids of a card's elements, those of the cards its `ShowCard`s open among them, and whether each is visible to
/// begin with.
fn card_visibility(card: &Card, base: &mut HashMap<String, bool>) {
    base_visibility(&card.body, base);
    shown_cards_visibility(&card.actions, base);
}

/// The ids in the cards the `ShowCard`s among `actions` open.
fn shown_cards_visibility(actions: &[Action], base: &mut HashMap<String, bool>) {
    for action in actions {
        if let ActionKind::ShowCard(shown) = &action.kind {
            card_visibility(shown, base);
        }
    }
}

/// The ids of `elements` (and of those inside them) and whether each is visible to begin with.
fn base_visibility(elements: &[Element], base: &mut HashMap<String, bool>) {
    for element in elements {
        if let Some(id) = &element.id {
            base.insert(id.clone(), element.visible);
        }
        match &element.kind {
            Kind::Container(container) => base_visibility(&container.items, base),
            Kind::Columns(set) => {
                for column in &set.columns {
                    if let Some(id) = &column.id {
                        base.insert(id.clone(), column.visible);
                    }
                    base_visibility(&column.items, base);
                }
            }
            Kind::Table(table) => {
                for cell in table.rows.iter().flatten() {
                    base_visibility(cell, base);
                }
            }
            Kind::Actions(actions) => shown_cards_visibility(actions, base),
            _ => {}
        }
    }
}

/// One card on its own surface: bordered, rounded, padded, [`CARD_WIDTH`] wide at most.
fn card_surface(draw: &Draw<'_>, path: &str, shown: &Card, cx: &mut App) -> AnyElement {
    let palette = draw.palette;
    let selector = draw.selector(path);
    div()
        .debug_selector(move || selector)
        .w_full()
        .max_w(px(CARD_WIDTH))
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .shadow_xs()
        .p(px(PADDING))
        .child(card_body(draw, path, shown, cx))
        .into_any_element()
}

/// A card's body, its buttons and the card a `ShowCard` of its opened, without its surface (a shown card is drawn
/// inside its parent's).
fn card_body(draw: &Draw<'_>, path: &str, shown: &Card, cx: &mut App) -> AnyElement {
    let elements = elements(draw, &format!("{path}b"), &shown.body, cx);
    let actions_path = format!("{path}a");
    let actions = (!shown.actions.is_empty()).then(|| action_set(draw, &actions_path, &shown.actions));
    let opened = opened_card(draw, &actions_path, &shown.actions, cx);
    let unshown = shown.unshown.then(|| {
        let selector = draw.selector(&format!("{path}unshown"));
        div().debug_selector(move || selector).child(unshown_line(draw.palette))
    });
    let body = div()
        .flex()
        .flex_col()
        .children(elements)
        .children(actions.map(|actions| div().pt(px(spacing_px(Spacing::Default))).child(actions)))
        .children(opened)
        .children(unshown);
    selectable(draw, &format!("{path}s"), shown.select_action.as_deref(), body)
}

/// The note under what a card shows when it carries what it can't show.
fn unshown_line(palette: Palette) -> AnyElement {
    div()
        .pt_2()
        .text_xs()
        .italic()
        .text_color(rgb(palette.muted_foreground))
        .child(crate::chat_view::UNSHOWN_CONTENT)
        .into_any_element()
}

/// The visible ones of `items`, each after its spacing (and its line, for a separator); the first has no space above.
fn elements(draw: &Draw<'_>, path: &str, items: &[Element], cx: &mut App) -> Vec<AnyElement> {
    let mut drawn = Vec::new();
    for (at, element) in items.iter().enumerate() {
        if !draw.state.shown(element.id.as_deref(), element.visible) {
            continue;
        }
        if !drawn.is_empty() {
            drawn.push(gap(element.spacing, element.separator, draw.palette));
        }
        drawn.push(element_view(draw, &format!("{path}{at}."), element, cx));
    }
    drawn
}

/// The space above an element: its spacing, a line across its middle for a separator.
fn gap(spacing: Spacing, separator: bool, palette: Palette) -> AnyElement {
    let space = spacing_px(spacing);
    if separator {
        let half = (space / 2.0).max(4.0);
        div()
            .w_full()
            .pt(px(half))
            .mb(px(half))
            .border_b_1()
            .border_color(rgb(palette.border))
            .into_any_element()
    } else {
        div().h(px(space)).into_any_element()
    }
}

/// A spacing's height, px (Teams' host config).
#[must_use]
pub fn spacing_px(spacing: Spacing) -> f32 {
    match spacing {
        Spacing::None => 0.0,
        Spacing::Small => 4.0,
        Spacing::Default => 12.0,
        Spacing::Medium => 16.0,
        Spacing::Large => 20.0,
        Spacing::ExtraLarge => 24.0,
        Spacing::Padding => PADDING,
    }
}

fn element_view(draw: &Draw<'_>, path: &str, element: &Element, cx: &mut App) -> AnyElement {
    match &element.kind {
        Kind::Text(text) => text_view(draw, path, text),
        Kind::RichText(rich) => rich_text_view(draw, path, rich),
        Kind::Image(image) => image_view(draw, path, image, cx),
        Kind::Facts(facts) => facts_view(draw, path, facts),
        // the others are found by their place, as the elements above are by their own
        Kind::ImageSet { images, size } => placed(draw, path, image_set(draw, path, images, *size, cx)),
        Kind::Container(container) => placed(draw, path, container_view(draw, path, container, cx)),
        Kind::Columns(set) => placed(draw, path, columns_view(draw, path, set, cx)),
        Kind::Actions(actions) => {
            let opened = opened_card(draw, path, actions, cx);
            let set = div()
                .flex()
                .flex_col()
                .child(action_set(draw, path, actions))
                .children(opened);
            placed(draw, path, set.into_any_element())
        }
        Kind::Table(table) => placed(draw, path, table_view(draw, path, table, cx)),
    }
}

/// `element` in a div the element's place names (`card-<message>-<path>`), for a test to find.
fn placed(draw: &Draw<'_>, path: &str, element: AnyElement) -> AnyElement {
    let selector = draw.selector(path);
    div()
        .debug_selector(move || selector)
        .w_full()
        .child(element)
        .into_any_element()
}

/// A text's size and line height, px (Teams' host config).
#[must_use]
pub fn text_px(size: TextSize) -> (f32, f32) {
    match size {
        TextSize::Small => (12.0, 16.0),
        TextSize::Default => (14.0, 20.0),
        TextSize::Medium => (16.0, 22.0),
        TextSize::Large => (18.0, 24.0),
        TextSize::ExtraLarge => (24.0, 32.0),
    }
}

/// A text's colour on the default surface.
fn text_colour(look: TextLook, palette: Palette) -> u32 {
    let colour = match look.color {
        TextColor::Default | TextColor::Dark => palette.foreground,
        TextColor::Light => palette.muted_foreground,
        TextColor::Accent => palette.brand,
        TextColor::Good => theme::SUCCESS,
        TextColor::Warning => WARNING,
        TextColor::Attention => theme::DANGER,
    };
    if look.subtle && matches!(look.color, TextColor::Default | TextColor::Dark) {
        palette.muted_foreground
    } else {
        colour
    }
}

fn weight_of(weight: TextWeight) -> FontWeight {
    match weight {
        TextWeight::Lighter => FontWeight::LIGHT,
        TextWeight::Default => FontWeight::NORMAL,
        TextWeight::Bolder => FontWeight::SEMIBOLD,
    }
}

/// A div in a text's look and alignment.
fn styled(look: TextLook, align: HAlign, palette: Palette) -> gpui::Div {
    let (size, line) = text_px(look.size);
    div()
        .w_full()
        .text_size(px(size))
        .line_height(px(line))
        .font_weight(weight_of(look.weight))
        .text_color(rgb(text_colour(look, palette)))
        .when(look.monospace, |div| div.font_family("monospace"))
        .map(|div| match align {
            HAlign::Left => div,
            HAlign::Center => div.text_center(),
            HAlign::Right => div.text_right(),
        })
}

fn text_view(draw: &Draw<'_>, path: &str, text: &TextBlock) -> AnyElement {
    let look = if text.heading && text.look.weight == TextWeight::Default {
        TextLook {
            weight: TextWeight::Bolder,
            ..text.look
        }
    } else {
        text.look
    };
    let rendered = laid_out(&text.text, draw.cycle);
    let id = SharedString::from(draw.selector(path));
    let selector = draw.selector(path);
    styled(look, text.align, draw.palette)
        .debug_selector(move || selector)
        .map(|div| match (text.wrap, text.max_lines) {
            (_, Some(lines)) if text.wrap => div.line_clamp(usize::try_from(lines).unwrap_or(1)),
            (true, _) => div,
            (false, _) => div.line_clamp(1).overflow_hidden(),
        })
        .child(rich_text::render(&id, &rendered, draw.palette))
        .into_any_element()
}

/// A rich text block: its runs as one paragraph, each in its marks (bold for a bolder run, a link for one that opens),
/// in the look of its first run.
fn rich_text_view(draw: &Draw<'_>, path: &str, rich: &RichTextBlock) -> AnyElement {
    let mut html = String::from("<p>");
    for run in &rich.runs {
        let mut open = Vec::new();
        if let Some(Action {
            kind: ActionKind::OpenUrl(url),
            ..
        }) = run.select_action.as_deref()
        {
            html.push_str("<a href=\"");
            html.push_str(&escape(url));
            html.push_str("\">");
            open.push("a");
        }
        for (on, tag) in [
            (run.look.weight == TextWeight::Bolder, "b"),
            (run.marks.italic, "i"),
            (run.marks.strike, "s"),
            (run.marks.underline, "u"),
        ] {
            if on {
                html.push('<');
                html.push_str(tag);
                html.push('>');
                open.push(tag);
            }
        }
        html.push_str(&escape(&expanded(&run.text, draw.cycle)));
        for tag in open.iter().rev() {
            html.push_str("</");
            html.push_str(tag);
            html.push('>');
        }
    }
    html.push_str("</p>");
    let rendered = Arc::new(rich_text::layout(&parse_html(&html), &Mentions::default()));
    let look = rich.runs.first().map(|run| run.look).unwrap_or_default();
    let id = SharedString::from(draw.selector(path));
    let selector = draw.selector(path);
    styled(look, rich.align, draw.palette)
        .debug_selector(move || selector)
        .child(rich_text::render(&id, &rendered, draw.palette))
        .into_any_element()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

thread_local! {
    /// The texts laid out, by their text and clock: a card is drawn every frame.
    static TEXTS: RefCell<HashMap<(String, bool), Arc<Rendered>>> = RefCell::new(HashMap::new());
}

/// A card text laid out: its functions written, its markdown read (see the module docs).
fn laid_out(text: &str, cycle: HourCycle) -> Arc<Rendered> {
    let key = (text.to_owned(), cycle == HourCycle::Twelve);
    if let Some(rendered) = TEXTS.with(|texts| texts.borrow().get(&key).cloned()) {
        return rendered;
    }
    let html = card::markdown_html(&expanded(text, cycle));
    let rendered = Arc::new(rich_text::layout(&parse_html(&html), &Mentions::default()));
    TEXTS.with(|texts| {
        let mut texts = texts.borrow_mut();
        if texts.len() >= TEXTS_KEPT {
            texts.clear();
        }
        texts.insert(key, Arc::clone(&rendered));
    });
    rendered
}

/// A text with its date and time functions written in the local time zone on `cycle`'s clock.
#[must_use]
pub fn expanded(text: &str, cycle: HourCycle) -> String {
    card::text_parts(text)
        .into_iter()
        .map(|part| match part {
            TextPart::Text(text) => text.to_owned(),
            TextPart::Date { at_ms, format } => local(at_ms).map_or_else(String::new, |at| {
                at.format(match format {
                    DateFormat::Compact => "%-m/%-d/%Y",
                    DateFormat::Short => "%a, %b %-d, %Y",
                    DateFormat::Long => "%A, %B %-d, %Y",
                })
                .to_string()
            }),
            TextPart::Time { at_ms } => {
                local(at_ms).map_or_else(String::new, |at| at.format(cycle.time_of_day()).to_string())
            }
        })
        .collect()
}

fn local(at_ms: i64) -> Option<DateTime<Local>> {
    DateTime::from_timestamp_millis(at_ms).map(|at| at.with_timezone(&Local))
}

/// A picture's side for a size, px (Teams' host config `imageSizes`); `None` for its own size or the width there is.
#[must_use]
pub fn image_px(size: ImageSize) -> Option<f32> {
    match size {
        ImageSize::Small => Some(32.0),
        ImageSize::Medium => Some(52.0),
        ImageSize::Large => Some(100.0),
        ImageSize::Auto | ImageSize::Stretch => None,
    }
}

fn image_view(draw: &Draw<'_>, path: &str, image: &Image, cx: &mut App) -> AnyElement {
    let drawn = picture_of(draw, path, image, image.size, cx);
    let row = div().w_full().flex().map(|div| match image.align {
        HAlign::Left => div.justify_start(),
        HAlign::Center => div.justify_center(),
        HAlign::Right => div.justify_end(),
    });
    let row = row.child(drawn);
    selectable(draw, &format!("{path}s"), image.select_action.as_deref(), row)
}

/// A picture at `size` (its own pixel width or height over it), while it is read a tinted box of its size.
fn picture_of(draw: &Draw<'_>, path: &str, image: &Image, size: ImageSize, cx: &mut App) -> AnyElement {
    let state = card_picture(&image.url, draw.source, cx);
    let sized = image_px(size);
    let (width, height) = match (image.width, image.height, sized) {
        (Some(width), height, _) => (Some(width), height),
        (None, Some(height), _) => (None, Some(height)),
        (None, None, Some(sized)) => (Some(sized), None),
        (None, None, None) => (None, None),
    };
    let selector = draw.selector(path);
    let frame = div()
        .debug_selector(move || selector)
        .flex_none()
        .overflow_hidden()
        .when(image.person, Styled::rounded_full);
    let frame = match state {
        PictureState::Missing => return div().into_any_element(),
        PictureState::Reading => frame
            .w(px(width.unwrap_or(sized.unwrap_or(64.0))))
            .h(px(height.unwrap_or(width.unwrap_or(sized.unwrap_or(64.0)))))
            .bg(tint(draw.palette.foreground, 0.06)),
        PictureState::Shown(shown, (natural_w, natural_h)) => {
            let (w, h) = picture_size((natural_w, natural_h), width, height, size == ImageSize::Stretch);
            let drawn = img(shown).w_full().h_full().object_fit(ObjectFit::Contain);
            match w {
                Some(w) => frame.w(px(w)).h(px(h)).child(drawn),
                None => frame
                    .w_full()
                    .max_w(px(CARD_WIDTH))
                    .child(img_stretched(drawn, natural_w, natural_h)),
            }
        }
    };
    frame.into_any_element()
}

/// A stretched picture: the width there is, its height by its aspect ratio.
fn img_stretched(drawn: gpui::Img, natural_w: u32, natural_h: u32) -> AnyElement {
    let ratio = ratio_of(natural_w, natural_h);
    div()
        .w_full()
        .relative()
        .child(div().w_full().pb(relative(ratio)))
        .child(div().absolute().top_0().left_0().size_full().child(drawn))
        .into_any_element()
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a picture's sides are far below f32's exact range"
)]
fn ratio_of(width: u32, height: u32) -> f32 {
    if width == 0 { 1.0 } else { height as f32 / width as f32 }
}

/// A picture's drawn size from its natural size and the width or height the card gives it: `None` width for one
/// stretched to the width there is. An auto-sized one keeps its own size, never wider than a card.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "a picture's sides are far below f32's exact range"
)]
pub fn picture_size(natural: (u32, u32), width: Option<f32>, height: Option<f32>, stretch: bool) -> (Option<f32>, f32) {
    let (nw, nh) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    match (width, height) {
        (Some(w), Some(h)) => (Some(w), h),
        (Some(w), None) => (Some(w), w * nh / nw),
        (None, Some(h)) => (Some(h * nw / nh), h),
        (None, None) if stretch || nw > CARD_WIDTH - 2.0 * PADDING => (None, 0.0),
        (None, None) => (Some(nw), nh),
    }
}

/// The card picture `url` names, as the engine has its thumbnail.
fn card_picture(url: &str, source: Option<&PictureSource>, cx: &mut App) -> PictureState {
    match (LinkPreviewPicture::card_picture_src(url), source) {
        (Some(src), Some(source)) => picture(&src, source, cx),
        _ => PictureState::Missing,
    }
}

fn image_set(draw: &Draw<'_>, path: &str, images: &[Image], size: ImageSize, cx: &mut App) -> AnyElement {
    let size = if size == ImageSize::Auto {
        ImageSize::Medium
    } else {
        size
    };
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .children(
            images
                .iter()
                .enumerate()
                .map(|(at, image)| picture_of(draw, &format!("{path}{at}."), image, size, cx)),
        )
        .into_any_element()
}

/// A surface's colour for a container style.
fn surface_of(style: ContainerStyle, palette: Palette) -> gpui::Hsla {
    match style {
        ContainerStyle::Default => rgb(palette.surface).into(),
        ContainerStyle::Emphasis => rgb(palette.background).into(),
        ContainerStyle::Accent => tint(palette.brand, 0.12),
        ContainerStyle::Good => tint(theme::SUCCESS, 0.12),
        ContainerStyle::Attention => tint(theme::DANGER, 0.12),
        ContainerStyle::Warning => tint(WARNING, 0.14),
    }
}

/// A div framed as a container's `frame` says: its style's surface, padded, with its border and corners.
fn framed(frame: Frame, palette: Palette) -> gpui::Div {
    let mut div = div().flex().flex_col();
    if let Some(style) = frame
        .style
        .filter(|style| *style != ContainerStyle::Default || frame.border)
    {
        div = div.bg(surface_of(style, palette)).p(px(PADDING));
        if frame.bleed {
            div = div.mx(px(-PADDING));
        }
    }
    if frame.border {
        div = div.border_1().border_color(rgb(palette.border));
    }
    if frame.rounded {
        div = div.rounded_md();
    }
    div
}

fn container_view(draw: &Draw<'_>, path: &str, container: &Container, cx: &mut App) -> AnyElement {
    let inner = framed(container.frame, draw.palette)
        .w_full()
        .map(|div| v_aligned(div, container.vertical))
        .children(elements(draw, path, &container.items, cx));
    selectable(draw, &format!("{path}s"), container.select_action.as_deref(), inner)
}

fn v_aligned(div: gpui::Div, align: VAlign) -> gpui::Div {
    match align {
        VAlign::Top => div.justify_start(),
        VAlign::Center => div.justify_center(),
        VAlign::Bottom => div.justify_end(),
    }
}

fn columns_view(draw: &Draw<'_>, path: &str, set: &ColumnSet, cx: &mut App) -> AnyElement {
    let mut row = framed(set.frame, draw.palette).flex_row().w_full();
    row = match set.align {
        HAlign::Left => row,
        HAlign::Center => row.justify_center(),
        HAlign::Right => row.justify_end(),
    };
    let mut first = true;
    for (at, column) in set.columns.iter().enumerate() {
        if !draw.state.shown(column.id.as_deref(), column.visible) {
            continue;
        }
        if !first {
            row = row.child(column_gap(column, draw.palette));
        }
        first = false;
        row = row.child(column_view(draw, &format!("{path}{at}."), column, cx));
    }
    selectable(draw, &format!("{path}s"), set.select_action.as_deref(), row)
}

/// The space between two columns (a line down it for a separator).
fn column_gap(column: &Column, palette: Palette) -> AnyElement {
    let space = spacing_px(column.spacing).max(if column.spacing == Spacing::None { 0.0 } else { 8.0 });
    if column.separator {
        let half = space / 2.0;
        div()
            .flex_none()
            .pl(px(half))
            .mr(px(half))
            .border_r_1()
            .border_color(rgb(palette.border))
            .into_any_element()
    } else {
        div().flex_none().w(px(space)).into_any_element()
    }
}

fn column_view(draw: &Draw<'_>, path: &str, column: &Column, cx: &mut App) -> AnyElement {
    let inner = framed(column.frame, draw.palette)
        .map(|div| v_aligned(div, column.vertical))
        .map(|div| match column.width {
            Width::Auto => div.flex_none(),
            Width::Pixels(width) => div.flex_none().w(px(width)),
            Width::Weight(weight) => div.flex_grow(weight).flex_shrink(1.0).flex_basis(px(0.0)).min_w_0(),
        })
        .children(elements(draw, path, &column.items, cx));
    selectable(draw, &format!("{path}s"), column.select_action.as_deref(), inner)
}

/// A fact set: its titles, semibold, in a column beside its values, each fact's title and value on one row of a grid
/// (a value that wraps keeps the next fact's title beside the next value). The titles' column is as wide as its
/// longest title, sharing the width evenly with the values when both can't have theirs.
fn facts_view(draw: &Draw<'_>, path: &str, facts: &[(String, String)]) -> AnyElement {
    let palette = draw.palette;
    let cell = |text: &str, at: usize, title: bool| {
        let id = SharedString::from(draw.selector(&format!("{path}{at}{}", if title { "t" } else { "v" })));
        let selector = id.to_string();
        div()
            .debug_selector(move || selector)
            .min_w_0()
            .when(title, |div| div.font_weight(FontWeight::SEMIBOLD))
            .child(rich_text::render(&id, &laid_out(text, draw.cycle), palette))
    };
    let selector = draw.selector(path);
    styled(TextLook::default(), HAlign::Left, palette)
        .debug_selector(move || selector)
        .grid()
        .grid_cols_max_content(2)
        .gap_x_3()
        .gap_y_2()
        .children(
            facts
                .iter()
                .enumerate()
                .flat_map(|(at, (title, value))| [cell(title, at, true), cell(value, at, false)]),
        )
        .into_any_element()
}

fn table_view(draw: &Draw<'_>, path: &str, table: &Table, cx: &mut App) -> AnyElement {
    let palette = draw.palette;
    let rows = table.rows.iter().enumerate().map(|(r, cells)| {
        let header = table.header && r == 0;
        let row = div()
            .flex()
            .flex_row()
            .w_full()
            .when(table.grid && r > 0, |div| {
                div.border_t_1().border_color(rgb(palette.border))
            })
            .when(header, |div| div.font_weight(FontWeight::SEMIBOLD));
        row.children(cells.iter().enumerate().map(|(c, items)| {
            let width = table.columns.get(c).copied().unwrap_or(Width::Weight(1.0));
            div()
                .p_2()
                .min_w_0()
                .when(table.grid && c > 0, |div| {
                    div.border_l_1().border_color(rgb(palette.border))
                })
                .map(|div| match width {
                    Width::Pixels(width) => div.flex_none().w(px(width)),
                    Width::Auto | Width::Weight(_) => div.flex_1(),
                })
                .children(elements(draw, &format!("{path}{r}.{c}."), items, cx))
        }))
    });
    div()
        .w_full()
        .flex()
        .flex_col()
        .when(table.grid, |div| {
            div.border_1().border_color(rgb(palette.border)).rounded_md()
        })
        .children(rows.collect::<Vec<_>>())
        .into_any_element()
}

/// The keys of the `ShowCard`s among `actions` at `path`.
fn show_card_keys(path: &str, actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .enumerate()
        .filter(|(_, action)| matches!(action.kind, ActionKind::ShowCard(_)))
        .map(|(at, _)| format!("{path}{at}"))
        .collect()
}

/// The card a `ShowCard` among `actions` opened, under them.
fn opened_card(draw: &Draw<'_>, path: &str, actions: &[Action], cx: &mut App) -> Option<AnyElement> {
    actions.iter().enumerate().find_map(|(at, action)| {
        let ActionKind::ShowCard(shown) = &action.kind else {
            return None;
        };
        let key = format!("{path}{at}");
        draw.state.is_open(&key).then(|| {
            div()
                .mt(px(spacing_px(Spacing::Default)))
                .pt(px(spacing_px(Spacing::Default)))
                .border_t_1()
                .border_color(rgb(draw.palette.border))
                .child(card_body(draw, &format!("{key}c"), shown, cx))
                .into_any_element()
        })
    })
}

/// A row of buttons, as Teams' action set draws them.
fn action_set(draw: &Draw<'_>, path: &str, actions: &[Action]) -> AnyElement {
    let keys = show_card_keys(path, actions);
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .children(
            actions
                .iter()
                .enumerate()
                .map(|(at, action)| action_button(draw, &format!("{path}{at}"), action, &keys)),
        )
        .into_any_element()
}

fn action_button(draw: &Draw<'_>, key: &str, action: &Action, set: &[String]) -> AnyElement {
    let id = SharedString::from(draw.selector(key));
    let mut button = Button::new(id.clone())
        .label(action.title.clone())
        .outline()
        .with_size(gpui_component::Size::Medium);
    if let Some(glyph) = action_glyph(action) {
        button = button.icon(glyph.icon());
    }
    if draw.state.is_open(key) {
        button = button.selected(true);
    }
    if matches!(action.kind, ActionKind::Submit) {
        button = button.tooltip(RUNS_IN_TEAMS);
    }
    let click = click_of(draw, key, action, set);
    let button = button.on_click(move |_, window, cx| {
        cx.stop_propagation();
        click(window, cx);
    });
    let selector = id.to_string();
    div()
        .debug_selector(move || selector)
        .flex_none()
        .font_weight(FontWeight::SEMIBOLD)
        .child(button)
        .into_any_element()
}

/// The glyph a button carries: Teams' own icon it names, a link out for one that opens in Teams on the web.
fn action_glyph(action: &Action) -> Option<Glyph> {
    match (action.teams_icon(), &action.kind) {
        (Some("Open" | "OpenRegular" | "Open16Regular"), _) => Some(Glyph::EXTERNAL_LINK),
        (_, ActionKind::Submit) => Some(Glyph::OPEN_OUT),
        _ => None,
    }
}

/// What a click on an action does (see the module docs).
fn click_of(draw: &Draw<'_>, key: &str, action: &Action, set: &[String]) -> OnClick {
    let on_state = draw.on_state.cloned();
    match &action.kind {
        ActionKind::OpenUrl(url) => {
            let url = url.clone();
            Rc::new(move |_, cx| cx.open_url(&url))
        }
        ActionKind::Submit => {
            let link = draw.in_teams.map(str::to_owned);
            Rc::new(move |_, cx| {
                if let Some(link) = &link {
                    cx.open_url(link);
                }
            })
        }
        ActionKind::ShowCard(_) => {
            let next = draw.state.flip_card(key, set);
            Rc::new(move |window, cx| {
                if let Some(on_state) = &on_state {
                    on_state(next.clone(), window, cx);
                }
            })
        }
        ActionKind::ToggleVisibility(targets) => {
            let next = draw.state.toggle(targets, draw.base);
            Rc::new(move |window, cx| {
                if let Some(on_state) = &on_state {
                    on_state(next.clone(), window, cx);
                }
            })
        }
    }
}

/// `inner`, doing what `action` does on a click anywhere on it (a pointing hand over it); as it is without one.
fn selectable(draw: &Draw<'_>, key: &str, action: Option<&Action>, inner: gpui::Div) -> AnyElement {
    let Some(action) = action.filter(|action| !matches!(action.kind, ActionKind::ShowCard(_))) else {
        return inner.into_any_element();
    };
    let click = click_of(draw, key, action, &[]);
    let selector = draw.selector(key);
    inner
        .debug_selector(move || selector)
        .id(SharedString::from(draw.selector(key)))
        .cursor_pointer()
        .hover(|style| style.opacity(0.9))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            click(window, cx);
        })
        .into_any_element()
}
