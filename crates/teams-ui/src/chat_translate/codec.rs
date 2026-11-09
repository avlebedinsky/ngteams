//! A message's rich text as the translator reads and writes it: its words, with its formatting as numbered markers
//! ([`encode`]), and back ([`decode`]).
//!
//! - **Markers**: what wraps words (a paragraph, a mark such as bold, a link, a coloured span, a heading, a quote, a
//!   list and each of its items) is `<xN>…</xN>`; what stands on its own and is never translated (a mention, an
//!   image or emoji, a block of code, a line break, a rule) is `<xN/>`. The translator keeps each marker once around
//!   the words it wraps in its translation; each is put back as the node it stood for, from the message's own
//!   ([`Shells`]), so a link's address, a mention's person or a picture never pass through the translator's hands.
//! - **Text** is escaped (`&`, `<`, `>`) so typed angle brackets aren't taken for markers, and unescaped on the way
//!   back.
//! - **Forgiving to read**: a marker that names nothing, a close without its open or a second copy of an atom is
//!   dropped; an open left unclosed is closed at the end; an atom the answer lost is put back at the end of the text,
//!   so a mention is never lost (a message is sent only with each of its mentions once). What comes back is the same
//!   model the message was, drawn only through the sanitized [`RichText`].

use std::fmt::Write as _;

use teams_proto::chat::{List, Marks, Node, RichText};

/// What each marker of an encoded text stands for, by its number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shells(Vec<Shell>);

/// What one marker stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shell {
    /// Words with these marks.
    Marked(Marks),
    /// A node whose children are the words it wraps (taken out here).
    Wrap(Node),
    /// One item of the list it is in.
    Item,
    /// A node put back as it was.
    Atom(Node),
}

impl Shells {
    /// How many markers there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there is none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn add(&mut self, shell: Shell) -> usize {
        self.0.push(shell);
        self.0.len() - 1
    }
}

/// `text` as the translator reads it, and what its markers stand for.
#[must_use]
pub fn encode(text: &RichText) -> (String, Shells) {
    let mut out = String::new();
    let mut shells = Shells::default();
    nodes(&text.nodes, &mut out, &mut shells);
    (out, shells)
}

fn nodes(list: &[Node], out: &mut String, shells: &mut Shells) {
    for node in list {
        encode_node(node, out, shells);
    }
}

fn wrap(shell: Shell, children: &[Node], out: &mut String, shells: &mut Shells) {
    let n = shells.add(shell);
    let _ = write!(out, "<x{n}>");
    nodes(children, out, shells);
    let _ = write!(out, "</x{n}>");
}

fn encode_node(node: &Node, out: &mut String, shells: &mut Shells) {
    match node {
        Node::Text { text, marks } if *marks == Marks::NONE => out.push_str(&escape(text)),
        Node::Text { text, marks } => {
            let n = shells.add(Shell::Marked(*marks));
            let _ = write!(out, "<x{n}>{}</x{n}>", escape(text));
        }
        Node::Paragraph(children) => wrap(Shell::Wrap(Node::Paragraph(Vec::new())), children, out, shells),
        Node::Link { href, children } => wrap(
            Shell::Wrap(Node::Link {
                href: href.clone(),
                children: Vec::new(),
            }),
            children,
            out,
            shells,
        ),
        Node::Span { style, children } => wrap(
            Shell::Wrap(Node::Span {
                style: *style,
                children: Vec::new(),
            }),
            children,
            out,
            shells,
        ),
        Node::Heading { level, children } => wrap(
            Shell::Wrap(Node::Heading {
                level: *level,
                children: Vec::new(),
            }),
            children,
            out,
            shells,
        ),
        Node::Indented { depth, children } => wrap(
            Shell::Wrap(Node::Indented {
                depth: *depth,
                children: Vec::new(),
            }),
            children,
            out,
            shells,
        ),
        Node::Quote(quote) => {
            let mut shell = quote.clone();
            shell.body = Vec::new();
            wrap(Shell::Wrap(Node::Quote(shell)), &quote.body, out, shells);
        }
        Node::List(list) => {
            let n = shells.add(Shell::Wrap(Node::List(List {
                ordered: list.ordered,
                start: list.start,
                items: Vec::new(),
            })));
            let _ = write!(out, "<x{n}>");
            for item in &list.items {
                wrap(Shell::Item, item, out, shells);
            }
            let _ = write!(out, "</x{n}>");
        }
        Node::Break | Node::Mention { .. } | Node::Image(_) | Node::Code(_) | Node::Rule => {
            let n = shells.add(Shell::Atom(node.clone()));
            let _ = write!(out, "<x{n}/>");
        }
    }
}

/// `text` with `&`, `<` and `>` escaped.
#[must_use]
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `text` with the entities a translator may write unescaped.
#[must_use]
pub fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", "\u{a0}")
        .replace("&amp;", "&")
}

/// One piece of an answer.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Token<'a> {
    Text(&'a str),
    Open(usize),
    Close(usize),
    Atom(usize),
}

/// The answer's markers and the text between them; anything else that looks like a tag is text.
pub(crate) fn tokens(answer: &str) -> Vec<Token<'_>> {
    let mut found = Vec::new();
    let mut rest = answer;
    let mut text_start = 0;
    let mut at = 0;
    while let Some(lt) = rest.find('<') {
        let tail = &rest[lt..];
        if let Some((token, len)) = marker(tail) {
            let start = at + lt;
            if start > text_start {
                found.push(Token::Text(&answer[text_start..start]));
            }
            found.push(token);
            text_start = start + len;
            at = text_start;
            rest = &answer[at..];
        } else {
            at += lt + 1;
            rest = &answer[at..];
        }
    }
    if text_start < answer.len() {
        found.push(Token::Text(&answer[text_start..]));
    }
    found
}

/// The marker `tail` starts with, and its length.
fn marker(tail: &str) -> Option<(Token<'_>, usize)> {
    let (close, body) = match tail.strip_prefix("</x") {
        Some(body) => (true, body),
        None => (false, tail.strip_prefix("<x")?),
    };
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 6 {
        return None;
    }
    let n: usize = body[..digits].parse().ok()?;
    let after = &body[digits..];
    let lead = if close { 3 } else { 2 };
    if after.starts_with('>') {
        let token = if close { Token::Close(n) } else { Token::Open(n) };
        return Some((token, lead + digits + 1));
    }
    let spaced = after.trim_start_matches(' ');
    if !close && spaced.starts_with("/>") {
        let len = lead + digits + (after.len() - spaced.len()) + 2;
        return Some((Token::Atom(n), len));
    }
    None
}

/// A node being built from an answer: the marker it came from and what is in it so far.
struct Open {
    n: usize,
    children: Vec<Node>,
    /// A list's items so far.
    items: Vec<Vec<Node>>,
}

impl Open {
    fn new(n: usize) -> Self {
        Self {
            n,
            children: Vec::new(),
            items: Vec::new(),
        }
    }
}

/// The rich text a translator's `answer` gives, its markers put back as the nodes `shells` holds (see the module
/// docs).
#[must_use]
pub fn decode(answer: &str, shells: &Shells) -> RichText {
    let mut stack: Vec<Open> = vec![Open::new(usize::MAX)];
    let mut used = vec![false; shells.len()];
    for token in tokens(answer) {
        match token {
            Token::Text(text) => {
                let marks = stack
                    .iter()
                    .rev()
                    .find_map(|open| match shells.0.get(open.n) {
                        Some(Shell::Marked(marks)) => Some(*marks),
                        _ => None,
                    })
                    .unwrap_or(Marks::NONE);
                push_text(top(&mut stack), &unescape(text), marks);
            }
            Token::Open(n) => {
                if matches!(shells.0.get(n), Some(Shell::Marked(_) | Shell::Wrap(_) | Shell::Item))
                    && !used.get(n).copied().unwrap_or(true)
                {
                    used[n] = true;
                    stack.push(Open::new(n));
                }
            }
            Token::Close(n) => close_to(n, shells, &mut stack),
            Token::Atom(n) => {
                if let Some(Shell::Atom(node)) = shells.0.get(n)
                    && !used[n]
                {
                    used[n] = true;
                    top(&mut stack).children.push(node.clone());
                }
            }
        }
    }
    while stack.len() > 1 {
        if let Some(open) = stack.pop() {
            close(open, shells, &mut stack);
        }
    }
    let mut built = stack.pop().map(|root| root.children).unwrap_or_default();
    restore_lost_atoms(&mut built, shells, &used);
    RichText {
        nodes: built,
        truncated: false,
    }
}

/// Closes what is open down to marker `n`, when it is open (a close without its open is dropped).
fn close_to(n: usize, shells: &Shells, stack: &mut Vec<Open>) {
    if !stack.iter().skip(1).any(|open| open.n == n) {
        return;
    }
    while let Some(open) = stack.pop() {
        let done = open.n == n;
        close(open, shells, stack);
        if done {
            return;
        }
    }
}

fn top(stack: &mut [Open]) -> &mut Open {
    let last = stack.len() - 1;
    &mut stack[last]
}

fn push_text(open: &mut Open, text: &str, marks: Marks) {
    if text.is_empty() {
        return;
    }
    if let Some(Node::Text {
        text: before,
        marks: before_marks,
    }) = open.children.last_mut()
        && *before_marks == marks
    {
        before.push_str(text);
        return;
    }
    open.children.push(Node::Text {
        text: text.to_owned(),
        marks,
    });
}

/// `open` finished: its node goes into the one around it.
fn close(open: Open, shells: &Shells, stack: &mut [Open]) {
    let Some(parent) = stack.last_mut() else {
        return;
    };
    let in_list = matches!(shells.0.get(parent.n), Some(Shell::Wrap(Node::List(_))));
    match shells.0.get(open.n) {
        Some(Shell::Item) if in_list => {
            // what came before the item in its list joins it
            let mut item = std::mem::take(&mut parent.children);
            item.extend(open.children);
            parent.items.push(item);
        }
        Some(Shell::Wrap(Node::List(list))) => {
            let mut items = open.items;
            if !open.children.is_empty() {
                match items.last_mut() {
                    Some(item) => item.extend(open.children),
                    None => items.push(open.children),
                }
            }
            parent.children.push(Node::List(List {
                ordered: list.ordered,
                start: list.start,
                items,
            }));
        }
        Some(Shell::Wrap(shell)) => parent.children.push(fill(shell.clone(), open.children)),
        // a mark's words were marked as they came; an item out of its list gives its words
        Some(Shell::Marked(_) | Shell::Item | Shell::Atom(_)) | None => parent.children.extend(open.children),
    }
}

/// `shell` with `children` in it.
fn fill(shell: Node, children: Vec<Node>) -> Node {
    match shell {
        Node::Paragraph(_) => Node::Paragraph(children),
        Node::Link { href, .. } => Node::Link { href, children },
        Node::Span { style, .. } => Node::Span { style, children },
        Node::Heading { level, .. } => Node::Heading { level, children },
        Node::Indented { depth, .. } => Node::Indented { depth, children },
        Node::Quote(mut quote) => {
            quote.body = children;
            Node::Quote(quote)
        }
        other => other,
    }
}

/// Puts back at the end what the answer lost of what can't be lost: a mention, an image or emoji, a block of code.
fn restore_lost_atoms(built: &mut Vec<Node>, shells: &Shells, used: &[bool]) {
    let lost: Vec<Node> = shells
        .0
        .iter()
        .zip(used)
        .filter_map(|(shell, used)| match shell {
            Shell::Atom(node @ (Node::Mention { .. } | Node::Image(_) | Node::Code(_))) if !used => Some(node.clone()),
            _ => None,
        })
        .collect();
    if lost.is_empty() {
        return;
    }
    // inline ones join the last paragraph when there is one
    match built.last_mut() {
        Some(Node::Paragraph(children)) => {
            for node in lost {
                if node.is_block() {
                    children.push(node);
                } else {
                    children.push(Node::Text {
                        text: " ".to_owned(),
                        marks: Marks::NONE,
                    });
                    children.push(node);
                }
            }
        }
        _ => built.extend(lost),
    }
}

/// The plain text a message shows, on one line, at most `max` bytes: what context lines and matches are made of.
#[must_use]
pub fn one_line(text: &RichText, max: usize) -> String {
    let plain = text.plain_text();
    let joined = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = crate::local_ai::cap(&joined, max);
    if line.len() < joined.len() {
        line.push('…');
    }
    line
}

#[cfg(test)]
#[path = "../../tests/support/chat_translate_codec_units.rs"]
mod tests;
