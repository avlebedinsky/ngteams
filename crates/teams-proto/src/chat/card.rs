//! A card a bot or a workflow posted (`RichText/Media_Card`), read into a small typed model a view draws natively, as
//! Teams' web client draws it (`docs/protocol/chat.md`, "Cards").
//!
//! The message's `content` is a `<URIObject type="SWIFT.1">` whose text is a fallback for clients that show no card
//! ("Card - access it on …"); the card itself is the base64 JSON of its `<Swift b64="…">`: an activity whose
//! `attachments` each carry a `contentType` and a `content`. An Adaptive Card
//! (`application/vnd.microsoft.card.adaptive`) is read element by element ([`Element`]); a hero or thumbnail card
//! (`application/vnd.microsoft.card.hero`, `…thumbnail`) is read as the Adaptive Card Teams draws for it (its title,
//! subtitle, text, pictures and buttons). What the model can't draw is never guessed at: an input, an element or an
//! action of a type it doesn't know (without a `fallback` to take its place), or an attachment of another kind marks
//! the card [`Card::unshown`], and a view says the message has content it can't show.
//!
//! Nothing here reads the network: a picture is kept as its link, a view asks the engine for it. A link is kept only
//! when it is `http`, `https`, `mailto` or `tel`.
//!
//! A text's markdown ([`markdown_html`]) is Adaptive Cards' subset: `**bold**`, `_italic_` or `*italic*`, `~~struck~~`,
//! `[text](link)`, `- ` or `* ` bulleted and `1. ` numbered lists, a blank line between paragraphs (a single line
//! break is a space, as Teams' renderer's markdown has it); it becomes HTML that `parse_html` reads like any message's.
//! Its date and time functions (`{{DATE(<ISO time>, COMPACT|SHORT|LONG)}}`, `{{TIME(<ISO time>)}}`) are found by
//! [`text_parts`]; a view writes them in the reader's own time zone.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;

/// The message type of a card.
pub const CARD_TYPE: &str = "RichText/Media_Card";
/// An Adaptive Card attachment's `contentType`.
pub const ADAPTIVE: &str = "application/vnd.microsoft.card.adaptive";
/// A hero card attachment's `contentType`.
pub const HERO: &str = "application/vnd.microsoft.card.hero";
/// A thumbnail card attachment's `contentType`.
pub const THUMBNAIL: &str = "application/vnd.microsoft.card.thumbnail";
/// How deep containers are read: an element deeper than this is left out and marks the card unshown.
pub const MAX_DEPTH: usize = 16;
/// The longest base64 card read, in bytes.
const MAX_SWIFT: usize = 1 << 20;

/// The cards a message carries, each attachment's in its order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardMessage {
    pub cards: Vec<Card>,
    /// An attachment of a kind this build doesn't draw (not a card).
    pub unshown: bool,
}

/// One card.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub body: Vec<Element>,
    /// Its buttons, under its body.
    pub actions: Vec<Action>,
    /// What a click anywhere on it does.
    pub select_action: Option<Box<Action>>,
    /// It has a part this build can't show (an input, an unknown element or action).
    pub unshown: bool,
    /// Its `fallbackText`, when it gave one.
    pub fallback_text: Option<String>,
    /// Its `speak` or its `summary`: what Teams reads out, kept for no view yet.
    pub summary: Option<String>,
}

/// An element of a card's body or of a container, with what every element has.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// Its `id`: what a toggle names.
    pub id: Option<String>,
    /// Its `isVisible` (a toggle changes it).
    pub visible: bool,
    pub spacing: Spacing,
    /// A line above it.
    pub separator: bool,
    pub kind: Kind,
}

/// What an element is.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Text(TextBlock),
    RichText(RichTextBlock),
    Image(Image),
    ImageSet {
        images: Vec<Image>,
        size: ImageSize,
    },
    Container(Container),
    Columns(ColumnSet),
    /// Its facts: each a title and a value, in two columns.
    Facts(Vec<(String, String)>),
    Actions(Vec<Action>),
    Table(Table),
}

/// The space above an element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spacing {
    None,
    Small,
    #[default]
    Default,
    Medium,
    Large,
    ExtraLarge,
    Padding,
}

/// A text's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextSize {
    Small,
    #[default]
    Default,
    Medium,
    Large,
    ExtraLarge,
}

/// A text's weight.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextWeight {
    Lighter,
    #[default]
    Default,
    Bolder,
}

/// A text's colour, as the host's palette names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextColor {
    #[default]
    Default,
    Dark,
    Light,
    Accent,
    Good,
    Warning,
    Attention,
}

/// How something lines up across its width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// How a container's content lines up down its height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

/// How a text looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextLook {
    pub size: TextSize,
    pub weight: TextWeight,
    pub color: TextColor,
    /// Drawn quieter (`isSubtle`).
    pub subtle: bool,
    /// The monospace font (`fontType: monospace`).
    pub monospace: bool,
}

/// A `TextBlock`: one text in one look, its markdown read ([`markdown_html`]).
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    pub text: String,
    pub look: TextLook,
    /// It wraps; else it is one line, cut with an ellipsis.
    pub wrap: bool,
    /// The most lines it takes.
    pub max_lines: Option<u32>,
    pub align: HAlign,
    /// Its `style: heading`.
    pub heading: bool,
}

/// A `RichTextBlock`: runs of text, each in its own look.
#[derive(Clone, Debug, PartialEq)]
pub struct RichTextBlock {
    pub runs: Vec<TextRun>,
    pub align: HAlign,
}

/// A `TextRun`: text in one look, never markdown.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub look: TextLook,
    pub marks: RunMarks,
    /// Drawn on the highlight colour.
    pub highlight: bool,
    /// What a click on it does.
    pub select_action: Option<Box<Action>>,
}

/// A run's marks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunMarks {
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
}

/// A picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub url: String,
    pub alt: Option<String>,
    pub size: ImageSize,
    /// Its own width or height in pixels (`"16px"`), over its size.
    pub width: Option<f32>,
    pub height: Option<f32>,
    /// Cut to a circle (`style: person`).
    pub person: bool,
    pub align: HAlign,
    pub select_action: Option<Box<Action>>,
}

/// A picture's size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageSize {
    #[default]
    Auto,
    Stretch,
    Small,
    Medium,
    Large,
}

/// A container's style: the surface it is drawn on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContainerStyle {
    #[default]
    Default,
    Emphasis,
    Good,
    Attention,
    Warning,
    Accent,
}

/// A container's look, shared by containers, columns and column sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Its style, when it set one: a styled container is padded on its surface.
    pub style: Option<ContainerStyle>,
    /// Reaching out to its parent's edges.
    pub bleed: bool,
    /// A border (`showBorder`) around it.
    pub border: bool,
    /// Rounded corners (`roundedCorners`).
    pub rounded: bool,
}

/// A `Container`: elements on one surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub items: Vec<Element>,
    pub frame: Frame,
    pub vertical: VAlign,
    pub select_action: Option<Box<Action>>,
}

/// A `ColumnSet`.
#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSet {
    pub columns: Vec<Column>,
    pub frame: Frame,
    pub align: HAlign,
    pub select_action: Option<Box<Action>>,
}

/// A `Column` of a column set.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub width: Width,
    pub items: Vec<Element>,
    pub frame: Frame,
    pub vertical: VAlign,
    pub spacing: Spacing,
    pub separator: bool,
    pub visible: bool,
    pub id: Option<String>,
    pub select_action: Option<Box<Action>>,
}

/// A column's width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Width {
    /// As wide as its content.
    Auto,
    /// A share of what is left (`stretch` is a weight of 1).
    Weight(f32),
    /// Its own width in pixels.
    Pixels(f32),
}

/// A `Table`: rows of cells, each cell's elements.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub columns: Vec<Width>,
    pub rows: Vec<Vec<Vec<Element>>>,
    /// Its first row is drawn as its header.
    pub header: bool,
    /// Lines between its cells.
    pub grid: bool,
}

/// A button, or what a click on something does.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub title: String,
    pub kind: ActionKind,
    pub style: ActionStyle,
    /// Its icon's link, when it is a picture (`icon:<name>` names one of Teams' own: [`Action::teams_icon`]).
    pub icon_url: Option<String>,
}

impl Action {
    /// The Teams icon its `iconUrl` names (`icon:Open`): its name.
    #[must_use]
    pub fn teams_icon(&self) -> Option<&str> {
        self.icon_url.as_deref()?.strip_prefix("icon:")
    }
}

/// What an action does.
#[derive(Clone, Debug, PartialEq)]
pub enum ActionKind {
    /// Opens a link.
    OpenUrl(String),
    /// Shows (or hides again) a card under the buttons.
    ShowCard(Box<Card>),
    /// Shows, hides or flips the elements it names: each element's id and what it becomes (`None` flips it).
    ToggleVisibility(Vec<(String, Option<bool>)>),
    /// Sends to the bot (`Action.Submit`, `Action.Execute`, a hero card's `imBack`, `messageBack`, `postBack` or
    /// `invoke`): what only Teams' bot path runs.
    Submit,
}

/// An action's style.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ActionStyle {
    #[default]
    Default,
    Positive,
    Destructive,
}

/// A part of a text: as written, or a date or time function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPart<'a> {
    Text(&'a str),
    /// `{{DATE(…)}}`: the instant (ms since the epoch) and how to write it.
    Date {
        at_ms: i64,
        format: DateFormat,
    },
    /// `{{TIME(…)}}`: the instant (ms since the epoch), written as a time of day.
    Time {
        at_ms: i64,
    },
}

/// How a date function writes its date.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateFormat {
    /// `11/4/2026`.
    #[default]
    Compact,
    /// `Wed, Nov 4, 2026`.
    Short,
    /// `Wednesday, November 4, 2026`.
    Long,
}

/// The cards of a card message's `content` (see the module docs); `None` when it carries no card JSON.
#[must_use]
pub fn card_message(content: &str) -> Option<CardMessage> {
    let encoded = swift_b64(content)?;
    if encoded.len() > MAX_SWIFT {
        return None;
    }
    let bytes = STANDARD.decode(encoded.trim()).ok()?;
    let activity: Value = serde_json::from_slice(&bytes).ok()?;
    let attachments = activity.get("attachments")?.as_array()?;
    let mut message = CardMessage::default();
    for attachment in attachments {
        let content = attachment.get("content");
        match (str_of(attachment, "contentType"), content) {
            (Some(ADAPTIVE), Some(card)) => message.cards.push(adaptive(card, 0)),
            (Some(HERO), Some(card)) => message.cards.push(hero(card, ImageSize::Stretch)),
            (Some(THUMBNAIL), Some(card)) => message.cards.push(hero(card, ImageSize::Medium)),
            _ => message.unshown = true,
        }
    }
    Some(message)
}

/// The `b64` of a content's `<Swift>`.
fn swift_b64(content: &str) -> Option<&str> {
    let at = content.find("<Swift")?;
    let rest = &content[at..];
    let end = rest.find('>')?;
    let tag = &rest[..end];
    let start = tag.find("b64=\"")? + "b64=\"".len();
    let value = &tag[start..];
    Some(&value[..value.find('"')?])
}

/// An Adaptive Card `card` at `depth` (a shown card's own depth).
fn adaptive(card: &Value, depth: usize) -> Card {
    let mut read = Reader::default();
    let body = read.elements(card.get("body"), depth);
    let actions = read.actions(card.get("actions"), depth);
    let select_action = read.select(card, depth);
    Card {
        body,
        actions,
        select_action,
        unshown: read.unshown,
        fallback_text: string_of(card, "fallbackText"),
        summary: string_of(card, "speak").or_else(|| string_of(card, "summary")),
    }
}

/// A hero or thumbnail card, as the Adaptive Card Teams draws for it; its pictures of `size`.
fn hero(card: &Value, size: ImageSize) -> Card {
    let mut read = Reader::default();
    let mut body = Vec::new();
    let text = |text: String, look: TextLook| Element::new(Kind::Text(TextBlock::wrapped(text, look)));
    if let Some(title) = string_of(card, "title") {
        body.push(text(
            title,
            TextLook {
                size: TextSize::Medium,
                weight: TextWeight::Bolder,
                ..TextLook::default()
            },
        ));
    }
    if let Some(subtitle) = string_of(card, "subtitle") {
        body.push(text(
            subtitle,
            TextLook {
                subtle: true,
                ..TextLook::default()
            },
        ));
    }
    if let Some(said) = string_of(card, "text") {
        body.push(text(said, TextLook::default()));
    }
    let images: Vec<Image> = array_of(card, "images")
        .iter()
        .filter_map(|image| read.image(image, 0))
        .map(|image| Image { size, ..image })
        .collect();
    body.extend(images.into_iter().map(|image| Element::new(Kind::Image(image))));
    let actions = array_of(card, "buttons")
        .iter()
        .filter_map(|button| read.button(button))
        .collect();
    let select_action = card.get("tap").and_then(|tap| read.button(tap)).map(Box::new);
    Card {
        body,
        actions,
        select_action,
        unshown: read.unshown,
        fallback_text: None,
        summary: None,
    }
}

impl Element {
    fn new(kind: Kind) -> Self {
        Self {
            id: None,
            visible: true,
            spacing: Spacing::Default,
            separator: false,
            kind,
        }
    }
}

impl TextBlock {
    fn wrapped(text: String, look: TextLook) -> Self {
        Self {
            text,
            look,
            wrap: true,
            max_lines: None,
            align: HAlign::Left,
            heading: false,
        }
    }
}

/// Reads a card's elements and actions, noting what it can't show.
#[derive(Default)]
struct Reader {
    unshown: bool,
}

impl Reader {
    fn elements(&mut self, items: Option<&Value>, depth: usize) -> Vec<Element> {
        let Some(items) = items.and_then(Value::as_array) else {
            return Vec::new();
        };
        items.iter().filter_map(|item| self.element(item, depth)).collect()
    }

    /// One element; its `fallback` in its place when its type isn't read (`"drop"` leaves it out quietly).
    fn element(&mut self, item: &Value, depth: usize) -> Option<Element> {
        if depth >= MAX_DEPTH {
            self.unshown = true;
            return None;
        }
        let Some(kind) = self.kind(item, depth) else {
            return self.fallback(item, depth);
        };
        Some(Element {
            id: string_of(item, "id"),
            visible: item.get("isVisible").and_then(Value::as_bool).unwrap_or(true),
            spacing: spacing_of(item),
            separator: bool_of(item, "separator"),
            kind,
        })
    }

    fn fallback(&mut self, item: &Value, depth: usize) -> Option<Element> {
        match item.get("fallback") {
            Some(Value::String(drop)) if drop == "drop" => None,
            Some(other @ Value::Object(_)) => self.element(other, depth + 1),
            _ => {
                self.unshown = true;
                None
            }
        }
    }

    /// The element's kind; `None` for a type this build doesn't draw.
    fn kind(&mut self, item: &Value, depth: usize) -> Option<Kind> {
        Some(match str_of(item, "type")? {
            "TextBlock" => Kind::Text(text_block(item)),
            "RichTextBlock" => Kind::RichText(self.rich_text(item, depth)),
            "Image" => Kind::Image(self.image(item, depth)?),
            "ImageSet" => Kind::ImageSet {
                images: array_of(item, "images")
                    .iter()
                    .filter_map(|image| self.image(image, depth))
                    .collect(),
                size: image_size(str_of(item, "imageSize")),
            },
            "Container" => Kind::Container(Container {
                items: self.elements(item.get("items"), depth + 1),
                frame: frame_of(item),
                vertical: v_align(str_of(item, "verticalContentAlignment")),
                select_action: self.select(item, depth),
            }),
            "ColumnSet" => Kind::Columns(ColumnSet {
                columns: array_of(item, "columns")
                    .iter()
                    .map(|column| self.column(column, depth + 1))
                    .collect(),
                frame: frame_of(item),
                align: h_align(str_of(item, "horizontalAlignment")),
                select_action: self.select(item, depth),
            }),
            "FactSet" => Kind::Facts(
                array_of(item, "facts")
                    .iter()
                    .map(|fact| {
                        (
                            string_of(fact, "title").unwrap_or_default(),
                            string_of(fact, "value").unwrap_or_default(),
                        )
                    })
                    .collect(),
            ),
            "ActionSet" => Kind::Actions(self.actions(item.get("actions"), depth)),
            "Table" => Kind::Table(self.table(item, depth)),
            _ => return None,
        })
    }

    fn rich_text(&mut self, item: &Value, depth: usize) -> RichTextBlock {
        let runs = array_of(item, "inlines")
            .iter()
            .filter_map(|inline| match inline {
                Value::String(text) => Some(TextRun {
                    text: text.clone(),
                    look: TextLook::default(),
                    marks: RunMarks::default(),
                    highlight: false,
                    select_action: None,
                }),
                Value::Object(_) if str_of(inline, "type") == Some("TextRun") => Some(TextRun {
                    text: string_of(inline, "text").unwrap_or_default(),
                    look: look_of(inline),
                    marks: RunMarks {
                        italic: bool_of(inline, "italic"),
                        strike: bool_of(inline, "strikethrough"),
                        underline: bool_of(inline, "underline"),
                    },
                    highlight: bool_of(inline, "highlight"),
                    select_action: self.select(inline, depth),
                }),
                _ => {
                    self.unshown = true;
                    None
                }
            })
            .collect();
        RichTextBlock {
            runs,
            align: h_align(str_of(item, "horizontalAlignment")),
        }
    }

    /// A picture; `None` (and the card unshown) for one without a link it may read.
    fn image(&mut self, item: &Value, depth: usize) -> Option<Image> {
        let Some(url) = str_of(item, "url").filter(|url| is_picture_link(url)) else {
            self.unshown = true;
            return None;
        };
        Some(Image {
            url: url.to_owned(),
            alt: string_of(item, "altText").or_else(|| string_of(item, "alt")),
            size: image_size(str_of(item, "size")),
            width: pixels(str_of(item, "width")),
            height: pixels(str_of(item, "height")),
            person: str_of(item, "style") == Some("person"),
            align: h_align(str_of(item, "horizontalAlignment")),
            select_action: self.select(item, depth),
        })
    }

    fn column(&mut self, item: &Value, depth: usize) -> Column {
        let width = match item.get("width") {
            Some(Value::Number(weight)) => Width::Weight(weight.as_f64().map_or(1.0, to_f32)),
            Some(Value::String(width)) => width_of(width),
            _ => Width::Weight(1.0),
        };
        Column {
            width,
            items: self.elements(item.get("items"), depth),
            frame: frame_of(item),
            vertical: v_align(str_of(item, "verticalContentAlignment")),
            spacing: spacing_of(item),
            separator: bool_of(item, "separator"),
            visible: item.get("isVisible").and_then(Value::as_bool).unwrap_or(true),
            id: string_of(item, "id"),
            select_action: self.select(item, depth),
        }
    }

    fn table(&mut self, item: &Value, depth: usize) -> Table {
        let columns = array_of(item, "columns")
            .iter()
            .map(|column| match column.get("width") {
                Some(Value::Number(weight)) => Width::Weight(weight.as_f64().map_or(1.0, to_f32)),
                Some(Value::String(width)) => width_of(width),
                _ => Width::Weight(1.0),
            })
            .collect();
        let rows = array_of(item, "rows")
            .iter()
            .map(|row| {
                array_of(row, "cells")
                    .iter()
                    .map(|cell| self.elements(cell.get("items"), depth + 1))
                    .collect()
            })
            .collect();
        Table {
            columns,
            rows,
            header: item.get("firstRowAsHeader").and_then(Value::as_bool).unwrap_or(true),
            grid: item.get("showGridLines").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    fn actions(&mut self, actions: Option<&Value>, depth: usize) -> Vec<Action> {
        let Some(actions) = actions.and_then(Value::as_array) else {
            return Vec::new();
        };
        actions.iter().filter_map(|action| self.action(action, depth)).collect()
    }

    fn select(&mut self, item: &Value, depth: usize) -> Option<Box<Action>> {
        self.action(item.get("selectAction")?, depth).map(Box::new)
    }

    /// One action; its `fallback` in its place when its type isn't read.
    fn action(&mut self, action: &Value, depth: usize) -> Option<Action> {
        let kind = match str_of(action, "type") {
            Some("Action.OpenUrl") => match str_of(action, "url").filter(|url| is_link(url)) {
                Some(url) => ActionKind::OpenUrl(url.to_owned()),
                None => return self.unread_action(action, depth),
            },
            Some("Action.ShowCard") if depth + 1 < MAX_DEPTH => match action.get("card") {
                Some(card) => {
                    let card = adaptive(card, depth + 1);
                    self.unshown |= card.unshown;
                    ActionKind::ShowCard(Box::new(card))
                }
                None => return self.unread_action(action, depth),
            },
            Some("Action.ToggleVisibility") => ActionKind::ToggleVisibility(toggles(action)),
            Some("Action.Submit" | "Action.Execute") => ActionKind::Submit,
            _ => return self.unread_action(action, depth),
        };
        Some(Action {
            title: string_of(action, "title").unwrap_or_default(),
            kind,
            style: match str_of(action, "style") {
                Some("positive") => ActionStyle::Positive,
                Some("destructive") => ActionStyle::Destructive,
                _ => ActionStyle::Default,
            },
            icon_url: string_of(action, "iconUrl"),
        })
    }

    fn unread_action(&mut self, action: &Value, depth: usize) -> Option<Action> {
        match action.get("fallback") {
            Some(Value::String(drop)) if drop == "drop" => None,
            Some(other @ Value::Object(_)) if depth + 1 < MAX_DEPTH => self.action(other, depth + 1),
            _ => {
                self.unshown = true;
                None
            }
        }
    }

    /// A hero card's button (or its `tap`): `openUrl` opens its link, the others send to the bot.
    fn button(&mut self, button: &Value) -> Option<Action> {
        let kind = match str_of(button, "type") {
            Some("openUrl") => {
                let Some(url) = str_of(button, "value").filter(|url| is_link(url)) else {
                    self.unshown = true;
                    return None;
                };
                ActionKind::OpenUrl(url.to_owned())
            }
            Some("imBack" | "messageBack" | "postBack" | "invoke") => ActionKind::Submit,
            _ => {
                self.unshown = true;
                return None;
            }
        };
        Some(Action {
            title: string_of(button, "title").unwrap_or_default(),
            kind,
            style: ActionStyle::Default,
            icon_url: None,
        })
    }
}

fn text_block(item: &Value) -> TextBlock {
    TextBlock {
        text: string_of(item, "text").unwrap_or_default(),
        look: look_of(item),
        wrap: bool_of(item, "wrap"),
        max_lines: item
            .get("maxLines")
            .and_then(Value::as_u64)
            .and_then(|lines| u32::try_from(lines).ok())
            .filter(|lines| *lines > 0),
        align: h_align(str_of(item, "horizontalAlignment")),
        heading: str_of(item, "style") == Some("heading"),
    }
}

fn look_of(item: &Value) -> TextLook {
    let lower = |key: &str| str_of(item, key).map(str::to_ascii_lowercase);
    TextLook {
        size: match lower("size").as_deref() {
            Some("small") => TextSize::Small,
            Some("medium") => TextSize::Medium,
            Some("large") => TextSize::Large,
            Some("extralarge") => TextSize::ExtraLarge,
            _ => TextSize::Default,
        },
        weight: match lower("weight").as_deref() {
            Some("lighter") => TextWeight::Lighter,
            Some("bolder") => TextWeight::Bolder,
            _ => TextWeight::Default,
        },
        color: match lower("color").as_deref() {
            Some("dark") => TextColor::Dark,
            Some("light") => TextColor::Light,
            Some("accent") => TextColor::Accent,
            Some("good") => TextColor::Good,
            Some("warning") => TextColor::Warning,
            Some("attention") => TextColor::Attention,
            _ => TextColor::Default,
        },
        subtle: bool_of(item, "isSubtle"),
        monospace: lower("fontType").as_deref() == Some("monospace"),
    }
}

fn frame_of(item: &Value) -> Frame {
    Frame {
        style: str_of(item, "style").map(|style| match style.to_ascii_lowercase().as_str() {
            "emphasis" => ContainerStyle::Emphasis,
            "good" => ContainerStyle::Good,
            "attention" => ContainerStyle::Attention,
            "warning" => ContainerStyle::Warning,
            "accent" => ContainerStyle::Accent,
            _ => ContainerStyle::Default,
        }),
        bleed: bool_of(item, "bleed"),
        border: bool_of(item, "showBorder"),
        rounded: bool_of(item, "roundedCorners"),
    }
}

fn toggles(action: &Value) -> Vec<(String, Option<bool>)> {
    array_of(action, "targetElements")
        .iter()
        .filter_map(|target| match target {
            Value::String(id) => Some((id.clone(), None)),
            Value::Object(_) => Some((
                string_of(target, "elementId")?,
                target.get("isVisible").and_then(Value::as_bool),
            )),
            _ => None,
        })
        .collect()
}

fn spacing_of(item: &Value) -> Spacing {
    match str_of(item, "spacing").map(str::to_ascii_lowercase).as_deref() {
        Some("none") => Spacing::None,
        Some("small") => Spacing::Small,
        Some("medium") => Spacing::Medium,
        Some("large") => Spacing::Large,
        Some("extralarge") => Spacing::ExtraLarge,
        Some("padding") => Spacing::Padding,
        _ => Spacing::Default,
    }
}

fn image_size(size: Option<&str>) -> ImageSize {
    match size.map(str::to_ascii_lowercase).as_deref() {
        Some("stretch") => ImageSize::Stretch,
        Some("small") => ImageSize::Small,
        Some("medium") => ImageSize::Medium,
        Some("large") => ImageSize::Large,
        _ => ImageSize::Auto,
    }
}

fn h_align(align: Option<&str>) -> HAlign {
    match align.map(str::to_ascii_lowercase).as_deref() {
        Some("center") => HAlign::Center,
        Some("right") => HAlign::Right,
        _ => HAlign::Left,
    }
}

fn v_align(align: Option<&str>) -> VAlign {
    match align.map(str::to_ascii_lowercase).as_deref() {
        Some("center") => VAlign::Center,
        Some("bottom") => VAlign::Bottom,
        _ => VAlign::Top,
    }
}

/// A width as a column or a table gives it: `auto`, `stretch`, `<n>px` or a weight as text.
fn width_of(width: &str) -> Width {
    let width = width.trim().to_ascii_lowercase();
    match width.as_str() {
        "auto" => Width::Auto,
        "stretch" => Width::Weight(1.0),
        _ => pixels(Some(&width))
            .map(Width::Pixels)
            .or_else(|| {
                width
                    .parse::<f32>()
                    .ok()
                    .filter(|weight| *weight > 0.0)
                    .map(Width::Weight)
            })
            .unwrap_or(Width::Weight(1.0)),
    }
}

/// `"<n>px"` as `n`.
fn pixels(value: Option<&str>) -> Option<f32> {
    value?
        .trim()
        .strip_suffix("px")?
        .trim()
        .parse::<f32>()
        .ok()
        .filter(|px| px.is_finite() && *px > 0.0)
}

#[expect(clippy::cast_possible_truncation, reason = "a card's weights are small numbers")]
fn to_f32(value: f64) -> f32 {
    value as f32
}

fn str_of<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    item.get(key)?.as_str()
}

fn string_of(item: &Value, key: &str) -> Option<String> {
    str_of(item, key)
        .map(str::to_owned)
        .filter(|text| !text.trim().is_empty())
}

fn bool_of(item: &Value, key: &str) -> bool {
    item.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn array_of<'a>(item: &'a Value, key: &str) -> &'a [Value] {
    item.get(key).and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// A link a button or a text may open: `http`, `https`, `mailto` or `tel`.
#[must_use]
pub fn is_link(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:", "tel:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// A link a picture may be read from: `http` or `https`.
#[must_use]
pub fn is_picture_link(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// The parts of a text: as written, and its date and time functions (a function that doesn't read stays as written).
#[must_use]
pub fn text_parts(text: &str) -> Vec<TextPart<'_>> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let Some(len) = rest[start..].find("}}") else { break };
        let inner = &rest[start + 2..start + len];
        let Some(part) = function(inner) else {
            parts.push(TextPart::Text(&rest[..start + 2]));
            rest = &rest[start + 2..];
            continue;
        };
        if start > 0 {
            parts.push(TextPart::Text(&rest[..start]));
        }
        parts.push(part);
        rest = &rest[start + len + 2..];
    }
    if !rest.is_empty() {
        parts.push(TextPart::Text(rest));
    }
    parts
}

/// A `DATE(…)` or `TIME(…)` function's part.
fn function(inner: &str) -> Option<TextPart<'static>> {
    let inner = inner.trim();
    let (name, args) = inner.split_once('(')?;
    let args = args.strip_suffix(')')?;
    let mut args = args.split(',').map(str::trim);
    let at_ms = iso_ms(args.next()?)?;
    match name.trim() {
        "DATE" => {
            let format = match args.next() {
                None => DateFormat::Compact,
                Some(format) => match format.to_ascii_uppercase().as_str() {
                    "COMPACT" => DateFormat::Compact,
                    "SHORT" => DateFormat::Short,
                    "LONG" => DateFormat::Long,
                    _ => return None,
                },
            };
            args.next().is_none().then_some(TextPart::Date { at_ms, format })
        }
        "TIME" => args.next().is_none().then_some(TextPart::Time { at_ms }),
        _ => None,
    }
}

/// An RFC 3339 time (`2026-11-04T18:15:06Z`, `…+03:00`) as ms since the epoch.
fn iso_ms(text: &str) -> Option<i64> {
    let parsed = time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A text's markdown (see the module docs) as HTML for `parse_html`: everything else in it escaped.
#[must_use]
pub fn markdown_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut html = String::new();
    let mut list: Option<bool> = None;
    let mut paragraph: Vec<&str> = Vec::new();
    for line in normalized.split('\n') {
        let trimmed = line.trim();
        let item = list_item(trimmed);
        if trimmed.is_empty() || item.is_some() {
            flush_paragraph(&mut html, &mut paragraph);
        }
        match item {
            Some((ordered, body)) => {
                if list != Some(ordered) {
                    close_list(&mut html, list);
                    html.push_str(if ordered { "<ol>" } else { "<ul>" });
                    list = Some(ordered);
                }
                html.push_str("<li>");
                html.push_str(&inline_html(body));
                html.push_str("</li>");
            }
            None if trimmed.is_empty() => {
                close_list(&mut html, list);
                list = None;
            }
            None => {
                close_list(&mut html, list);
                list = None;
                paragraph.push(trimmed);
            }
        }
    }
    flush_paragraph(&mut html, &mut paragraph);
    close_list(&mut html, list);
    html
}

fn flush_paragraph(html: &mut String, paragraph: &mut Vec<&str>) {
    if paragraph.is_empty() {
        return;
    }
    html.push_str("<p>");
    html.push_str(&inline_html(&paragraph.join(" ")));
    html.push_str("</p>");
    paragraph.clear();
}

fn close_list(html: &mut String, list: Option<bool>) {
    match list {
        Some(true) => html.push_str("</ol>"),
        Some(false) => html.push_str("</ul>"),
        None => {}
    }
}

/// A list item's line: whether it is numbered, and its text.
fn list_item(line: &str) -> Option<(bool, &str)> {
    if let Some(body) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return Some((false, body.trim_start()));
    }
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let body = line.get(digits..)?.strip_prefix(". ")?;
    (digits > 0 && digits < 10).then(|| (true, body.trim_start()))
}

/// One paragraph's inline markdown as HTML.
fn inline_html(text: &str) -> String {
    let mut html = String::new();
    let mut open: Vec<&'static str> = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((link, after)) = link_at(rest) {
            html.push_str(&link);
            rest = after;
            continue;
        }
        let (marker, tag) = match c {
            '*' if rest.starts_with("**") => ("**", "b"),
            '_' if rest.starts_with("__") => ("__", "b"),
            '~' if rest.starts_with("~~") => ("~~", "s"),
            '*' | '_' => (&rest[..1], "i"),
            _ => ("", ""),
        };
        if !tag.is_empty() && toggles_mark(&open, tag, rest, marker) {
            if open.last() == Some(&tag) {
                open.pop();
                push_tag(&mut html, tag, true);
            } else {
                open.push(tag);
                push_tag(&mut html, tag, false);
            }
            rest = &rest[marker.len()..];
            continue;
        }
        escape_into(&mut html, &rest[..c.len_utf8()]);
        rest = &rest[c.len_utf8()..];
    }
    while let Some(tag) = open.pop() {
        push_tag(&mut html, tag, true);
    }
    html
}

/// `<tag>`, or `</tag>` when `closing`.
fn push_tag(html: &mut String, tag: &str, closing: bool) {
    html.push_str(if closing { "</" } else { "<" });
    html.push_str(tag);
    html.push('>');
}

/// Whether `marker` at the start of `rest` opens or closes `tag`: it closes the innermost open one, and opens one only
/// when the same marker closes it later in the text.
fn toggles_mark(open: &[&str], tag: &str, rest: &str, marker: &str) -> bool {
    if open.last() == Some(&tag) {
        return true;
    }
    !open.contains(&tag) && rest[marker.len()..].contains(marker) && !rest[marker.len()..].starts_with(' ')
}

/// A `[text](link)` at the start of `rest`: its HTML (the text alone when the link may not be opened) and what follows.
fn link_at(rest: &str) -> Option<(String, &str)> {
    let inner = rest.strip_prefix('[')?;
    let close = inner.find("](")?;
    let label = &inner[..close];
    let after = &inner[close + 2..];
    let end = after.find(')')?;
    let url = after[..end].trim();
    let mut html = String::new();
    if is_link(url) {
        html.push_str("<a href=\"");
        escape_into(&mut html, url);
        html.push_str("\">");
        html.push_str(&inline_html(label));
        html.push_str("</a>");
    } else {
        html.push_str(&inline_html(label));
    }
    Some((html, &after[end + 1..]))
}

fn escape_into(html: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '"' => html.push_str("&quot;"),
            _ => html.push(c),
        }
    }
}

/// A card message's plain words, for a search or a notification: its texts' and facts' in order, without markdown.
#[must_use]
pub fn plain_words(message: &CardMessage) -> Vec<String> {
    let mut words = Vec::new();
    for card in &message.cards {
        collect_words(&card.body, &mut words);
    }
    words
}

fn collect_words(elements: &[Element], words: &mut Vec<String>) {
    for element in elements.iter().filter(|element| element.visible) {
        match &element.kind {
            Kind::Text(text) => words.push(text.text.clone()),
            Kind::RichText(rich) => words.push(rich.runs.iter().map(|run| run.text.as_str()).collect()),
            Kind::Facts(facts) => words.extend(facts.iter().map(|(title, value)| format!("{title} {value}"))),
            Kind::Container(container) => collect_words(&container.items, words),
            Kind::Columns(set) => {
                for column in &set.columns {
                    collect_words(&column.items, words);
                }
            }
            Kind::Table(table) => {
                for cell in table.rows.iter().flatten() {
                    collect_words(cell, words);
                }
            }
            Kind::Image(_) | Kind::ImageSet { .. } | Kind::Actions(_) => {}
        }
    }
}
