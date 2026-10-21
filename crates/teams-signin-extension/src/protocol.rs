//! The guard's wire: one line each way per question, on a Unix stream socket from each web process to
//! `teams-signin`'s guard.
//!
//! - `armed <page id> <pid>`: the page's `send-request` handler is connected; answered `ok`.
//! - `request <page id> <method> <url>`: may this request go? Answered `allow` or `deny`.
//!
//! Anything else is answered `deny`, and the extension takes any answer but the expected one as a deny. A field never
//! holds whitespace: a method or URL that does (`WebKit` serializes URLs with spaces escaped, so only a malformed one
//! would) is denied by the extension without asking.

/// The longest line the guard reads (a URL with its query: the authorize URL is a few kilobytes).
pub const LINE_LIMIT: usize = 64 * 1024;
pub const OK: &str = "ok";
pub const ALLOW: &str = "allow";
pub const DENY: &str = "deny";

/// One question from a web process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Query {
    Armed { page: u64, pid: u32 },
    Request { page: u64, method: String, url: String },
}

impl Query {
    /// The question as one line, without its newline; `None` when a field holds whitespace or is empty, or the line
    /// would be longer than [`LINE_LIMIT`].
    #[must_use]
    pub fn to_line(&self) -> Option<String> {
        let line = match self {
            Self::Armed { page, pid } => format!("armed {page} {pid}"),
            Self::Request { page, method, url } => {
                if !is_field(method) || !is_field(url) {
                    return None;
                }
                format!("request {page} {method} {url}")
            }
        };
        (line.len() <= LINE_LIMIT).then_some(line)
    }

    /// Reads one line (without its newline); `None` for anything that isn't exactly one of the two forms.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        if line.len() > LINE_LIMIT {
            return None;
        }
        let mut fields = line.split(' ');
        let query = match (fields.next()?, fields.next()?, fields.next()?, fields.next()) {
            ("armed", page, pid, None) => Self::Armed {
                page: number(page)?,
                pid: number(pid)?,
            },
            ("request", page, method, Some(url)) if is_field(method) && is_field(url) => Self::Request {
                page: number(page)?,
                method: method.to_owned(),
                url: url.to_owned(),
            },
            _ => return None,
        };
        fields.next().is_none().then_some(query)
    }
}

/// Non-empty, no whitespace or control character.
fn is_field(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Decimal digits only (no sign, no leading `+`).
fn number<T: std::str::FromStr>(text: &str) -> Option<T> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_round_trip() {
        for query in [
            Query::Armed { page: 7, pid: 4242 },
            Query::Request {
                page: 7,
                method: "POST".into(),
                url: "https://login.example.test/common/login?x=1".into(),
            },
        ] {
            let line = query.to_line().unwrap();
            assert_eq!(Query::parse(&line), Some(query));
        }
    }

    #[test]
    fn a_field_with_whitespace_or_an_odd_line_is_refused() {
        let spaced = Query::Request {
            page: 1,
            method: "GET".into(),
            url: "https://a.test/x y".into(),
        };
        assert_eq!(spaced.to_line(), None);
        let newline = Query::Request {
            page: 1,
            method: "GE\nT".into(),
            url: "https://a.test/".into(),
        };
        assert_eq!(newline.to_line(), None);
        for line in [
            "",
            "armed 1",
            "armed 1 2 3",
            "armed -1 2",
            "armed +1 2",
            "request 1 GET",
            "request 1 GET https://a.test/ extra",
            "request x GET https://a.test/",
            "request 1  https://a.test/",
            "hello 1 2",
        ] {
            assert_eq!(Query::parse(line), None, "{line:?}");
        }
        let long = format!("request 1 GET https://a.test/{}", "x".repeat(LINE_LIMIT));
        assert_eq!(Query::parse(&long), None);
        let too_long = Query::Request {
            page: 1,
            method: "GET".into(),
            url: format!("https://a.test/{}", "x".repeat(LINE_LIMIT)),
        };
        assert_eq!(too_long.to_line(), None);
    }
}
