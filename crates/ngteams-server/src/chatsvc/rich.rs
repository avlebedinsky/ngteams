//! What the chat service makes of a rich message's mentions and quotes, as Teams gives them back
//! (`docs/protocol/chat.md`, "Mentions" and "A message as read"):
//!
//! - a mention's compose wrapper (`<readonly itemtype="…/Mention">`) goes, its span stays; the content is then
//!   written out again by html5ever, as Teams writes such a message out (`itemscope=""`);
//! - `properties.mentions` stays a JSON string, each entry's `itemid` a number;
//! - `properties.qtdMsgs` becomes a list of objects: the quoted message's id a number, `validationResult` `Valid` for a
//!   message of the chat the writer may read, and `hasValidMsgReferences` beside it.
//!
//! Content without a mention's wrapper is kept byte for byte.

use indexmap::IndexMap;
use scraper::{Html, Node};
use serde_json::{Value, json};

/// A list entry, its keys in the order they came (whatever `serde_json`'s `preserve_order`).
type Entry = IndexMap<String, Value>;

const MENTION_ITEMTYPE: &str = "http://schema.skype.com/Mention";

/// A sent message's mentions or quotes that aren't a list of objects.
#[derive(Debug)]
pub(super) struct Malformed;

/// `content` as read back: mentions out of their compose wrapper.
pub(super) fn content(content: &str) -> String {
    if !content.contains("<readonly") {
        return content.to_owned();
    }
    let mut html = Html::parse_fragment(content);
    let wrappers: Vec<_> = html
        .tree
        .nodes()
        .filter(|node| {
            matches!(node.value(), Node::Element(element)
                if element.name() == "readonly" && element.attr("itemtype") == Some(MENTION_ITEMTYPE))
        })
        .map(|node| node.id())
        .collect();
    if wrappers.is_empty() {
        return content.to_owned();
    }
    for wrapper in wrappers {
        let children: Vec<_> = html
            .tree
            .get(wrapper)
            .map(|node| node.children().map(|child| child.id()).collect())
            .unwrap_or_default();
        if let Some(mut node) = html.tree.get_mut(wrapper) {
            for child in children {
                node.insert_id_before(child);
            }
            node.detach();
        }
    }
    html.root_element().inner_html()
}

/// The entries of a list property, sent as a JSON string of a list (or as the list itself).
fn entries(value: &Value) -> Result<Vec<Entry>, Malformed> {
    match value {
        Value::String(text) => serde_json::from_str(text).map_err(|_| Malformed),
        Value::Array(_) => serde_json::from_value(value.clone()).map_err(|_| Malformed),
        _ => Err(Malformed),
    }
}

/// A string of digits as the number it is; anything else as it came.
fn numbered(value: &Value) -> Value {
    match value {
        Value::String(text) if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) => {
            text.parse::<u64>().map_or_else(|_| value.clone(), Value::from)
        }
        _ => value.clone(),
    }
}

/// `properties.mentions` as read back: a JSON string, each entry's `itemid` a number.
pub(super) fn mentions(value: &Value) -> Result<Value, Malformed> {
    let mut list = entries(value)?;
    for entry in &mut list {
        if let Some(item) = entry.get("itemid") {
            let item = numbered(item);
            entry.insert("itemid".into(), item);
        }
    }
    serde_json::to_string(&list).map(Value::String).map_err(|_| Malformed)
}

/// The ids of the messages `qtdMsgs` names: those given as an id of digits.
pub(super) fn quoted_ids(value: &Value) -> Result<Vec<i64>, Malformed> {
    Ok(entries(value)?
        .iter()
        .filter_map(|entry| match numbered(entry.get("messageId").unwrap_or(&Value::Null)) {
            Value::Number(number) => number.as_i64(),
            _ => None,
        })
        .collect())
}

/// `properties.qtdMsgs` as read back, the quotes of the `valid` messages (of the chat, readable by the writer) marked
/// so; and whether every quote is.
pub(super) fn quotes(value: &Value, valid: &[i64]) -> Result<(Value, bool), Malformed> {
    let mut all = true;
    let list: Vec<Value> = entries(value)?
        .into_iter()
        .map(|entry| {
            let id = numbered(entry.get("messageId").unwrap_or(&Value::Null));
            let found = id.as_i64().is_some_and(|id| valid.contains(&id));
            all &= found;
            json!({
                "messageId": id,
                "sender": entry.get("sender").cloned().unwrap_or(Value::Null),
                "time": entry.get("time").cloned().unwrap_or(Value::Null),
                "message": null,
                "validationResult": if found { "Valid" } else { "Invalid" },
                "sharedRefId": null,
                "replyChainId": null,
            })
        })
        .collect();
    let any = !list.is_empty();
    Ok((Value::Array(list), all && any))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_without_a_mentions_wrapper_is_kept_as_it_came() {
        let text = "<p>a <b>bold</b> line&nbsp;<readonly>plain</readonly></p><blockquote itemscope>x</blockquote>";
        assert_eq!(content(text), text);
        assert_eq!(content("<p>no markup to change</p>"), "<p>no markup to change</p>");
    }

    #[test]
    fn a_mentions_wrapper_goes_and_the_rest_of_the_markup_stays() {
        let sent = "<p>&lt;x&gt; &amp; <readonly class=\"skipProofing\" itemtype=\"http://schema.skype.com/Mention\" \
                    spellcheck=\"false\"><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">\
                    李雷 🚀</span></readonly>&nbsp;tail</p>";
        assert_eq!(
            content(sent),
            "<p>&lt;x&gt; &amp; <span itemtype=\"http://schema.skype.com/Mention\" itemscope=\"\" itemid=\"0\">\
             李雷 🚀</span>&nbsp;tail</p>"
        );
    }

    #[test]
    fn list_properties_take_a_string_or_a_list_of_objects_and_nothing_else() {
        assert!(mentions(&json!("not json")).is_err());
        assert!(mentions(&json!("{}")).is_err());
        assert!(mentions(&json!("[1]")).is_err());
        assert!(mentions(&json!(7)).is_err());
        assert_eq!(
            mentions(&json!([{"itemid": "12", "mri": "x"}, {"itemid": "a"}])).unwrap(),
            json!("[{\"itemid\":12,\"mri\":\"x\"},{\"itemid\":\"a\"}]")
        );
        // the keys stay in the order sent, whatever serde_json's features
        assert_eq!(
            mentions(&json!("[{\"z\":\"3\",\"itemid\":\"1\",\"a\":null}]")).unwrap(),
            json!("[{\"z\":\"3\",\"itemid\":1,\"a\":null}]")
        );
        assert_eq!(
            quoted_ids(&json!("[{\"messageId\":\"5\"},{\"messageId\":6},{}]")).unwrap(),
            [5, 6]
        );
        let (list, all) = quotes(&json!([{"messageId": "5"}, {"messageId": "x"}]), &[5]).unwrap();
        assert!(!all);
        assert_eq!(list[0]["validationResult"], "Valid");
        assert_eq!(list[1]["validationResult"], "Invalid");
        assert_eq!(list[1]["messageId"], "x");
        assert!(!quotes(&json!("[]"), &[]).unwrap().1);
    }
}
