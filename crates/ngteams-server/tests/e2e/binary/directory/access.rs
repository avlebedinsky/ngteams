//! What one account may read of the directory and of presence: nothing of another deployment's (another tenant's)
//! people, no field Teams keeps private, and raw requests that name other ids change nothing of anyone else.

use serde_json::{Value, json};
use teams_core::Availability;
use teams_core::engine::OwnStatus;
use teams_proto::{GRAPH_AUDIENCE, LOKI_AUDIENCE, MIDDLE_TIER_AUDIENCE, ManualAvailability, PRESENCE_AUDIENCE};

use super::super::push::support::Rig;
use super::super::support::Node;
use super::support::{SHORT_PRESENCE, Seat, bearer, reads_as};
use crate::support::Backend;

async fn send(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

fn get(node: &Node, token: &str, path: &str) -> reqwest::RequestBuilder {
    node.http
        .get(format!("{}{path}", node.endpoint))
        .bearer_auth(token)
        .header("X-ClientType", "Teams")
}

fn post(node: &Node, token: &str, path: &str, body: &Value) -> reqwest::RequestBuilder {
    node.http
        .post(format!("{}{path}", node.endpoint))
        .bearer_auth(token)
        .json(body)
}

const SHORT: &str = "/api/mt/part/own-01/beta/users/fetchShortProfile";
const READ: &str = "/ups/own/v1/presence/getpresence/";

/// Every key of every object in `value`, nested too.
fn all_keys(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                into.push(key.clone());
                all_keys(inner, into);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| all_keys(item, into)),
        _ => {}
    }
}

/// a's tokens at our deployment, one per service.
struct Tokens {
    token: String,
    presence: String,
    graph: String,
}

impl Tokens {
    async fn of(node: &Node) -> Self {
        Self {
            token: bearer(node, "a@own.test", MIDDLE_TIER_AUDIENCE).await,
            presence: bearer(node, "a@own.test", PRESENCE_AUDIENCE).await,
            graph: bearer(node, "a@own.test", GRAPH_AUDIENCE).await,
        }
    }
}

/// The other deployment's tokens open nothing here.
async fn foreign_tokens(ours: &Rig, theirs: &Rig, b: &Seat) {
    // the other deployment's tokens open nothing here, and ours nothing there
    for (audience, path) in [
        (GRAPH_AUDIENCE, format!("/graph/v1.0/users/{}", b.oid())),
        (
            GRAPH_AUDIENCE,
            format!("/graph/v1.0/users/{}/photos/96x96/$value", b.oid()),
        ),
        (GRAPH_AUDIENCE, "/graph/v1.0/me".to_owned()),
        (LOKI_AUDIENCE, format!("/loki/api/v2/person?aadObjectId={}", b.oid())),
        (
            LOKI_AUDIENCE,
            format!("/loki/api/v1/organization?aadObjectId={}", b.oid()),
        ),
    ] {
        let foreign = bearer(&theirs.first, "a@own.test", audience).await;
        assert_eq!(send(get(&ours.first, &foreign, &path)).await.0, 401, "{path}");
    }
    let foreign = bearer(&theirs.first, "a@own.test", MIDDLE_TIER_AUDIENCE).await;
    assert_eq!(send(post(&ours.first, &foreign, SHORT, &json!([b.mri]))).await.0, 401);
    let foreign = bearer(&theirs.first, "a@own.test", PRESENCE_AUDIENCE).await;
    assert_eq!(
        send(post(&ours.first, &foreign, READ, &json!([{"mri": b.mri}])))
            .await
            .0,
        401
    );
}

/// Their people are nobody here.
async fn strangers_are_nobody(ours: &Rig, tokens: &Tokens, stranger: &Seat) {
    let Tokens { token, presence, graph } = tokens;
    // their people are nobody here, even under the same address and the same id form
    let (status, found) = send(post(&ours.first, token, SHORT, &json!([stranger.mri]))).await;
    assert_eq!((status, &found["value"]), (200, &json!([])));
    let (_, read) = send(post(&ours.first, presence, READ, &json!([{"mri": stranger.mri}]))).await;
    assert_eq!(read[0]["status"], 40400, "{read}");
    let (status, _) = send(get(
        &ours.first,
        graph,
        &format!("/graph/v1.0/users/{}", stranger.oid()),
    ))
    .await;
    assert_eq!(status, 404);
}

/// No private field in any answer, whatever is asked for.
async fn no_private_field(ours: &Rig, tokens: &Tokens, b: &Seat) {
    let Tokens { token, presence, graph } = tokens;
    // no private field, whatever is asked for
    let (_, read) = send(post(&ours.first, presence, READ, &json!([{"mri": b.mri}]))).await;
    let (status, whole) = send(get(&ours.first, graph, &format!("/graph/v1.0/users/{}", b.oid()))).await;
    assert_eq!(status, 200);
    let mut keys = Vec::new();
    all_keys(&whole, &mut keys);
    keys.sort();
    assert_eq!(
        keys,
        [
            "@odata.context",
            "businessPhones",
            "displayName",
            "givenName",
            "id",
            "jobTitle",
            "mail",
            "mobilePhone",
            "officeLocation",
            "preferredLanguage",
            "surname",
            "userPrincipalName",
        ]
    );
    let private = "passwordHash,password_hash,passwordVersion,password_version,isAdmin,is_admin,deletedAt,\
                   mustChangePassword,signInActivity,accountEnabled,createdDateTime,manager,directReports,pwd_ver";
    let (status, selected) = send(get(
        &ours.first,
        graph,
        &format!("/graph/v1.0/users/{}?$select={private}", b.oid()),
    ))
    .await;
    eprintln!("Graph $select of private fields: {status} {selected}");
    let mut keys = Vec::new();
    all_keys(&selected, &mut keys);
    assert!(
        keys.iter().all(|key| key == "@odata.context" || key == "id"),
        "{selected}"
    );
    let (_, short) = send(post(&ours.first, token, SHORT, &json!([b.mri]))).await;
    let loki = bearer(&ours.first, "a@own.test", LOKI_AUDIENCE).await;
    let (_, person) = send(get(
        &ours.first,
        &loki,
        &format!("/loki/api/v2/person?aadObjectId={}", b.oid()),
    ))
    .await;
    let (_, line) = send(get(
        &ours.first,
        &loki,
        &format!("/loki/api/v1/organization?aadObjectId={}", b.oid()),
    ))
    .await;
    for answer in [&whole, &short, &person, &line, &read] {
        let text = answer.to_string().to_lowercase();
        for secret in ["argon2", "password", "pwd", "is_admin", "isadmin", "deleted", "secret"] {
            assert!(!text.contains(secret), "{secret} in {answer}");
        }
    }
}

/// Raw requests naming other ids change nothing of anyone else.
async fn other_ids(ours: &Rig, tokens: &Tokens, a: &Seat, b: &mut Seat) {
    let Tokens { presence, .. } = tokens;
    // raw requests naming other ids
    b.in_use().await;
    reads_as(a, &b.mri, "Available").await;
    // a reports "Away" for b's endpoint id: a's own presence, never b's
    let endpoint = json!({"id": b.epid, "availability": "Away", "activity": "Away", "activityReporting": "Transport",
        "deviceType": "Web"});
    let (status, _) = send(
        ours.first
            .http
            .put(format!("{}/ups/own/v1/me/endpoints/", ours.first.endpoint))
            .bearer_auth(presence)
            .header("x-ms-endpoint-id", &b.epid)
            .json(&endpoint),
    )
    .await;
    eprintln!("a reports b's endpoint id: {status}");
    assert_eq!(a.reads(&b.mri).await.as_deref(), Some("Available"));
    b.report(OwnStatus::Manual(ManualAvailability::Busy)).await;
    reads_as(a, &b.mri, "Busy").await;
    // a subscribes b's endpoint, or a's own endpoint to b's push URL: refused
    let subscription = |epid: &str, uri: String| {
        let body = json!({"trouterUri": uri, "shouldPurgePreviousSubscriptions": true,
            "subscriptionsToAdd": [{"mri": b.mri, "source": "ups"}], "subscriptionsToRemove": []});
        ours.first
            .http
            .post(format!(
                "{}/ups/own/v1/pubsub/subscriptions/{epid}",
                ours.first.endpoint
            ))
            .bearer_auth(presence)
            .header("x-ms-endpoint-id", epid)
            .header("x-ms-correlation-id", uuid::Uuid::new_v4().to_string())
            .header("x-ms-client-version", "1415/26100212011")
            .json(&body)
    };
    let foreign_uri = format!("{}/trouter/v4/f/{}/unifiedPresenceService", ours.origin, b.epid);
    for (epid, uri) in [(b.epid.clone(), foreign_uri.clone()), (a.epid.clone(), foreign_uri)] {
        let (status, answer) = send(subscription(&epid, uri)).await;
        assert_eq!(status, 400, "{answer}");
    }
    // a can't clear or set b's status: the request has no target, and a token of b's is b's alone
    let (status, _) = send(
        ours.first
            .http
            .put(format!("{}/ups/own/v1/me/forceavailability/", ours.first.endpoint))
            .bearer_auth(presence)
            .json(&json!({"availability": "Available", "mri": b.mri})),
    )
    .await;
    eprintln!("a's forceavailability naming b: {status}");
    reads_as(a, &b.mri, "Busy").await;
}

/// Graph reads by address or with a path that walks out of `users/`, and a token for another service.
async fn odd_paths(ours: &Rig, tokens: &Tokens, b: &Seat) {
    let graph = &tokens.graph;
    let b_presence = bearer(&ours.first, "b@own.test", PRESENCE_AUDIENCE).await;
    // a Graph read by address or with a path that walks out of users/
    for path in [
        "/graph/v1.0/users/b@own.test".to_owned(),
        format!("/graph/v1.0/users/{}%2F..%2F..%2Fme", b.oid()),
        format!("/graph/v1.0/users/..%2F{}/photos/96x96/$value", b.oid()),
        "/graph/v1.0/users/%2E%2E/photos/96x96/$value".to_owned(),
    ] {
        let (status, answer) = send(get(&ours.first, graph, &path)).await;
        eprintln!("{path}: {status} {answer}");
        assert_ne!(status, 500, "{path}");
    }
    let (status, _) = send(get(&ours.first, &b_presence, "/graph/v1.0/me")).await;
    assert_eq!(status, 401, "a presence token is no Graph token");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_account_reads_nothing_of_another_tenant_and_no_private_field() {
    let ours = Rig::start_with(Backend::Sqlite, SHORT_PRESENCE).await;
    let theirs = Rig::start_with(Backend::Sqlite, "").await;
    let mut a = Seat::sign_in(&ours.first, &ours.origin, "a@own.test").await;
    let b = Seat::sign_in(&ours.first, &ours.origin, "b@own.test").await;
    let mut stranger = Seat::sign_in(&theirs.first, &theirs.origin, "a@own.test").await;
    stranger.in_use().await;
    ours.first.cli(
        &[
            "profile",
            "set",
            "b@own.test",
            "--job-title",
            "Engineer",
            "--manager",
            "c@own.test",
        ],
        "",
    );
    theirs
        .first
        .cli(&["profile", "set", "b@own.test", "--job-title", "Secret"], "");

    let tokens = Tokens::of(&ours.first).await;
    foreign_tokens(&ours, &theirs, &b).await;
    strangers_are_nobody(&ours, &tokens, &stranger).await;
    no_private_field(&ours, &tokens, &b).await;
    let mut b = b;
    other_ids(&ours, &tokens, &a, &mut b).await;
    odd_paths(&ours, &tokens, &b).await;

    // a's view of the other tenant's account shows no presence, however long it watches
    let mark = a.mark();
    assert_eq!(a.watch(&stranger.mri).await, Availability::Unknown);
    a.idle_for(std::time::Duration::from_secs(3)).await;
    let told = a.told(mark, &stranger.mri);
    assert!(told.iter().all(|shown| *shown == Availability::Unknown), "{told:?}");
    stranger.close();
    drop((a, b));
    theirs.stop().await;
    ours.stop().await;
}
