//! A block of code in a message (`pre`), as Teams' code block editor sends one (`docs/protocol/chat.md`, "Code
//! blocks"): `<pre class="language-<id>[ language-was-…-selected]" itemid="codeBlockEditor-<guid>"><code>…</code></pre>`,
//! its lines split by `<br>`, its spaces `&nbsp;`, its tokens in `<span class="hljs-<kind>">`, after a paragraph of the
//! editor's own (`<p itemtype="http://schema.skype.com/CodeBlockEditor">&nbsp;</p>`) that a reader never sees. Older
//! and pasted ones are a bare `pre` (with or without a language class), their lines `<br>` or line feeds.
//!
//! Only the language's id (a short run of letters, digits, `+`, `#`, `-` and `_`) and each token's kind (from a closed
//! set of highlight.js classes) are kept of the markup: no other attribute reaches the model.

use serde::{Deserialize, Serialize};

/// The `itemtype` of the code block editor's own paragraph before its `pre`.
pub const CODE_BLOCK_ITEMTYPE: &str = "http://schema.skype.com/CodeBlockEditor";

/// The longest language id kept.
const MAX_LANGUAGE: usize = 32;

/// A block of code: the language its `pre`'s class names, and its text in runs of one token kind each, a line feed
/// between its lines.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code {
    /// The language id its `pre`'s first `language-<id>` class names, lowercased (`json`, `bash`, `plaintext`).
    pub language: Option<String>,
    pub runs: Vec<CodeRun>,
}

/// A run of a code block's text and the kind of token it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeRun {
    pub text: String,
    pub token: Token,
}

/// The kind of a token in a code block, as its highlight.js class names it: those the captures showed (`string`,
/// `function`, `keyword`, `comment`, `default`) and the few others the editor's languages commonly give; any other
/// class is [`Token::Plain`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Token {
    #[default]
    Plain,
    Keyword,
    String,
    /// A function's or a property's name (Teams' editor marks a JSON key so).
    Function,
    Comment,
    Number,
}

impl Token {
    /// The kind a `span`'s `class` names: its first `hljs-<kind>` class; `None` without one.
    #[must_use]
    pub fn of_class(class: &str) -> Option<Self> {
        let kind = class
            .split_ascii_whitespace()
            .find_map(|name| name.strip_prefix("hljs-"))?;
        Some(match kind {
            "keyword" | "built_in" | "literal" | "type" | "selector-tag" => Self::Keyword,
            "string" | "regexp" | "symbol" => Self::String,
            "function" | "title" | "attr" | "attribute" | "property" => Self::Function,
            "comment" | "quote" => Self::Comment,
            "number" => Self::Number,
            _ => Self::Plain,
        })
    }
}

impl Code {
    /// Its text as a reader copies it.
    #[must_use]
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }

    /// Adds `text` as `token`: a no-break space as a space and a CRLF or CR as a line feed (the code as it is copied),
    /// joined to the run before when it is of the same kind.
    pub(crate) fn push(&mut self, text: &str, token: Token) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n").replace('\u{a0}', " ");
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.token == token => last.text.push_str(&text),
            _ => self.runs.push(CodeRun { text, token }),
        }
    }

    /// Drops the one line feed at its end (a browser draws no empty line after it) and the runs left empty.
    pub(crate) fn finish(&mut self) {
        if let Some(last) = self.runs.last_mut()
            && last.text.ends_with('\n')
        {
            last.text.pop();
        }
        self.runs.retain(|run| !run.text.is_empty());
    }
}

/// The language a `pre`'s `class` names: its first `language-<id>` class's id, lowercased, when it is 1 to
/// [`MAX_LANGUAGE`] letters, digits, `+`, `#`, `-` or `_`.
#[must_use]
pub fn language_of(class: &str) -> Option<String> {
    let id = class
        .split_ascii_whitespace()
        .find_map(|name| name.strip_prefix("language-"))?;
    let shaped = !id.is_empty()
        && id.len() <= MAX_LANGUAGE
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'#' | b'-' | b'_'));
    shaped.then(|| id.to_ascii_lowercase())
}
