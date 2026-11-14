//! Who a team's routes answer: an account's own token for the route's service, an enabled account, and only what
//! that account may see or change.

use reqwest::Method;
use serde_json::Value;
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
    let mut tokens = Vec::new();
    for (audience, path) in &routes {
        let own = token(&server, "ada@own.test", audience).await;
        assert_eq!(call(&server, &own, Method::GET, path, None).await.0, 200, "{path}");
        assert_eq!(call(&server, &other, Method::GET, path, None).await.0, 401, "{path}");
        assert_eq!(call(&server, "", Method::GET, path, None).await.0, 401, "{path}");
        assert_eq!(call(&server, "a.b.c", Method::GET, path, None).await.0, 401, "{path}");
        tokens.push(own);
    }
    let (_, me) = call(&server, &tokens[5], Method::GET, "/graph/v1.0/me", None).await;
    assert_eq!(me["id"], ada.id.as_str());
    assert_eq!(me["userPrincipalName"], "ada@own.test");

    // a live token of a disabled account opens nothing
    ngteams_server::admin::set_disabled(&server.app.db, "ada@own.test", true)
        .await
        .unwrap();
    for ((_, path), token) in routes.iter().zip(&tokens) {
        assert_eq!(call(&server, token, Method::GET, path, None).await.0, 403, "{path}");
    }
    ada.stop();
    server.stop().await;
}

/// Two teams of Ada's with Bert on both (the second public), a channel of the first, and Cora on neither.
struct World {
    server: TestServer,
    seats: [Seat; 3],
    names: Vec<(&'static str, String)>,
}

impl World {
    async fn new() -> Self {
        let server = server(Backend::Sqlite).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        let (other, other_group) = ada.make_team("Other", true).await;
        ada.add(&team, &group, &[&bert]).await;
        ada.add(&other, &other_group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", false).await;
        let names = vec![
            ("{MT}", MT.to_owned()),
            ("{PROV}", PROV.to_owned()),
            ("{CSA}", CSA.to_owned()),
            ("{CHAT}", CHAT.to_owned()),
            ("{team}", team),
            ("{group}", group),
            ("{other}", other),
            ("{other_group}", other_group),
            ("{channel}", channel),
            ("{ada}", ada.mri.clone()),
            ("{bert}", bert.mri.clone()),
            ("{cora}", cora.mri.clone()),
            ("{long}", "x".repeat(257)),
            ("{guid}", uuid::Uuid::new_v4().to_string()),
        ];
        Self {
            server,
            seats: [ada, bert, cora],
            names,
        }
    }

    fn fill(&self, text: &str) -> String {
        self.names
            .iter()
            .fold(text.to_owned(), |text, (name, value)| text.replace(name, value))
    }

    fn name(&self, name: &str) -> &str {
        &self.names.iter().find(|(known, _)| *known == name).unwrap().1
    }

    /// Sends `method path` (their `{names}` filled in) as `who`, with the token of the path's service.
    async fn send(&self, who: &str, method: &str, path: &str, body: &str) -> (u16, Value) {
        let audience = match &path[..=path.find('}').unwrap()] {
            "{MT}" => MIDDLE_TIER_AUDIENCE,
            "{PROV}" => PROVISIONING_AUDIENCE,
            "{CSA}" => CSA_AUDIENCE,
            _ => IC3_AUDIENCE,
        };
        let token = token(&self.server, &format!("{who}@own.test"), audience).await;
        let body = (!body.is_empty()).then(|| serde_json::from_str(&self.fill(body)).unwrap());
        let method = Method::from_bytes(method.as_bytes()).unwrap();
        call(&self.server, &token, method, &self.fill(path), body).await
    }

    async fn stop(self) {
        for seat in self.seats {
            seat.stop();
        }
        self.server.stop().await;
    }
}

/// What someone on neither team tries: each answers 404 `NotFound`.
const OUTSIDE: [(&str, &str, &str); 25] = [
    (
        "PATCH",
        "{MT}/teams/{team}",
        r#"{"displayName":"Mine","description":""}"#,
    ),
    ("DELETE", "{MT}/teams/{team}/delete", ""),
    ("GET", "{MT}/teams/{team}/joinCode", ""),
    ("PUT", "{MT}/teams/{team}/joinCode", ""),
    (
        "PUT",
        "{MT}/teams/{team}/bulkUpdateRoledMembers",
        r#"{"users":[{"mri":"{cora}","role":1}],"groupId":"{group}"}"#,
    ),
    ("POST", "{MT}/teams/{team}/channels", r#"{"displayName":"Mine"}"#),
    (
        "PATCH",
        "{MT}/teams/{team}/channels/{channel}",
        r#"{"displayName":"Mine"}"#,
    ),
    ("DELETE", "{MT}/teams/{team}/channels/{channel}", ""),
    ("PUT", "{MT}/teams/{channel}/setDefaultChannel", ""),
    ("POST", "{MT}/teams/sponsorJoinRequest/{group}", r#"["{cora}"]"#),
    ("POST", "{MT}/teams/findJoinRequests", r#"{"GroupIdList":["{group}"]}"#),
    ("GET", "{MT}/analytics/teams/{group}/summary-timeseries", ""),
    (
        "GET",
        "{MT}/analytics/teams/{group}/channels/{channel}/summary-timeseries",
        "",
    ),
    ("GET", "{MT}/users/{cora}/profilepicturev2/teams/{group}", ""),
    ("GET", "{PROV}/groups/{group}/channels", ""),
    ("POST", "{PROV}/groups/{group}/archive", ""),
    ("POST", "{PROV}/groups/{group}/clone", r#"{"displayName":"Mine"}"#),
    ("POST", "{PROV}/teams/{group}/channels", r#"{"displayName":"Mine"}"#),
    ("POST", "{PROV}/teams/{group}/channels/{channel}/archive", ""),
    ("PUT", "{PROV}/channels/{channel}/members", r#"{"users":[]}"#),
    ("PUT", "{PROV}/team/{team}/hide", ""),
    (
        "PUT",
        "{PROV}/conversations/show",
        r#"{"teamIds":["{team}"],"channelIds":[]}"#,
    ),
    ("GET", "{CSA}/api/v1/teams/{team}/tagCards", ""),
    (
        "POST",
        "{CSA}/api/v1/teams/{team}/tags",
        r#"{"tagName":"x","memberIds":[]}"#,
    ),
    ("PUT", "{CSA}/api/v1/teams/{team}/tagSettings", "{}"),
];

#[tokio::test(flavor = "multi_thread")]
async fn someone_outside_a_team_learns_nothing_of_it_and_changes_nothing() {
    let world = World::new().await;
    for (method, path, body) in OUTSIDE {
        let (status, answer) = world.send("cora", method, path, body).await;
        assert_eq!(status, 404, "{method} {path}: {answer}");
        assert_eq!(answer["errorCode"], "NotFound", "{method} {path}");
    }
    // the chat service's own refusal for a thread one isn't in
    let settings = r#"{"spaceAdminSettings":"{\"userSettings\":{}}"}"#;
    let path = "{CHAT}/threads/{team}/properties?name=spaceAdminSettings";
    assert_eq!(world.send("cora", "PUT", path, settings).await.0, 403);
    let path = "{CHAT}/threads/{channel}/members/{cora}";
    assert_eq!(
        world.send("cora", "PUT", path, r#"{"interest":"Interested"}"#).await.0,
        403
    );
    let team = world.seats[0].team(world.name("{team}")).await;
    assert_eq!((team.channels.len(), team.about.people), (2, 2));
    world.stop().await;
}

/// Requests of the teams' owner the server refuses: who, method, path, body, status.
const MALFORMED: [(&str, &str, &str, u16); 29] = [
    (
        "POST",
        "{MT}/teams/create",
        r#"{"displayName":"","accessType":1,"topicThreadTopic":"General"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/create",
        r#"{"displayName":"{long}","accessType":1,"topicThreadTopic":"General"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/create",
        r#"{"displayName":"Fine","accessType":2,"topicThreadTopic":"General"}"#,
        400,
    ),
    ("POST", "{MT}/teams/create", r#""text""#, 400),
    // a channel is made in the team the path names, whatever group the body claims
    (
        "POST",
        "{MT}/teams/{team}/channels",
        r#"{"displayName":"Side","groupId":"{other_group}","channelType":"Standard"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/{team}/channels",
        r#"{"displayName":"a/b","groupId":"{group}","channelType":"Standard"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/{team}/channels",
        r#"{"displayName":".hidden","groupId":"{group}","channelType":"Standard"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/{team}/channels",
        r#"{"displayName":"P","groupId":"{group}","channelType":"Private"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/{team}/channels",
        r#"{"displayName":"L","groupId":"{group}","channelType":"Standard","chatModalityType":"Grid"}"#,
        400,
    ),
    (
        "PATCH",
        "{MT}/teams/{other}/channels/{channel}",
        r#"{"displayName":"Moved","description":""}"#,
        404,
    ),
    ("DELETE", "{MT}/teams/{team}/channels/{team}", "", 403),
    ("PUT", "{MT}/teams/{team}/setDefaultChannel", "", 400),
    (
        "PUT",
        "{MT}/teams/{team}/bulkUpdateRoledMembers",
        r#"{"users":[],"groupId":"{other_group}"}"#,
        400,
    ),
    (
        "PUT",
        "{MT}/teams/{team}/bulkUpdateRoledMembers",
        r#"{"users":[{"mri":"{bert}","role":7}],"groupId":"{group}"}"#,
        400,
    ),
    (
        "PUT",
        "{MT}/teams/{team}/members",
        r#"{"userMri":"{bert}","updateType":"Left"}"#,
        400,
    ),
    (
        "PUT",
        "{MT}/teams/{team}/members",
        r#"{"userMri":"8:orgid:nobody","updateType":"Removed"}"#,
        400,
    ),
    (
        "PUT",
        "{MT}/teams/{team}/members",
        r#"{"userMri":"{cora}","updateType":"Removed"}"#,
        404,
    ),
    ("POST", "{MT}/teams/joinByCode", r#""""#, 400),
    (
        "POST",
        "{MT}/teams/joinRequestAdminAction",
        r#"{"GroupId":"{group}","UserMri":"{bert}","ActionType":"Maybe"}"#,
        400,
    ),
    (
        "POST",
        "{MT}/teams/joinRequestAdminAction",
        r#"{"GroupId":"{group}","UserMri":"{cora}","ActionType":"Accept"}"#,
        404,
    ),
    (
        "GET",
        "{MT}/analytics/teams/{group}/summary-timeseries?timeperiod=forever",
        "",
        400,
    ),
    (
        "GET",
        "{MT}/analytics/teams/{other_group}/channels/{channel}/summary-timeseries",
        "",
        404,
    ),
    (
        "POST",
        "{PROV}/groups/{group}/clone",
        r#"{"displayName":"Copy","visibility":"Secret"}"#,
        400,
    ),
    ("POST", "{PROV}/teams/{other_group}/channels/{channel}/archive", "", 404),
    ("POST", "{PROV}/teams/{group}/channels/{team}/archive", "", 403),
    ("PUT", "{PROV}/channels/{team}/members", r#"{"users":[]}"#, 403),
    ("GET", "{PROV}/teams/{group}/channels/OID:{guid}@elsewhere", "", 404),
    ("PUT", "{PROV}/conversations/show", r#"{"teamIds":"{team}"}"#, 400),
    (
        "PATCH",
        "{CSA}/api/v1/teams/{team}/tags/{guid}",
        r#"{"tagName":"x"}"#,
        404,
    ),
];

/// The chat service's thread routes refusing a property or an interest that isn't the thread's to take.
const MISPLACED: [(&str, &str); 6] = [
    (
        "{CHAT}/threads/{channel}/properties?name=spaceAdminSettings",
        r#"{"spaceAdminSettings":"{\"userSettings\":{}}"}"#,
    ),
    (
        "{CHAT}/threads/{team}/properties?name=channelSettings",
        r#"{"channelSettings":"{}"}"#,
    ),
    (
        "{CHAT}/threads/{team}/properties?name=spaceAdminSettings",
        r#"{"spaceAdminSettings":"[]"}"#,
    ),
    ("{CHAT}/threads/{team}/properties?name=topic", r#"{"topic":"{}"}"#),
    ("{CHAT}/threads/{team}/properties", r#"{"topic":"{}"}"#),
    ("{CHAT}/threads/{channel}/members/{ada}", r#"{"interest":"Maybe"}"#),
];

#[tokio::test(flavor = "multi_thread")]
async fn a_request_names_its_target_exactly() {
    let world = World::new().await;
    for (method, path, body, expected) in MALFORMED {
        let (status, answer) = world.send("ada", method, path, body).await;
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }
    for (path, body) in MISPLACED {
        assert_eq!(world.send("ada", "PUT", path, body).await.0, 400, "{path}");
    }
    world.stop().await;
}

/// What a member tries beyond a member's part: method, path, body, status.
const BEYOND: [(&str, &str, &str, u16); 9] = [
    (
        "PUT",
        "{MT}/teams/{team}/members",
        r#"{"userMri":"{bert}","updateType":"Promoted"}"#,
        403,
    ),
    ("PUT", "{MT}/teams/{channel}/setDefaultChannel", "", 403),
    ("PATCH", "{MT}/teams/{team}", r#"{"description":"mine"}"#, 403),
    ("POST", "{MT}/teams/{team}/profilepicture?threadId={team}", "{}", 404),
    ("POST", "{PROV}/groups/{group}/unarchive", "", 403),
    (
        "PUT",
        "{PROV}/channels/{channel}/members",
        r#"{"users":[{"id":"{bert}","isModerator":true}]}"#,
        403,
    ),
    (
        "PUT",
        "{CHAT}/threads/{team}/properties?name=spaceAdminSettings",
        r#"{"spaceAdminSettings":"{\"userSettings\":{}}"}"#,
        403,
    ),
    (
        "PUT",
        "{CHAT}/threads/{channel}/properties?name=channelSettings",
        r#"{"channelSettings":"{}"}"#,
        403,
    ),
    (
        "PUT",
        "{CHAT}/threads/{channel}/members/{ada}",
        r#"{"interest":"Interested"}"#,
        403,
    ),
];

#[tokio::test(flavor = "multi_thread")]
async fn a_member_grants_itself_nothing() {
    let world = World::new().await;
    for (method, path, body, expected) in BEYOND {
        let (status, answer) = world.send("bert", method, path, body).await;
        assert_eq!(status, expected, "{method} {path}: {answer}");
    }
    // a public team's member adds a member, never an owner, and changes nobody's role
    let path = "{MT}/teams/{other}/bulkUpdateRoledMembers";
    let add =
        |who: &str, role: u8| format!(r#"{{"users":[{{"mri":"{who}","role":{role}}}],"groupId":"{{other_group}}"}}"#);
    for (who, role, failed) in [("{cora}", 1, 1), ("{bert}", 1, 1), ("{cora}", 0, 0)] {
        let (status, answer) = world.send("bert", "PUT", path, &add(who, role)).await;
        let failures = answer["value"]["failedMembers"].as_array().map(Vec::len);
        assert_eq!((status, failures), (200, Some(failed)), "{who} {role}: {answer}");
    }
    let other = world.name("{other}");
    assert!(!world.seats[2].team(other).await.owner && !world.seats[1].team(other).await.owner);
    world.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_team_takes_as_many_private_channels_as_teams_allows() {
    let world = World::new().await;
    let path = "{PROV}/teams/{group}/channels";
    let named =
        |n: usize| format!(r#"{{"spaceThreadId":"{{team}}","displayName":"Room {n}","channelType":"Private"}}"#);
    for n in 0..30 {
        assert_eq!(world.send("ada", "POST", path, &named(n)).await.0, 200, "{n}");
    }
    let (status, answer) = world.send("ada", "POST", path, &named(30)).await;
    assert_eq!(
        (status, answer["errorCode"].as_str()),
        (403, Some("MaxPrivateChannelLimitExceeded"))
    );
    // a standard channel is still made, and Bert, in none of the private ones, is told of none
    let standard = r#"{"displayName":"Open","groupId":"{group}","channelType":"Standard"}"#;
    assert_eq!(
        world
            .send("ada", "POST", "{MT}/teams/{team}/channels", standard)
            .await
            .0,
        201
    );
    let listed = world.seats[1].team(world.name("{team}")).await;
    assert_eq!(listed.channels.len(), 3);
    world.stop().await;
}
