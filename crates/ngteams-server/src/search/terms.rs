//! What a search looks for: the user's text as words, never a query syntax, each matched as the start of a word, case
//! and accents aside ([`folded`]); and the text around a message's match, marked up as Substrate marks it (`<c0>`
//! around a match, `<ddd/>` at a cut).

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// The most words of a query looked for; the rest are left out.
const MAX_TERMS: usize = 16;
/// The longest word looked for, in characters.
const MAX_TERM: usize = 64;
/// How much of a message's text a summary holds, in characters.
const SUMMARY: usize = 200;
/// How much text a summary keeps before its first match, in characters.
const LEAD: usize = 60;
/// What the web client's All tab puts before the query of its message request.
const NOT_DELETED: &str = "NOT (isClientSoftDeleted:TRUE) AND ";

/// The words of a query, lower-cased, each looked for as the start of a word.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Terms(Vec<String>);

impl Terms {
    /// The words of `query`, folded: its runs of letters and digits; an asked-for exclusion of deleted messages is no
    /// word.
    #[must_use]
    pub fn of(query: &str) -> Self {
        let query = query.trim();
        let query = query.strip_prefix(NOT_DELETED).unwrap_or(query);
        let mut terms: Vec<String> = Vec::new();
        for word in words(&folded(query)) {
            let word: String = word.chars().take(MAX_TERM).collect();
            if !terms.contains(&word) && terms.len() < MAX_TERMS {
                terms.push(word);
            }
        }
        Self(terms)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The `LIKE` patterns of the words, one each: a text of folded words holds each word it starts.
    #[must_use]
    pub fn patterns(&self) -> Vec<String> {
        self.0.iter().map(|term| format!("%{term}%")).collect()
    }

    /// Whether every word starts a word of one of `texts`.
    #[must_use]
    pub fn all_in(&self, texts: &[&str]) -> bool {
        let texts: Vec<String> = texts.iter().map(|text| folded(text)).collect();
        self.0.iter().all(|term| {
            texts
                .iter()
                .any(|text| words(text).any(|word| word.starts_with(term.as_str())))
        })
    }

    /// Postgres' `to_tsquery` of the words: each a prefix, all of them.
    #[must_use]
    pub fn ts_query(&self) -> String {
        self.0
            .iter()
            .map(|term| format!("{term}:*"))
            .collect::<Vec<_>>()
            .join(" & ")
    }

    /// FTS5's match expression of the words: each quoted and a prefix, all of them.
    #[must_use]
    pub fn fts_match(&self) -> String {
        self.0
            .iter()
            .map(|term| format!("\"{term}\"*"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `text` around its first match, HTML-escaped, each matched word between `<c0>` and `</c0>`, `<ddd/>` where text
    /// is left out; the text's start when nothing in it matches.
    #[must_use]
    pub fn summary(&self, text: &str) -> String {
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let spans = self.matches(text);
        let first = spans.first().map_or(0, |&(start, _)| start);
        let first = chars.iter().position(|&(at, _)| at >= first).unwrap_or(0);
        let mut from = first.saturating_sub(LEAD);
        while from > 0 && !chars[from - 1].1.is_whitespace() {
            from -= 1;
            if first - from > LEAD * 2 {
                break;
            }
        }
        let to = (from + SUMMARY).min(chars.len());
        let start = chars.get(from).map_or(text.len(), |&(at, _)| at);
        let end = chars.get(to).map_or(text.len(), |&(at, _)| at);
        let mut out = String::new();
        if start > 0 {
            out.push_str("<ddd/>");
        }
        let mut at = start;
        for &(match_start, match_end) in spans.iter().filter(|&&(s, e)| s >= start && e <= end) {
            out.push_str(&escaped(&text[at..match_start]));
            out.push_str("<c0>");
            out.push_str(&escaped(&text[match_start..match_end]));
            out.push_str("</c0>");
            at = match_end;
        }
        out.push_str(&escaped(&text[at..end]));
        if end < text.len() {
            out.push_str("<ddd/>");
        }
        out
    }

    /// The byte spans of the words of `text` a term starts.
    fn matches(&self, text: &str) -> Vec<(usize, usize)> {
        let base = text.as_ptr() as usize;
        words(text)
            .filter(|word| {
                let word = folded(word);
                words(&word).any(|word| self.0.iter().any(|term| word.starts_with(term.as_str())))
            })
            .map(|word| {
                let start = word.as_ptr() as usize - base;
                (start, start + word.len())
            })
            .collect()
    }
}

/// The words of `text` as the index holds them: its runs of letters and digits, folded, apart by a space.
#[must_use]
pub fn indexed(text: &str) -> String {
    words(&folded(text)).collect::<Vec<_>>().join(" ")
}

/// `text` as the search compares it: lower-cased, its compatibility forms decomposed (NFKD), its Latin letters
/// without their accents and with the letters that have none as their base (`ø` as `o`, `ß` as `ss`); so a word
/// typed without accents finds what was written with them, the same on both backends. Other scripts keep their
/// marks: they change the letter, not its accent.
#[must_use]
pub fn folded(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_latin = false;
    for c in text.nfkd() {
        if after_latin && ('\u{300}'..='\u{36F}').contains(&c) {
            continue;
        }
        after_latin = is_latin(c);
        for c in c.to_lowercase() {
            match c {
                'ø' => out.push('o'),
                'đ' | 'ð' => out.push('d'),
                'ł' => out.push('l'),
                'ħ' => out.push('h'),
                'ŧ' => out.push('t'),
                'ı' => out.push('i'),
                'ß' => out.push_str("ss"),
                'æ' => out.push_str("ae"),
                'œ' => out.push_str("oe"),
                'þ' => out.push_str("th"),
                c => out.push(c),
            }
        }
    }
    out.nfc().collect()
}

/// Whether `c` is a letter of the Latin script.
fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic() || ('\u{C0}'..='\u{24F}').contains(&c) || ('\u{1E00}'..='\u{1EFF}').contains(&c)
}

/// The runs of letters and digits of `text`, with the marks on them.
fn words(text: &str) -> impl Iterator<Item = &str> + Clone {
    text.split(|c: char| !(c.is_alphanumeric() || is_combining_mark(c)))
        .filter(|word| !word.is_empty())
}

/// `text` with `&`, `<`, `>` and `"` as entities.
#[must_use]
pub fn escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Terms, folded, indexed};

    #[test]
    fn a_query_is_its_words_never_a_syntax() {
        let terms = Terms::of("NOT (isClientSoftDeleted:TRUE) AND \"Rocket* OR  rocket NOT (");
        assert_eq!(terms.ts_query(), "rocket:* & or:* & not:*");
        assert_eq!(terms.fts_match(), "\"rocket\"* \"or\"* \"not\"*");
        assert!(Terms::of("  -*  ").is_empty());
        assert_eq!(Terms::of(&"word ".repeat(40)).0.len(), 1);
        let many = (0..40).map(|n| format!("w{n}")).collect::<Vec<_>>().join(" ");
        assert_eq!(Terms::of(&many).0.len(), 16);
        assert_eq!(Terms::of(&"x".repeat(100)).0[0].len(), 64);
    }

    #[test]
    fn words_match_at_their_start() {
        let terms = Terms::of("bert s");
        assert!(terms.all_in(&["Bert Sample"]));
        assert!(terms.all_in(&["Ada", "bert@own.test", "Sam"]));
        assert!(!terms.all_in(&["Robert Jones"]));
        assert_eq!(terms.patterns(), ["%bert%", "%s%"]);
    }

    #[test]
    fn a_summary_marks_the_matches_and_its_cuts() {
        let terms = Terms::of("rock");
        assert_eq!(
            terms.summary("The <rocket> & the Rockies"),
            "The &lt;<c0>rocket</c0>&gt; &amp; the <c0>Rockies</c0>"
        );
        let long = format!("{} rocket {}", "lead ".repeat(40), "tail ".repeat(60));
        let summary = terms.summary(&long);
        assert!(summary.starts_with("<ddd/>lead "), "{summary}");
        assert!(summary.ends_with("<ddd/>"), "{summary}");
        assert!(summary.contains("<c0>rocket</c0>"));
        assert_eq!(Terms::of("zz").summary("plain \"text\""), "plain &quot;text&quot;");
        assert_eq!(terms.summary(""), "");
    }

    #[test]
    fn accents_and_case_fold_away_in_latin_only() {
        assert_eq!(folded("Zoë Ångström-Øvergaard"), "zoe angstrom-overgaard");
        assert_eq!(
            folded("Crème BRÛLÉE, Straße, Æsir, Œuvre, Þór, Łódź, Đorđe"),
            "creme brulee, strasse, aesir, oeuvre, thor, lodz, dorde"
        );
        assert_eq!(folded("Cafe\u{301} ﬁne ２"), "cafe fine 2");
        assert_eq!(folded("Йод ё が 한국 🚀"), "йод ё が 한국 🚀");
        assert_eq!(indexed("Café Ünïcode 🚀 ☕"), "cafe unicode");
        let terms = Terms::of("CAFE unicode");
        assert!(terms.all_in(&["Café Ünïcode"]));
        assert!(Terms::of("angstrom overg").all_in(&["Zoë Ångström-Øvergaard"]));
        assert!(Terms::of("Ångs").all_in(&["Zoe Angstrom"]));
        assert!(!Terms::of("йод").all_in(&["иод"]));
        assert_eq!(
            Terms::of("creme brulee").summary("Crème brûlée for dessert at the cafe\u{301}"),
            "<c0>Crème</c0> <c0>brûlée</c0> for dessert at the cafe\u{301}"
        );
        assert_eq!(
            Terms::of("cafe").summary("the cafe\u{301}!"),
            "the <c0>cafe\u{301}</c0>!"
        );
    }
}
