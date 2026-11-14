//! Who a team's routes answer: an account's own token for the route's service, an enabled account, and only what
//! that account may see or change.

use reqwest::Method;
use serde_json::{Value, json};
use teams_proto::{CSA_AUDIENCE, GRAPH_AUDIENCE, IC3_AUDIENCE, MIDDLE_TIER_AUDIENCE, PROVISIONING_AUDIENCE};

use super::{Seat, server};
use crate::support::{Backend, TestServer};

const CSA: &str = "/api/csa/own";
const MT: &str = "/api/mt/part/own-01/beta";
const PROV: &str = "/fabric/own/templates/api";
const CHAT: &str = "/api/chatsvc/own/v1";

async fn token(server: &TestServer, upn: &str, audience: &str) -> String {
    let session = server.session(upn).await;
    session.access_token(audience).await.unwrap().token.expose().to_owned()
}

async fn call(server: &TestServer, token: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let mut request = reqwest::Client::new().request(method, server.url(path));
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn each_service_takes_only_its_own_audience_from_an_enabled_account() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    let routes = [
        (CSA_AUDIENCE, format!("{CSA}/api/v3/teams/users/me")),
        (CSA_AUDIENCE, format!("{CSA}/api/v1/teams/{team}/tagCards")),
        (MIDDLE_TIER_AUDIENCE, format!("{MT}/teams/suggested")),
        (PROVISIONING_AUDIENCE, format!("{PROV}/groups/{group}/channels")),
        (PROVISIONING_AUDIENCE, format!("{PROV}/users/me/invitations")),
        (GRAPH_AUDIENCE, "/graph/v1.0/me".to_owned()),
    ];
    let other = token(&server, "ada@own.test", IC3_AUDIENCE).await;
    for (audience, path) in &routes {
        let own = token(&server, "ada@own.test", audience).await;
        assert_eq!(call(&server, &own, Method::GET, path, None).await.0, 200, "{path}");
        assert_eq!(call(&server, &other, Method::GET, path, None).await.0, 401, "{path}");
        assert_eq!(call(&server, "", Method::GET, path, None).await.0, 401, "{path}");
        assert_eq!(call(&server, "a.b.c", Method::GET, path, None).await.0, 401, "{path}");
    }
    let graph = token(&server, "ada@own.test", GRAPH_AUDIENCE).await;
    let (_, me) = call(&server, &graph, Method::GET, "/graph/v1.0/me", None).await;
    assert_eq!(me["id"], ada.id.as_str());
    assert_eq!(me["userPrincipalName"], "ada@own.test");

    // a live token of a disabled account opens nothing
    let tokens: Vec<String> = {
        let mut tokens = Vec::new();
        for (audience, _) in &routes {
            tokens.push(token(&server, "ada@own.test", audience).await);
        }
        tokens
    };
    ngteams_server::admin::set_disabled(&server.app.db, "ada@own.test", true)
        .await
        .unwrap();
    for ((_, path), token) in routes.iter().zip(&tokens) {
        assert_eq!(call(&server, token, Method::GET, path, None).await.0, 403, "{path}");
    }
    ada.stop();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn someone_outside_a_team_learns_nothing_of_it_and_changes_nothing() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let cora = Seat::new(&server, "cora@own.test", "Cora").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    let channel = ada.make_channel(&team, &group, "Launch", true).await;
    let mt = token(&server, "cora@own.test", MIDDLE_TIER_AUDIENCE).await;
    let prov = token(&server, "cora@own.test", PROVISIONING_AUDIENCE).await;
    let csa = token(&server, "cora@own.test", CSA_AUDIENCE).await;
    let ic3 = token(&server, "cora@own.test", IC3_AUDIENCE).await;
    let own = json!({"users": [{"mri": cora.mri, "role": 1}], "groupId": group});
    let named = json!({"displayName": "Mine", "description": ""});
    let attempts = [
        (&mt, Method::PATCH, format!("{MT}/teams/{team}"), Some(named.clone())),
        (&mt, Method::DELETE, format!("{MT}/teams/{team}/delete"), None),
        (&mt, Method::GET, format!("{MT}/teams/{team}/joinCode"), None),
        (&mt, Method::PUT, format!("{MT}/teams/{team}/joinCode"), None),
        (
            &mt,
            Method::PUT,
            format!("{MT}/teams/{team}/bulkUpdateRoledMembers"),
            Some(own),
        ),
        (
            &mt,
            Method::POST,
            format!("{MT}/teams/{team}/channels"),
            Some(named.clone()),
        ),
        (
            &mt,
            Method::PATCH,
            format!("{MT}/teams/{team}/channels/{channel}"),
            Some(named.clone()),
        ),
        (
            &mt,
            Method::DELETE,
            format!("{MT}/teams/{team}/channels/{channel}"),
            None,
        ),
        (
            &mt,
            Method::PUT,
            format!("{MT}/teams/{channel}/setDefaultChannel"),
            None,
        ),
        (
            &mt,
            Method::POST,
            format!("{MT}/teams/sponsorJoinRequest/{group}"),
            Some(json!([cora.mri])),
        ),
        (
            &mt,
            Method::POST,
            format!("{MT}/teams/findJoinRequests"),
            Some(json!({"GroupIdList": [group]})),
        ),
        (
            &mt,
            Method::GET,
            format!("{MT}/analytics/teams/{group}/summary-timeseries"),
            None,
        ),
        (
            &mt,
            Method::GET,
            format!("{MT}/analytics/teams/{group}/channels/{channel}/summary-timeseries"),
            None,
        ),
        (
            &mt,
            Method::GET,
            format!("{MT}/users/{}/profilepicturev2/teams/{group}", cora.mri),
            None,
        ),
        (&prov, Method::GET, format!("{PROV}/groups/{group}/channels"), None),
        (&prov, Method::POST, format!("{PROV}/groups/{group}/archive"), None),
        (
            &prov,
            Method::POST,
            format!("{PROV}/groups/{group}/clone"),
            Some(named.clone()),
        ),
        (
            &prov,
            Method::POST,
            format!("{PROV}/teams/{group}/channels"),
            Some(named.clone()),
        ),
        (
            &prov,
            Method::POST,
            format!("{PROV}/teams/{group}/channels/{channel}/archive"),
            None,
        ),
        (
            &prov,
            Method::PUT,
            format!("{PROV}/channels/{channel}/members"),
            Some(json!({"users": []})),
        ),
        (&prov, Method::PUT, format!("{PROV}/team/{team}/hide"), None),
        (
            &prov,
            Method::PUT,
            format!("{PROV}/conversations/show"),
            Some(json!({"teamIds": [team], "channelIds": []})),
        ),
        (&csa, Method::GET, format!("{CSA}/api/v1/teams/{team}/tagCards"), None),
        (
            &csa,
            Method::POST,
            format!("{CSA}/api/v1/teams/{team}/tags"),
            Some(json!({"tagName": "x", "memberIds": []})),
        ),
        (
            &csa,
            Method::PUT,
            format!("{CSA}/api/v1/teams/{team}/tagSettings"),
            Some(json!({})),
        ),
    ];
    for (token, method, path, body) in attempts {
        let (status, answer) = call(&server, token, method.clone(), &path, body).await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
        assert_eq!(answer["errorCode"], "NotFound", "{method} {path}");
    }
    // the chat service's own refusal for a thread one isn't in
    let settings = json!({"spaceAdminSettings": "{\"userSettings\":{}}"});
    let path = format!("{CHAT}/threads/{team}/properties?name=spaceAdminSettings");
    assert_eq!(call(&server, &ic3, Method::PUT, &path, Some(settings)).await.0, 403);
    let path = format!("{CHAT}/threads/{channel}/members/{}", cora.mri);
    let interest = json!({"interest": "Interested"});
    assert_eq!(call(&server, &ic3, Method::PUT, &path, Some(interest)).await.0, 403);
    assert_eq!(ada.team(&team).await.channels.len(), 2);
    assert_eq!(ada.team(&team).await.about.people, 1);
    ada.stop();
    cora.stop();
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_names_its_target_exactly_and_grants_itself_nothing() {
    let server = server(Backend::Sqlite).await;
    let ada = Seat::new(&server, "ada@own.test", "Ada").await;
    let bert = Seat::new(&server, "bert@own.test", "Bert").await;
    let (team, group) = ada.make_team("Rocket", false).await;
    let (other, other_group) = ada.make_team("Other", true).await;
    ada.add(&team, &group, &[&bert]).await;
    ada.add(&other, &other_group, &[&bert]).await;
    let channel = ada.make_channel(&team, &group, "Launch", false).await;
    let owner = token(&server, "ada@own.test", MIDDLE_TIER_AUDIENCE).await;
    let member = token(&server, "bert@own.test", MIDDLE_TIER_AUDIENCE).await;
    let owner_prov = token(&server, "ada@own.test", PROVISIONING_AUDIENCE).await;
    let ic3 = token(&server, "ada@own.test", IC3_AUDIENCE).await;
    let long = "x".repeat(257);
    let create = |name: &str, access: Value| json!({"displayName": name, "description": "", "accessType": access, "topicThreadTopic": "General"});
    let channel_named = |name: &str, group: &str| json!({"displayName": name, "description": "", "groupId": group, "channelType": "Standard"});
    let post = Method::POST;
    let refused = [
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/create"),
            create("", json!(1)),
            400,
        ),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/create"),
            create(&long, json!(1)),
            400,
        ),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/create"),
            create("Fine", json!(2)),
            400,
        ),
        (&owner, post.clone(), format!("{MT}/teams/create"), json!("text"), 400),
        // a channel is made in the team the path names, whatever group the body claims
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/{team}/channels"),
            channel_named("Side", &other_group),
            400,
        ),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/{team}/channels"),
            channel_named("a/b", &group),
            400,
        ),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/{team}/channels"),
            channel_named(".hidden", &group),
            400,
        ),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/{team}/channels"),
            json!({"displayName": "P", "groupId": group, "channelType": "Private"}),
            400,
        ),
        (
            &owner,
            Method::PATCH,
            format!("{MT}/teams/{other}/channels/{channel}"),
            json!({"displayName": "Moved", "description": ""}),
            404,
        ),
        (
            &owner,
            Method::DELETE,
            format!("{MT}/teams/{team}/channels/{team}"),
            json!(null),
            403,
        ),
        (
            &owner,
            Method::PUT,
            format!("{MT}/teams/{team}/setDefaultChannel"),
            json!(null),
            400,
        ),
        (
            &owner,
            Method::PUT,
            format!("{MT}/teams/{team}/bulkUpdateRoledMembers"),
            json!({"users": [], "groupId": other_group}),
            400,
        ),
        (
            &owner,
            Method::PUT,
            format!("{MT}/teams/{team}/bulkUpdateRoledMembers"),
            json!({"users": [{"mri": bert.mri, "role": 7}], "groupId": group}),
            400,
        ),
        (
            &owner,
            Method::PUT,
            format!("{MT}/teams/{team}/members"),
            json!({"userMri": bert.mri, "updateType": "Left"}),
            400,
        ),
        (
            &owner,
            Method::PUT,
            format!("{MT}/teams/{team}/members"),
            json!({"userMri": "8:orgid:nobody", "updateType": "Removed"}),
            400,
        ),
        (&owner, post.clone(), format!("{MT}/teams/joinByCode"), json!(""), 400),
        (
            &owner,
            post.clone(),
            format!("{MT}/teams/joinRequestAdminAction"),
            json!({"GroupId": group, "UserMri": bert.mri, "ActionType": "Maybe"}),
            400,
        ),
        (
            &owner,
            Method::GET,
            format!("{MT}/analytics/teams/{group}/summary-timeseries?timeperiod=forever"),
            json!(null),
            400,
        ),
        (
            &owner,
            Method::GET,
            format!("{MT}/analytics/teams/{other_group}/channels/{channel}/summary-timeseries"),
            json!(null),
            404,
        ),
        // a member doesn't make itself or anyone an owner, nor a public team's member someone's role
        (
            &member,
            Method::PUT,
            format!("{MT}/teams/{team}/members"),
            json!({"userMri": bert.mri, "updateType": "Promoted"}),
            403,
        ),
        (
            &member,
            Method::PUT,
            format!("{MT}/teams/{channel}/setDefaultChannel"),
            json!(null),
            403,
        ),
        (
            &member,
            Method::PATCH,
            format!("{MT}/teams/{team}"),
            json!({"description": "mine"}),
            403,
        ),
        (
            &owner_prov,
            post.clone(),
            format!("{PROV}/groups/{group}/clone"),
            json!({"displayName": "Copy", "visibility": "Secret"}),
            400,
        ),
        (
            &owner_prov,
            post.clone(),
            format!("{PROV}/teams/{other_group}/channels/{channel}/archive"),
            json!(null),
            404,
        ),
        (
            &owner_prov,
            post.clone(),
            format!("{PROV}/teams/{group}/channels/{team}/archive"),
            json!(null),
            403,
        ),
        (
            &owner_prov,
            Method::PUT,
            format!("{PROV}/channels/{team}/members"),
            json!({"users": []}),
            403,
        ),
        (
            &owner_prov,
            Method::GET,
            format!("{PROV}/teams/{group}/channels/OID:{}@elsewhere", uuid::Uuid::new_v4()),
            json!(null),
            404,
        ),
        (
            &ic3,
            Method::PUT,
            format!("{CHAT}/threads/{channel}/properties?name=spaceAdminSettings"),
            json!({"spaceAdminSettings": "{\"userSettings\":{}}"}),
            400,
        ),
        (
            &ic3,
            Method::PUT,
            format!("{CHAT}/threads/{team}/properties?name=channelSettings"),
            json!({"channelSettings": "{}"}),
            400,
        ),
        (
            &ic3,
            Method::PUT,
            format!("{CHAT}/threads/{team}/properties?name=topic"),
            json!({"topic": "{}"}),
            400,
        ),
        (
            &ic3,
            Method::PUT,
            format!("{CHAT}/threads/{team}/properties"),
            json!({"topic": "{}"}),
            400,
        ),
        (
            &ic3,
            Method::PUT,
            format!("{CHAT}/threads/{channel}/members/{}", ada.mri),
            json!({"interest": "Maybe"}),
            400,
        ),
    ];
    for (token, method, path, body, expected) in refused {
        let body = (!body.is_null()).then_some(body);
        let (status, answer) = call(&server, token, method.clone(), &path, body).await;
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }
    // a public team's member adds a member, never an owner
    let add = |seat: &Seat, role: u8| json!({"users": [{"mri": seat.mri, "role": role}], "groupId": other_group});
    let cora = Seat::new(&server, "cora@own.test", "Cora").await;
    let path = format!("{MT}/teams/{other}/bulkUpdateRoledMembers");
    let (status, answer) = call(&server, &member, Method::PUT, &path, Some(add(&cora, 1))).await;
    assert_eq!(
        (status, answer["value"]["failedMembers"][0]["mri"].as_str()),
        (200, Some(cora.mri.as_str()))
    );
    let (status, answer) = call(&server, &member, Method::PUT, &path, Some(add(&bert, 1))).await;
    assert_eq!(
        (status, answer["value"]["failedMembers"][0]["mri"].as_str()),
        (200, Some(bert.mri.as_str()))
    );
    let (status, answer) = call(&server, &member, Method::PUT, &path, Some(add(&cora, 0))).await;
    assert_eq!(
        (status, answer["value"]["failedMembers"].as_array().map(Vec::len)),
        (200, Some(0))
    );
    assert!(!cora.team(&other).await.owner && !bert.team(&other).await.owner);
    for seat in [ada, bert, cora] {
        seat.stop();
    }
    server.stop().await;
}
