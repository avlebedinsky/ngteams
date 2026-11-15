//! The server's answers beside the synthetic fixtures the client's parsers are tested with: every key a fixture's
//! entry has is there, with a value of the same JSON type.

use serde_json::Value;
use teams_proto::chat::ChatReads;
use teams_proto::{CsaTeams, Read as _};

use super::{Seat, server};
use crate::support::Backend;

fn fixture(source: &str) -> Value {
    serde_json::from_str::<Value>(source).unwrap()["response"]["body"].clone()
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// `ours` has every key of `golden` but `except`, of the same type (a fixture's `null` says nothing of the type).
pub(crate) fn same_shape(ours: &Value, golden: &Value, except: &[&str], at: &str) {
    for (key, value) in golden.as_object().unwrap() {
        if except.contains(&key.as_str()) {
            continue;
        }
        let own = ours.get(key).unwrap_or_else(|| panic!("{at} has no {key}"));
        assert!(
            value.is_null() || kind(own) == kind(value),
            "{at}.{key}: {own} for {value}"
        );
        if value.is_object() && !value.as_object().unwrap().is_empty() {
            same_shape(own, value, &[], &format!("{at}.{key}"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_teams_read_has_the_shape_of_csas_synthetic_fixture() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    let channel = ada.make_channel(&team, &group, "Launch", false).await;
    ada.engine
        .channel_section(teams_proto::ChannelSectionAction::Move {
            channel: channel.clone(),
            section: format!(
                "{}~{}~Favorites",
                ngteams_server::db::meta::tenant(&server.app.db).await.unwrap(),
                ada.id
            ),
        })
        .await
        .unwrap();
    let query: Vec<String> = CsaTeams
        .endpoint()
        .query()
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    assert_eq!(
        query.join("&"),
        fixture_request()["query"].as_str().unwrap(),
        "the read the fixture answers"
    );
    let answer = ada.client.read(&CsaTeams).await.unwrap();
    let ours: Value = serde_json::from_slice(answer.body()).unwrap();
    let golden = fixture(include_str!("../../../../../tests/fixtures/csa/teams.json"));

    same_shape(&ours, &golden, &["teams", "conversationFolders"], "answer");
    same_shape(
        &ours["conversationFolders"],
        &golden["conversationFolders"],
        &["conversationFolders"],
        "folders",
    );
    let folder = &ours["conversationFolders"]["conversationFolders"][0];
    let golden_folder = &golden["conversationFolders"]["conversationFolders"][0];
    same_shape(folder, golden_folder, &["conversationFolderItems"], "folder");
    same_shape(
        &folder["conversationFolderItems"][0],
        &golden_folder["conversationFolderItems"][0],
        &[],
        "folder item",
    );

    let (own_team, golden_team) = (&ours["teams"][0], &golden["teams"][0]);
    same_shape(own_team, golden_team, &["channels"], "team");
    // the fixture lists its standard channel first, then the general one with a meeting running (the calls' part)
    same_shape(&own_team["channels"][1], &golden_team["channels"][0], &[], "channel");
    same_shape(
        &own_team["channels"][0],
        &golden_team["channels"][1],
        &["activeMeetups"],
        "general channel",
    );
    assert_eq!(
        own_team["channels"][0]["threadType"],
        golden_team["channels"][1]["threadType"]
    );
    assert_eq!(
        own_team["channels"][1]["threadType"],
        golden_team["channels"][0]["threadType"]
    );
    assert_eq!(own_team["memberRoleString"], golden_team["memberRoleString"]);
    assert_eq!(own_team["accessType"], golden_team["accessType"]);
    ada.stop();
    server.stop().await;
}

fn fixture_request() -> Value {
    serde_json::from_str::<Value>(include_str!("../../../../../tests/fixtures/csa/teams.json")).unwrap()["request"]
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_thread_and_members_have_the_shape_of_the_chat_services_fixtures() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    let reads = ChatReads::new(&*ada.client);
    let thread = serde_json::to_value(reads.thread(&team).await.unwrap()).unwrap();
    let golden = fixture(include_str!("../../../../../tests/fixtures/chatsvc/thread-team.json"));
    same_shape(&thread, &golden, &["members"], "thread");
    same_shape(&thread["members"][0], &golden["members"][0], &[], "thread member");
    for (key, value) in [
        ("threadType", "space"),
        ("spaceType", "standard"),
        ("visibility", "private"),
    ] {
        assert_eq!(thread["properties"][key], value);
        assert_eq!(golden["properties"][key], value);
    }
    assert_eq!(thread["properties"]["groupId"], group.as_str());
    assert_eq!(thread["properties"]["spaceThreadTopic"], "Rocket");
    assert_eq!(thread["properties"]["topicThreadTopic"], "General");

    let members = serde_json::to_value(reads.members(&team).await.unwrap()).unwrap();
    let golden = fixture(include_str!(
        "../../../../../tests/fixtures/chatsvc/thread-team-members.json"
    ));
    same_shape(&members, &golden, &["members"], "member list");
    same_shape(&members["members"][0], &golden["members"][0], &[], "member");
    assert_eq!(members["members"][0]["role"], "Admin");
    ada.stop();
    server.stop().await;
}
