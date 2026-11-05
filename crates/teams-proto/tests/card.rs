//! A card message's cards (`teams_proto::chat::card`): the Swift payload read into the model, the markdown, the date
//! and time functions. Every card here is synthetic, in the shapes the Workflows bot's cards took.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use teams_proto::LinkPreviewPicture;
use teams_proto::chat::card::{
    ActionKind, ActionStyle, CardMessage, ContainerStyle, DateFormat, HAlign, ImageSize, Kind, Spacing, TextColor,
    TextPart, TextSize, TextWeight, VAlign, Width, card_message, markdown_html, plain_words, text_parts,
};

/// A `RichText/Media_Card` content carrying `activity`, as the chat service gives it.
fn content(activity: &Value) -> String {
    let b64 = STANDARD.encode(serde_json::to_vec(activity).unwrap());
    format!(
        "<URIObject type=\"SWIFT.1\" url_thumbnail=\"https://example.invalid/card.png\">Card - access it on \
         <a href=\"https://go.skype.com/cards.unsupported\">https://go.skype.com/cards.unsupported</a>. \
         <Title>Card</Title><Swift b64=\"{b64}\"></Swift><Description></Description></URIObject>"
    )
}

fn adaptive(card: &Value) -> CardMessage {
    let activity = json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}],
    });
    card_message(&content(&activity)).unwrap()
}

/// The webhook template's post: a bold title, its text and the footer the template adds.
#[test]
fn reads_the_webhook_templates_card() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "version": "1.2",
        "body": [
            {"type": "TextBlock", "text": "Build finished", "weight": "bolder", "wrap": true},
            {"type": "TextBlock", "text": "All checks passed.", "wrap": true},
            {"type": "Container", "separator": true, "items": [
                {"type": "TextBlock", "size": "small", "weight": "lighter", "wrap": true,
                 "text": "Sam Lab used a Workflow template to send this card. [Get template](https://teams.microsoft.com/l/task/x?url=y)"}
            ]}
        ]
    }));
    assert!(!message.unshown);
    let card = &message.cards[0];
    assert!(!card.unshown);
    assert_eq!(card.body.len(), 3);
    let Kind::Text(title) = &card.body[0].kind else {
        panic!("a text")
    };
    assert_eq!(title.text, "Build finished");
    assert_eq!(title.look.weight, TextWeight::Bolder);
    assert!(title.wrap);
    assert!(card.body[2].separator);
    let Kind::Container(footer) = &card.body[2].kind else {
        panic!("a container")
    };
    let Kind::Text(footer) = &footer.items[0].kind else {
        panic!("a text")
    };
    assert_eq!(footer.look.size, TextSize::Small);
    assert_eq!(footer.look.weight, TextWeight::Lighter);
    assert_eq!(
        markdown_html(&footer.text),
        "<p>Sam Lab used a Workflow template to send this card. \
         <a href=\"https://teams.microsoft.com/l/task/x?url=y\">Get template</a></p>"
    );
    assert_eq!(
        plain_words(&message),
        vec![
            "Build finished".to_owned(),
            "All checks passed.".to_owned(),
            footer.text.clone()
        ]
    );
}

/// The Planner card's shapes: columns of widths, an emphasis container with a border and a click, hidden elements,
/// pictures of their own size and a button with Teams' icon.
fn planner_like() -> CardMessage {
    adaptive(&json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "ColumnSet", "columns": [
                {"type": "Column", "width": "auto", "items": [
                    {"type": "Image", "url": "https://example.invalid/icon.png", "width": "16px", "height": "16px"}
                ]},
                {"type": "Column", "width": "stretch", "verticalContentAlignment": "center", "items": [
                    {"type": "TextBlock", "text": "Task created", "isSubtle": true, "wrap": true}
                ]},
                {"type": "Column", "width": 2, "items": []},
                {"type": "Column", "width": "40px", "items": []}
            ]},
            {"type": "Container", "style": "emphasis", "showBorder": true, "roundedCorners": true, "spacing": "small",
             "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/message", "title": "Open"},
             "items": [{"type": "TextBlock", "text": "Due", "isVisible": false, "id": "due"}]}
        ],
        "actions": [
            {"type": "Action.OpenUrl", "title": "View item", "iconUrl": "icon:Open", "url": "https://example.invalid/t"},
            {"type": "Action.Submit", "title": "Approve", "style": "positive"},
            {"type": "Action.ToggleVisibility", "title": "More", "targetElements": ["due", {"elementId": "x", "isVisible": true}]},
            {"type": "Action.ShowCard", "title": "Comment", "card": {"type": "AdaptiveCard", "body": [
                {"type": "TextBlock", "text": "Thanks"}
            ]}}
        ]
    }))
}

#[test]
fn reads_columns_and_containers() {
    let message = planner_like();
    let card = &message.cards[0];
    assert!(!card.unshown);
    let Kind::Columns(set) = &card.body[0].kind else {
        panic!("columns")
    };
    let widths: Vec<Width> = set.columns.iter().map(|column| column.width).collect();
    assert_eq!(
        widths,
        vec![Width::Auto, Width::Weight(1.0), Width::Weight(2.0), Width::Pixels(40.0)]
    );
    assert_eq!(set.columns[1].vertical, VAlign::Center);
    let Kind::Image(icon) = &set.columns[0].items[0].kind else {
        panic!("an image")
    };
    assert_eq!(
        (icon.width, icon.height, icon.size),
        (Some(16.0), Some(16.0), ImageSize::Auto)
    );
    let Kind::Text(subtle) = &set.columns[1].items[0].kind else {
        panic!("a text")
    };
    assert!(subtle.look.subtle);
    assert_eq!(card.body[1].spacing, Spacing::Small);
    let Kind::Container(container) = &card.body[1].kind else {
        panic!("a container")
    };
    assert_eq!(container.frame.style, Some(ContainerStyle::Emphasis));
    assert!(container.frame.border && container.frame.rounded);
    assert!(matches!(
        container.select_action.as_deref().map(|action| &action.kind),
        Some(ActionKind::OpenUrl(url)) if url == "https://example.invalid/message"
    ));
    assert_eq!(container.items[0].id.as_deref(), Some("due"));
    assert!(!container.items[0].visible);
}

#[test]
fn reads_actions() {
    let message = planner_like();
    let card = &message.cards[0];
    assert_eq!(card.actions.len(), 4);
    assert_eq!(card.actions[0].teams_icon(), Some("Open"));
    assert_eq!(card.actions[1].kind, ActionKind::Submit);
    assert_eq!(card.actions[1].style, ActionStyle::Positive);
    assert_eq!(
        card.actions[2].kind,
        ActionKind::ToggleVisibility(vec![("due".to_owned(), None), ("x".to_owned(), Some(true))])
    );
    let ActionKind::ShowCard(shown) = &card.actions[3].kind else {
        panic!("a shown card")
    };
    assert_eq!(shown.body.len(), 1);
}

/// Facts, rich text runs, image sets and tables.
#[test]
fn reads_facts_rich_text_image_sets_and_tables() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "FactSet", "facts": [{"title": "Plan", "value": "Lab plan"}, {"title": "Due"}]},
            {"type": "RichTextBlock", "horizontalAlignment": "center", "inlines": [
                "plain ",
                {"type": "TextRun", "text": "loud", "weight": "bolder", "italic": true, "color": "attention",
                 "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/r"}}
            ]},
            {"type": "ImageSet", "imageSize": "small", "images": [
                {"type": "Image", "url": "https://example.invalid/1.png", "style": "person"},
                {"type": "Image", "url": "javascript:alert(1)"}
            ]},
            {"type": "Table", "firstRowAsHeader": false, "columns": [{"width": 1}, {"width": "60px"}], "rows": [
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "a"}]},
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "b"}]}
                ]}
            ]}
        ]
    }));
    let card = &message.cards[0];
    let Kind::Facts(facts) = &card.body[0].kind else {
        panic!("facts")
    };
    assert_eq!(
        facts,
        &vec![
            ("Plan".to_owned(), "Lab plan".to_owned()),
            ("Due".to_owned(), String::new())
        ]
    );
    let Kind::RichText(rich) = &card.body[1].kind else {
        panic!("rich text")
    };
    assert_eq!(rich.align, HAlign::Center);
    assert_eq!(rich.runs[0].text, "plain ");
    assert!(rich.runs[1].marks.italic);
    assert_eq!(rich.runs[1].look.color, TextColor::Attention);
    assert!(rich.runs[1].select_action.is_some());
    let Kind::ImageSet { images, size } = &card.body[2].kind else {
        panic!("an image set")
    };
    assert_eq!(*size, ImageSize::Small);
    assert_eq!(images.len(), 1, "a picture of a link that isn't http(s) is left out");
    assert!(images[0].person);
    assert!(card.unshown, "and the card says it has content it can't show");
    let Kind::Table(table) = &card.body[3].kind else {
        panic!("a table")
    };
    assert_eq!(table.columns, vec![Width::Weight(1.0), Width::Pixels(60.0)]);
    assert!(!table.header);
    assert!(table.grid);
    assert_eq!(table.rows[0].len(), 2);
}

/// An input, an unknown element without a fallback and an unknown action mark the card unshown; an element's
/// fallback stands in for it, and `"drop"` leaves it out quietly.
#[test]
fn what_it_cant_show_is_never_guessed_at() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Media", "sources": [], "fallback": {"type": "TextBlock", "text": "No media here"}},
            {"type": "Rating", "fallback": "drop"},
        ]
    }));
    let card = &message.cards[0];
    assert!(!card.unshown);
    assert_eq!(card.body.len(), 1);
    let Kind::Text(text) = &card.body[0].kind else {
        panic!("the fallback")
    };
    assert_eq!(text.text, "No media here");

    let with_input = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "comment"}, {"type": "TextBlock", "text": "kept"}],
        "actions": [{"type": "Action.Popover", "title": "?"}]
    }));
    assert!(with_input.cards[0].unshown);
    assert_eq!(with_input.cards[0].body.len(), 1);
    assert!(with_input.cards[0].actions.is_empty());

    let other =
        json!({"attachments": [{"contentType": "application/vnd.microsoft.teams.card.o365connector", "content": {}}]});
    let other = card_message(&content(&other)).unwrap();
    assert!(other.unshown);
    assert!(other.cards.is_empty());

    assert_eq!(card_message("<p>no card</p>"), None);
    assert_eq!(
        card_message("<URIObject><Swift b64=\"not base64!\"></Swift></URIObject>"),
        None
    );
}

/// A hero card is read as the Adaptive Card Teams draws for it.
#[test]
fn reads_a_hero_card() {
    let activity = json!({"attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": {
        "title": "Hello", "subtitle": "from a bot", "text": "Body",
        "images": [{"url": "https://example.invalid/h.png"}],
        "buttons": [{"type": "openUrl", "title": "Docs", "value": "https://example.invalid/docs"},
                    {"type": "imBack", "title": "Yes", "value": "yes"}]
    }}]});
    let message = card_message(&content(&activity)).unwrap();
    let card = &message.cards[0];
    assert_eq!(card.body.len(), 4);
    let Kind::Text(title) = &card.body[0].kind else {
        panic!("a title")
    };
    assert_eq!(
        (title.look.size, title.look.weight),
        (TextSize::Medium, TextWeight::Bolder)
    );
    let Kind::Image(image) = &card.body[3].kind else {
        panic!("a picture")
    };
    assert_eq!(image.size, ImageSize::Stretch);
    assert!(matches!(&card.actions[0].kind, ActionKind::OpenUrl(url) if url.ends_with("/docs")));
    assert_eq!(card.actions[1].kind, ActionKind::Submit);
}

#[test]
fn markdown_is_adaptive_cards_subset() {
    assert_eq!(
        markdown_html("**bold** and _it_ and *it* ~~gone~~"),
        "<p><b>bold</b> and <i>it</i> and <i>it</i> <s>gone</s></p>"
    );
    assert_eq!(
        markdown_html("1. Go there.\r\n2. Select **More options**(**...**).\r\n- a\n- b"),
        "<ol><li>Go there.</li><li>Select <b>More options</b>(<b>...</b>).</li></ol><ul><li>a</li><li>b</li></ul>"
    );
    assert_eq!(markdown_html("one\ntwo\n\nthree"), "<p>one two</p><p>three</p>");
    assert_eq!(
        markdown_html("<script>x</script> & [bad](javascript:x) [ok](mailto:a@b.c)"),
        "<p>&lt;script&gt;x&lt;/script&gt; &amp; bad <a href=\"mailto:a@b.c\">ok</a></p>"
    );
    assert_eq!(markdown_html("2 * 3 = 6, snake_case"), "<p>2 * 3 = 6, snake_case</p>");
    assert_eq!(markdown_html(""), "");
}

#[test]
fn finds_date_and_time_functions() {
    assert_eq!(
        text_parts("Sam {{TIME(2026-11-04T18:15:06Z)}} on {{DATE(2026-11-04T18:15:06Z, SHORT)}}."),
        vec![
            TextPart::Text("Sam "),
            TextPart::Time {
                at_ms: 1_793_816_106_000
            },
            TextPart::Text(" on "),
            TextPart::Date {
                at_ms: 1_793_816_106_000,
                format: DateFormat::Short
            },
            TextPart::Text("."),
        ]
    );
    assert_eq!(
        text_parts("{{DATE(2026-11-04T21:15:06+03:00)}}"),
        vec![TextPart::Date {
            at_ms: 1_793_816_106_000,
            format: DateFormat::Compact
        }]
    );
    assert_eq!(
        text_parts("{{NOPE(1)}} {{DATE(bad)}}"),
        vec![
            TextPart::Text("{{"),
            TextPart::Text("NOPE(1)}} {{"),
            TextPart::Text("DATE(bad)}}"),
        ]
    );
}

/// A card's picture is asked for by the async gateway's thumbnail of its link, never fetched from its site.
#[test]
fn a_card_picture_is_read_through_the_gateway() {
    let src = LinkPreviewPicture::card_picture_src("https://example.invalid/a b.png?x=1&y=2").unwrap();
    let picture = LinkPreviewPicture::of(&src).unwrap();
    assert_eq!(picture.picture(), "https://example.invalid/a b.png?x=1&y=2");
    assert_eq!(
        LinkPreviewPicture::card_picture_src("ftp://example.invalid/a.png"),
        None
    );
    assert_eq!(LinkPreviewPicture::card_picture_src("data:image/png;base64,AAAA"), None);
}
