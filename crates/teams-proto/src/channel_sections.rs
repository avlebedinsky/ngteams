//! Account-owned channel sections on CSA. Every update uses a freshly read hierarchy version; a conflict is shown,
//! never replayed against a newer hierarchy. Item and section order are the server's order.
use crate::{Endpoint, Method, Request, Service};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChannelSections {
    pub version: u64,
    pub sections: Vec<ChannelSection>,
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChannelSection {
    pub id: String,
    pub name: String,
    pub favorite: bool,
    pub expanded: bool,
    pub items: Vec<String>,
}

impl std::fmt::Debug for ChannelSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSection")
            .field("id", &self.id)
            .field("items", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl ChannelSections {
    #[must_use]
    pub fn read(value: &Value) -> Option<Self> {
        let value = value
            .get("conversationFolders")
            .filter(|v| v.is_object())
            .unwrap_or(value);
        if value.get("isPartialResponse").and_then(Value::as_bool) != Some(false) {
            return None;
        }
        let version = value.get("folderHierarchyVersion")?.as_u64()?;
        let folders = value.get("conversationFolders")?.as_array()?;
        let order = value.get("conversationFolderOrder")?.as_array()?;
        let ids = order.iter().map(Value::as_str).collect::<Option<Vec<_>>>()?;
        if ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
            || folders
                .iter()
                .filter_map(|folder| folder.get("id").and_then(Value::as_str))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != folders.len()
        {
            return None;
        }
        let mut sections = Vec::new();
        for id in ids {
            let folder = folders
                .iter()
                .find(|folder| folder.get("id").and_then(Value::as_str) == Some(id))?;
            if folder.get("isDeleted").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let kind = folder.get("folderType")?.as_str()?;
            if !matches!(kind, "Favorites" | "UserDefined" | "Custom") {
                continue;
            }
            sections.push(ChannelSection {
                id: id.to_owned(),
                name: folder.get("name")?.as_str()?.to_owned(),
                favorite: kind == "Favorites",
                expanded: folder.get("isExpanded").and_then(Value::as_bool).unwrap_or(true),
                items: folder
                    .get("conversationFolderItems")?
                    .as_array()?
                    .iter()
                    .map(|item| item.get("conversationId")?.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?,
            });
        }
        Some(Self { version, sections })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelSectionAction {
    Remove { channel: String },
    Expanded { section: String, expanded: bool },
    Create { channel: String, name: String },
    Move { channel: String, section: String },
}

/// Constructed with the latest whole hierarchy. The body cannot add recipients or change anyone else's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelSectionWrite {
    body: Value,
    expanded: bool,
}

impl ChannelSectionWrite {
    #[must_use]
    pub fn new(action: &ChannelSectionAction, current: &ChannelSections) -> Option<Self> {
        if let ChannelSectionAction::Expanded { section, expanded } = action {
            if !current.sections.iter().any(|known| known.id == *section) {
                return None;
            }
            return Some(Self {
                body: json!({"folderHierarchyVersion":current.version,"supportsAdditionalSystemGeneratedFolders":true,"supportsSliceItems":true,"folderIds":[section],"isExpanded":expanded}),
                expanded: true,
            });
        }
        let channel = match action {
            ChannelSectionAction::Create { channel, .. }
            | ChannelSectionAction::Move { channel, .. }
            | ChannelSectionAction::Remove { channel } => channel,
            ChannelSectionAction::Expanded { .. } => return None,
        };
        if channel.is_empty() {
            return None;
        }
        let from = current.sections.iter().find(|section| section.items.contains(channel));
        let mut body = json!({
            "folderHierarchyVersion": current.version,
            "supportsAdditionalSystemGeneratedFolders": true, "supportsSliceItems": true,
        });
        let mut actions = match action {
            ChannelSectionAction::Expanded { .. } => return None,
            ChannelSectionAction::Remove { .. } => {
                let from = from?;
                body["actions"] = json!([{ "action":"RemoveItem", "folderId":from.id,"itemId":channel }]);
                return Some(Self { body, expanded: false });
            }
            ChannelSectionAction::Create { name, .. } => {
                let name = name.trim();
                if name.is_empty() || name.chars().count() > 80 {
                    return None;
                }
                body["folderName"] = json!(name);
                body["sortType"] = json!("UserDefinedCustomOrder");
                body["isExpanded"] = json!(true);
                vec![json!({ "action": "AddFolder", "itemId": channel })]
            }
            ChannelSectionAction::Move { section, .. } => {
                let destination = current.sections.iter().find(|known| known.id == *section)?;
                if destination.items.contains(channel) {
                    return None;
                }
                vec![
                    json!({ "action": "AddItem", "folderId": section, "itemId": channel, "itemIndex": destination.items.len() }),
                ]
            }
        };
        if let Some(from) = from {
            actions.push(json!({ "action": "RemoveItem", "folderId": from.id, "itemId": channel }));
        }
        body["actions"] = json!(actions);
        Some(Self { body, expanded: false })
    }
}

pub(crate) fn sections_endpoint() -> Endpoint {
    Endpoint::new(["api", "v1", "teams", "users", "me", "conversationFolders"])
        .with_query("supportsAdditionalSystemGeneratedFolders", "true")
        .with_query("supportsSliceItems", "true")
}

impl Request for ChannelSectionWrite {
    fn service(&self) -> Service {
        Service::Csa
    }
    fn method(&self) -> Method {
        if self.expanded { Method::Patch } else { Method::Post }
    }
    fn endpoint(&self) -> Endpoint {
        if self.expanded {
            Endpoint::new([
                "api",
                "v1",
                "teams",
                "users",
                "me",
                "conversationFolders",
                "batchUpdateExpandedState",
            ])
            .with_query("supportsAdditionalSystemGeneratedFolders", "true")
            .with_query("supportsSliceItems", "true")
        } else {
            sections_endpoint()
        }
    }
    fn body(&self) -> Option<Value> {
        Some(self.body.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hierarchy() -> Value {
        json!({"isPartialResponse":false,"folderHierarchyVersion":12,
        "conversationFolderOrder":["second","first"],"conversationFolders":[
          {"id":"first","name":"Synthetic A","folderType":"UserDefined","isDeleted":false,"conversationFolderItems":[{"conversationId":"channel"}]},
          {"id":"second","name":"Favorites","folderType":"Favorites","isDeleted":false,"conversationFolderItems":[]}]})
    }
    #[test]
    fn a_move_uses_latest_version_and_server_order_without_leaking_other_folder_items() {
        let sections = ChannelSections::read(&hierarchy()).expect("whole hierarchy");
        assert_eq!(
            sections
                .sections
                .iter()
                .map(|section| section.id.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        let write = ChannelSectionWrite::new(
            &ChannelSectionAction::Move {
                channel: "channel".to_owned(),
                section: "second".to_owned(),
            },
            &sections,
        )
        .expect("move");
        let body = write.body().expect("body");
        assert_eq!(body["folderHierarchyVersion"], 12);
        assert_eq!(
            body["actions"],
            json!([{"action":"AddItem","folderId":"second","itemId":"channel","itemIndex":0},{"action":"RemoveItem","folderId":"first","itemId":"channel"}])
        );
    }
    #[test]
    fn partial_unknown_destination_and_current_destination_cannot_produce_a_write() {
        let mut value = hierarchy();
        value["isPartialResponse"] = json!(true);
        assert!(ChannelSections::read(&value).is_none());
        let sections = ChannelSections::read(&hierarchy()).expect("whole");
        for to in ["missing", "first"] {
            assert!(
                ChannelSectionWrite::new(
                    &ChannelSectionAction::Move {
                        channel: "channel".to_owned(),
                        section: to.to_owned()
                    },
                    &sections
                )
                .is_none()
            );
        }
        assert!(
            ChannelSectionWrite::new(
                &ChannelSectionAction::Create {
                    channel: "channel".to_owned(),
                    name: "  ".to_owned()
                },
                &sections
            )
            .is_none()
        );
    }
}
