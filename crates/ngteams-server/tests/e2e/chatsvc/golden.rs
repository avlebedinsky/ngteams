//! Edits, deletes and reactions as the real engine makes them, read back in the forms the synthetic fixtures record
//! (`tests/fixtures/chatsvc/{edit,delete,reactions}.json`). JSON is compared as parsed values.

use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_proto::chat::{MessageText, iso_millis};

use super::activity::sent;
use super::messages::Pair;
use super::rich::{fixture, read_back, served};
use crate::support::backends;

const EDIT: &str = include_str!("../../../../../tests/fixtures/chatsvc/edit.json");
const DELETE: &str = include_str!("../../../../../tests/fixtures/chatsvc/delete.json");
const REACTIONS: &str = include_str!("../../../../../tests/fixtures/chatsvc/reactions.json");

fn swaps(pair: &Pair) -> [(&str, &str); 4] {
    [
        ("19:<guid:a>_<guid:b>@unq.gbl.spaces", pair.chat.as_str()),
        ("<guid:a>", pair.a_id.as_str()),
        ("<guid:b>", pair.b_id.as_str()),
        ("Lab A", "Ada"),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edited_and_a_deleted_message_read_back_as_edit_json_and_delete_json() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let swaps = swaps(&pair);

        let id = pair.send("a made-up message").await;
        sent(
            &pair.a,
            Action::Edit {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                text: MessageText::plain("a made-up message, edited").unwrap(),
            },
        )
        .await;
        let read = read_back(&pair.server, "b@own.test", &pair.b_id, &pair.chat, &id).await;
        let mut expected = served(fixture(EDIT, &swaps)["read_back"].clone(), &read);
        expected["properties"]["edittime"] = read["properties"]["edittime"].clone();
        // the edit keeps the time the message was composed at, as the editing client wrote it
        expected["properties"]["composetime"] = read["properties"]["composetime"].clone();
        assert_eq!(read, expected);
        assert_ne!(read["version"], read["id"]);
        assert_eq!(
            read["properties"]["edittime"].to_string(),
            read["version"].as_str().unwrap()
        );
        assert_eq!(
            read["properties"]["composetime"].as_str().and_then(iso_millis),
            read["composetime"].as_str().and_then(iso_millis)
        );

        let id = pair.send("a made-up message to delete").await;
        sent(
            &pair.a,
            Action::Delete {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
            },
        )
        .await;
        let read = read_back(&pair.server, "b@own.test", &pair.b_id, &pair.chat, &id).await;
        let mut expected = served(fixture(DELETE, &swaps)["read_back"].clone(), &read);
        expected["skypeeditedid"] = read["clientmessageid"].clone();
        expected["properties"]["deletetime"] = read["properties"]["deletetime"].clone();
        assert_eq!(read, expected);
        assert_eq!(
            read["properties"]["deletetime"].to_string(),
            read["version"].as_str().unwrap()
        );
        pair.stop().await;
    }
}

/// `golden`'s reactions with each standing one's times (the service's, and the value the client sent) as `read`'s.
fn reactions(golden: &Value, read: &Value) -> Value {
    let mut emotions = golden["properties"]["emotions"].clone();
    for (key, entry) in emotions.as_array_mut().unwrap().iter_mut().enumerate() {
        for (user, standing) in entry["users"].as_array_mut().unwrap().iter_mut().enumerate() {
            let actual = &read["properties"]["emotions"][key]["users"][user];
            standing["time"] = actual["time"].clone();
            standing["value"] = actual["value"].clone();
        }
    }
    json!({"annotationsSummary": golden["annotationsSummary"], "emotions": emotions})
}

#[tokio::test(flavor = "multi_thread")]
async fn every_quick_reaction_set_and_taken_back_reads_back_as_reactions_json() {
    for backend in backends() {
        let pair = Pair::start(backend).await;
        let golden = fixture(REACTIONS, &swaps(&pair));
        let keys: Vec<String> = serde_json::from_value(golden["offered"].clone()).unwrap();
        let id = pair.send("a made-up message").await;
        let read = || read_back(&pair.server, "b@own.test", &pair.b_id, &pair.chat, &id);
        let seen = |read: &Value| {
            json!({"annotationsSummary": read["annotationsSummary"],
            "emotions": read["properties"]["emotions"]})
        };

        for key in &keys {
            let react = Action::React {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: key.clone(),
            };
            sent(&pair.a, react).await;
        }
        let all = read().await;
        assert_eq!(seen(&all), reactions(&golden["read_back_all_set"], &all));
        for user in all["properties"]["emotions"].as_array().unwrap() {
            let standing = &user["users"][0];
            assert!(standing["value"].as_str().unwrap().parse::<u64>().is_ok(), "{standing}");
            assert!(standing["time"].is_u64(), "{standing}");
        }

        for (round, key) in keys.iter().enumerate() {
            let unreact = Action::Unreact {
                conversation: pair.chat.clone(),
                message_id: id.clone(),
                key: key.clone(),
            };
            sent(&pair.a, unreact).await;
            let golden = match round {
                0 => &golden["read_back_first_taken_back"],
                3 => &golden["read_back_all_taken_back"],
                _ => continue,
            };
            let now = read().await;
            assert_eq!(seen(&now), reactions(golden, &now));
        }
        pair.stop().await;
    }
}
