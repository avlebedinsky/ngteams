//! Our own renderer of a message's rich text, node by node: marks, line breaks, paragraphs, mentions (the account's
//! own more strongly), links (opened through GPUI's `open_url`, here the test platform's record), quotes, images (a
//! placeholder), a pasted picture (a block of its own, its bytes asked of the engine) and emoticons (inline at the
//! text's height, or large and without a bubble in a message of emoticons alone); laid out from `teams-proto`'s
//! sanitized model and drawn on GPUI's test context.
//! Every id, name and link is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::{Arc, Mutex};

use gpui::{
    Context, FontStyle, FontWeight, HighlightStyle, IntoElement, Modifiers, ParentElement as _, Render, SharedString,
    Styled as _, TestAppContext, Window, div, point, px,
};
use teams_proto::chat::{Marks, parse_html};
use teams_ui::rich_text::{
    self, Block, IMAGE_PLACEHOLDER, LinkRange, Mentioned, Mentions, QuoteBlock, Rendered, Style, TextBlock, highlight,
    opens,
};
use teams_ui::theme::LIGHT_PALETTE;

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

fn laid_out(content: &str) -> Rendered {
    rich_text::layout(&parse_html(content), &Mentions::default())
}

/// The text block at `at` of the top level.
fn text_block(rendered: &Rendered, at: usize) -> &TextBlock {
    match &rendered.blocks[at] {
        Block::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
}

fn marks(style: Style) -> Marks {
    style.marks
}

#[test]
fn marks_style_their_pieces() {
    let rendered = laid_out("<p>plain <b>bold</b> <i>it</i> <u>under</u> <s>gone</s> <code>x = 1</code></p>");
    let block = text_block(&rendered, 0);
    assert_eq!(block.text, "plain bold it under gone x = 1");
    let styled: Vec<(&str, Marks)> = block
        .styled()
        .into_iter()
        .map(|(text, style)| (text, marks(style)))
        .collect();
    assert_eq!(
        styled,
        [
            ("bold", Marks::BOLD),
            ("it", Marks::ITALIC),
            ("under", Marks::UNDERLINE),
            ("gone", Marks::STRIKE),
            ("x = 1", Marks::CODE),
        ]
    );
    // the pieces cover the text in order, none overlapping
    let mut end = 0;
    for (range, _) in &block.pieces {
        assert_eq!(range.start, end);
        end = range.end;
    }
    assert_eq!(end, block.text.len());
}

#[test]
fn a_line_break_is_a_new_line_and_paragraphs_are_blocks() {
    let rendered = laid_out("<p>one<br>two</p><p>three</p><p>  </p>");
    assert_eq!(rendered.blocks.len(), 2, "an empty paragraph shows nothing");
    assert_eq!(text_block(&rendered, 0).text, "one\ntwo");
    assert_eq!(text_block(&rendered, 1).text, "three");
    // plain text, as a `Text` message comes
    let plain = rich_text::layout(&teams_proto::chat::RichText::plain("just words"), &Mentions::default());
    assert_eq!(text_block(&plain, 0).text, "just words");
}

#[test]
fn a_mention_is_highlighted_and_the_account_s_own_more_strongly() {
    let content = "<p>hi <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span> and \
                   <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"1\">Lab Bee</span></p>";
    // as read back: a JSON string of a list, the itemid a number
    let properties = format!(
        "{{\"mentions\":\"[{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":0,\\\"mri\\\":\\\"{ME}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab A\\\"}},{{\\\"@type\\\":\\\"http://schema.skype.com/Mention\\\",\\\"itemid\\\":1,\\\"mri\\\":\\\"{LAB_B}\\\",\\\"mentionType\\\":\\\"person\\\",\\\"displayName\\\":\\\"Lab Bee\\\"}}]\"}}"
    );
    let mentions = Mentions::of(Some(&properties), ME);
    let rendered = rich_text::layout(&parse_html(content), &mentions);
    let block = text_block(&rendered, 0);
    let mentioned: Vec<(&str, Option<Mentioned>)> = block
        .styled()
        .into_iter()
        .map(|(text, style)| (text, style.mention))
        .collect();
    assert_eq!(
        mentioned,
        [("Lab A", Some(Mentioned::Me)), ("Lab Bee", Some(Mentioned::Other))]
    );
    assert!(rendered.mentions_me(), "the account's own mention marks the message");

    // mentions in a form no capture showed name nobody: still highlighted, never as the account's own
    let unreadable = format!("{{\"mentions\":{{\"0\":\"{ME}\"}}}}");
    let rendered = rich_text::layout(&parse_html(content), &Mentions::of(Some(&unreadable), ME));
    let kinds: Vec<Option<Mentioned>> = text_block(&rendered, 0)
        .styled()
        .into_iter()
        .map(|(_, style)| style.mention)
        .collect();
    assert_eq!(kinds, [Some(Mentioned::Other), Some(Mentioned::Other)]);
    assert!(!rendered.mentions_me());
    assert_eq!(Mentions::of(None, ME), Mentions::default());
    assert_eq!(Mentions::of(Some("not json"), ME), Mentions::default());
}

#[test]
fn a_link_is_a_clickable_range_and_only_the_web_s_and_mail_s_are_links() {
    let rendered = laid_out(
        "<p>see <a href=\"https://example.test/page\">the <b>page</b></a> or \
         <a href=\"javascript:alert(1)\">this</a> or <a href=\"mailto:someone@example.test\">mail</a></p>",
    );
    let block = text_block(&rendered, 0);
    assert_eq!(block.text, "see the page or this or mail");
    assert_eq!(
        block.links,
        [
            LinkRange {
                range: 4..12,
                href: "https://example.test/page".to_owned(),
            },
            LinkRange {
                range: 24..28,
                href: "mailto:someone@example.test".to_owned(),
            },
        ]
    );
    // the bold part of the link is both
    assert!(block.styled().contains(&(
        "page",
        Style {
            marks: Marks::BOLD,
            link: true,
            ..Style::default()
        }
    )));
    assert!(opens("https://example.test/") && opens("http://example.test/") && opens("mailto:x@example.test"));
    for refused in [
        "javascript:alert(1)",
        "file:///etc/passwd",
        "data:text/html,x",
        "ftp://example.test/",
        "page",
    ] {
        assert!(!opens(refused), "{refused}");
    }
}

#[test]
fn a_reply_s_quote_names_its_sender_and_a_plain_quote_none() {
    let rendered = laid_out(&format!(
        "<blockquote itemscope itemtype=\"http://schema.skype.com/Reply\" itemid=\"1760000010000\">\
         <strong itemprop=\"mri\" itemid=\"{LAB_B}\">Lab Bee</strong><span itemprop=\"time\" itemid=\"1760000010000\">\
         </span><p itemprop=\"preview\">the original</p></blockquote><p>my answer</p>\
         <blockquote><p>plain words</p></blockquote>"
    ));
    assert_eq!(rendered.blocks.len(), 3);
    let Block::Quote(reply) = &rendered.blocks[0] else {
        panic!("a quote first: {rendered:?}");
    };
    assert_eq!(reply.sender_name.as_deref(), Some("Lab Bee"));
    assert_eq!(reply.message_id.as_deref(), Some("1760000010000"));
    assert_eq!(
        reply.blocks,
        [Block::Text(TextBlock {
            text: "the original".to_owned(),
            pieces: vec![(0..12, Style::default())],
            links: Vec::new(),
            emoticons: Vec::new(),
        })]
    );
    assert_eq!(text_block(&rendered, 1).text, "my answer");
    let Block::Quote(plain) = &rendered.blocks[2] else {
        panic!("a plain quote last: {rendered:?}");
    };
    assert_eq!(
        (
            plain.sender_name.as_ref(),
            plain.message_id.as_ref(),
            plain.blocks.len()
        ),
        (None, None, 1)
    );
}

#[test]
fn an_image_is_a_placeholder_and_never_its_source() {
    let rendered = laid_out(
        "<p>look <img src=\"https://images.example.test/cat.png\" alt=\"a cat\"> and \
         <img src=\"https://images.example.test/dog.png\"></p>",
    );
    let block = text_block(&rendered, 0);
    assert_eq!(
        block.text,
        format!("look [{IMAGE_PLACEHOLDER}: a cat] and [{IMAGE_PLACEHOLDER}]")
    );
    assert!(!block.text.contains("example.test"), "the source never shows");
    let images: Vec<&str> = block
        .styled()
        .into_iter()
        .filter(|(_, style)| style.image)
        .map(|(text, _)| text)
        .collect();
    assert_eq!(images.len(), 2);
    assert!(block.links.is_empty(), "an image is no link");
}

const CHAT: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

#[test]
fn a_chat_message_links_to_itself_in_teams_on_the_web() {
    assert_eq!(
        rich_text::message_web_link(CHAT, "1760000000123").as_deref(),
        Some(
            "https://teams.microsoft.com/l/message/19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-\
             00000000000b@unq.gbl.spaces/1760000000123?context=%7B%22contextType%22%3A%22chat%22%7D"
        )
    );
    assert!(rich_text::message_web_link("19:group0001@thread.v2", "1").is_some());
    for (chat, message) in [
        ("19:channel0001@thread.tacv2", "1"),
        ("19:group0001@thread.v2;messageid=1", "1"),
        ("19:a b@thread.v2", "1"),
        ("19:group0001@thread.v2/../x", "1"),
        ("48:notes", "1"),
        (CHAT, ""),
        (CHAT, "12a"),
        (CHAT, "1?x=2"),
    ] {
        assert_eq!(rich_text::message_web_link(chat, message), None, "{chat} {message}");
    }
}

#[test]
fn an_image_of_a_chat_message_is_followed_by_its_link_to_the_web() {
    let link = rich_text::message_web_link(CHAT, "1760000000123").unwrap();
    let text = parse_html(
        "<p>look <img src=\"https://images.example.test/cat.png\" alt=\"a cat\"> \
         <a href=\"https://example.test/page\"><img src=\"https://images.example.test/in-link.png\"></a></p>\
         <blockquote><p>quoted <img src=\"https://images.example.test/quoted.png\"></p></blockquote>",
    );
    let rendered = rich_text::layout_with_web_link(&text, &Mentions::default(), Some(&link));
    let block = text_block(&rendered, 0);
    assert!(
        block.text.starts_with(&format!(
            "look [{IMAGE_PLACEHOLDER}: a cat] {}",
            rich_text::IMAGE_WEB_LINK
        )),
        "{}",
        block.text
    );
    assert!(!block.text.contains("example.test"), "the source never shows");
    // the image's own link, then the anchor around the second image: never a second link inside it
    let hrefs: Vec<&str> = block.links.iter().map(|link| link.href.as_str()).collect();
    assert_eq!(hrefs, [link.as_str(), "https://example.test/page"]);
    assert_eq!(&block.text[block.links[0].range.clone()], rich_text::IMAGE_WEB_LINK);
    // a quoted image belongs to another message: no link
    let quoted = rendered
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Quote(quote) => Some(quote),
            Block::Text(_) | Block::Picture(_) => None,
        })
        .unwrap();
    let Block::Text(inner) = &quoted.blocks[0] else {
        panic!("quoted text expected");
    };
    assert!(inner.links.is_empty(), "{inner:?}");
    // without a link, or with one the renderer wouldn't open, the placeholder stands alone
    for web_link in [None, Some("javascript:alert(1)")] {
        let alone = rich_text::layout_with_web_link(&text, &Mentions::default(), web_link);
        assert!(!text_block(&alone, 0).text.contains(rich_text::IMAGE_WEB_LINK));
        assert_eq!(alone, laid_out_text(&text));
    }
}

fn laid_out_text(text: &teams_proto::chat::RichText) -> Rendered {
    rich_text::layout(text, &Mentions::default())
}

#[test]
fn nothing_active_or_unknown_reaches_the_layout() {
    let rendered = laid_out(
        "<p onclick=\"steal()\">safe<script>alert(1)</script><style>p{}</style><iframe src=\"https://x.test\">\
         </iframe><marquee>moving</marquee></p>",
    );
    assert_eq!(rendered.blocks.len(), 1);
    assert_eq!(text_block(&rendered, 0).text, "safemoving");
    assert!(!rendered.truncated);
    let long = format!("<p>{}</p>", "x".repeat(70 * 1024));
    assert!(laid_out(&long).truncated);
}

/// A view that draws one rendered message, as the chat view does.
struct Holder {
    rendered: Arc<Rendered>,
}

impl Render for Holder {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let id = SharedString::from("message-1");
        div()
            .size_full()
            .child(rich_text::render(&id, &self.rendered, LIGHT_PALETTE))
    }
}

#[test]
fn every_node_draws_and_a_link_opens_through_open_url() {
    let content = "<p><a href=\"https://example.test/opened\">example</a></p><p>a <b>bold</b> <code>c</code> \
                   <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span><br>\
                   <img src=\"https://images.example.test/x.png\" alt=\"x\"></p><blockquote><p>quoted</p></blockquote>";
    let properties = format!("{{\"mentions\":[{{\"itemid\":\"0\",\"mri\":\"{ME}\",\"mentionType\":\"person\"}}]}}");
    let rendered = Arc::new(rich_text::layout(
        &parse_html(content),
        &Mentions::of(Some(&properties), ME),
    ));
    assert!(matches!(&rendered.blocks[2], Block::Quote(QuoteBlock { .. })));
    let cx = TestAppContext::single();
    let app: &'static mut TestAppContext = Box::leak(Box::new(cx));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    for selector in ["rich-link", "rich-text", "rich-quote"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    let link = cx.debug_bounds("rich-link").expect("the link is drawn");
    // a click on the link's first letters
    cx.simulate_click(link.origin + point(px(3.0), px(4.0)), Modifiers::none());
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.test/opened"));
}

#[test]
fn a_click_beside_a_link_opens_nothing() {
    let rendered = Arc::new(laid_out("<p><a href=\"https://example.test/\">x</a></p>"));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let link = cx.debug_bounds("rich-link").expect("the link is drawn");
    // far right of the line, past its one letter
    cx.simulate_click(
        point(link.origin.x + link.size.width - px(4.0), link.origin.y + px(4.0)),
        Modifiers::none(),
    );
    assert_eq!(cx.opened_url(), None);
}

/// The highlight a piece styled `style` is drawn with, in the light palette.
fn drawn_as(style: Style) -> HighlightStyle {
    highlight(style, LIGHT_PALETTE)
}

fn marked(marks: Marks) -> HighlightStyle {
    drawn_as(Style {
        marks,
        ..Style::default()
    })
}

fn mentioned(whom: Mentioned) -> HighlightStyle {
    drawn_as(Style {
        mention: Some(whom),
        ..Style::default()
    })
}

#[test]
fn each_mark_a_link_and_an_image_are_drawn_with_a_highlight_of_their_own() {
    assert_eq!(drawn_as(Style::default()), HighlightStyle::default());
    assert_eq!(marked(Marks::BOLD).font_weight, Some(FontWeight::BOLD));
    assert_eq!(marked(Marks::ITALIC).font_style, Some(FontStyle::Italic));
    assert!(marked(Marks::UNDERLINE).underline.is_some());
    assert!(marked(Marks::STRIKE).strikethrough.is_some());
    assert!(marked(Marks::CODE).background_color.is_some());
    let link = drawn_as(Style {
        link: true,
        ..Style::default()
    });
    assert!(link.underline.is_some() && link.color.is_some());
    let image = drawn_as(Style {
        image: true,
        ..Style::default()
    });
    assert_eq!(image.font_style, Some(FontStyle::Italic));
}

#[test]
fn a_mention_is_drawn_in_the_brand_colour_the_account_s_own_bold_in_the_mention_colour() {
    let (me, other) = (mentioned(Mentioned::Me), mentioned(Mentioned::Other));
    assert_eq!(
        other.color,
        Some(gpui::rgb(LIGHT_PALETTE.brand).into()),
        "someone else's in the brand colour"
    );
    assert_eq!((other.font_weight, other.background_color), (None, None));
    assert_eq!(
        me.color,
        Some(gpui::rgb(LIGHT_PALETTE.mention).into()),
        "the account's own in Teams' orange"
    );
    assert_eq!(me.font_weight, Some(FontWeight::BOLD), "the account's own is bold");
    assert_eq!(me.background_color, None, "no box behind a mention, as in Teams");

    // a block is drawn with its pieces' highlights
    let content =
        "<p>a <b>bold</b> <span itemscope itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab A</span></p>";
    let properties = format!("{{\"mentions\":[{{\"itemid\":\"0\",\"mri\":\"{ME}\",\"mentionType\":\"person\"}}]}}");
    let rendered = rich_text::layout(&parse_html(content), &Mentions::of(Some(&properties), ME));
    let block = text_block(&rendered, 0);
    let drawn = rich_text::highlights(block, LIGHT_PALETTE);
    let of = |text: &str| {
        drawn
            .iter()
            .find(|(range, _)| &block.text[range.clone()] == text)
            .map(|(_, highlight)| *highlight)
    };
    assert_eq!(of("bold"), Some(marked(Marks::BOLD)));
    assert_eq!(of("Lab A"), Some(me));
    assert_eq!(drawn.len(), 2, "unstyled text has no highlight: {drawn:?}");
}

/// A pasted picture's `src`, its view on the media service's host (a made-up object).
const PICTURE: &str = "https://us-api.asm.skype.com/v1/objects/0-eus-d9-00000000000000000000000000000abc/views/imgo";

/// An emoticon's `<img>` as Teams sends it, wrapped in its span (`docs/protocol/chat.md`).
fn emoticon(id: &str, alt: &str, side: u32) -> String {
    format!(
        "<span title=\"{id}\" type=\"({id})\" class=\"animated-emoticon-{side}-{id}\" itemscope=\"\"><img itemscope=\"\" \
         itemtype=\"http://schema.skype.com/Emoji\" itemid=\"{id}\" \
         src=\"https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/{id}/default/\
         20_f.png\" alt=\"{alt}\" style=\"width:{side}px; height:{side}px\"></span>"
    )
}

#[test]
fn an_emoticon_keeps_its_place_in_the_text_and_a_pasted_picture_is_a_block() {
    let content = format!(
        "<p>made-up board summary:</p>\
         <p><img src=\"{PICTURE}\" itemtype=\"http://schema.skype.com/AMSImage\" width=\"884\" height=\"228\" \
         alt=\"image\"></p>\
         <p>12 made-up cards moved</p><p>7 made-up cards added{}</p>",
        emoticon("speechless", "\u{1f610}", 20)
    );
    let link = "https://teams.microsoft.com/l/message/19:x@thread.v2/1";
    let rendered = rich_text::layout_with_web_link(&parse_html(&content), &Mentions::default(), Some(link));
    assert_eq!(rendered.blocks.len(), 4, "{rendered:#?}");
    let Block::Picture(picture) = &rendered.blocks[1] else {
        panic!("a picture block: {rendered:#?}");
    };
    assert_eq!((picture.src.as_str(), picture.size), (PICTURE, Some((884, 228))));
    // drawn when it can't be shown: its placeholder and the link to the message
    assert_eq!(
        picture.fallback.text,
        format!("[{IMAGE_PLACEHOLDER}: image] {}", rich_text::IMAGE_WEB_LINK)
    );
    assert_eq!(picture.fallback.links[0].href, link);

    let last = text_block(&rendered, 3);
    assert_eq!(
        last.text,
        format!("7 made-up cards added{}", rich_text::EMOTICON_PLACE)
    );
    assert_eq!(last.emoticons.len(), 1);
    assert_eq!(last.emoticons[0].id, "speechless");
    assert_eq!(&last.text[last.emoticons[0].range.clone()], rich_text::EMOTICON_PLACE);
    assert_eq!(last.plain(), "7 made-up cards added\u{1f610}");
    assert!(!last.text.contains(IMAGE_PLACEHOLDER), "no placeholder for an emoticon");
    assert!(
        rendered.emoticons_only().is_none() && !rendered.bare,
        "text with it: drawn in its bubble"
    );

    // a picture a quote shows, or one inside a link, stays a placeholder; so does an emoticon without its id
    let quoted = format!(
        "<blockquote><p><img src=\"{PICTURE}\" alt=\"q\"></p></blockquote>\
         <p><a href=\"https://example.test/page\"><img src=\"{PICTURE}\" alt=\"l\"></a> \
         <img itemtype=\"http://schema.skype.com/Emoji\" src=\"https://images.example.test/e.png\" alt=\"e\"></p>"
    );
    let rendered = rich_text::layout(&parse_html(&quoted), &Mentions::default());
    assert!(
        !rendered.blocks.iter().any(|block| matches!(block, Block::Picture(_))),
        "{rendered:#?}"
    );
    assert!(
        text_block(&rendered, 1)
            .text
            .contains(&format!("[{IMAGE_PLACEHOLDER}: e]"))
    );
}

#[test]
fn a_message_of_emoticons_alone_is_drawn_bare() {
    let one = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    let rendered = rich_text::layout(&parse_html(&one), &Mentions::default());
    let emoticons = rendered.emoticons_only().unwrap();
    assert_eq!(emoticons.len(), 1);
    assert_eq!(emoticons[0].id, "1f44b_wavinghand");
    assert!(rendered.bare, "no bubble behind it");

    let three = format!(
        "<p>{} &nbsp;{}</p><p>{}</p>",
        emoticon("smile", "\u{1f642}", 20),
        emoticon("hi", "\u{1f44b}", 20),
        emoticon("yes", "\u{1f44d}", 20)
    );
    let rendered = rich_text::layout(&parse_html(&three), &Mentions::default());
    assert_eq!(rendered.emoticons_only().map(|all| all.len()), Some(3));
    assert!(rendered.bare);

    for not_alone in [
        format!("<p>hi {}</p>", emoticon("smile", "\u{1f642}", 20)),
        format!(
            "<p>{}</p><p><img src=\"{PICTURE}\"></p>",
            emoticon("smile", "\u{1f642}", 20)
        ),
        format!(
            "<p><a href=\"https://example.test/\">{}</a></p>",
            emoticon("smile", "\u{1f642}", 20)
        ),
        format!(
            "<p>{}</p>",
            emoticon("smile", "\u{1f642}", 20).repeat(rich_text::MOST_LARGE_EMOTICONS + 1)
        ),
        "<p>just text</p>".to_owned(),
    ] {
        let rendered = rich_text::layout(&parse_html(&not_alone), &Mentions::default());
        assert!(rendered.emoticons_only().is_none() && !rendered.bare, "{not_alone}");
    }
}

/// The engine's side a message's pictures and emoticons are asked of: answers from its own pictures, no animation,
/// and records what it was asked.
#[derive(Default)]
struct FakePictures {
    pictures: Mutex<std::collections::HashMap<String, teams_core::PictureBytes>>,
    asked: Mutex<Vec<&'static str>>,
}

impl rich_text::PictureEngine for FakePictures {
    fn me(&self) -> &str {
        ME
    }

    fn message_picture(&self, src: String) -> teams_core::Reply<Option<teams_core::PictureBytes>> {
        self.asked.lock().unwrap().push("message_picture");
        teams_core::Reply::ready(Ok(self.pictures.lock().unwrap().get(&src).cloned()))
    }

    fn reaction_animation(&self, _: String) -> teams_core::Reply<Option<teams_core::Strip>> {
        self.asked.lock().unwrap().push("reaction_animation");
        teams_core::Reply::ready(Ok(None))
    }

    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

impl FakePictures {
    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

/// A made-up picture's PNG, `width` by `height`.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(width, height, image::Rgba([40, 120, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

#[test]
fn a_pasted_picture_is_asked_of_the_engine_and_drawn_within_the_width_there_is() {
    let core = Arc::new(FakePictures::default());
    core.pictures.lock().unwrap().insert(
        PICTURE.to_owned(),
        Arc::new(teams_proto::PastedPicture {
            format: teams_proto::PastedFormat::Png,
            bytes: png(40, 10),
        }),
    );
    // the message gives it a size wider than any window: drawn at the width there is, a quarter as high
    let content = format!("<p>look</p><p><img src=\"{PICTURE}\" width=\"40000\" height=\"10000\"></p>");
    let source = rich_text::PictureSource::of(core.clone());
    let rendered = Arc::new(rich_text::layout(&parse_html(&content), &Mentions::default()).with_source(source));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.run_until_parked();
    assert!(core.asked().contains(&"message_picture"));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let drawn = cx.debug_bounds("rich-picture").expect("the picture is drawn");
    let window = cx.update(|window, _| window.viewport_size());
    assert!(drawn.size.width <= window.width, "{drawn:?} in {window:?}");
    // its block's padding aside, a quarter as high as wide
    let picture_height = drawn.size.height - px(8.0);
    assert!(
        (f32::from(picture_height) - f32::from(drawn.size.width) / 4.0).abs() < 2.0,
        "{drawn:?}"
    );
}

#[test]
fn an_emoticon_alone_is_drawn_large_and_one_in_text_at_the_text_s_height() {
    let core = Arc::new(FakePictures::default());
    let alone = format!("<div>{}</div>", emoticon("1f44b_wavinghand", "\u{1f44b}", 58));
    let rendered = Arc::new(
        rich_text::layout(&parse_html(&alone), &Mentions::default())
            .with_source(rich_text::PictureSource::of(core.clone())),
    );
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let large = cx.debug_bounds("rich-emoticons").expect("drawn large");
    assert!(large.size.height >= px(58.0), "{large:?}");
    assert!(core.asked().contains(&"reaction_animation"), "its animation asked for");

    let inline = format!("<p>hi there{}</p>", emoticon("smile", "\u{1f642}", 20));
    let rendered = Arc::new(laid_out(&inline));
    let app: &'static mut TestAppContext = Box::leak(Box::new(TestAppContext::single()));
    let (_, cx) = app.add_window_view(|_, _| Holder { rendered });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    let line = cx.debug_bounds("rich-text").expect("the line is drawn");
    assert!(line.size.height < px(40.0), "one line of text: {line:?}");
}
