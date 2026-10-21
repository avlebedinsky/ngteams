//! Our own renderer of a message's rich text, node by node: marks, line breaks, paragraphs, mentions (the account's
//! own more strongly), links (opened through GPUI's `open_url`, here the test platform's record), quotes and images
//! (a placeholder, never fetched); laid out from `teams-proto`'s sanitized model and drawn on GPUI's test context.
//! Every id, name and link is made up.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::sync::Arc;

use gpui::{
    Context, IntoElement, Modifiers, ParentElement as _, Render, SharedString, Styled as _, TestAppContext, Window,
    div, point, px,
};
use teams_proto::chat::{Marks, parse_html};
use teams_ui::rich_text::{
    self, Block, IMAGE_PLACEHOLDER, LinkRange, Mentioned, Mentions, QuoteBlock, Rendered, Style, TextBlock, opens,
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
        Block::Quote(quote) => panic!("a quote, not text: {quote:?}"),
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

    // mentions in a form no capture showed name nobody: still highlighted, never as the account's own
    let unreadable = format!("{{\"mentions\":{{\"0\":\"{ME}\"}}}}");
    let rendered = rich_text::layout(&parse_html(content), &Mentions::of(Some(&unreadable), ME));
    let kinds: Vec<Option<Mentioned>> = text_block(&rendered, 0)
        .styled()
        .into_iter()
        .map(|(_, style)| style.mention)
        .collect();
    assert_eq!(kinds, [Some(Mentioned::Other), Some(Mentioned::Other)]);
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
