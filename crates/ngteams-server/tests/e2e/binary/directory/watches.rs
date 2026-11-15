//! Watches that end: a client that quit or stopped watching gets no presence pushes any more, and many short client
//! sessions leave nothing behind in Redis.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use teams_core::Availability;
use teams_core::engine::OwnStatus;
use teams_proto::ManualAvailability;

use super::super::push::support::{IC3, Raw, Rig, register, token};
use super::super::support::Node;
use super::support::{Seat, bearer};
use crate::support::{Backend, PASSWORD, PG_ENV, backends, redis_url};

/// Every Redis key matching `pattern`, with the seconds it still lives (`-1`: for good).
async fn keys(redis: &mut redis::aio::MultiplexedConnection, pattern: &str) -> Vec<(String, i64)> {
    let found: Vec<String> = redis::cmd("KEYS").arg(pattern).query_async(redis).await.unwrap();
    let mut keys = Vec::new();
    for key in found {
        let ttl: i64 = redis::cmd("TTL").arg(&key).query_async(redis).await.unwrap();
        keys.push((key, ttl));
    }
    keys.sort();
    keys
}

/// The members of the sorted set `key` that start with `prefix`.
async fn members(redis: &mut redis::aio::MultiplexedConnection, key: &str, prefix: &str) -> Vec<String> {
    let all: Vec<String> = redis::cmd("ZRANGE")
        .arg(key)
        .arg(0)
        .arg(-1)
        .query_async(redis)
        .await
        .unwrap();
    all.into_iter().filter(|member| member.starts_with(prefix)).collect()
}

async fn connection(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

fn postgres_and_redis(test: &str) -> Option<String> {
    if !backends().contains(&Backend::Postgres) {
        eprintln!("SKIP {test}: set {PG_ENV}");
        return None;
    }
    redis_url()
}

/// How long a subscription holds in the 50-session test, in seconds.
const SUBSCRIPTION: i64 = 15;

/// 50 short sessions of b's client, half at each node, each watching a, c and d and quitting once it saw a. While
/// they run, what each session watches lives only as long as its subscription; once the last has ended nothing of
/// theirs is left in Redis, and a's later changes go out at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fifty_short_client_sessions_leave_nothing_behind_in_redis() {
    let Some(url) = postgres_and_redis("50 sessions") else {
        return;
    };
    let config = format!("[presence]\nendpoint_ttl = 5\nsubscription_ttl = {SUBSCRIPTION}\n");
    let rig = Rig::start_with(Backend::Postgres, &config).await;
    let mut a = Seat::sign_in(&rig.first, &rig.origin, "a@own.test").await;
    a.in_use().await;
    let c = Seat::sign_in(&rig.first, &rig.origin, "c@own.test").await;
    let d = Seat::sign_in(rig.other(), &rig.origin, "d@own.test").await;
    let b_session = Arc::new(rig.first.sign_in("b@own.test", PASSWORD).await);
    let mut redis = connection(&url).await;
    let b_oid = {
        let skype = b_session.skypetoken().await.unwrap();
        skype
            .skype_id
            .as_deref()
            .unwrap()
            .strip_prefix("orgid:")
            .unwrap()
            .to_owned()
    };
    let watching = format!("ngts:presence:watching:{b_oid}|*");
    let mut epids = Vec::new();
    let started = tokio::time::Instant::now();
    for n in 0..50 {
        let node = if n % 2 == 0 { &rig.first } else { rig.other() };
        let mut b = Seat::on(node, &rig.origin, "b@own.test", Arc::clone(&b_session)).await;
        let mark = b.mark();
        for watched in [&a.mri, &c.mri, &d.mri] {
            b.watch(watched).await;
        }
        b.sees(mark, &a.mri, &Availability::Available).await;
        b.sees(mark, &d.mri, &Availability::Offline).await;
        epids.push(b.epid.clone());
        b.close();
    }
    eprintln!("50 sessions took {:?}", started.elapsed());

    let kept = keys(&mut redis, &watching).await;
    eprintln!("right after the sessions: {} watching keys of b", kept.len());
    assert!(kept.len() <= 50, "{kept:?}");
    for (key, ttl) in &kept {
        // 0: under a second left
        assert!((0..=SUBSCRIPTION).contains(ttl), "{key} lives {ttl}");
    }
    for of in [&a.mri, &c.mri, &d.mri] {
        let key = format!("ngts:presence:watchers:{of}");
        let ttl: i64 = redis::cmd("TTL").arg(&key).query_async(&mut redis).await.unwrap();
        assert!(ttl == -2 || (0..=SUBSCRIPTION).contains(&ttl), "{key} lives {ttl}");
        let named: i64 = redis::cmd("ZCARD").arg(&key).query_async(&mut redis).await.unwrap();
        eprintln!("{key}: {named} watchers, lives {ttl}");
    }

    // once the last subscription ended, nothing of b's sessions is left
    tokio::time::sleep(Duration::from_secs(u64::try_from(SUBSCRIPTION).unwrap() + 2)).await;
    assert_eq!(keys(&mut redis, &watching).await, [], "b's ended sessions still watch");
    for of in [&a.mri, &c.mri, &d.mri] {
        let key = format!("ngts:presence:watchers:{of}");
        let left = members(&mut redis, &key, &b_oid).await;
        assert_eq!(left, Vec::<String>::new(), "{key} still names b's ended sessions");
    }
    let all = keys(&mut redis, "ngts:presence:*").await;
    let forever: Vec<_> = all
        .iter()
        .filter(|(key, ttl)| *ttl == -1 && key != "ngts:presence:due:accounts")
        .collect();
    assert_eq!(forever, Vec::<&(String, i64)>::new(), "keys kept for good");
    for epid in &epids {
        let shown = all.iter().filter(|(key, _)| key.contains(epid.as_str())).count();
        assert_eq!(shown, 0, "{epid} is still named");
    }

    // a new session of b still hears of a at once
    let mut b = Seat::on(rig.other(), &rig.origin, "b@own.test", Arc::clone(&b_session)).await;
    let mark = b.mark();
    b.watch(&a.mri).await;
    b.sees(mark, &a.mri, &Availability::Available).await;
    let mark = b.mark();
    a.report(OwnStatus::Manual(ManualAvailability::Busy)).await;
    b.sees(mark, &a.mri, &Availability::Busy).await;
    a.report(OwnStatus::ClearManual).await;
    drop((a, b, c, d));
    rig.stop().await;
}

/// A bare push socket of `upn` at `node` with the endpoint `epid`, connected and registered; its push URL.
async fn socket(node: &Node, origin: &str, upn: &str, epid: &str) -> (Raw, String) {
    let ic3 = token(node, upn, IC3).await;
    let mut socket = Raw::open(node, origin, epid).await;
    let info = socket.connect(&ic3).await;
    let surl = info["surl"].as_str().unwrap().to_owned();
    assert_eq!(register(node, &ic3, epid, &surl).await, 202);
    (socket, surl)
}

/// UPS's subscription of `epid` (its push URL `surl`) to `add`, less `remove`, as the client sends it.
async fn subscribe(node: &Node, token: &str, epid: &str, surl: &str, add: &[&str], remove: &[&str]) -> Value {
    let sourced =
        |names: &[&str]| -> Vec<Value> { names.iter().map(|mri| json!({"mri": mri, "source": "ups"})).collect() };
    let body = json!({"trouterUri": format!("{surl}unifiedPresenceService"), "shouldPurgePreviousSubscriptions": false,
        "subscriptionsToAdd": sourced(add), "subscriptionsToRemove": sourced(remove)});
    let response = node
        .http
        .post(format!("{}/ups/own/v1/pubsub/subscriptions/{epid}", node.endpoint))
        .bearer_auth(token)
        .header("x-ms-endpoint-id", epid)
        .header("x-ms-correlation-id", uuid::Uuid::new_v4().to_string())
        .header("x-ms-client-version", "1415/26100212011")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

/// The presence deliveries `socket` gets within `wait`: each one's body.
async fn deliveries(socket: &mut Raw, wait: Duration) -> Vec<Value> {
    let until = tokio::time::Instant::now() + wait;
    let mut found = Vec::new();
    loop {
        let left = until.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            return found;
        }
        let Ok(Some(frame)) = socket.next_within(left).await else {
            return found;
        };
        let Some(delivery) = frame.strip_prefix("3:::") else {
            continue;
        };
        let delivery: Value = serde_json::from_str(delivery).unwrap();
        if delivery["url"]
            .as_str()
            .unwrap_or_default()
            .ends_with("/unifiedPresenceService")
        {
            found.push(serde_json::from_str(delivery["body"].as_str().unwrap()).unwrap());
        }
    }
}

fn availability_in(body: &Value, mri: &str) -> Option<String> {
    body["presence"]
        .as_array()?
        .iter()
        .find(|item| item["mri"] == mri)
        .and_then(|item| item["presence"]["availability"].as_str())
        .map(str::to_owned)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_quit_or_stopped_watching_gets_no_more_presence_pushes() {
    for backend in backends() {
        let rig = Rig::start_with(backend, "").await;
        let mut a = Seat::sign_in(&rig.first, &rig.origin, "a@own.test").await;
        a.in_use().await;

        // b's client watches a, then quits; b's next client, on the same account, watches nobody
        let mut b = Seat::sign_in(rig.other(), &rig.origin, "b@own.test").await;
        let mark = b.mark();
        b.watch(&a.mri).await;
        b.sees(mark, &a.mri, &Availability::Available).await;
        b.close();
        let next = uuid::Uuid::new_v4().to_string();
        let (mut quiet, _) = socket(rig.other(), &rig.origin, "b@own.test", &next).await;

        // c's endpoint watches a, then takes a out of what it watches
        let watcher = uuid::Uuid::new_v4().to_string();
        let (mut watching, surl) = socket(&rig.first, &rig.origin, "c@own.test", &watcher).await;
        let c_token = bearer(&rig.first, "c@own.test", teams_proto::PRESENCE_AUDIENCE).await;
        let taken = subscribe(&rig.first, &c_token, &watcher, &surl, &[&a.mri], &[]).await;
        assert_eq!(taken["addStatuses"][0]["status"], 20200);
        let snapshot = deliveries(&mut watching, Duration::from_secs(3)).await;
        assert_eq!(snapshot.len(), 1, "{snapshot:?}");
        assert_eq!(snapshot[0]["isSnapshot"], true);

        a.report(OwnStatus::Manual(ManualAvailability::DoNotDisturb)).await;
        let changed = deliveries(&mut watching, Duration::from_secs(3)).await;
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(availability_in(&changed[0], &a.mri).as_deref(), Some("DoNotDisturb"));
        let removed = subscribe(&rig.first, &c_token, &watcher, &surl, &[], &[&a.mri]).await;
        assert_eq!(removed["removeStatuses"][0]["status"], 20200, "{removed}");

        a.report(OwnStatus::Manual(ManualAvailability::Away)).await;
        a.report(OwnStatus::ClearManual).await;
        assert_eq!(
            deliveries(&mut watching, Duration::from_secs(3)).await,
            Vec::<Value>::new()
        );
        assert_eq!(
            deliveries(&mut quiet, Duration::from_secs(1)).await,
            Vec::<Value>::new()
        );
        drop(a);
        rig.stop().await;
    }
}
