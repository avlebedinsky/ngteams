//! A link the app doesn't open is logged by its scheme alone, never more of its text. Every link is made up.

use super::*;

#[test]
fn an_unrecognised_link_is_named_by_its_usual_scheme_alone() {
    assert_eq!(scheme("msteams:/l/chat/0/0?users=someone@example.test"), "msteams");
    assert_eq!(scheme("  HTTPS://example.test/a-private-path  "), "https");
    assert_eq!(scheme("http://example.test/"), "http");
    for other in ["https", "ftp://example.test/", "msteamsx:/l/chat", "not a link", ""] {
        assert_eq!(scheme(other), "other", "{other:?}");
    }
}
