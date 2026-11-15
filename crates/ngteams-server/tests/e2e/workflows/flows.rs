//! A webhook workflow's life through the engine, as the card editor and the Overview drive it: Save to a channel and
//! to a chat, the Overview's reads, Copy webhook link, Rename, Turn off and on, owners, Delete.

use serde_json::{Value, json};
use teams_core::engine::Action;
use teams_proto::{Mri, Principal, WorkflowFailure, WorkflowRequest};
use teams_store::OutboxState;

use super::{
    CHANNEL_TEMPLATE, CHAT_TEMPLATE, Seat, Target, ask, environment, rows, save, save_body, server, teams_connection,
    template, webhook_link,
};
use crate::support::backends;

/// Makes a group chat of `seat` with `others`; its thread.
pub(crate) async fn group_chat(seat: &Seat, others: &[&Seat], topic: &str) -> String {
    let made = seat
        .engine
        .write(Action::CreateChat {
            others: others.iter().map(|other| Mri::new(&other.mri)).collect(),
            topic: Some(topic.into()),
        })
        .await
        .unwrap();
    assert_eq!(made.state, OutboxState::Sent, "{made:?}");
    made.message_id.unwrap()
}

fn flow_request(environment: &str, flow: &str) -> WorkflowRequest {
    WorkflowRequest::Get {
        environment: environment.to_owned(),
        flow: flow.to_owned(),
    }
}

/// The Overview: the workflow, its trigger, runs, owners and its webhook link.
async fn overview(ada: &Seat, environment: &str, channel_flow: &Value, chat_flow: &Value) {
    let environment = environment.to_owned();
    let id = channel_flow["name"].as_str().unwrap().to_owned();
    let read = ask(ada, flow_request(&environment, &id)).await.unwrap();
    assert_eq!(read["name"], id.as_str());
    assert_eq!(
        read["properties"]["lastModifiedTime"],
        channel_flow["properties"]["lastModifiedTime"]
    );
    let triggers = ask(
        ada,
        WorkflowRequest::Triggers {
            environment: environment.clone(),
            flow: id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&triggers)[0]["name"], "manual");
    assert_eq!(rows(&triggers)[0]["properties"]["state"], "Enabled");
    let history = ask(
        ada,
        WorkflowRequest::History {
            environment: environment.clone(),
            flow: id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(rows(&history).is_empty());
    assert!(history.get("nextLink").is_none());
    let owners = ask(
        ada,
        WorkflowRequest::Owners {
            environment: environment.clone(),
            flow: id.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&owners)[0]["properties"]["principal"]["id"], ada.id.as_str());
    link_is_copied(ada, &environment, &id, chat_flow).await;
}

/// Copy webhook link: one link per workflow, the same each time, only for its trigger.
async fn link_is_copied(ada: &Seat, environment: &str, id: &str, chat_flow: &Value) {
    let (environment, id) = (environment.to_owned(), id.to_owned());
    let link = webhook_link(ada, &environment, &id).await;
    assert!(link.starts_with(&format!("{}/workflows/", super::ORIGIN)), "{link}");
    let url = url::Url::parse(&link).unwrap();
    let sig = url.query_pairs().find(|(key, _)| key == "sig").unwrap().1;
    assert!(sig.len() >= 43);
    assert_eq!(webhook_link(ada, &environment, &id).await, link);
    let other_trigger = ask(
        ada,
        WorkflowRequest::WebhookAddress {
            environment: environment.clone(),
            flow: id.clone(),
            trigger: "other".into(),
        },
    )
    .await;
    assert_eq!(other_trigger, Err(WorkflowFailure::NotFound));
    assert_ne!(
        webhook_link(ada, &environment, chat_flow["name"].as_str().unwrap()).await,
        link
    );
}

/// The account's list holds both; nobody else sees, reads or changes them.
async fn only_its_owner_sees_it(ada: &Seat, bert: &Seat, environment: &str, id: &str) {
    let (environment, id) = (environment.to_owned(), id.to_owned());
    let listed = ask(
        ada,
        WorkflowRequest::List {
            environment: environment.clone(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&listed).len(), 2);
    let theirs = ask(
        bert,
        WorkflowRequest::List {
            environment: environment.clone(),
        },
    )
    .await
    .unwrap();
    assert!(rows(&theirs).is_empty());
    assert_eq!(
        ask(bert, flow_request(&environment, &id)).await,
        Err(WorkflowFailure::NotFound)
    );
    let renamed_by_other = ask(
        bert,
        WorkflowRequest::Rename {
            environment: environment.clone(),
            flow: id.clone(),
            name: "Taken".into(),
        },
    )
    .await;
    assert_eq!(renamed_by_other, Err(WorkflowFailure::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_saves_a_channel_and_a_chat_workflow_and_reads_their_overview() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let environment = environment(&ada).await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;

        let channel_flow = save(
            &ada,
            &environment,
            &Target::Channel {
                group: &group,
                channel: &team,
            },
        )
        .await;
        let id = channel_flow["name"].as_str().unwrap().to_owned();
        let properties = &channel_flow["properties"];
        assert_eq!(properties["displayName"], "Send webhook alerts to General");
        assert_eq!(properties["state"], "Started");
        assert_eq!(properties["templateName"], CHANNEL_TEMPLATE);
        assert_eq!(properties["creator"]["objectId"], ada.id.as_str());
        assert_eq!(
            properties["definition"]["parameters"]["teams.channel"]["defaultValue"],
            team.as_str()
        );
        assert_eq!(properties["definition"]["triggers"]["manual"]["kind"], "TeamsWebhook");
        assert!(
            properties["definition"]["actions"]["Do_Not_Remove_FlowIL"]["metadata"]["userIntentMarkup"].is_string()
        );
        let chat_flow = save(&ada, &environment, &Target::Chat(&chat)).await;
        assert_eq!(chat_flow["properties"]["templateName"], CHAT_TEMPLATE);

        overview(&ada, &environment, &channel_flow, &chat_flow).await;
        only_its_owner_sees_it(&ada, &bert, &environment, &id).await;
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_posts_only_where_its_maker_may_and_only_as_the_server_can_run_it() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let environment = environment(&ada).await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let chat = group_chat(&ada, &[&bert], "Project room").await;
        let channel = Target::Channel {
            group: &group,
            channel: &team,
        };
        let channel_template = template(&cora, CHANNEL_TEMPLATE).await;
        let chat_template = template(&cora, CHAT_TEMPLATE).await;
        let connection = teams_connection(&cora, &environment).await;
        let create = |properties: Value| WorkflowRequest::Create {
            environment: environment.clone(),
            properties,
        };

        // not in the team, not in the chat, a group id that isn't the channel's team's
        assert_eq!(
            ask(&cora, create(save_body(&channel_template, &channel, &connection))).await,
            Err(WorkflowFailure::Forbidden)
        );
        assert_eq!(
            ask(
                &cora,
                create(save_body(&chat_template, &Target::Chat(&chat), &connection))
            )
            .await,
            Err(WorkflowFailure::Forbidden)
        );
        let ada_connection = teams_connection(&ada, &environment).await;
        let wrong_group = save_body(
            &channel_template,
            &Target::Channel {
                group: "00000000-0000-4000-8000-000000000000",
                channel: &team,
            },
            &ada_connection,
        );
        assert_eq!(ask(&ada, create(wrong_group)).await, Err(WorkflowFailure::Forbidden));
        // a chat template pointed at a channel's thread is no chat
        assert_eq!(
            ask(
                &ada,
                create(save_body(&chat_template, &Target::Chat(&team), &ada_connection))
            )
            .await,
            Err(WorkflowFailure::Forbidden)
        );
        // another account's connection, a definition the server can't run, no name
        assert_eq!(
            ask(&ada, create(save_body(&channel_template, &channel, &connection))).await,
            Err(WorkflowFailure::Service(400))
        );
        let mut scheduled = save_body(&channel_template, &channel, &ada_connection);
        scheduled["definition"]["triggers"] =
            json!({"Recurrence": {"type": "Recurrence", "recurrence": {"frequency": "Week"}}});
        assert_eq!(ask(&ada, create(scheduled)).await, Err(WorkflowFailure::Service(400)));
        let mut unnamed = save_body(&channel_template, &channel, &ada_connection);
        unnamed["displayName"] = json!(" ");
        assert_eq!(ask(&ada, create(unnamed)).await, Err(WorkflowFailure::Service(400)));
        // another environment is none of the account's
        let elsewhere = ask(
            &ada,
            WorkflowRequest::Create {
                environment: "Default-elsewhere".into(),
                properties: save_body(&channel_template, &channel, &ada_connection),
            },
        )
        .await;
        assert_eq!(elsewhere, Err(WorkflowFailure::NotFound));
        // saved stopped, as the full editor saves
        let mut stopped = save_body(&channel_template, &channel, &ada_connection);
        stopped["state"] = json!("Stopped");
        let made = ask(&ada, create(stopped)).await.unwrap();
        assert_eq!(made["properties"]["state"], "Stopped");
        assert!(made["properties"].get("secret").is_none());
        ada.stop();
        bert.stop();
        cora.stop();
        server.stop().await;
    }
}

/// Rename, then Edit: the card editor's Save of a workflow, checked against the version it read.
async fn rename_and_edit(ada: &Seat, environment: &str, flow: &Value, group: &str, team: &str) {
    let (environment, id) = (environment.to_owned(), flow["name"].as_str().unwrap().to_owned());
    let env = || environment.clone();
    let flow_id = || id.clone();
    let renamed = ask(
        ada,
        WorkflowRequest::Rename {
            environment: env(),
            flow: flow_id(),
            name: "Build alerts".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed["properties"]["displayName"], "Build alerts");
    assert_ne!(
        renamed["properties"]["lastModifiedTime"],
        flow["properties"]["lastModifiedTime"]
    );
    assert_eq!(renamed["properties"]["definition"], flow["properties"]["definition"]);
    let read = ask(ada, flow_request(&environment, &id)).await.unwrap();
    assert_eq!(read["properties"]["displayName"], "Build alerts");

    // Edit: the card editor's Save of a workflow, checked against the version it read
    let mut edited = save_body(
        &template(ada, CHANNEL_TEMPLATE).await,
        &Target::Channel { group, channel: team },
        &teams_connection(ada, &environment).await,
    );
    edited["displayName"] = json!("Build alerts");
    let updated = ask(
        ada,
        WorkflowRequest::Update {
            environment: env(),
            flow: flow_id(),
            properties: edited.clone(),
            expected_modified: read["properties"]["lastModifiedTime"].as_str().map(str::to_owned),
        },
    )
    .await
    .unwrap();
    assert_eq!(updated["properties"]["displayName"], "Build alerts");
    let stale = ask(
        ada,
        WorkflowRequest::Update {
            environment: env(),
            flow: flow_id(),
            properties: edited,
            expected_modified: read["properties"]["lastModifiedTime"].as_str().map(str::to_owned),
        },
    )
    .await;
    assert_eq!(stale, Err(WorkflowFailure::Changed));
}

/// Turn off, Turn on.
async fn turn_off_and_on(ada: &Seat, environment: &str, id: &str) {
    let (environment, id) = (environment.to_owned(), id.to_owned());
    let env = || environment.clone();
    let flow_id = || id.clone();
    let turn = |enabled| WorkflowRequest::SetEnabled {
        environment: env(),
        flow: flow_id(),
        enabled,
    };
    ask(ada, turn(false)).await.unwrap();
    let off = ask(ada, flow_request(&environment, &id)).await.unwrap();
    assert_eq!(off["properties"]["state"], "Stopped");
    let triggers = ask(
        ada,
        WorkflowRequest::Triggers {
            environment: env(),
            flow: flow_id(),
        },
    )
    .await
    .unwrap();
    assert_eq!(rows(&triggers)[0]["properties"]["state"], "Disabled");
    ask(ada, turn(true)).await.unwrap();
    assert_eq!(
        ask(ada, flow_request(&environment, &id)).await.unwrap()["properties"]["state"],
        "Started"
    );
}

/// Add a co-owner (someone on the team; Cora isn't, so she can't be one), what the co-owner may do, Remove.
async fn share(ada: &Seat, bert: &Seat, cora: &Seat, environment: &str, id: &str) {
    let (environment, id) = (environment.to_owned(), id.to_owned());
    let env = || environment.clone();
    let flow_id = || id.clone();
    let principal = |seat: &Seat, name: &str| Principal {
        id: seat.id.clone(),
        name: name.into(),
        email: None,
    };
    let add = |owner| WorkflowRequest::AddOwner {
        environment: env(),
        flow: flow_id(),
        owner,
    };
    ask(ada, add(principal(bert, "Bert"))).await.unwrap();
    assert_eq!(
        ask(ada, add(principal(cora, "Cora"))).await,
        Err(WorkflowFailure::Forbidden)
    );
    let nobody = Principal {
        id: "00000000-0000-4000-8000-000000000001".into(),
        name: "Nobody".into(),
        email: None,
    };
    assert_eq!(ask(ada, add(nobody)).await, Err(WorkflowFailure::Service(400)));
    let owners = ask(
        bert,
        WorkflowRequest::Owners {
            environment: env(),
            flow: flow_id(),
        },
    )
    .await
    .unwrap();
    let ids: Vec<&str> = rows(&owners)
        .iter()
        .map(|owner| owner["properties"]["principal"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [ada.id.as_str(), bert.id.as_str()]);
    co_owner_controls(ada, bert, &environment, &id).await;
}

/// The co-owner has full control: it lists, reads and renames the workflow; the maker stays, Remove takes the
/// co-owner off.
async fn co_owner_controls(ada: &Seat, bert: &Seat, environment: &str, id: &str) {
    let (environment, id) = (environment.to_owned(), id.to_owned());
    let env = || environment.clone();
    let flow_id = || id.clone();
    // the co-owner has full control: it lists, reads and renames the workflow
    assert_eq!(
        rows(&ask(bert, WorkflowRequest::List { environment: env() }).await.unwrap()).len(),
        1
    );
    ask(
        bert,
        WorkflowRequest::Rename {
            environment: env(),
            flow: flow_id(),
            name: "Shared alerts".into(),
        },
    )
    .await
    .unwrap();
    // the maker stays; Remove takes the co-owner off
    let remove = |owner: &str| WorkflowRequest::RemoveOwner {
        environment: env(),
        flow: flow_id(),
        owner: owner.to_owned(),
    };
    assert_eq!(ask(bert, remove(&ada.id)).await, Err(WorkflowFailure::Service(400)));
    ask(ada, remove(&bert.id)).await.unwrap();
    assert_eq!(
        ask(bert, flow_request(&environment, &id)).await,
        Err(WorkflowFailure::NotFound)
    );
    assert_eq!(ask(ada, remove(&bert.id)).await, Err(WorkflowFailure::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_overview_renames_turns_off_and_on_shares_and_deletes_a_workflow() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let environment = environment(&ada).await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let flow = save(
            &ada,
            &environment,
            &Target::Channel {
                group: &group,
                channel: &team,
            },
        )
        .await;
        let id = flow["name"].as_str().unwrap().to_owned();
        let env = || environment.clone();
        let flow_id = || id.clone();

        rename_and_edit(&ada, &environment, &flow, &group, &team).await;
        turn_off_and_on(&ada, &environment, &id).await;
        share(&ada, &bert, &cora, &environment, &id).await;

        // Delete
        ask(
            &ada,
            WorkflowRequest::Delete {
                environment: env(),
                flow: flow_id(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            ask(&ada, flow_request(&environment, &id)).await,
            Err(WorkflowFailure::NotFound)
        );
        assert!(rows(&ask(&ada, WorkflowRequest::List { environment: env() }).await.unwrap()).is_empty());
        ada.stop();
        bert.stop();
        cora.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_webhook_workflow_runs_only_from_its_link_and_names_only_its_own_runs() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let environment = environment(&ada).await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let channel = Target::Channel {
            group: &group,
            channel: &team,
        };
        let flow = save(&ada, &environment, &channel).await;
        let id = flow["name"].as_str().unwrap().to_owned();
        let run = ask(
            &ada,
            WorkflowRequest::Run {
                environment: environment.clone(),
                flow: id.clone(),
                trigger: "manual".into(),
            },
        )
        .await;
        assert_eq!(run, Err(WorkflowFailure::Service(400)));
        let details = ask(
            &ada,
            WorkflowRequest::RunActions {
                environment: environment.clone(),
                flow: id.clone(),
                run: "no-such-run".into(),
            },
        )
        .await;
        assert_eq!(details, Err(WorkflowFailure::NotFound));
        let badly_named = ask(
            &ada,
            WorkflowRequest::CreateConnection {
                environment: environment.clone(),
                connector: "shared_teams".into(),
                connection: "not a name!".into(),
            },
        )
        .await;
        assert_eq!(badly_named, Err(WorkflowFailure::Service(400)));
        ada.stop();
        server.stop().await;
    }
}
