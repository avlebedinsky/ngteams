//! The app's diagnostic log at `debug` (M2 Task 24a), as two native clients of the lab write it against the mock chat
//! service and its Trouter: a sends b a message holding a SAS link (a link's `href`, and the link typed in each of the
//! forms text escapes a query in), b posts one with the same link in raw HTML in each form, each is received over the
//! push channel and caught up. At `debug` the log carries the requests, answers and frames in full, the messages'
//! words included, and a grep of every file of it finds no signature in any form. One test: the logger is the
//! process's.
//!
//! The signatures are made up, each a 44-character base64 HMAC-SHA256 as a real SAS link's `sig` is: not JWT-shaped
//! and far shorter than the last pass's opaque run, so only its name can catch it.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

mod common;

use common::{every_file, jwt, message, native, ready};
use teams_core::engine::{Action, EngineHandle};
use teams_core::sync::CatchUpReport;
use teams_mock::{AB_CHAT, ChatServiceMock, LAB_A, LAB_B};
use teams_proto::chat::{MessageText, Node, RichText};
use teams_store::OutboxState;

const A_NAME: &str = "Alpha Person";
const B_NAME: &str = "Bravo Person";
/// Words of a's message and of b's: at `debug` they are in the log.
const A_WORDS: &str = "DEBUGWORDSALPHA here is the file";
const B_WORDS: &str = "DEBUGWORDSBRAVO and here is mine";

/// A made-up SAS signature: 44 characters of base64, `/`, `+` and the closing `=` included.
fn signature(seed: &str) -> String {
    let text: String = format!("{seed}Rk9/mQ2+Zt7LpW4xNv8/Hc1+Yb6JdS3aEf0Gi5Ko9Uh2Tr")
        .chars()
        .take(43)
        .collect();
    format!("{text}=")
}

fn percent(sig: &str) -> String {
    sig.replace('/', "%2F").replace('+', "%2B").replace('=', "%3D")
}

fn percent_twice(sig: &str) -> String {
    percent(sig).replace('%', "%25")
}

/// A SAS link to a blob, its query's separator and `=` as given, signed with `sig`.
fn link(separator: &str, equals: &str, sig: &str) -> String {
    format!(
        "https://example.blob.core.windows.net/files/report.pdf?sv=2024-01-01{separator}se=2026-10-30{separator}sp=r\
         {separator}sig{equals}{sig}"
    )
}

/// The forms a SAS link's `&sig=` takes in a message's HTML and the text the log meets: `(separator, =, signature as
/// written)`.
fn forms(sig: &str) -> [(&'static str, &'static str, String); 8] {
    [
        ("&amp;", "=", sig.to_owned()),
        ("&#38;", "=", sig.to_owned()),
        ("&#x26;", "=", sig.to_owned()),
        ("&amp;", "&#61;", sig.to_owned()),
        ("%26", "%3D", percent(sig)),
        ("%2526", "%253D", percent_twice(sig)),
        ("\\u0026", "=", sig.to_owned()),
        ("&", "=", sig.to_owned()),
    ]
}

/// a's message: a link whose `href` is the SAS link (its `&` escaped as `&amp;` in the HTML), then the link typed in
/// each form, `typed[i]` signed with the i-th signature.
fn a_message(href_sig: &str, typed: &[String]) -> MessageText {
    let mut nodes = RichText::plain(&format!("{A_WORDS}: ")).nodes;
    nodes.push(Node::Link {
        href: link("&", "=", href_sig),
        children: RichText::plain("report").nodes,
    });
    for (at, sig) in typed.iter().enumerate() {
        let (separator, equals, written) = &forms(sig)[at];
        nodes.extend(RichText::plain(&format!(" typed {}", link(separator, equals, written))).nodes);
    }
    MessageText::new(
        RichText {
            nodes: vec![Node::Paragraph(nodes)],
            truncated: false,
        },
        [],
    )
    .unwrap()
}

/// b's message in raw HTML: a link's `href` (`&amp;`) and the link written in each form, `written[i]` signed with
/// the i-th signature.
fn b_html(href_sig: &str, written: &[String]) -> String {
    let mut html = format!(
        r#"<p>{B_WORDS}: <a href="{}" title="report">report</a>"#,
        link("&amp;", "=", href_sig)
    );
    for (at, sig) in written.iter().enumerate() {
        let (separator, equals, written) = &forms(sig)[at];
        html.push_str(" also ");
        html.push_str(&link(separator, equals, written));
    }
    html.push_str("</p>");
    html
}

/// a sends its message to b, b posts its own; each is received over the push channel, then both catch up.
async fn exchange(mock: &ChatServiceMock, a: &mut EngineHandle, b: &mut EngineHandle, sigs: &[String]) {
    let (a_sigs, b_sigs) = sigs.split_at(sigs.len() / 2);
    let action = Action::Send {
        conversation: AB_CHAT.to_owned(),
        text: a_message(&a_sigs[0], &a_sigs[1..]),
    };
    let row = a.write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{row:?}");
    message(b, &row.message_id.unwrap()).await;
    let id = mock.post_as(LAB_B, AB_CHAT, &b_html(&b_sigs[0], &b_sigs[1..])).unwrap();
    message(a, &id).await;
    for handle in [a, b] {
        let report: CatchUpReport = handle.sync().await.unwrap();
        assert!(report.failed.is_empty(), "{report:?}");
    }
}

#[tokio::test]
async fn the_debug_log_has_the_messages_and_no_signature_in_any_form() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    let log = teams_log::install(&teams_log::Config {
        dir: logs.clone(),
        setting: teams_log::Level::Debug,
        env: None,
        rotation: teams_log::Rotation::default(),
    })
    .unwrap();
    let mock = ChatServiceMock::start().await.unwrap();
    let trouter = mock.start_trouter().unwrap();
    for (account, seed) in [(LAB_A, "a"), (LAB_B, "b")] {
        assert!(mock.set_token(account, jwt(seed)));
    }
    let (a_dir, b_dir) = (dir.path().join("a"), dir.path().join("b"));
    let mut a = native(&mock, &trouter, LAB_A, [A_NAME, B_NAME], &a_dir);
    let mut b = native(&mock, &trouter, LAB_B, [A_NAME, B_NAME], &b_dir);
    ready(&mut a).await;
    ready(&mut b).await;
    // one signature for each href and each form, a's and b's
    let per_message = 1 + forms("").len();
    let sigs: Vec<String> = (0..2 * per_message).map(|at| signature(&format!("{at:02}s"))).collect();
    exchange(&mock, &mut a, &mut b, &sigs).await;
    a.shutdown();
    b.shutdown();

    assert!(log.flush_written());
    let text = every_file(&logs);
    for kept in [
        "DEBUG teams_wire::http: request POST ",
        "DEBUG teams_wire::trouter: frame received 3:::",
        A_WORDS,
        B_WORDS,
        "sv=2024-01-01",
        "sig=[redacted]",
    ] {
        assert!(text.contains(kept), "the log has {kept:?}:\n{text}");
    }
    for (at, sig) in sigs.iter().enumerate() {
        for form in [sig.clone(), percent(sig), percent_twice(sig), sig[..20].to_owned()] {
            assert!(
                !text.contains(&form),
                "the log holds signature {at} as {form:?}:\n{text}"
            );
        }
    }
}
