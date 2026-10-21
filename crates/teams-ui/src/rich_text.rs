//! Our own renderer of a message's rich text: `teams-proto`'s sanitized, depth- and size-capped [`RichText`] laid out
//! as blocks of styled text, then drawn with GPUI's own text elements. No message's HTML ever reaches a markdown or
//! HTML view (the kit's text view is never used): the content is parsed by `teams-proto` into its small model, and
//! only that model is drawn.
//!
//! - [`layout`] turns the model into [`Rendered`]: [`Block::Text`] (a paragraph's text with its styled pieces and its
//!   links) and [`Block::Quote`] (a reply's or a plain quote, with what it quotes in blocks of its own). A line break
//!   is a `\n` in the text; marks (bold, italic, underline, strike, code) style their pieces; a mention is highlighted,
//!   the account's own more strongly (a mention's `itemid` names its entry in `properties.mentions`, whose MRI says
//!   whom it names; an entry this build can't read names nobody); a link is a clickable range; an image is a
//!   placeholder (`Image`, with its `alt`), never fetched.
//! - [`render`] draws it: a link opens through GPUI's `open_url` (the portal or `xdg-open`; in tests the test
//!   platform's record or the UI sandbox's no-op opener), and only an `http`, `https` or `mailto` one (the model
//!   keeps no other, and the renderer checks again).

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveElement as _, InteractiveText, IntoElement,
    ParentElement as _, SharedString, StrikethroughStyle, Styled as _, StyledText, UnderlineStyle, Window, div, px,
    rgb,
};
use serde_json::{Value, json};
use teams_proto::chat::{Field, Marks, Mention, Node, Quote, RichText};

use crate::theme::Palette;

/// What an image shows until images are fetched (M2 Task 22).
pub const IMAGE_PLACEHOLDER: &str = "Image";

/// How a piece of text is styled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub marks: Marks,
    /// A mention, and whether it names the account itself.
    pub mention: Option<Mentioned>,
    /// Inside a link.
    pub link: bool,
    /// An image's placeholder.
    pub image: bool,
}

/// Whom a mention names, as far as the message says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mentioned {
    /// The account itself: highlighted more strongly.
    Me,
    /// Someone else, or nobody this build can tell.
    Other,
}

/// A clickable link: its range in its block's text and where it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRange {
    pub range: Range<usize>,
    pub href: String,
}

/// A paragraph's text, its styled pieces (in order, not overlapping, covering the text) and its links.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextBlock {
    pub text: String,
    pub pieces: Vec<(Range<usize>, Style)>,
    pub links: Vec<LinkRange>,
}

impl TextBlock {
    fn push(&mut self, text: &str, style: Style, link: Option<&str>) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        match self.pieces.last_mut() {
            Some((range, last)) if *last == style && range.end == start => range.end = end,
            _ => self.pieces.push((start..end, style)),
        }
        let Some(href) = link else { return };
        match self.links.last_mut() {
            Some(last) if last.href == href && last.range.end == start => last.range.end = end,
            _ => self.links.push(LinkRange {
                range: start..end,
                href: href.to_owned(),
            }),
        }
    }

    /// Nothing a reader would see.
    fn is_blank(&self) -> bool {
        self.text.trim().is_empty() && !self.pieces.iter().any(|(_, style)| style.image)
    }

    /// The pieces with any style but the plain one.
    #[must_use]
    pub fn styled(&self) -> Vec<(&str, Style)> {
        self.pieces
            .iter()
            .filter(|(_, style)| *style != Style::default())
            .map(|(range, style)| (&self.text[range.clone()], *style))
            .collect()
    }
}

/// A quote: whom it quotes (a reply's), and what it shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuoteBlock {
    /// The quoted message's sender's name, as the quote carries it (a reply's).
    pub sender_name: Option<String>,
    /// The quoted message's id (a reply's).
    pub message_id: Option<String>,
    pub blocks: Vec<Block>,
}

/// One block of a message's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Text(TextBlock),
    Quote(QuoteBlock),
}

/// A message's rich text laid out to draw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    pub blocks: Vec<Block>,
    /// The content was longer than `teams-proto` reads: only its first part is shown.
    pub truncated: bool,
}

/// The mentions of a message: which `itemid`s name the account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mentions {
    mine: HashSet<String>,
}

impl Mentions {
    /// The mentions `properties` (a stored message's `properties`, JSON text) lists, read as `teams-proto` reads
    /// them; those naming `me` are the account's own. An entry in a form no capture showed names nobody.
    #[must_use]
    pub fn of(properties: Option<&str>, me: &str) -> Self {
        let mine = read_mentions(properties)
            .into_iter()
            .filter(|mention| mention.mri.as_deref() == Some(me))
            .map(|mention| mention.item_id)
            .collect();
        Self { mine }
    }

    fn whom(&self, item_id: &str) -> Mentioned {
        if self.mine.contains(item_id) {
            Mentioned::Me
        } else {
            Mentioned::Other
        }
    }
}

/// `properties.mentions` read by `teams-proto`'s own message reader; nothing when it isn't in a captured form.
fn read_mentions(properties: Option<&str>) -> Vec<Mention> {
    let Some(properties) = properties.and_then(|text| serde_json::from_str::<Value>(text).ok()) else {
        return Vec::new();
    };
    let message = json!({ "id": "0", "properties": properties });
    match serde_json::from_value::<teams_proto::chat::Message>(message).map(|message| message.mentions()) {
        Ok(Field::Known(mentions)) => mentions,
        _ => Vec::new(),
    }
}

/// Lays out `text` (see the module docs).
#[must_use]
pub fn layout(text: &RichText, mentions: &Mentions) -> Rendered {
    let mut builder = Builder::new(mentions);
    builder.walk(&text.nodes);
    Rendered {
        blocks: builder.finish(),
        truncated: text.truncated,
    }
}

/// Builds the blocks of one level (the message's, or a quote's).
struct Builder<'a> {
    mentions: &'a Mentions,
    blocks: Vec<Block>,
    current: TextBlock,
    /// The link the walk is in.
    link: Option<String>,
}

impl<'a> Builder<'a> {
    fn new(mentions: &'a Mentions) -> Self {
        Self {
            mentions,
            blocks: Vec::new(),
            current: TextBlock::default(),
            link: None,
        }
    }

    fn push(&mut self, text: &str, style: Style) {
        let style = Style {
            link: self.link.is_some(),
            ..style
        };
        self.current.push(text, style, self.link.as_deref());
    }

    /// Ends the paragraph being built, unless it shows nothing.
    fn end_block(&mut self) {
        let block = std::mem::take(&mut self.current);
        if !block.is_blank() {
            self.blocks.push(Block::Text(block));
        }
    }

    /// Walks `nodes`; recursive over a model `teams-proto` caps at its `MAX_DEPTH`.
    fn walk(&mut self, nodes: &[Node]) {
        for node in nodes {
            match node {
                Node::Text { text, marks } => self.push(
                    text,
                    Style {
                        marks: *marks,
                        ..Style::default()
                    },
                ),
                Node::Break => self.push("\n", Style::default()),
                Node::Paragraph(children) => {
                    self.end_block();
                    self.walk(children);
                    self.end_block();
                }
                Node::Mention { item_id, text } => self.push(
                    text,
                    Style {
                        mention: Some(self.mentions.whom(item_id)),
                        ..Style::default()
                    },
                ),
                Node::Link { href, children } => {
                    let outer = self.link.replace(href.clone());
                    self.walk(children);
                    self.link = outer;
                }
                Node::Image(image) => self.image(image.alt.as_deref()),
                Node::Quote(quote) => self.quote(quote),
            }
        }
    }

    fn image(&mut self, alt: Option<&str>) {
        let text = match alt.map(str::trim).filter(|alt| !alt.is_empty()) {
            Some(alt) => format!("[{IMAGE_PLACEHOLDER}: {alt}]"),
            None => format!("[{IMAGE_PLACEHOLDER}]"),
        };
        self.push(
            &text,
            Style {
                image: true,
                ..Style::default()
            },
        );
    }

    fn quote(&mut self, quote: &Quote) {
        self.end_block();
        let mut inner = Builder::new(self.mentions);
        inner.walk(&quote.body);
        self.blocks.push(Block::Quote(QuoteBlock {
            sender_name: quote.sender_name.clone().filter(|name| !name.trim().is_empty()),
            message_id: quote.message_id.clone(),
            blocks: inner.finish(),
        }));
    }

    fn finish(mut self) -> Vec<Block> {
        self.end_block();
        self.blocks
    }
}

/// Whether the renderer opens `href`: an absolute `http`, `https` or `mailto` URL.
#[must_use]
pub fn opens(href: &str) -> bool {
    let lower = href.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
}

/// A tint of `color` (0xRRGGBB) at `alpha`.
fn tint(color: u32, alpha: f32) -> gpui::Hsla {
    let mut tinted: gpui::Hsla = rgb(color).into();
    tinted.a = alpha;
    tinted
}

/// GPUI's highlight for a piece's style.
fn highlight(style: Style, palette: Palette) -> HighlightStyle {
    let mut highlight = HighlightStyle::default();
    if style.marks.contains(Marks::BOLD) {
        highlight.font_weight = Some(FontWeight::BOLD);
    }
    if style.marks.contains(Marks::ITALIC) {
        highlight.font_style = Some(FontStyle::Italic);
    }
    if style.marks.contains(Marks::UNDERLINE) || style.link {
        highlight.underline = Some(UnderlineStyle {
            thickness: px(1.0),
            ..UnderlineStyle::default()
        });
    }
    if style.marks.contains(Marks::STRIKE) {
        highlight.strikethrough = Some(StrikethroughStyle {
            thickness: px(1.0),
            ..StrikethroughStyle::default()
        });
    }
    if style.marks.contains(Marks::CODE) {
        highlight.background_color = Some(tint(palette.border, 1.0));
    }
    if style.link || style.mention.is_some() {
        highlight.color = Some(rgb(palette.brand).into());
    }
    if style.mention == Some(Mentioned::Me) {
        highlight.font_weight = Some(FontWeight::BOLD);
        highlight.background_color = Some(tint(palette.brand, 0.18));
    }
    if style.image {
        highlight.color = Some(rgb(palette.muted_foreground).into());
        highlight.font_style = Some(FontStyle::Italic);
    }
    highlight
}

/// Draws `rendered`; `id` keeps its links' elements apart from another message's.
#[must_use]
pub fn render(id: &SharedString, rendered: &Arc<Rendered>, palette: Palette) -> AnyElement {
    let mut path = Vec::new();
    div()
        .flex()
        .flex_col()
        .gap_1()
        .children(blocks(id, &mut path, &rendered.blocks, palette))
        .into_any_element()
}

/// The elements of `blocks`; `path` names the block's place, for its element id. Recursive over quotes, as deep as
/// the model.
fn blocks(id: &SharedString, path: &mut Vec<usize>, blocks: &[Block], palette: Palette) -> Vec<AnyElement> {
    blocks
        .iter()
        .enumerate()
        .map(|(at, block)| {
            path.push(at);
            let element = match block {
                Block::Text(text) => text_block(id, path, text, palette),
                Block::Quote(quote) => quote_block(id, path, quote, palette),
            };
            path.pop();
            element
        })
        .collect()
}

fn quote_block(id: &SharedString, path: &mut Vec<usize>, quote: &QuoteBlock, palette: Palette) -> AnyElement {
    let header = quote.sender_name.clone().map(|name| {
        div()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(palette.muted_foreground))
            .child(name)
    });
    div()
        .debug_selector(|| "rich-quote".to_owned())
        .pl_2()
        .py_1()
        .border_l_2()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.hover))
        .flex()
        .flex_col()
        .children(header)
        .children(blocks(id, path, &quote.blocks, palette))
        .into_any_element()
}

fn text_block(id: &SharedString, path: &[usize], block: &TextBlock, palette: Palette) -> AnyElement {
    let highlights: Vec<(Range<usize>, HighlightStyle)> = block
        .pieces
        .iter()
        .filter(|(_, style)| *style != Style::default())
        .map(|(range, style)| (range.clone(), highlight(*style, palette)))
        .collect();
    let styled = StyledText::new(block.text.clone()).with_highlights(highlights);
    if block.links.is_empty() {
        return div()
            .w_full()
            .debug_selector(|| "rich-text".to_owned())
            .child(styled)
            .into_any_element();
    }
    let element_id = SharedString::from(format!("{id}-{path:?}"));
    let hrefs: Vec<String> = block.links.iter().map(|link| link.href.clone()).collect();
    let ranges = block.links.iter().map(|link| link.range.clone()).collect();
    let text = InteractiveText::new(element_id, styled).on_click(ranges, move |at, _: &mut Window, cx: &mut App| {
        if let Some(href) = hrefs.get(at).filter(|href| opens(href)) {
            cx.open_url(href);
        }
    });
    div()
        .w_full()
        .debug_selector(|| "rich-link".to_owned())
        .child(text)
        .into_any_element()
}
