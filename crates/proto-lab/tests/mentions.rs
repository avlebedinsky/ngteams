//! How ab-chat-full checks that a's mention named b (`proto_lab::scenario::ab_chat_full::mentions`): the post's
//! `properties.mentions`, a JSON string of a list, both read strictly. Made-up MRIs only.

// The full Linux browser/scenario harness is outside the Windows provisioning command set.
#![cfg(unix)]

use proto_lab::scenario::ab_chat_full::mentions;
use serde_json::json;

const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";

fn post(mentions: &str) -> String {
    json!({"content": "made up", "properties": {"mentions": mentions}}).to_string()
}

fn list(mri: &str) -> String {
    json!([{"@type": "http://schema.skype.com/Mention", "itemid": "0", "mri": mri, "mentionType": "person"}])
        .to_string()
}

#[test]
fn a_post_naming_b_in_its_mentions_mentions_b() {
    assert!(mentions(Some(&post(&list(B))), B));
    assert!(!mentions(Some(&post(&list(C))), B));
}

#[test]
fn anything_but_a_strictly_read_list_of_mentions_doesnt() {
    let not_a_list = json!({"mri": B}).to_string();
    // a lax reader takes the first `mri` and finds b
    let twice = format!(r#"[{{"mri": "{B}", "MRI": "{C}"}}]"#);
    let bodies = [
        // the mentions not a string, not a list, a key twice in another case
        json!({"properties": {"mentions": [{"mri": B}]}}).to_string(),
        post(&not_a_list),
        post(&twice),
        // the post with a key twice in another case (a lax reader keeps the first), or something after it
        format!(
            r#"{{"properties": {{"mentions": {}}}, "Properties": {{}}}}"#,
            json!(list(B))
        ),
        format!("{} {{}}", post(&list(B))),
        "not json".to_owned(),
    ];
    for body in bodies {
        assert!(!mentions(Some(&body), B), "{body}");
    }
    assert!(!mentions(None, B));
}
