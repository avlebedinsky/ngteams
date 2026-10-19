//! The string literals of a JavaScript source, found by a small lexer.
//!
//! It knows only what it needs to tell a string from everything else in minified code: `'…'` and `"…"` strings,
//! template literals with nested `${…}` expressions (scanned as code, so literals inside them are found too),
//! comments, and regular expression literals, told apart from division by the token before the `/`. Escapes are
//! decoded, so `https:\/\/host` reads as `https://host`; each `${…}` of a template becomes `{}`.
//!
//! Nothing is evaluated. Input that isn't valid JavaScript never panics: an unterminated string ends at its line
//! break and is dropped, an unterminated template or comment at the end of the input.

/// One string literal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Literal {
    /// The byte offset of the opening quote or backtick.
    pub start: usize,
    /// The byte offset just past the closing quote or backtick.
    pub end: usize,
    /// The decoded text; a template's `${…}` parts are `{}`.
    pub text: String,
    /// A template literal (backticks).
    pub template: bool,
}

/// The string literals of a JavaScript (or JSON) source, in the order they close.
#[must_use]
pub fn literals(source: &str) -> Vec<Literal> {
    Lexer::new(source, Syntax::Script).run()
}

/// The quoted strings of a markup (HTML) source: the same lexer without comments and regular expressions, which
/// markup text such as `</title>` or `https://` outside quotes would otherwise start.
#[must_use]
pub fn markup_literals(source: &str) -> Vec<Literal> {
    Lexer::new(source, Syntax::Markup).run()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    Script,
    Markup,
}

/// Words after which a `/` starts a regular expression rather than a division.
const REGEX_AFTER: [&[u8]; 14] = [
    b"return",
    b"typeof",
    b"instanceof",
    b"in",
    b"of",
    b"new",
    b"delete",
    b"void",
    b"throw",
    b"case",
    b"do",
    b"else",
    b"yield",
    b"await",
];

/// An open template literal.
struct Template {
    start: usize,
    text: Vec<u8>,
    /// `Some(depth)` while inside one of its `${…}` expressions, counting the `{` opened there.
    expression: Option<usize>,
}

struct Lexer<'a> {
    bytes: &'a [u8],
    syntax: Syntax,
    pos: usize,
    /// Whether a `/` here would start a regular expression.
    regex_ok: bool,
    templates: Vec<Template>,
    found: Vec<Literal>,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, syntax: Syntax) -> Self {
        Self {
            bytes: source.as_bytes(),
            syntax,
            pos: 0,
            regex_ok: true,
            templates: Vec::new(),
            found: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<Literal> {
        while self.pos < self.bytes.len() {
            match self.templates.last() {
                Some(template) if template.expression.is_none() => self.template_text(),
                _ => self.code(),
            }
        }
        self.found
    }

    fn code(&mut self) {
        let byte = self.bytes[self.pos];
        match byte {
            b'"' | b'\'' => self.string(byte),
            b'`' => {
                self.templates.push(Template {
                    start: self.pos,
                    text: Vec::new(),
                    expression: None,
                });
                self.pos += 1;
            }
            b'/' if self.syntax == Syntax::Script => self.slash(),
            b'{' => {
                if let Some(depth) = self.expression_depth() {
                    *depth += 1;
                }
                self.punctuation();
            }
            b'}' => self.close_brace(),
            b')' | b']' => {
                self.pos += 1;
                self.regex_ok = false;
            }
            _ if is_word(byte) => self.word(),
            _ if byte.is_ascii_whitespace() => self.pos += 1,
            _ => self.punctuation(),
        }
    }

    fn punctuation(&mut self) {
        self.pos += 1;
        self.regex_ok = true;
    }

    fn expression_depth(&mut self) -> Option<&mut usize> {
        self.templates
            .last_mut()
            .and_then(|template| template.expression.as_mut())
    }

    fn close_brace(&mut self) {
        if let Some(template) = self.templates.last_mut() {
            match template.expression {
                Some(0) => template.expression = None,
                Some(depth) => template.expression = Some(depth - 1),
                None => {}
            }
        }
        self.punctuation();
    }

    fn word(&mut self) {
        let start = self.pos;
        while self.pos < self.bytes.len() && is_word(self.bytes[self.pos]) {
            self.pos += 1;
        }
        self.regex_ok = REGEX_AFTER.contains(&&self.bytes[start..self.pos]);
    }

    fn string(&mut self, quote: u8) {
        let start = self.pos;
        let mut text = Vec::new();
        self.pos += 1;
        self.regex_ok = false;
        while let Some(&byte) = self.bytes.get(self.pos) {
            match byte {
                _ if byte == quote => {
                    self.pos += 1;
                    self.emit(start, &text, false);
                    return;
                }
                b'\\' => self.pos = unescape(self.bytes, self.pos, &mut text),
                // an unterminated string: drop it and go on from the line break
                b'\n' => return,
                _ => {
                    text.push(byte);
                    self.pos += 1;
                }
            }
        }
    }

    fn template_text(&mut self) {
        let byte = self.bytes[self.pos];
        let next = self.bytes.get(self.pos + 1).copied();
        let Some(template) = self.templates.last_mut() else {
            return;
        };
        match byte {
            b'`' => {
                self.pos += 1;
                self.regex_ok = false;
                if let Some(template) = self.templates.pop() {
                    self.emit(template.start, &template.text, true);
                }
            }
            b'\\' => self.pos = unescape(self.bytes, self.pos, &mut template.text),
            b'$' if next == Some(b'{') => {
                template.text.extend_from_slice(b"{}");
                template.expression = Some(0);
                self.pos += 2;
                self.regex_ok = true;
            }
            _ => {
                template.text.push(byte);
                self.pos += 1;
            }
        }
    }

    fn slash(&mut self) {
        match self.bytes.get(self.pos + 1) {
            Some(b'/') => self.skip_past(b"\n"),
            Some(b'*') => {
                self.pos += 2;
                self.skip_past(b"*/");
            }
            _ if self.regex_ok => self.skip_regex(),
            _ => self.punctuation(),
        }
    }

    /// Moves past the next `end`, or to the end of the input.
    fn skip_past(&mut self, end: &[u8]) {
        self.pos = self.bytes[self.pos..]
            .windows(end.len())
            .position(|window| window == end)
            .map_or(self.bytes.len(), |found| self.pos + found + end.len());
    }

    fn skip_regex(&mut self) {
        let mut in_class = false;
        self.pos += 1;
        self.regex_ok = false;
        while let Some(&byte) = self.bytes.get(self.pos) {
            self.pos += 1;
            match byte {
                b'\\' => self.pos += 1,
                // not a regular expression after all; go on from the line break
                b'\n' => return,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => {
                    while self.pos < self.bytes.len() && is_word(self.bytes[self.pos]) {
                        self.pos += 1;
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    fn emit(&mut self, start: usize, text: &[u8], template: bool) {
        self.found.push(Literal {
            start,
            end: self.pos,
            text: String::from_utf8_lossy(text).into_owned(),
            template,
        });
    }
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// Decodes the escape at `bytes[pos]` (a backslash) into `text`; returns the position after it.
fn unescape(bytes: &[u8], pos: usize, text: &mut Vec<u8>) -> usize {
    let Some(&escaped) = bytes.get(pos + 1) else {
        return bytes.len();
    };
    let simple = match escaped {
        b'n' => Some(b'\n'),
        b't' => Some(b'\t'),
        b'r' => Some(b'\r'),
        b'b' => Some(0x08),
        b'f' => Some(0x0c),
        b'v' => Some(0x0b),
        b'0' => Some(0),
        // a line continuation
        b'\n' => return pos + 2,
        b'x' | b'u' => None,
        other => Some(other),
    };
    if let Some(byte) = simple {
        text.push(byte);
        return pos + 2;
    }
    let (code, next) = if escaped == b'x' {
        hex(bytes, pos + 2, 2)
    } else if bytes.get(pos + 2) == Some(&b'{') {
        let close = bytes[pos + 3..].iter().position(|&byte| byte == b'}');
        close.map_or((None, pos + 2), |close| {
            (hex(bytes, pos + 3, close).0, pos + 3 + close + 1)
        })
    } else {
        hex(bytes, pos + 2, 4)
    };
    match code.and_then(char::from_u32) {
        Some(decoded) => text.extend_from_slice(decoded.encode_utf8(&mut [0; 4]).as_bytes()),
        // a malformed escape keeps its letter, as JavaScript would refuse it anyway
        None => text.push(escaped),
    }
    next
}

/// Reads `len` hex digits at `start`; returns the value (if they are all hex) and the position after them.
fn hex(bytes: &[u8], start: usize, len: usize) -> (Option<u32>, usize) {
    let Some(digits) = bytes.get(start..start + len) else {
        return (None, start);
    };
    let value = std::str::from_utf8(digits)
        .ok()
        .filter(|digits| !digits.is_empty() && digits.len() <= 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .and_then(|digits| u32::from_str_radix(digits, 16).ok());
    match value {
        Some(value) => (Some(value), start + len),
        None => (None, start),
    }
}
