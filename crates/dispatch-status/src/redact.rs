//! Every string the page shows from a transcript or the ledger goes through [`shown`]: token-like values redacted,
//! whitespace collapsed, clipped.

/// Put in place of a redacted value.
const REDACTED: &str = "[redacted]";

/// A key whose value is a whole header to the end of its line or quote.
const HEADER_KEYS: [&str; 4] = ["authorization", "proxy-authorization", "cookie", "set-cookie"];

/// A key containing one of these names a secret value (and `sig` and `*signature*`: [`names_secret`]).
const SECRET_KEY_PARTS: [&str; 12] = [
    "token",
    "secret",
    "passw",
    "pwd",
    "apikey",
    "api_key",
    "api-key",
    "credential",
    "tempauth",
    "code_verifier",
    "refresh",
    "cookie",
];

/// The shortest run of opaque characters taken for a token.
const OPAQUE_MIN: usize = 40;

/// `text` redacted, its whitespace collapsed to single spaces, and clipped to `max` characters.
pub fn shown(text: &str, max: usize) -> String {
    clip(&redact(text), max)
}

/// `text` with every token-like value replaced by `[redacted]`.
pub fn redact(text: &str) -> String {
    let text = keyed_values(text);
    let text = bearer_values(&text);
    let text = secret_flags(&text);
    opaque_runs(&text)
}

/// The value after a command-line flag that names a secret (`--password x`, `--token x`, `--client-secret x`); a
/// `--flag=value` is [`keyed_values`]'s.
fn secret_flags(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || bytes[index - 1].is_ascii_whitespace();
        if !(at_word_start && bytes[index] == b'-') {
            index += 1;
            continue;
        }
        let flag_end = word_end(bytes, index);
        let flag = text[index..flag_end].trim_start_matches('-').to_ascii_lowercase();
        let mut start = flag_end;
        while start < bytes.len() && (bytes[start] == b' ' || bytes[start] == b'\t') {
            start += 1;
        }
        if !flag.contains('=') && names_secret(&flag) && start > flag_end && start < bytes.len() && bytes[start] != b'-'
        {
            let end = match bytes[start] {
                quote @ (b'"' | b'\'') => bytes[start + 1..]
                    .iter()
                    .position(|&byte| byte == quote)
                    .map_or(bytes.len(), |offset| start + offset + 2),
                _ => word_end(bytes, start),
            };
            out.push_str(&text[copied..start]);
            out.push_str(REDACTED);
            copied = end;
            index = end;
            continue;
        }
        index = flag_end;
    }
    out.push_str(&text[copied..]);
    out
}

/// Whether a (lowercase) key or flag name names a secret: one of [`SECRET_KEY_PARTS`] in it, or a signature (`sig`,
/// `x_sig`, `*signature*`; not `signoff`, `signal` or `assignee`).
fn names_secret(key: &str) -> bool {
    SECRET_KEY_PARTS.iter().any(|part| key.contains(part))
        || key == "sig"
        || key.ends_with("_sig")
        || key.ends_with("-sig")
        || key.contains("signature")
}

/// Where the word starting at `start` ends: the next whitespace, or the end.
fn word_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(u8::is_ascii_whitespace)
        .map_or(bytes.len(), |offset| start + offset)
}

/// Whitespace collapsed, trimmed, and at most `max` characters (an ellipsis marking a cut).
pub fn clip(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// The values of `key=value` and `key: value` pairs whose key names a header or a secret.
fn keyed_values(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let separator = byte == b'=' || (byte == b':' && !is_double_colon(bytes, index));
        if separator && let Some(kind) = key_before(bytes, index) {
            let (start, end) = value_after(bytes, index + 1, kind);
            if end > start {
                out.push_str(&text[copied..start]);
                out.push_str(REDACTED);
                copied = end;
                index = end;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

fn is_double_colon(bytes: &[u8], index: usize) -> bool {
    bytes.get(index + 1) == Some(&b':') || (index > 0 && bytes[index - 1] == b':')
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyKind {
    /// A header: its value runs to the end of the line or the quote around it.
    Header,
    /// A secret: its value is one word or one quoted string.
    Secret,
}

/// Whether the key just before the separator at `separator` (spaces and one closing quote between them allowed) names
/// a header or a secret.
fn key_before(bytes: &[u8], separator: usize) -> Option<KeyKind> {
    let mut end = separator;
    while end > 0 && bytes[end - 1] == b' ' {
        end -= 1;
    }
    if end > 0 && matches!(bytes[end - 1], b'"' | b'\'') {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_key_byte(bytes[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    let key = String::from_utf8_lossy(&bytes[start..end]).to_ascii_lowercase();
    let key = key.trim_start_matches('-');
    if HEADER_KEYS.contains(&key) {
        return Some(KeyKind::Header);
    }
    names_secret(key).then_some(KeyKind::Secret)
}

/// The byte range of the value that starts after `from`: a quoted string, a header's rest of line, or one word.
fn value_after(bytes: &[u8], from: usize, kind: KeyKind) -> (usize, usize) {
    let mut start = from;
    while start < bytes.len() && bytes[start] == b' ' {
        start += 1;
    }
    match bytes.get(start) {
        Some(&quote @ (b'"' | b'\'')) => {
            let inner = start + 1;
            let end = bytes[inner..]
                .iter()
                .position(|&byte| byte == quote || byte == b'\n')
                .map_or(bytes.len(), |offset| inner + offset);
            (inner, end)
        }
        Some(_) => {
            let stops: &[u8] = match kind {
                KeyKind::Header => b"\"'\n",
                KeyKind::Secret => b" \t\n&;,'\"<>)]}",
            };
            let end = bytes[start..]
                .iter()
                .position(|byte| stops.contains(byte))
                .map_or(bytes.len(), |offset| start + offset);
            (start, end)
        }
        None => (start, start),
    }
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

/// The token after `Bearer ` (case-folded, spaces between), when it is at least 8 characters long.
fn bearer_values(text: &str) -> String {
    const BEARER: &[u8] = b"bearer";
    let lower = text.to_ascii_lowercase().into_bytes();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        let at_word_start = index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
        if at_word_start && lower[index..].starts_with(BEARER) {
            let gap = index + BEARER.len();
            let mut start = gap;
            while start < bytes.len() && bytes[start] == b' ' {
                start += 1;
            }
            let end = bytes[start..]
                .iter()
                .position(|&byte| !is_token_byte(byte))
                .map_or(bytes.len(), |offset| start + offset);
            if start > gap && end - start >= 8 {
                out.push_str(&text[copied..start]);
                out.push_str(REDACTED);
                copied = end;
                index = end;
                continue;
            }
        }
        index += 1;
    }
    out.push_str(&text[copied..]);
    out
}

/// JWTs and long opaque runs.
fn opaque_runs(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while index < bytes.len() {
        if !is_token_byte(bytes[index]) {
            index += 1;
            continue;
        }
        let end = bytes[index..]
            .iter()
            .position(|&byte| !is_token_byte(byte))
            .map_or(bytes.len(), |offset| index + offset);
        let run = &text[index..end];
        let redacted = redact_run(run);
        if redacted != run {
            out.push_str(&text[copied..index]);
            out.push_str(&redacted);
            copied = end;
        }
        index = end;
    }
    out.push_str(&text[copied..]);
    out
}

/// One run of token characters: a JWT in it, a long opaque piece of it (between `/` and `.`), or the whole run when
/// it is plain base64.
fn redact_run(run: &str) -> String {
    if run.len() >= OPAQUE_MIN && is_base64(run) {
        return REDACTED.to_owned();
    }
    let mut out = String::with_capacity(run.len());
    let mut rest = run;
    while !rest.is_empty() {
        if let Some(length) = jwt_length(rest) {
            out.push_str(REDACTED);
            rest = &rest[length..];
            continue;
        }
        let piece = rest.find(['/', '.']).map_or(rest.len(), |at| at.max(1));
        let (head, tail) = rest.split_at(piece);
        if head.len() >= OPAQUE_MIN && is_mixed(head) {
            out.push_str(REDACTED);
        } else {
            out.push_str(head);
        }
        rest = tail;
    }
    out
}

/// Standard base64 with a `+` or `/` in it and mixed character classes: no path or identifier looks like that.
fn is_base64(run: &str) -> bool {
    run.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
        && run.bytes().any(|byte| matches!(byte, b'+' | b'/'))
        && is_mixed(run)
        && run.bytes().any(|byte| byte.is_ascii_digit())
}

/// A digit beside letters, or both cases: an identifier or a phrase in snake case is neither.
fn is_mixed(piece: &str) -> bool {
    let digit = piece.bytes().any(|byte| byte.is_ascii_digit());
    let lower = piece.bytes().any(|byte| byte.is_ascii_lowercase());
    let upper = piece.bytes().any(|byte| byte.is_ascii_uppercase());
    (digit && (lower || upper)) || (lower && upper)
}

/// The length of the JWT `text` starts with: `eyJ`, two dot-separated base64url parts of at least 6, and maybe a third.
fn jwt_length(text: &str) -> Option<usize> {
    if !text.starts_with("eyJ") {
        return None;
    }
    let part = |from: usize| {
        text.as_bytes()[from..]
            .iter()
            .position(|&byte| !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
            .map_or(text.len(), |offset| from + offset)
    };
    let first = part(0);
    if first < 9 || text.as_bytes().get(first) != Some(&b'.') {
        return None;
    }
    let second = part(first + 1);
    if second - first - 1 < 6 {
        return None;
    }
    if text.as_bytes().get(second) == Some(&b'.') {
        return Some(part(second + 1));
    }
    Some(second)
}

#[cfg(test)]
mod tests {
    use super::{clip, redact};

    #[test]
    fn redacts_headers_and_keyed_secrets() {
        assert_eq!(
            redact(r#"curl -H "Authorization: Bearer abc.def-123" https://x"#),
            r#"curl -H "Authorization: [redacted]" https://x"#
        );
        assert_eq!(redact("Cookie: a=b; c=d"), "Cookie: [redacted]");
        assert_eq!(redact("x?access_token=abc123&y=2"), "x?access_token=[redacted]&y=2");
        assert_eq!(
            redact(r#"{"refresh_token": "abc"}"#),
            r#"{"refresh_token": "[redacted]"}"#
        );
        assert_eq!(redact("CLIENT_SECRET=s3cr3t ./run"), "CLIENT_SECRET=[redacted] ./run");
        assert_eq!(redact("link?sv=1&sig=abc%2Fdef"), "link?sv=1&sig=[redacted]");
        assert_eq!(
            redact("login --password hunter2 now"),
            "login --password [redacted] now"
        );
        assert_eq!(redact("login --password=hunter2"), "login --password=[redacted]");
        assert_eq!(
            redact("x --token abc --api-key \"a b\" y"),
            "x --token [redacted] --api-key [redacted] y"
        );
        assert_eq!(redact("git commit --signoff -m msg"), "git commit --signoff -m msg");
        assert_eq!(
            redact("git am --signoff 0001-fix.patch"),
            "git am --signoff 0001-fix.patch"
        );
        assert_eq!(redact("gh pr edit --assignee me"), "gh pr edit --assignee me");
        assert_eq!(redact("u?X-Amz-Signature=abc&b=1"), "u?X-Amz-Signature=[redacted]&b=1");
        assert_eq!(redact("token bearer abcdefghij"), "token bearer [redacted]");
    }

    #[test]
    fn redacts_jwts_and_opaque_runs() {
        assert_eq!(redact("t eyJhbGciOi.eyJzdWIiOiIx.c2lnbmF0dXJl x"), "t [redacted] x");
        let opaque = "Ab3dEf6hIj9kLm2nOp5qRs8tUv1wXy4zAb7dEf0hIj";
        assert_eq!(redact(&format!("key {opaque} end")), "key [redacted] end");
        assert_eq!(
            redact("b64 QUJDREVGR0hJSktMTU5PUFFSU1RVVldY/WVphYmNkZWZnaGlqa2xtbm9w+cXI="),
            "b64 [redacted]"
        );
    }

    #[test]
    fn leaves_paths_identifiers_and_rust_paths_alone() {
        let plain = [
            "Run a_plain_build_at_the_root_leaves_the_personal_build_out",
            "Reading crates/teams-fence/src/contacting/chat.rs",
            "teams_auth::token::NATIVE_CLIENT_REDIRECT",
            "https://example.com/a/b",
            "Show the tokens left",
        ];
        for text in plain {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn clips_and_collapses() {
        assert_eq!(clip("  a \n b  ", 10), "a b");
        assert_eq!(clip("abcdefghij", 5), "abcd…");
        assert_eq!(clip("ab  cd", 4), "ab…");
    }
}
