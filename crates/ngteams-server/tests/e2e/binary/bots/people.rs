//! Bots in no people read on one `all` node on SQLite: not the admin's account list, search or account routes, not a
//! client's profile lookup by MRI, not its people search (which asks for bots too), not the operator's user list.

use std::sync::Arc;

use reqwest::Method;
use serde_json::json;
use teams_auth::secrets::Secret;
use teams_proto::{AddablePeopleSearch, Client, Mri, ShortProfileLookup};

use super::super::directory::support::transport;
use super::super::support::{Deployment, Node};
use super::support::{APPS, USERS, WORKFLOWS_BOT, bearer, call, first_admin, register, seed_people};
use crate::support::{Backend, PASSWORD};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bots_are_in_no_people_read_on_one_node() {
    let mut deployment = Deployment::new(Backend::Sqlite, false, None).await;
    // an own-server client takes only an https origin; it reaches the node at its loopback address
    deployment.origin = deployment.origin.replacen("http:", "https:", 1);
    let mut node = deployment.node("a");
    node.cli(&["migrate"], "");
    node.start().await;
    let root = first_admin(&node).await;
    let people = seed_people(&node, &root, "ppl", 40).await;
    let mut bots = Vec::new();
    for name in ["Echo (synthetic)", "Polls (synthetic)", "Ada Helper", "Seeded Person"] {
        bots.push(register(&node, &root, name).await.0);
    }
    admin_reads(&node, &root, &people, &bots).await;
    admin_searches(&node, &root, &bots).await;
    client_reads(&node, &deployment.origin, &people[1], &bots).await;
    let token = bearer(&node, "ppl.1@bots.own.test").await;
    let (status, _) = call(&node, Method::GET, APPS, Some(&token), None).await;
    assert_eq!(status, 403, "a person is no admin of apps");
    node.stop().await;
}

async fn admin_reads(node: &Node, root: &Secret, people: &[String], bots: &[String]) {
    eprintln!("people: the admin's list and account routes");
    let (status, listed) = call(node, Method::GET, &format!("{USERS}?limit=200"), Some(root), None).await;
    assert_eq!(status, 200);
    let ids: Vec<&str> = listed["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|user| user["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), people.len() + 1, "people and root only");
    for bot in bots.iter().map(String::as_str).chain([WORKFLOWS_BOT]) {
        assert!(!ids.contains(&bot));
        let user = format!("{USERS}/{bot}");
        for (method, path, body) in [
            (Method::GET, user.clone(), None),
            (Method::PATCH, user.clone(), Some(json!({"enabled": true}))),
            (
                Method::POST,
                format!("{user}/password"),
                Some(json!({"password": PASSWORD})),
            ),
            (Method::DELETE, user.clone(), None),
        ] {
            let (status, answer) = call(node, method.clone(), &path, Some(root), body).await;
            assert_eq!(status, 404, "{method} {path}: {answer}");
        }
    }
    assert!(!node.cli(&["user", "list"], "").contains("app:"));
}

async fn admin_searches(node: &Node, root: &Secret, bots: &[String]) {
    eprintln!("people: the admin's search");
    for query in ["Echo", "synthetic", "Seeded", "app:", "28:"] {
        let escaped: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let (_, found) = call(
            node,
            Method::GET,
            &format!("{USERS}?q={escaped}&limit=200"),
            Some(root),
            None,
        )
        .await;
        let found = found["users"].as_array().unwrap();
        let bot_found = found
            .iter()
            .any(|user| bots.iter().any(|bot| user["id"] == bot.as_str()));
        assert!(!bot_found, "{query}");
    }
    let (_, named_like_a_bot) = call(node, Method::GET, &format!("{USERS}?q=Echo"), Some(root), None).await;
    assert_eq!(
        named_like_a_bot["users"].as_array().unwrap().len(),
        1,
        "the person named Echo (synthetic)"
    );
}

async fn client_reads(node: &Node, origin: &str, person: &str, bots: &[String]) {
    eprintln!("people: a client's profile lookup and people search");
    let session = Arc::new(node.sign_in("ppl.1@bots.own.test", PASSWORD).await);
    let me = format!("8:orgid:{person}");
    let client = Client::new(transport(&session, &node.endpoint, origin, &me).await);
    let mut asked = vec![Mri::new(me.clone())];
    for bot in bots {
        asked.push(Mri::new(format!("28:{bot}")));
        asked.push(Mri::new(format!("8:orgid:{bot}")));
    }
    asked.push(Mri::new(format!("28:{WORKFLOWS_BOT}")));
    let lookup = ShortProfileLookup::new(asked).unwrap();
    let answer = client.post_read(lookup.clone()).await.unwrap();
    let profiles = lookup.read_answer(answer.body()).unwrap();
    let found: Vec<&str> = profiles.iter().map(|profile| profile.mri.as_str()).collect();
    assert_eq!(found, [me.as_str()]);
    for keyword in ["Echo", "Polls", "Ada", "Seeded", "synthetic"] {
        let answer = client
            .send(&AddablePeopleSearch {
                keyword: keyword.to_owned(),
            })
            .await
            .unwrap();
        assert_eq!(answer.status(), 200, "{keyword}");
        let found = AddablePeopleSearch::read_answer(answer.body());
        let bot_found = found
            .iter()
            .any(|person| person.mri.starts_with("28:") || bots.iter().any(|bot| person.mri.contains(bot.as_str())));
        assert!(!bot_found, "{keyword}: {found:?}");
        if keyword == "Echo" {
            assert_eq!(found.len(), 1, "the person named Echo (synthetic): {found:?}");
        }
    }
}
