//! The presence service's wire, request by request: what the client's typed entries never send (another form, another
//! account's endpoint, too much), and the deliveries a bare socket gets.

use std::time::Duration;

use serde_json::{Value, json};
use teams_proto::PRESENCE_AUDIENCE;

use crate::support::{Backend, TestServer};
use crate::trouter::refusals::{EPID, IC3, Raw, query, register, registration};
use crate::trouter::server_with;

const OTHER_EPID: &str = "00000000-0000-4000-8000-0000000000e2";
const NEVER: &str = "0001-01-01T00:00:00Z";

async fn token(server: &TestServer, upn: &str, audience: &str) -> String {
    let session = server.session(upn).await;
    session.access_token(audience).await.unwrap().token.expose().to_owned()
}

fn ups(server: &TestServer, path: &str) -> String {
    server.url(&format!("/ups/own/v1/{path}"))
}

async fn answer(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn read(server: &TestServer, token: &str, body: &Value) -> (u16, Value) {
    let request = reqwest::Client::new()
        .post(ups(server, "presence/getpresence/"))
        .bearer_auth(token)
        .json(body);
    answer(request).await
}

/// The presence of `mri` as `token`'s account reads it.
async fn shown(server: &TestServer, token: &str, mri: &str) -> Value {
    let (status, answer) = read(server, token, &json!([{"mri": mri}])).await;
    assert_eq!(status, 200, "{answer}");
    answer[0]["presence"].clone()
}

async fn put(server: &TestServer, token: &str, path: &str, epid: &str, body: Option<Value>) -> u16 {
    let mut request = reqwest::Client::new()
        .put(ups(server, path))
        .bearer_auth(token)
        .header("x-ms-endpoint-id", epid)
        .header("content-type", "application/json");
    if let Some(body) = body {
        request = request.body(body.to_string());
    }
    request.send().await.unwrap().status().as_u16()
}

fn endpoint(epid: &str, availability: &str, activity: &str) -> Value {
    json!({"id": epid, "availability": availability, "activity": activity, "activityReporting": "Transport",
        "deviceType": "Web"})
}

struct Trio {
    server: TestServer,
    a: String,
    b: String,
    a_token: String,
    b_token: String,
}

async fn trio(configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Trio {
    let server = server_with(Backend::Sqlite, configure).await;
    let a = format!("8:orgid:{}", server.add_user("wa@own.test", "Ada").await);
    let b = format!("8:orgid:{}", server.add_user("wb@own.test", "Bert").await);
    let a_token = token(&server, "wa@own.test", PRESENCE_AUDIENCE).await;
    let b_token = token(&server, "wb@own.test", PRESENCE_AUDIENCE).await;
    Trio {
        server,
        a,
        b,
        a_token,
        b_token,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_presence_read_answers_each_account_asked_in_teams_shape() {
    let Trio {
        server, a, b, a_token, ..
    } = trio(|_| {}).await;
    let stranger = format!("8:orgid:{}", uuid::Uuid::new_v4());
    let asked = json!([{"mri": a, "source": "ups"}, {"mri": b}, {"mri": stranger}, {"mri": "28:bot"}]);
    let (status, answer) = read(&server, &a_token, &asked).await;
    assert_eq!(status, 200, "{answer}");
    let items = answer.as_array().unwrap();
    assert_eq!(items.len(), 4);
    let offline = |network: &str| {
        json!({"sourceNetwork": network, "calendarData": {"isOutOfOffice": false}, "capabilities": [],
            "availability": "Offline", "activity": "Offline", "lastActiveTime": NEVER})
    };
    assert_eq!(items[0]["mri"], a);
    assert_eq!(items[0]["status"], 20000);
    assert_eq!(items[0]["etagMatch"], false);
    assert!(items[0]["etag"].is_string());
    assert_eq!(items[0]["source"], "ups");
    assert_eq!(items[0]["presence"], offline("Self"));
    assert_eq!(items[1]["mri"], b);
    assert_eq!(items[1]["presence"], offline("SameEnterprise"));
    assert_eq!(items[1].get("source"), None);
    for unknown in &items[2..] {
        assert_eq!(unknown["status"], 40400);
        assert_eq!(unknown.get("presence"), None);
    }

    let ic3 = token(&server, "wa@own.test", IC3).await;
    assert_eq!(read(&server, &ic3, &asked).await.0, 401);
    let bare = reqwest::Client::new().post(ups(&server, "presence/getpresence/"));
    assert_eq!(bare.json(&asked).send().await.unwrap().status(), 401);
    let many: Vec<Value> = (0..651)
        .map(|_| json!({"mri": format!("8:orgid:{}", uuid::Uuid::new_v4())}))
        .collect();
    for refused in [json!([]), json!({"mri": a}), json!([{"id": a}]), json!(many)] {
        assert_eq!(read(&server, &a_token, &refused).await.0, 400, "{refused}");
    }
    ngteams_server::admin::set_disabled(&server.app.db, server.app.fanout.as_ref(), "wa@own.test", true)
        .await
        .unwrap();
    assert_eq!(read(&server, &a_token, &asked).await.0, 403);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_shows_as_its_endpoints_and_its_manual_status_say() {
    let Trio {
        server,
        a,
        b,
        a_token,
        b_token,
    } = trio(|_| {}).await;
    // a manual status alone shows nothing
    assert_eq!(
        put(
            &server,
            &b_token,
            "me/forceavailability/",
            EPID,
            Some(json!({"availability": "Busy"}))
        )
        .await,
        200
    );
    assert_eq!(shown(&server, &a_token, &b).await["availability"], "Offline");
    assert_eq!(put(&server, &b_token, "me/forceavailability/", EPID, None).await, 200);

    let available = endpoint(EPID, "Available", "Available");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", EPID, Some(available)).await,
        201
    );
    let seen = shown(&server, &a_token, &b).await;
    assert_eq!(seen["availability"], "Available");
    assert_eq!(seen["activity"], "Available");
    assert_eq!(seen["deviceType"], "Web");
    let last = seen["lastActiveTime"].as_str().unwrap();
    let last = chrono::DateTime::parse_from_rfc3339(last).unwrap();
    assert!((chrono::Utc::now() - last.to_utc()).num_seconds().abs() < 60, "{last}");
    assert_eq!(shown(&server, &b_token, &b).await["sourceNetwork"], "Self");
    assert_eq!(shown(&server, &a_token, &a).await["availability"], "Offline");

    // of two endpoints the busier one shows, with its activity
    let in_a_call = endpoint(OTHER_EPID, "Busy", "InACall");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", OTHER_EPID, Some(in_a_call)).await,
        201
    );
    let seen = shown(&server, &a_token, &b).await;
    assert_eq!(
        (&seen["availability"], &seen["activity"]),
        (&json!("Busy"), &json!("InACall"))
    );
    // a manual status is over them all, until it is reset
    for word in ["DoNotDisturb", "Away", "BeRightBack", "Available", "Offline"] {
        assert_eq!(
            put(
                &server,
                &b_token,
                "me/forceavailability/",
                EPID,
                Some(json!({"availability": word}))
            )
            .await,
            200
        );
        let seen = shown(&server, &a_token, &b).await;
        assert_eq!((&seen["availability"], &seen["activity"]), (&json!(word), &json!(word)));
    }
    assert_eq!(put(&server, &b_token, "me/forceavailability/", EPID, None).await, 200);
    assert_eq!(shown(&server, &a_token, &b).await["availability"], "Busy");

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_or_a_status_in_another_form_is_refused() {
    let Trio { server, b_token, .. } = trio(|_| {}).await;
    for (path, epid, body) in [
        (
            "me/endpoints/",
            EPID,
            json!({"id": EPID, "availability": "Dancing", "activity": "Available"}),
        ),
        (
            "me/endpoints/",
            EPID,
            json!({"id": EPID, "availability": "Available", "activity": "a b"}),
        ),
        (
            "me/endpoints/",
            EPID,
            json!({"id": OTHER_EPID, "availability": "Available", "activity": "Available"}),
        ),
        (
            "me/endpoints/",
            "not-a-guid",
            json!({"id": "not-a-guid", "availability": "Available", "activity": "x"}),
        ),
        ("me/endpoints/", EPID, json!([])),
        ("me/forceavailability/", EPID, json!({"availability": "Dancing"})),
        ("me/forceavailability/", EPID, json!({"status": "Busy"})),
    ] {
        assert_eq!(
            put(&server, &b_token, path, epid, Some(body.clone())).await,
            400,
            "{path} {body}"
        );
    }
    let ic3 = token(&server, "wb@own.test", IC3).await;
    assert_eq!(put(&server, &ic3, "me/forceavailability/", EPID, None).await, 401);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_holds_only_so_many_endpoints() {
    let Trio { server, b_token, .. } = trio(|config| config.presence.max_endpoints = 2).await;
    let epid = |n: u8| format!("00000000-0000-4000-8000-0000000000{n:02x}");
    for n in 0..2 {
        let body = endpoint(&epid(n), "Available", "Available");
        assert_eq!(put(&server, &b_token, "me/endpoints/", &epid(n), Some(body)).await, 201);
    }
    let third = endpoint(&epid(2), "Available", "Available");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", &epid(2), Some(third)).await,
        429
    );
    // one it holds already is reported again
    let again = endpoint(&epid(1), "Away", "Away");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", &epid(1), Some(again)).await,
        201
    );
    server.stop().await;
}

/// A messaging socket of `upn`, connected and registered; its push URL.
async fn socket(server: &TestServer, upn: &str, epid: &str) -> (Raw, String) {
    let ic3 = token(server, upn, IC3).await;
    let mut socket = Raw::open(server, &query(epid, "TeamsCDL")).await;
    let info = socket.connect(&ic3).await;
    let surl = info["surl"].as_str().unwrap().to_owned();
    let body = registration("TeamsCDLWebWorker", epid, &surl, 3600);
    assert_eq!(register(server, Some(&ic3), &body).await, 202);
    (socket, surl)
}

struct Subscribe<'a> {
    epid: &'a str,
    uri: String,
    purge: bool,
    add: Vec<&'a str>,
    remove: Vec<&'a str>,
}

impl Subscribe<'_> {
    fn body(&self) -> Value {
        let sourced =
            |names: &[&str]| -> Vec<Value> { names.iter().map(|mri| json!({"mri": mri, "source": "ups"})).collect() };
        json!({"trouterUri": self.uri, "shouldPurgePreviousSubscriptions": self.purge,
            "subscriptionsToAdd": sourced(&self.add), "subscriptionsToRemove": sourced(&self.remove)})
    }

    fn request(&self, server: &TestServer, token: &str) -> reqwest::RequestBuilder {
        reqwest::Client::new()
            .post(ups(server, &format!("pubsub/subscriptions/{}", self.epid)))
            .bearer_auth(token)
            .header("x-ms-endpoint-id", self.epid)
            .header("x-ms-correlation-id", uuid::Uuid::new_v4().to_string())
            .header("x-ms-client-version", "1415/26100212011")
            .json(&self.body())
    }

    async fn send(&self, server: &TestServer, token: &str) -> (u16, Value) {
        answer(self.request(server, token)).await
    }
}

/// The next presence delivery of `socket`: its body.
async fn pushed(socket: &mut Raw) -> Value {
    loop {
        let frame = socket.next().await.expect("the socket closed");
        let Some(delivery) = frame.strip_prefix("3:::") else {
            continue;
        };
        let delivery: Value = serde_json::from_str(delivery).unwrap();
        if delivery["url"].as_str().unwrap().ends_with("/unifiedPresenceService") {
            assert_eq!(delivery["method"], "POST");
            assert_eq!(delivery["headers"]["X-Trouter-Delivery-Control"], "async; ttl=600");
            return serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap();
        }
    }
}

async fn quiet(socket: &mut Raw) -> bool {
    socket.next_within(Duration::from_millis(600)).await.is_err()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscribed_endpoint_gets_a_snapshot_and_then_each_change_until_it_unsubscribes() {
    let Trio {
        server,
        a,
        b,
        a_token,
        b_token,
    } = trio(|_| {}).await;
    let (mut ada, surl) = socket(&server, "wa@own.test", EPID).await;
    let stranger = format!("8:orgid:{}", uuid::Uuid::new_v4());
    let subscribe = |purge: bool, add: Vec<&'static str>, remove: Vec<&'static str>| Subscribe {
        epid: EPID,
        uri: format!("{surl}unifiedPresenceService"),
        purge,
        add,
        remove,
    };
    let (b_mri, stranger_mri): (&'static str, &'static str) = (b.clone().leak(), stranger.leak());
    let a_mri: &'static str = a.clone().leak();
    let (status, taken) = subscribe(true, vec![b_mri, stranger_mri, "28:bot"], vec![])
        .send(&server, &a_token)
        .await;
    assert_eq!(status, 200, "{taken}");
    assert_eq!(
        taken,
        json!({"addStatuses": [
            {"mri": b_mri, "source": "ups", "status": 20200},
            {"mri": stranger_mri, "source": "ups", "status": 40020},
            {"mri": "28:bot", "source": "ups", "status": 40020},
        ], "removeStatuses": []})
    );
    let snapshot = pushed(&mut ada).await;
    assert_eq!(snapshot["isSnapshot"], true);
    assert_eq!(snapshot["presence"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["presence"][0]["mri"], b_mri);
    assert_eq!(snapshot["presence"][0]["source"], "ups");
    assert_eq!(snapshot["presence"][0]["presence"]["availability"], "Offline");
    assert_eq!(snapshot["presence"][0]["presence"]["sourceNetwork"], "SameEnterprise");

    let available = endpoint(OTHER_EPID, "Available", "Available");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", OTHER_EPID, Some(available.clone())).await,
        201
    );
    let change = pushed(&mut ada).await;
    assert_eq!(change.get("isSnapshot"), None);
    assert_eq!(change["presence"][0]["mri"], b_mri);
    assert_eq!(change["presence"][0]["presence"]["availability"], "Available");
    assert!(change["presence"][0]["etag"].is_string());
    // the same report again changes nothing, so nothing is pushed
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", OTHER_EPID, Some(available)).await,
        201
    );
    assert!(quiet(&mut ada).await);

    // more people are added without the purge; the snapshot names only them
    let (status, taken) = subscribe(false, vec![a_mri], vec![]).send(&server, &a_token).await;
    assert_eq!((status, &taken["addStatuses"][0]["status"]), (200, &json!(20200)));
    let snapshot = pushed(&mut ada).await;
    assert_eq!(snapshot["presence"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["presence"][0]["presence"]["sourceNetwork"], "Self");
    let busy = Some(json!({"availability": "Busy"}));
    assert_eq!(
        put(&server, &b_token, "me/forceavailability/", OTHER_EPID, busy).await,
        200
    );
    assert_eq!(
        pushed(&mut ada).await["presence"][0]["presence"]["availability"],
        "Busy"
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removal_and_the_purge_end_what_an_endpoint_watches() {
    let Trio {
        server,
        a,
        b,
        a_token,
        b_token,
    } = trio(|_| {}).await;
    let (mut ada, surl) = socket(&server, "wa@own.test", EPID).await;
    let subscribe = |purge: bool, add: Vec<&'static str>, remove: Vec<&'static str>| Subscribe {
        epid: EPID,
        uri: format!("{surl}unifiedPresenceService"),
        purge,
        add,
        remove,
    };
    let (a_mri, b_mri): (&'static str, &'static str) = (a.leak(), b.leak());
    let available = endpoint(OTHER_EPID, "Available", "Available");
    assert_eq!(
        put(&server, &b_token, "me/endpoints/", OTHER_EPID, Some(available)).await,
        201
    );
    let (status, _) = subscribe(true, vec![b_mri], vec![]).send(&server, &a_token).await;
    assert_eq!(status, 200);
    assert_eq!(
        pushed(&mut ada).await["presence"][0]["presence"]["availability"],
        "Available"
    );

    // a removal ends one person's changes
    let (status, taken) = subscribe(false, vec![], vec![b_mri]).send(&server, &a_token).await;
    assert_eq!(status, 200);
    assert_eq!(
        taken,
        json!({"addStatuses": [], "removeStatuses": [{"mri": b_mri, "source": "ups", "status": 20200}]})
    );
    let busy = Some(json!({"availability": "Busy"}));
    assert_eq!(
        put(&server, &b_token, "me/forceavailability/", OTHER_EPID, busy).await,
        200
    );
    assert!(quiet(&mut ada).await);
    assert_eq!(server.app.presence.watchers(b_mri).await.unwrap(), 0);

    // the purge drops what the endpoint watched before
    let (status, _) = subscribe(false, vec![b_mri], vec![]).send(&server, &a_token).await;
    assert_eq!(status, 200);
    assert_eq!(
        pushed(&mut ada).await["presence"][0]["presence"]["availability"],
        "Busy"
    );
    let (status, _) = subscribe(true, vec![a_mri], vec![]).send(&server, &a_token).await;
    assert_eq!(status, 200);
    pushed(&mut ada).await;
    assert_eq!(server.app.presence.watchers(b_mri).await.unwrap(), 0);
    assert_eq!(server.app.presence.watchers(a_mri).await.unwrap(), 1);
    assert_eq!(
        put(&server, &b_token, "me/forceavailability/", OTHER_EPID, None).await,
        200
    );
    assert!(quiet(&mut ada).await);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subscription_is_taken_only_in_teams_form_for_the_accounts_own_endpoint() {
    let Trio {
        server,
        a,
        b,
        a_token,
        b_token,
    } = trio(|config| config.presence.max_subscriptions = 2).await;
    let (_ada, surl) = socket(&server, "wa@own.test", EPID).await;
    let uri = format!("{surl}unifiedPresenceService");
    let good = |add: Vec<&'static str>| Subscribe {
        epid: EPID,
        uri: uri.clone(),
        purge: false,
        add,
        remove: vec![],
    };
    let b_mri: &'static str = b.clone().leak();
    let a_mri: &'static str = a.clone().leak();

    // UPS answers a subscription without its mandatory headers 400
    for missing in ["x-ms-correlation-id", "x-ms-client-version"] {
        let mut request = good(vec![b_mri]).request(&server, &a_token).build().unwrap();
        request.headers_mut().remove(missing);
        let response = reqwest::Client::new().execute(request).await.unwrap();
        assert_eq!(response.status(), 400);
        let text = response.text().await.unwrap();
        assert!(
            text.contains(&format!("Missing mandatory header '{missing}'")),
            "{text}"
        );
    }
    let mut other_header = good(vec![b_mri]).request(&server, &a_token).build().unwrap();
    other_header
        .headers_mut()
        .insert("x-ms-endpoint-id", OTHER_EPID.parse().unwrap());
    assert_eq!(
        reqwest::Client::new().execute(other_header).await.unwrap().status(),
        400
    );
    // another account's endpoint, another path, another host
    let foreign = |uri: &str| Subscribe {
        epid: EPID,
        uri: uri.to_owned(),
        purge: false,
        add: vec![b_mri],
        remove: vec![],
    };
    assert_eq!(foreign(&uri).send(&server, &b_token).await.0, 400);
    assert_eq!(
        foreign(&format!("{surl}messaging")).send(&server, &a_token).await.0,
        400
    );
    let elsewhere = uri.replace("own.example.test", "elsewhere.example.test");
    assert_eq!(foreign(&elsewhere).send(&server, &a_token).await.0, 400);
    let malformed = reqwest::Client::new()
        .post(ups(&server, &format!("pubsub/subscriptions/{EPID}")))
        .bearer_auth(&a_token)
        .header("x-ms-endpoint-id", EPID)
        .header("x-ms-correlation-id", "c")
        .header("x-ms-client-version", "v")
        .json(&json!({"trouterUri": uri}));
    assert_eq!(malformed.send().await.unwrap().status(), 400);
    let bare = Subscribe {
        epid: "not-a-guid",
        ..good(vec![b_mri])
    };
    assert_eq!(bare.send(&server, &a_token).await.0, 400);

    // an endpoint watches only so many people
    let extra = format!("8:orgid:{}", server.add_user("wc@own.test", "Cora").await);
    let extra: &'static str = extra.leak();
    let (status, taken) = good(vec![b_mri, a_mri, extra]).send(&server, &a_token).await;
    assert_eq!(status, 200);
    let statuses: Vec<i64> = taken["addStatuses"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["status"].as_i64().unwrap())
        .collect();
    assert_eq!(statuses, [20200, 20200, 40040]);
    // one it watches already is taken again
    let (_, taken) = good(vec![b_mri]).send(&server, &a_token).await;
    assert_eq!(taken["addStatuses"][0]["status"], 20200);
    server.stop().await;
}

/// The seconds each Redis key matching `pattern` still lives (`-1`: for good).
async fn lifetimes(redis: &mut redis::aio::MultiplexedConnection, pattern: &str) -> Vec<i64> {
    let keys: Vec<String> = redis::cmd("KEYS").arg(pattern).query_async(redis).await.unwrap();
    let mut lifetimes = Vec::new();
    for key in keys {
        lifetimes.push(redis::cmd("TTL").arg(&key).query_async(redis).await.unwrap());
    }
    lifetimes
}

#[tokio::test(flavor = "multi_thread")]
async fn what_an_endpoint_watches_is_kept_in_redis_only_for_the_subscription_lifetime() {
    let Some(url) = crate::support::redis_url() else { return };
    let shared = url.clone();
    let Trio {
        server, a, b, a_token, ..
    } = trio(|config| {
        config.redis_url = Some(shared);
        config.presence.subscription_ttl = 90;
    })
    .await;
    let (mut ada, surl) = socket(&server, "wa@own.test", EPID).await;
    let b_mri: &'static str = b.clone().leak();
    let subscribe = Subscribe {
        epid: EPID,
        uri: format!("{surl}unifiedPresenceService"),
        purge: true,
        add: vec![b_mri],
        remove: vec![],
    };
    assert_eq!(subscribe.send(&server, &a_token).await.0, 200);
    pushed(&mut ada).await;

    let mut redis = redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    let a_oid = a.strip_prefix("8:orgid:").unwrap();
    for pattern in [
        format!("ngts:presence:watching:{a_oid}|*"),
        format!("ngts:presence:watchers:{b}"),
    ] {
        let lifetimes = lifetimes(&mut redis, &pattern).await;
        assert_eq!(lifetimes.len(), 1, "{pattern}");
        assert!((1..=90).contains(&lifetimes[0]), "{pattern} lives {lifetimes:?}");
    }
    server.stop().await;
}
