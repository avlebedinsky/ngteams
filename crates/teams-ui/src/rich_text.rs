//! Our own renderer of a message's rich text: `teams-proto`'s sanitized, depth- and size-capped [`RichText`] laid out
//! as blocks of styled text, then drawn with GPUI's own text elements. No message's HTML ever reaches a markdown or
//! HTML view (the kit's text view is never used): the content is parsed by `teams-proto` into its small model, and
//! only that model is drawn.
//!
//! - [`layout`] turns the model into [`Rendered`]: [`Block::Text`] (a paragraph's text with its styled pieces and its
//!   links), [`Block::Quote`] (a reply's or a plain quote, with what it quotes in blocks of its own) and
//!   [`Block::Forward`] (a forwarded message's content, laid out as the message's own: its pictures drawn, each image
//!   placeholder's link to the message on the web). A line break is a `\n` in the text; marks (bold, italic,
//!   underline, strike, code) style their pieces; a mention is drawn in the brand colour, the account's own bold in
//!   the mention colour, neither on a box (a mention's `itemid` names its entry in `properties.mentions`, whose MRI
//!   says whom it names; an entry this build can't read names nobody); a mention of everyone in the chat shows
//!   [`EVERYONE`] and is the account's own in a message someone else sent ([`Mentions::of_message`]); a link is a
//!   clickable range; an emoticon (`<img itemtype="http://schema.skype.com/Emoji" itemid="<id>">`) keeps the place of
//!   a few spaces in its paragraph's text ([`InlineEmoticon`]); a picture pasted into the message (an `<img>` naming
//!   a view of an AMS object) or a GIF picked from Teams' GIF picker (one on Giphy's media host), not inside a link
//!   or a quote other than a forward, is a block of its own ([`Block::Picture`], a GIF playing); any other image is a
//!   placeholder (`Image`, with its `alt`), followed in a chat's message by a link that opens the message in Teams on
//!   the web ([`layout_with_web_link`], [`message_web_link`]), as is a picture that can't be shown. A message of
//!   emoticons alone ([`Rendered::emoticons_only`]) is drawn as Teams draws one: large, playing, without its bubble. A
//!   list is [`Block::List`]: each item's blocks beside its marker (its number, or a bullet by how deep the list is
//!   nested: disc, circle, square), indented as a browser indents one. A paragraph of no-break spaces alone (the web
//!   client's `<p>&nbsp;</p>`, its blank line) is drawn as a blank line; one of ASCII whitespace alone, none. A block
//!   of code is [`Block::Code`]: Teams' code card ([`crate::code`]), its lines numbered and its tokens coloured by
//!   their kind.
//! - [`render`] draws it: paragraphs line under line with no gap between them, as Teams sets them; a forward as
//!   Teams' card, a bordered box with its header (the arrow, the original's sender and time, the view's to make:
//!   [`render_with_header`]) above the content; pictures and emoticons through `crate::inline`, their bytes asked of
//!   the engine of the [`PictureSource`] the view gave ([`Rendered::with_source`]); a link opens through GPUI's
//!   `open_url` (the portal or `xdg-open`; in tests the test platform's record or the UI sandbox's no-op opener), and
//!   only an `http`, `https` or `mailto` one (the model keeps no other, and the renderer checks again).

use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, FontStyle, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StrikethroughStyle, Styled as _, UnderlineStyle, Window, div, px, rgb,
};
use serde_json::{Value, json};
use teams_proto::InlinePicture;
use teams_proto::chat::{Code, Field, Image, List, Marks, Mention, MentionKind, Node, Quote, RichText, WEB_CLIENT};

pub use crate::code::CodeBlock;
use crate::inline::{Emoticon, InlineText, LARGE_EMOTICON, Picture};
pub use crate::inline::{PictureEngine, PictureSource};
use crate::selection::{self, Place, Scope, SelectableText};
use crate::theme::Palette;

/// What an image that isn't shown says.
pub const IMAGE_PLACEHOLDER: &str = "Image";

/// The place an emoticon keeps in its paragraph's text: an em and an en space, about as wide as the emoticon drawn
/// over it at the text's height.
pub const EMOTICON_PLACE: &str = "\u{2003}\u{2002}";

/// The most emoticons a message of emoticons alone is drawn large with.
pub const MOST_LARGE_EMOTICONS: usize = 10;

/// The text of the link after an image's placeholder ([`layout_with_web_link`]).
pub const IMAGE_WEB_LINK: &str = "open in Teams on the web";

/// The web client's link to one message of a chat, in the deep-link form Microsoft documents
/// (`<WEB_CLIENT>/l/message/<chat id>/<message id>?context={"contextType":"chat"}`, the origin shared with
/// `teams_proto::chat::web_chat_link`, which opens a whole chat): where an image the app doesn't show can be seen. Only for a chat's id (`19:…@unq.gbl.spaces` or `19:…@thread.v2`, of letters, digits,
/// `-`, `_`, `.`, `:` and `@`) and a message id of digits; `None` for anything else (a channel, a reply chain).
#[must_use]
pub fn message_web_link(chat: &str, message: &str) -> Option<String> {
    let chat_shaped = chat.starts_with("19:")
        && (chat.ends_with("@unq.gbl.spaces") || chat.ends_with("@thread.v2"))
        && chat
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@'));
    let message_shaped = !message.is_empty() && message.bytes().all(|byte| byte.is_ascii_digit());
    (chat_shaped && message_shaped)
        .then(|| format!("{WEB_CLIENT}/l/message/{chat}/{message}?context=%7B%22contextType%22%3A%22chat%22%7D"))
}

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

/// An emoticon in a paragraph: the place it keeps in the text ([`EMOTICON_PLACE`]), its id in Teams' catalog and its
/// emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineEmoticon {
    pub range: Range<usize>,
    pub id: String,
    pub alt: Option<String>,
}

/// A paragraph's text, its styled pieces (in order, not overlapping, covering the text), its links and its emoticons.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextBlock {
    pub text: String,
    pub pieces: Vec<(Range<usize>, Style)>,
    pub links: Vec<LinkRange>,
    pub emoticons: Vec<InlineEmoticon>,
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

    /// Nothing a reader would see: markup whitespace alone (a no-break space is a blank line's, kept).
    fn is_blank(&self) -> bool {
        self.text.trim_matches(|c: char| c.is_ascii_whitespace()).is_empty()
            && !self.pieces.iter().any(|(_, style)| style.image)
            && self.emoticons.is_empty()
    }

    /// The text as a reader would copy it: each emoticon's place given its emoji.
    #[must_use]
    pub fn plain(&self) -> String {
        let mut plain = String::with_capacity(self.text.len());
        let mut at = 0;
        for emoticon in &self.emoticons {
            plain.push_str(&self.text[at..emoticon.range.start]);
            plain.push_str(emoticon.alt.as_deref().unwrap_or_default());
            at = emoticon.range.end;
        }
        plain.push_str(&self.text[at..]);
        plain
    }

    /// The text in `range` of its drawn text as a reader would copy it: an emoticon the range reaches as its emoji.
    #[must_use]
    pub fn plain_range(&self, range: Range<usize>) -> String {
        let mut plain = String::with_capacity(range.len());
        let mut at = range.start;
        for emoticon in &self.emoticons {
            if emoticon.range.end <= range.start || emoticon.range.start >= range.end {
                continue;
            }
            plain.push_str(&self.text[at..emoticon.range.start.max(at)]);
            plain.push_str(emoticon.alt.as_deref().unwrap_or_default());
            at = emoticon.range.end.min(range.end);
        }
        plain.push_str(&self.text[at..range.end]);
        plain
    }

    /// Whether it shows nothing but emoticons (and space).
    fn only_emoticons(&self) -> bool {
        let mut at = 0;
        for emoticon in &self.emoticons {
            if !self.text[at..emoticon.range.start].trim().is_empty() {
                return false;
            }
            at = emoticon.range.end;
        }
        self.text[at..].trim().is_empty() && self.links.is_empty()
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

/// A picture pasted into a message, or a GIF it shows: the view or GIF its `src` names, its `alt`, the size the message
/// gives it, and what is drawn when it can't be shown (its placeholder, with the link to the message in Teams on the web).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PictureBlock {
    pub src: String,
    pub alt: Option<String>,
    pub size: Option<(u32, u32)>,
    pub fallback: TextBlock,
}

/// A list: numbered from `start` when `ordered`, else bulleted; `level` how many lists it is in (its bullet's shape);
/// each item's blocks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListBlock {
    pub ordered: bool,
    pub start: u32,
    pub level: usize,
    pub items: Vec<Vec<Block>>,
}

impl ListBlock {
    /// The marker of its item at `index`, as a browser draws it: `3.`, or a disc, circle or square bullet by its level.
    #[must_use]
    pub fn marker(&self, index: usize) -> String {
        if self.ordered {
            let number = u64::from(self.start) + u64::try_from(index).unwrap_or(u64::MAX);
            return format!("{number}.");
        }
        match self.level {
            0 => "\u{2022}",
            1 => "\u{25e6}",
            _ => "\u{25aa}",
        }
        .to_owned()
    }

    /// The lines of its item at `index`: the first after the item's marker, indented by the list's level.
    fn item_lines(&self, index: usize, item: &[Block]) -> Vec<String> {
        let head = format!("{}{}", "    ".repeat(self.level), self.marker(index));
        let mut lines: Vec<String> = item.iter().flat_map(Block::plain_lines).collect();
        match lines.first_mut() {
            Some(first) => *first = format!("{head} {first}"),
            None => lines.push(head),
        }
        lines
    }
}

/// A forwarded message's content (`teams_proto::chat::Quote::forward`), laid out as the message's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForwardBlock {
    pub blocks: Vec<Block>,
}

/// One block of a message's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Text(TextBlock),
    Quote(QuoteBlock),
    Picture(PictureBlock),
    List(ListBlock),
    Forward(ForwardBlock),
    Code(CodeBlock),
}

impl Block {
    /// Whether its own text (not a quote's) mentions the account.
    fn mentions_me(&self) -> bool {
        match self {
            Self::Text(text) => text
                .pieces
                .iter()
                .any(|(_, style)| style.mention == Some(Mentioned::Me)),
            Self::List(list) => list.items.iter().flatten().any(Self::mentions_me),
            // a forward's mentions are the original's, of its own chat
            Self::Quote(_) | Self::Picture(_) | Self::Forward(_) | Self::Code(_) => false,
        }
    }

    /// Its lines as a reader copies them: a paragraph's text (an emoticon as its emoji), a list's items each after its
    /// marker, a forward's content's, a block of code's lines; a quote and a picture none.
    #[must_use]
    pub fn plain_lines(&self) -> Vec<String> {
        match self {
            Self::Text(text) => vec![text.plain()],
            Self::List(list) => list
                .items
                .iter()
                .enumerate()
                .flat_map(|(index, item)| list.item_lines(index, item))
                .collect(),
            Self::Forward(forward) => forward.blocks.iter().flat_map(Self::plain_lines).collect(),
            Self::Code(code) => code.lines.iter().map(|line| line.text.clone()).collect(),
            Self::Quote(_) | Self::Picture(_) => Vec::new(),
        }
    }
}

/// A unit of a message's text that is selected as one: a paragraph, or a line of a block of code ([`units`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit<'r> {
    /// Its path among the message's blocks, as [`render_selectable`] places it.
    pub path: Vec<usize>,
    text: UnitText<'r>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum UnitText<'r> {
    Paragraph(&'r TextBlock),
    /// A line of code as drawn (a tab as spaces).
    Code(String),
}

impl Unit<'_> {
    /// The length of its drawn text, bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.text {
            UnitText::Paragraph(block) => block.text.len(),
            UnitText::Code(line) => line.len(),
        }
    }

    /// Whether its drawn text is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `range` of its drawn text as a reader copies it (an emoticon as its emoji).
    #[must_use]
    pub fn copied(&self, range: Range<usize>) -> String {
        let drawn = match &self.text {
            UnitText::Paragraph(block) => block.text.as_str(),
            UnitText::Code(line) => line.as_str(),
        };
        // a selection made before the text changed (an edit came) may fall inside a character
        let range = drawn.floor_char_boundary(range.start)..drawn.floor_char_boundary(range.end);
        match &self.text {
            UnitText::Paragraph(block) => block.plain_range(range),
            UnitText::Code(line) => line.get(range).unwrap_or_default().to_owned(),
        }
    }
}

/// The selectable units of `rendered`, in reading order: each paragraph (a quote's, a list item's and a forward's
/// too) and each line of a block of code; a picture's placeholder isn't one.
#[must_use]
pub fn units(rendered: &Rendered) -> Vec<Unit<'_>> {
    let mut units = Vec::new();
    collect_units(&rendered.blocks, &mut Vec::new(), &mut units);
    units
}

/// `blocks`' units into `units`, their paths as [`blocks`] places them.
fn collect_units<'r>(blocks: &'r [Block], path: &mut Vec<usize>, units: &mut Vec<Unit<'r>>) {
    for (at, block) in blocks.iter().enumerate() {
        path.push(at);
        match block {
            Block::Text(text) => units.push(Unit {
                path: path.clone(),
                text: UnitText::Paragraph(text),
            }),
            Block::Quote(QuoteBlock { blocks, .. }) | Block::Forward(ForwardBlock { blocks }) => {
                collect_units(blocks, path, units);
            }
            Block::List(list) => {
                for (index, item) in list.items.iter().enumerate() {
                    path.push(index);
                    collect_units(item, path, units);
                    path.pop();
                }
            }
            Block::Code(code) => units.extend(code.lines.iter().enumerate().map(|(index, line)| {
                let mut path = path.clone();
                path.push(index);
                Unit {
                    path,
                    text: UnitText::Code(line.shown().0),
                }
            })),
            Block::Picture(_) => {}
        }
        path.pop();
    }
}

/// A message's rich text laid out to draw.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    pub blocks: Vec<Block>,
    /// The content was longer than `teams-proto` reads: only its first part is shown.
    pub truncated: bool,
    /// Drawn bare, with no bubble behind it: Teams draws a message that is only an emoticon or a sticker large and on
    /// the background, and one of pictures alone as its pictures' frames on the background (the header and the avatar
    /// stay). The chat view reads it for each message's bubble.
    pub bare: bool,
    /// Where its pictures and emoticons are asked for; without one a picture is its placeholder and an emoticon the
    /// reaction pack's picture.
    pub source: Option<PictureSource>,
}

impl Rendered {
    /// It, its pictures and emoticons asked of `source`'s engine.
    #[must_use]
    pub fn with_source(mut self, source: PictureSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Its emoticons, when it shows nothing else and has 1 to [`MOST_LARGE_EMOTICONS`] of them: a message Teams draws
    /// large, playing, without its bubble ([`Self::bare`]).
    #[must_use]
    pub fn emoticons_only(&self) -> Option<Vec<&InlineEmoticon>> {
        let mut emoticons = Vec::new();
        for block in &self.blocks {
            let Block::Text(text) = block else { return None };
            if !text.only_emoticons() {
                return None;
            }
            emoticons.extend(&text.emoticons);
        }
        (1..=MOST_LARGE_EMOTICONS)
            .contains(&emoticons.len())
            .then_some(emoticons)
    }

    /// Whether it shows pictures alone (one at least): a message Teams draws without its bubble ([`Self::bare`]).
    #[must_use]
    pub fn pictures_only(&self) -> bool {
        !self.blocks.is_empty() && self.blocks.iter().all(|block| matches!(block, Block::Picture(_)))
    }

    /// Whether its own text (not a quote of another message) mentions the account.
    #[must_use]
    pub fn mentions_me(&self) -> bool {
        self.blocks.iter().any(Block::mentions_me)
    }

    /// The pictures it draws as its own (a forward's and a list's too, not a quote's), in the order they are drawn: the
    /// ones a click opens in the picture viewer.
    #[must_use]
    pub fn pictures(&self) -> Vec<&PictureBlock> {
        fn walk<'r>(blocks: &'r [Block], into: &mut Vec<&'r PictureBlock>) {
            for block in blocks {
                match block {
                    Block::Picture(picture) => into.push(picture),
                    Block::List(list) => list.items.iter().for_each(|item| walk(item, into)),
                    Block::Forward(forward) => walk(&forward.blocks, into),
                    Block::Text(_) | Block::Quote(_) | Block::Code(_) => {}
                }
            }
        }
        let mut pictures = Vec::new();
        walk(&self.blocks, &mut pictures);
        pictures
    }
}

/// The mentions of a message: which `itemid`s name the account, and which mention everyone in the chat.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mentions {
    mine: HashSet<String>,
    everyone: HashSet<String>,
}

/// What a mention of everyone shows, whatever name its sender's client wrote in it: the web client's English label
/// (`compose_mention_everyone_tag`), as the web client shows its own.
pub const EVERYONE: &str = "Everyone";

impl Mentions {
    /// The mentions `properties` (a stored message's `properties`, JSON text) lists, read as `teams-proto` reads
    /// them; those naming `me` are the account's own, and so is a mention of everyone in the chat (`mentionType`
    /// `everyone`, as a message someone else sent reads: [`Self::of_message`]). An entry in a form no capture showed
    /// names nobody.
    #[must_use]
    pub fn of(properties: Option<&str>, me: &str) -> Self {
        let mut mentions = Self::default();
        for mention in read_mentions(properties) {
            if mention.kind == MentionKind::Everyone {
                mentions.everyone.insert(mention.item_id.clone());
                mentions.mine.insert(mention.item_id);
            } else if mention.mri.as_deref() == Some(me) {
                mentions.mine.insert(mention.item_id);
            }
        }
        mentions
    }

    /// The mentions of `message` as the account reads them ([`Self::of`]): a mention of everyone names the account
    /// only in a message someone else sent, as the web client marks one (the sender isn't notified of its own).
    #[must_use]
    pub fn of_message(message: &teams_store::Message, me: &str) -> Self {
        let mut mentions = Self::of(message.properties.as_deref(), me);
        if message.sender.as_deref() == Some(me) {
            let Self { mine, everyone } = &mut mentions;
            mine.retain(|item_id| !everyone.contains(item_id));
        }
        mentions
    }

    /// Whether one of them names the account.
    #[must_use]
    pub fn names_me(&self) -> bool {
        !self.mine.is_empty()
    }

    fn whom(&self, item_id: &str) -> Mentioned {
        if self.mine.contains(item_id) {
            Mentioned::Me
        } else {
            Mentioned::Other
        }
    }

    /// What the mention `item_id` shows: [`EVERYONE`] for a mention of everyone, else `text`, the name in the message.
    fn shown<'t>(&self, item_id: &str, text: &'t str) -> &'t str {
        if self.everyone.contains(item_id) {
            EVERYONE
        } else {
            text
        }
    }
}

/// The MRI each mention of a message names, by its `itemid`, as `properties` (a stored message's, JSON text) lists
/// them; an entry in a form no capture showed, or naming no MRI, names nobody (M2 Task 13: editing a message keeps its
/// mentions by MRI).
#[must_use]
pub fn mentioned(properties: Option<&str>) -> std::collections::HashMap<String, String> {
    read_mentions(properties)
        .into_iter()
        .filter_map(|mention| Some((mention.item_id, mention.mri?)))
        .collect()
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

/// Lays out `text` (see the module docs); an image is its placeholder alone.
#[must_use]
pub fn layout(text: &RichText, mentions: &Mentions) -> Rendered {
    layout_with_web_link(text, mentions, None)
}

/// Lays out `text` as [`layout`] does; with `web_link` (the message's [`message_web_link`]), each image of the message
/// itself (not one a quote shows, nor one inside a link) is followed by a link, [`IMAGE_WEB_LINK`], to `web_link`.
#[must_use]
pub fn layout_with_web_link(text: &RichText, mentions: &Mentions, web_link: Option<&str>) -> Rendered {
    let mut builder = Builder::new(mentions);
    builder.web_link = web_link.filter(|link| opens(link)).map(str::to_owned);
    builder.walk(&text.nodes);
    let mut rendered = Rendered {
        blocks: builder.finish(),
        truncated: text.truncated,
        bare: false,
        source: None,
    };
    rendered.bare = rendered.emoticons_only().is_some() || rendered.pictures_only();
    rendered
}

/// Builds the blocks of one level (the message's, or a quote's).
struct Builder<'a> {
    mentions: &'a Mentions,
    blocks: Vec<Block>,
    current: TextBlock,
    /// The link the walk is in.
    link: Option<String>,
    /// Where an image's placeholder links to ([`layout_with_web_link`]).
    web_link: Option<String>,
    /// A quote's (not a forward's): its pictures stay placeholders, as another message's.
    quoted: bool,
    /// How many lists the walk is in.
    level: usize,
}

impl<'a> Builder<'a> {
    fn new(mentions: &'a Mentions) -> Self {
        Self {
            mentions,
            blocks: Vec::new(),
            current: TextBlock::default(),
            link: None,
            web_link: None,
            quoted: false,
            level: 0,
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
                    self.mentions.shown(item_id, text),
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
                Node::Image(image) => self.image(image),
                Node::Quote(quote) => self.quote(quote),
                Node::List(list) => self.list(list),
                Node::Code(code) => self.code(code),
            }
        }
    }

    /// A block of code: a block of its own, cutting the paragraph it is in.
    fn code(&mut self, code: &Code) {
        self.end_block();
        self.blocks.push(Block::Code(CodeBlock::of(code)));
    }

    /// A list: each item's blocks laid out as this level's are, one list deeper.
    fn list(&mut self, list: &List) {
        self.end_block();
        let items = list
            .items
            .iter()
            .map(|item| {
                let mut inner = Builder::new(self.mentions);
                inner.web_link.clone_from(&self.web_link);
                inner.link.clone_from(&self.link);
                inner.quoted = self.quoted;
                inner.level = self.level + 1;
                inner.walk(item);
                inner.finish()
            })
            .collect();
        self.blocks.push(Block::List(ListBlock {
            ordered: list.ordered,
            start: list.start,
            level: self.level,
            items,
        }));
    }

    /// An emoticon keeps its place in the text; a picture of the message's own (not a quote's, nor inside a link) is a
    /// block; any other image is its placeholder.
    fn image(&mut self, image: &Image) {
        let id = image.item_id.as_deref().filter(|id| !id.trim().is_empty());
        if let (true, Some(id)) = (image.is_emoji(), id) {
            let start = self.current.text.len();
            self.push(EMOTICON_PLACE, Style::default());
            self.current.emoticons.push(InlineEmoticon {
                range: start..self.current.text.len(),
                id: id.to_owned(),
                alt: image.alt.clone(),
            });
            return;
        }
        if !self.quoted && self.link.is_none() && InlinePicture::of(&image.src).is_some() {
            self.end_block();
            let mut fallback = Builder::new(self.mentions);
            fallback.web_link.clone_from(&self.web_link);
            fallback.placeholder(image.alt.as_deref());
            self.blocks.push(Block::Picture(PictureBlock {
                src: image.src.clone(),
                alt: image.alt.clone(),
                size: image.size,
                fallback: fallback.current,
            }));
            return;
        }
        self.placeholder(image.alt.as_deref());
    }

    /// An image's placeholder, with the link to the message after it ([`layout_with_web_link`]).
    fn placeholder(&mut self, alt: Option<&str>) {
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
        // inside a link the placeholder is that link's already
        if self.link.is_none()
            && let Some(web_link) = self.web_link.clone()
        {
            self.push(" ", Style::default());
            self.link = Some(web_link);
            self.push(IMAGE_WEB_LINK, Style::default());
            self.link = None;
        }
    }

    fn quote(&mut self, quote: &Quote) {
        self.end_block();
        if quote.forward {
            self.forward(&quote.body);
            return;
        }
        let mut inner = Builder::new(self.mentions);
        inner.quoted = true;
        inner.walk(&quote.body);
        self.blocks.push(Block::Quote(QuoteBlock {
            sender_name: quote.sender_name.clone().filter(|name| !name.trim().is_empty()),
            message_id: quote.message_id.clone(),
            blocks: inner.finish(),
        }));
    }

    /// A forwarded message's content: laid out as this level's own (its pictures drawn as the message's, its images'
    /// placeholders linked to the message on the web), in a block of its own.
    fn forward(&mut self, body: &[Node]) {
        let mut inner = Builder::new(self.mentions);
        inner.web_link.clone_from(&self.web_link);
        inner.quoted = self.quoted;
        inner.level = self.level;
        inner.walk(body);
        self.blocks
            .push(Block::Forward(ForwardBlock { blocks: inner.finish() }));
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
pub(crate) fn tint(color: u32, alpha: f32) -> gpui::Hsla {
    let mut tinted: gpui::Hsla = rgb(color).into();
    tinted.a = alpha;
    tinted
}

/// GPUI's highlight for a piece's style: bold, italic, underline (a link's too), strikethrough, the code background,
/// the brand colour for a link or someone else's mention, and the account's own mention bold in the mention colour.
#[must_use]
pub fn highlight(style: Style, palette: Palette) -> HighlightStyle {
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
    // as Teams draws them: no box behind a mention, the account's own bold in the mention colour
    if style.mention == Some(Mentioned::Me) {
        highlight.font_weight = Some(FontWeight::BOLD);
        highlight.color = Some(rgb(palette.mention).into());
    }
    if style.image {
        highlight.color = Some(rgb(palette.muted_foreground).into());
        highlight.font_style = Some(FontStyle::Italic);
    }
    highlight
}

/// Draws `rendered`; `id` keeps its links' and pictures' elements apart from another message's. A message of emoticons
/// alone is drawn large and playing ([`Rendered::emoticons_only`]). A forward's card has no header: see
/// [`render_with_header`].
#[must_use]
pub fn render(id: &SharedString, rendered: &Arc<Rendered>, palette: Palette) -> AnyElement {
    render_with_header(id, rendered, palette, None)
}

/// Draws `rendered` as [`render`] does, with `header` (a forward's: the arrow, the original's sender and time) at the
/// top of its first forward's card.
#[must_use]
pub fn render_with_header(
    id: &SharedString,
    rendered: &Arc<Rendered>,
    palette: Palette,
    header: Option<AnyElement>,
) -> AnyElement {
    render_selectable(id, rendered, palette, header, None)
}

/// Draws `rendered` as [`render_with_header`] does, its text selectable in `scope` (its message's place in its view's
/// selection: [`crate::selection`]).
#[must_use]
pub fn render_selectable(
    id: &SharedString,
    rendered: &Arc<Rendered>,
    palette: Palette,
    header: Option<AnyElement>,
    scope: Option<&Scope>,
) -> AnyElement {
    let draw = Draw {
        id,
        palette,
        source: rendered.source.as_ref(),
        header: Cell::new(header),
        scope,
    };
    if let Some(emoticons) = rendered.emoticons_only() {
        return div()
            .debug_selector(|| "rich-emoticons".to_owned())
            .flex()
            .flex_wrap()
            .gap_1()
            .children(emoticons.into_iter().map(|emoticon| {
                Emoticon::new(
                    &emoticon.id,
                    emoticon.alt.as_deref(),
                    LARGE_EMOTICON,
                    true,
                    draw.source.cloned(),
                )
            }))
            .into_any_element();
    }
    let mut path = Vec::new();
    div()
        .flex()
        .flex_col()
        .children(blocks(&draw, &mut path, &rendered.blocks))
        .into_any_element()
}

/// What every block of one message is drawn with.
struct Draw<'a> {
    id: &'a SharedString,
    palette: Palette,
    source: Option<&'a PictureSource>,
    /// The header of the first forward's card, until it is drawn.
    header: Cell<Option<AnyElement>>,
    /// Where its text is selected, when it is selectable.
    scope: Option<&'a Scope>,
}

/// The elements of `blocks`; `path` names the block's place, for its element id. Recursive over quotes, as deep as
/// the model.
fn blocks(draw: &Draw<'_>, path: &mut Vec<usize>, blocks: &[Block]) -> Vec<AnyElement> {
    blocks
        .iter()
        .enumerate()
        .map(|(at, block)| {
            path.push(at);
            let element = match block {
                Block::Text(text) => text_block(draw, path, text, draw.scope.map(|scope| scope.at(path))),
                Block::Quote(quote) => quote_block(draw, path, quote),
                Block::Picture(picture) => picture_block(draw, path, picture),
                Block::List(list) => list_block(draw, path, list),
                Block::Forward(forward) => forward_block(draw, path, forward),
                Block::Code(code) => crate::code::card(
                    SharedString::from(format!("{}-code-{path:?}", draw.id)),
                    code,
                    draw.palette,
                    draw.scope.map(|scope| scope.at(path)),
                ),
            };
            path.pop();
            element
        })
        .collect()
}

/// A picture of the message's own, as Teams draws one: in its frame of the border colour, bounded by Teams' sizes and
/// the bubble's width, its aspect ratio kept (`crate::inline`); its placeholder when it can't be shown. A click on it
/// opens it in the picture viewer when the view opens pictures (its scope's [`Scope::open`]), on the element (and
/// test selector) `<id>-open-<path>`.
fn picture_block(draw: &Draw<'_>, path: &[usize], picture: &PictureBlock) -> AnyElement {
    // a picture's placeholder isn't selectable: a picture is never copied as text
    let fallback = text_block(draw, path, &picture.fallback, None);
    let frame = rgb(draw.palette.border).into();
    let mut drawn = Picture::new(
        SharedString::from(format!("{}-picture-{path:?}", draw.id)),
        &picture.src,
        picture.size,
        draw.source.cloned(),
        frame,
        fallback,
    );
    // a click on it opens it in the picture viewer, when the view opens pictures
    if let Some(open) = draw.scope.and_then(|scope| scope.open.clone()) {
        let src = picture.src.clone();
        drawn = drawn.opening(
            SharedString::from(format!("{}-open-{path:?}", draw.id)),
            Rc::new(move |window: &mut Window, cx: &mut App| open.open(&src, window, cx)),
        );
    }
    div()
        .debug_selector(|| "rich-picture".to_owned())
        .py_1()
        .child(drawn)
        .into_any_element()
}

/// The width of a list item's marker column, as a browser indents a list (its marker right-aligned before the item).
const LIST_INDENT: f32 = 40.0;

/// A list, as a browser draws one: each item a row of its marker, right-aligned in the list's indent, and its blocks.
fn list_block(draw: &Draw<'_>, path: &mut Vec<usize>, list: &ListBlock) -> AnyElement {
    let rows: Vec<AnyElement> = list
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            path.push(index);
            let content = blocks(draw, path, item);
            path.pop();
            div()
                .flex()
                .items_start()
                .child(
                    div()
                        .w(px(LIST_INDENT))
                        .flex_none()
                        .flex()
                        .justify_end()
                        .pr(px(6.0))
                        .whitespace_nowrap()
                        .child(list.marker(index)),
                )
                .child(div().flex_1().min_w_0().flex().flex_col().children(content))
                .into_any_element()
        })
        .collect();
    div()
        .debug_selector(|| "rich-list".to_owned())
        .flex()
        .flex_col()
        .children(rows)
        .into_any_element()
}

fn quote_block(draw: &Draw<'_>, path: &mut Vec<usize>, quote: &QuoteBlock) -> AnyElement {
    let palette = draw.palette;
    let header = quote.sender_name.clone().map(|name| {
        div()
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(rgb(palette.muted_foreground))
            .child(name)
    });
    div()
        .debug_selector(|| "rich-quote".to_owned())
        .my_1()
        .pl_2()
        .py_1()
        .border_l_2()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.hover))
        .flex()
        .flex_col()
        .children(header)
        .children(blocks(draw, path, &quote.blocks))
        .into_any_element()
}

/// The width of a forward card's left edge, thicker than its other sides, as Teams draws it.
const FORWARD_EDGE: f32 = 4.0;

/// A forwarded message's content as Teams draws it: a card with a hairline border and a thicker left edge, its header
/// (the view's: [`render_with_header`]) over the content, which is drawn as the message's own.
fn forward_block(draw: &Draw<'_>, path: &mut Vec<usize>, forward: &ForwardBlock) -> AnyElement {
    let palette = draw.palette;
    let header = draw.header.take().map(|header| div().pb_2().child(header));
    div()
        .debug_selector(|| "rich-forward".to_owned())
        .my_1()
        .px_4()
        .py_3()
        .rounded_md()
        .border_1()
        .border_l(px(FORWARD_EDGE))
        .border_color(rgb(palette.border))
        .flex()
        .flex_col()
        .children(header)
        .children(blocks(draw, path, &forward.blocks))
        .into_any_element()
}

/// The highlights a text block is drawn with: each styled piece's range and its [`highlight`].
#[must_use]
pub fn highlights(block: &TextBlock, palette: Palette) -> Vec<(Range<usize>, HighlightStyle)> {
    block
        .pieces
        .iter()
        .filter(|(_, style)| *style != Style::default())
        .map(|(range, style)| (range.clone(), highlight(*style, palette)))
        .collect()
}

/// A paragraph: its styled text, selectable at `place` when it has one, its links opening on a click, its emoticons
/// drawn over their places.
fn text_block(draw: &Draw<'_>, path: &[usize], block: &TextBlock, place: Option<Place>) -> AnyElement {
    let selector = if block.links.is_empty() {
        "rich-text"
    } else {
        "rich-link"
    };
    let text = SelectableText::new(
        SharedString::from(format!("{}-{path:?}", draw.id)),
        SharedString::from(block.text.clone()),
        highlights(block, draw.palette),
        block.links.clone(),
        place,
        selection::colour(draw.palette),
    );
    let layout = text.layout().clone();
    let text = text.into_any_element();
    let text = if block.emoticons.is_empty() {
        text
    } else {
        let emoticons = block
            .emoticons
            .iter()
            .map(|emoticon| {
                let drawn = Emoticon::new(
                    &emoticon.id,
                    emoticon.alt.as_deref(),
                    px(20.0),
                    false,
                    draw.source.cloned(),
                );
                (emoticon.range.clone(), drawn.into_any_element())
            })
            .collect();
        InlineText::new(text, layout, emoticons).into_any_element()
    };
    div()
        .w_full()
        .debug_selector(move || selector.to_owned())
        .child(text)
        .into_any_element()
}
