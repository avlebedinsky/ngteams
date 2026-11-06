//! A message's rich text to the translator's markers and back (synthetic text only): every kind of node survives a
//! translation that keeps its markers, one that moves, drops or repeats them stays whole, and typed angle brackets stay
//! text.

use teams_proto::chat::{Code, Image, Quote, SpanStyle, parse_html};

use super::*;

fn text(words: &str) -> Node {
    Node::Text {
        text: words.to_owned(),
        marks: Marks::NONE,
    }
}

fn mention() -> Node {
    Node::Mention {
        item_id: "0".to_owned(),
        text: "Lab B".to_owned(),
    }
}

fn emoji() -> Node {
    Node::Image(Image {
        src: "https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/smile/default/20_f.png".to_owned(),
        alt: Some("🙂".to_owned()),
        item_type: Some(teams_proto::chat::EMOJI_ITEMTYPE.to_owned()),
        item_id: Some("smile".to_owned()),
        size: None,
    })
}

/// A message with every kind of node.
fn everything() -> RichText {
    RichText {
        nodes: vec![
            Node::Quote(Quote {
                message_id: Some("1700000000000".to_owned()),
                sender: Some("8:orgid:synthetic-b".to_owned()),
                sender_name: Some("Lab B".to_owned()),
                time: Some("1700000000000".to_owned()),
                body: vec![text("What time is lunch?")],
                forward: false,
            }),
            Node::Paragraph(vec![
                text("Hello "),
                mention(),
                text(", the "),
                Node::Text {
                    text: "meeting".to_owned(),
                    marks: Marks::BOLD,
                },
                text(" is at "),
                Node::Link {
                    href: "https://example.com/agenda".to_owned(),
                    children: vec![text("noon")],
                },
                Node::Break,
                emoji(),
            ]),
            Node::Heading {
                level: 2,
                children: vec![text("Plan")],
            },
            Node::List(List {
                ordered: true,
                start: 3,
                items: vec![vec![text("first")], vec![text("second")]],
            }),
            Node::Span {
                style: SpanStyle::default(),
                children: vec![text("coloured")],
            },
            Node::Indented {
                depth: 1,
                children: vec![text("indented")],
            },
            Node::Code(Code::default()),
            Node::Rule,
        ],
        truncated: false,
    }
}

#[test]
fn every_node_comes_back_from_an_answer_that_keeps_the_markers() {
    let message = everything();
    let (encoded, shells) = encode(&message);
    assert!(!shells.is_empty());
    assert!(
        !encoded.contains("example.com"),
        "a link's address never goes to the tool"
    );
    assert!(!encoded.contains("8:orgid"), "nor a mention's person");
    assert!(encoded.contains("Hello "));
    assert_eq!(decode(&encoded, &shells), message);
    // the words translated, the markers kept
    let translated = encoded
        .replace("Hello ", "Hallo ")
        .replace("first", "erstens")
        .replace("What time is lunch?", "Wann ist Mittag?");
    let back = decode(&translated, &shells);
    let plain = back.plain_text();
    assert!(plain.contains("Hallo Lab B"), "{plain}");
    assert!(plain.contains("erstens") && plain.contains("Wann ist Mittag?"));
    assert_eq!(back.nodes.len(), message.nodes.len());
}

#[test]
fn a_parsed_message_survives_the_round_trip() {
    let parsed = parse_html(
        "<p>Ping <span itemscope=\"\" itemtype=\"http://schema.skype.com/Mention\" itemid=\"0\">Lab B</span> \
         <b>now</b> &amp; <i>later</i> &lt;tag&gt;</p><ul><li>one</li><li>two</li></ul><pre>let x = 1;</pre>",
    );
    let (encoded, shells) = encode(&parsed);
    assert!(
        encoded.contains("&amp;") && encoded.contains("&lt;tag&gt;"),
        "{encoded}"
    );
    assert_eq!(decode(&encoded, &shells), parsed);
}

#[test]
fn moved_dropped_and_repeated_markers_leave_the_text_whole() {
    let message = RichText {
        nodes: vec![Node::Paragraph(vec![
            text("Ask "),
            mention(),
            text(" about the "),
            Node::Text {
                text: "plan".to_owned(),
                marks: Marks::ITALIC,
            },
            text(" "),
            emoji(),
        ])],
        truncated: false,
    };
    let (encoded, shells) = encode(&message);
    assert_eq!(encoded, "<x0>Ask <x1/> about the <x2>plan</x2> <x3/></x0>");
    // moved: the mention first, as German puts it
    let moved = decode("<x0>Frag <x1/> nach dem <x2>Plan</x2> <x3/></x0>", &shells);
    assert_eq!(moved.plain_text().trim(), "Frag Lab B nach dem Plan 🙂");
    // the mention and the emoji lost, a marker made up, one repeated, an unclosed open, a stray close
    let lossy = decode("<x0>Frag nach dem <x2>Plan</x2><x9/><x2>wieder</x2></x7> <x5>", &shells);
    let Node::Paragraph(children) = &lossy.nodes[0] else {
        panic!("a paragraph: {lossy:?}");
    };
    assert!(children.contains(&mention()), "the mention is put back: {children:?}");
    assert!(children.contains(&emoji()), "and the emoji");
    assert_eq!(children.iter().filter(|node| **node == mention()).count(), 1);
    assert!(
        lossy.plain_text().contains("wieder"),
        "a repeated marker's words stay, unmarked"
    );
    // an atom given twice is kept once
    let twice = decode("<x0><x1/><x1/> hi <x3/></x0>", &shells);
    assert_eq!(twice.plain_text().matches("Lab B").count(), 1);
}

#[test]
fn text_that_looks_like_a_marker_stays_text() {
    let message = RichText::plain("a <x1> b < c > d & e <x2/>");
    let (encoded, shells) = encode(&message);
    assert!(shells.is_empty());
    assert_eq!(encoded, "a &lt;x1&gt; b &lt; c &gt; d &amp; e &lt;x2/&gt;");
    assert_eq!(decode(&encoded, &shells), message);
    // what the tool may write unescaped
    assert_eq!(
        decode(
            "x &quot;q&quot; &#39;s&#39; &apos;t&apos; &nbsp;< y <x12345678>",
            &shells
        )
        .plain_text(),
        "x \"q\" 's' 't' \u{a0}< y <x12345678>"
    );
    assert_eq!(decode("<x 3/> <xa> <x3", &shells).plain_text(), "<x 3/> <xa> <x3");
}

#[test]
fn list_items_and_atoms_outside_their_places_are_kept() {
    let message = RichText {
        nodes: vec![Node::List(List {
            ordered: false,
            start: 1,
            items: vec![vec![text("one")], vec![text("two")]],
        })],
        truncated: false,
    };
    let (encoded, shells) = encode(&message);
    assert_eq!(encoded, "<x0><x1>one</x1><x2>two</x2></x0>");
    // words between items join the next item; words after the last join it
    let loose = decode("<x0>lead <x1>eins</x1><x2>zwei</x2> tail</x0>", &shells);
    let Node::List(list) = &loose.nodes[0] else {
        panic!("a list: {loose:?}");
    };
    assert_eq!(list.items.len(), 2);
    assert_eq!(
        RichText {
            nodes: list.items[0].clone(),
            truncated: false
        }
        .plain_text(),
        "lead eins"
    );
    assert!(
        RichText {
            nodes: list.items[1].clone(),
            truncated: false
        }
        .plain_text()
        .contains("tail")
    );
    // an item out of its list gives its words
    assert_eq!(decode("<x1>eins</x1>", &shells).plain_text(), "eins");
    // a list with words but no item
    let bare = decode("<x0>nur Text</x0>", &shells);
    assert!(matches!(&bare.nodes[0], Node::List(list) if list.items.len() == 1));
    // an image lost from a message without a paragraph comes back at its end
    let (_, picture) = encode(&RichText {
        nodes: vec![emoji()],
        truncated: false,
    });
    assert_eq!(decode("nothing", &picture).nodes.last(), Some(&emoji()));
    // a block lost from a paragraph's message joins the paragraph
    let (_, coded) = encode(&RichText {
        nodes: vec![Node::Paragraph(vec![text("see")]), Node::Code(Code::default())],
        truncated: false,
    });
    let restored = decode("<x0>siehe</x0>", &coded);
    assert!(
        matches!(&restored.nodes[0], Node::Paragraph(children) if children.iter().any(|node| matches!(node, Node::Code(_))))
    );
}

#[test]
fn a_message_on_one_line_is_capped() {
    let message = RichText {
        nodes: vec![
            Node::Paragraph(vec![text("first  line")]),
            Node::Paragraph(vec![text("second")]),
        ],
        truncated: false,
    };
    assert_eq!(one_line(&message, 100), "first line second");
    assert_eq!(one_line(&message, 5), "first…");
    assert_eq!(escape("<&>"), "&lt;&amp;&gt;");
    assert_eq!(unescape("&amp;lt;"), "&lt;");
    assert_eq!(Shells::default().len(), 0);
}
