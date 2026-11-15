//! Presence as the users of three clients meet it on the real binary: a signs in, sets each status the account menu
//! has, signs in on a second device, goes idle, quits and comes back; b watches a from the other node and sees each
//! change pushed; c, who never watched a, reads a on demand. Then the nodes and Redis restart under them.

use std::sync::Arc;
use std::time::Duration;

use teams_core::Availability;
use teams_core::engine::OwnStatus;
use teams_proto::ManualAvailability;

use super::super::push::support::{OWN_REDIS_ENV, OwnRedis, Rig};
use super::support::{SHORT_PRESENCE, Seat, manual_expires, manual_row, reads_as};
use crate::support::{Backend, PASSWORD, PG_ENV, backends};

/// Longer than an endpoint's lifetime in [`SHORT_PRESENCE`] and the sweep after it.
const LAPSED: Duration = Duration::from_secs(8);

/// `a`'s MRI, from a sign-in of its own (no client of a runs yet).
async fn mri_of(
    node: &super::super::support::Node,
    upn: &str,
) -> (
    String,
    Arc<teams_auth::session::Session<teams_auth::secrets::MemorySecretStore>>,
) {
    let session = Arc::new(node.sign_in(upn, PASSWORD).await);
    let skype = session.skypetoken().await.unwrap();
    (format!("8:{}", skype.skype_id.as_deref().unwrap()), session)
}

/// `from` sets `status`; `watcher` sees `shown` pushed and `reader` reads it.
async fn shows(from: &Seat, status: OwnStatus, watcher: &mut Seat, reader: &Seat, shown: &Availability) {
    let mark = watcher.mark();
    from.report(status).await;
    watcher.sees(mark, &from.mri, shown).await;
    reads_as(reader, &from.mri, &format!("{shown:?}")).await;
}

fn manual(status: ManualAvailability) -> OwnStatus {
    OwnStatus::Manual(status)
}

/// a sets each status of the account menu, then resets it; b and c see each.
async fn every_status(a: &Seat, b: &mut Seat, c: &Seat) {
    for (status, shown) in [
        (ManualAvailability::Busy, Availability::Busy),
        (ManualAvailability::DoNotDisturb, Availability::DoNotDisturb),
        (ManualAvailability::Away, Availability::Away),
        (ManualAvailability::Available, Availability::Available),
        (ManualAvailability::Busy, Availability::Busy),
    ] {
        shows(a, manual(status), b, c, &shown).await;
    }
    shows(a, OwnStatus::ClearManual, b, c, &Availability::Available).await;
}

/// a's second device, on the other node: a status set there shows, and quitting the first device changes nothing.
async fn second_device(rig: &Rig, mut a: Seat, b: &mut Seat, c: &Seat) -> Seat {
    let mut a2 = Seat::sign_in(rig.other(), &rig.origin, "a@own.test").await;
    assert_ne!(a2.epid, a.epid);
    a2.in_use().await;
    shows(
        &a2,
        manual(ManualAvailability::DoNotDisturb),
        b,
        c,
        &Availability::DoNotDisturb,
    )
    .await;
    let mark = b.mark();
    a.close();
    b.idle_for(LAPSED).await;
    assert_eq!(b.told(mark, &a2.mri), [], "a's other device still runs");
    reads_as(c, &a2.mri, "DoNotDisturb").await;
    shows(&a2, OwnStatus::ClearManual, b, c, &Availability::Available).await;
    a2
}

/// a's last client goes idle, comes back, sets Busy and quits; a's next start shows Busy again.
async fn idle_and_quit(mut a: Seat, b: &mut Seat, c: &Seat) {
    // the endpoint lapses, and b and c see a offline, last seen
    let mark = b.mark();
    a.idle();
    b.sees(mark, &a.mri, &Availability::Offline).await;
    let seen = c.read(&[&a.mri]).await.remove(0);
    assert_eq!(seen.availability.as_deref(), Some("Offline"));
    assert!(seen.last_active_ms.is_some(), "Teams shows when a was last seen");
    let mark = b.mark();
    a.in_use().await;
    b.sees(mark, &a.mri, &Availability::Available).await;

    shows(&a, manual(ManualAvailability::Busy), b, c, &Availability::Busy).await;
    let mark = b.mark();
    a.close();
    b.sees(mark, &a.mri, &Availability::Offline).await;
    let mark = b.mark();
    a.start().await;
    a.in_use().await;
    b.sees(mark, &a.mri, &Availability::Busy).await;
    reads_as(c, &a.mri, "Busy").await;
    a.report(OwnStatus::ClearManual).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_clients_see_a_sign_in_each_status_a_second_device_idle_and_quit() {
    for backend in backends() {
        let rig = Rig::start_with(backend, SHORT_PRESENCE).await;
        let (a_mri, a_session) = mri_of(&rig.first, "a@own.test").await;
        let mut b = Seat::sign_in(rig.other(), &rig.origin, "b@own.test").await;
        let c = Seat::sign_in(&rig.first, &rig.origin, "c@own.test").await;

        // b's view shows a, who never signed in: Offline, and "never" is no last-seen time
        let mark = b.mark();
        assert_eq!(b.watch(&a_mri).await, Availability::Unknown);
        b.sees(mark, &a_mri, &Availability::Offline).await;
        let never = c.read(&[&a_mri]).await.remove(0);
        assert_eq!(never.availability.as_deref(), Some("Offline"));
        assert_eq!(never.last_active_ms, None);

        // a signs in: Available, live on b's node
        let mark = b.mark();
        let mut a = Seat::on(&rig.first, &rig.origin, "a@own.test", Arc::clone(&a_session)).await;
        a.in_use().await;
        b.sees(mark, &a_mri, &Availability::Available).await;
        reads_as(&c, &a_mri, "Available").await;
        assert!(c.read(&[&a_mri]).await[0].last_active_ms.is_some());

        every_status(&a, &mut b, &c).await;
        let a2 = second_device(&rig, a, &mut b, &c).await;
        idle_and_quit(a2, &mut b, &c).await;
        drop((b, c));
        rig.stop().await;
    }
}

/// a, Busy by hand on the first node, watched by b on the other node (or the same one on one node).
async fn busy_and_watched(rig: &Rig) -> (Seat, Seat, Seat) {
    let mut a = Seat::sign_in(&rig.first, &rig.origin, "a@own.test").await;
    let mut b = Seat::sign_in(rig.other(), &rig.origin, "b@own.test").await;
    let c = Seat::sign_in(&rig.first, &rig.origin, "c@own.test").await;
    a.in_use().await;
    a.report(OwnStatus::Manual(ManualAvailability::Busy)).await;
    let mark = b.mark();
    b.watch(&a.mri).await;
    b.sees(mark, &a.mri, &Availability::Busy).await;
    (a, b, c)
}

/// b's client still hears of a's changes: a sets `status`, b sees `shown`.
async fn still_pushed(a: &Seat, b: &mut Seat, status: ManualAvailability, shown: &Availability) {
    let mark = b.mark();
    a.report(OwnStatus::Manual(status)).await;
    b.sees(mark, &a.mri, shown).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn presence_comes_back_right_after_each_node_and_redis_restart() {
    let Some(redis) = OwnRedis::take().await else {
        eprintln!("SKIP presence over restarts: set {OWN_REDIS_ENV}=<podman container>=<its redis URL>");
        return;
    };
    if !backends().contains(&Backend::Postgres) {
        eprintln!("SKIP presence over restarts: set {PG_ENV}");
        return;
    }
    redis.start().await;
    let mut rig = Rig::start_on(Backend::Postgres, SHORT_PRESENCE, Some(redis.url.clone())).await;
    let (a, mut b, c) = busy_and_watched(&rig).await;

    // b's node restarts: b's client reconnects there and still hears of a
    let mark = b.mark();
    let node = rig.second.as_mut().unwrap();
    node.stop().await;
    node.start().await;
    b.online(mark).await;
    still_pushed(
        &a,
        &mut b,
        ManualAvailability::DoNotDisturb,
        &Availability::DoNotDisturb,
    )
    .await;

    // a's node restarts: a's status stays, nobody saw a offline
    let mark = b.mark();
    rig.first.stop().await;
    rig.first.start().await;
    b.idle_for(Duration::from_secs(2)).await;
    reads_as(&b, &a.mri, "DoNotDisturb").await;
    assert_eq!(b.told(mark, &a.mri), [], "a restart of a's node isn't a's sign-out");
    still_pushed(&a, &mut b, ManualAvailability::Away, &Availability::Away).await;

    // Redis restarts (its data saved on the way down, as the image's default does)
    redis.stop().await;
    redis.start().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    reads_as(&c, &a.mri, "Away").await;
    still_pushed(&a, &mut b, ManualAvailability::Busy, &Availability::Busy).await;

    // Redis restarts with nothing kept (a cache without persistence): what the running clients report brings a back,
    // under the status a chose, which the database keeps
    redis_forgets_on_restart(&redis);
    redis.stop().await;
    redis.start().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    reads_as(&c, &a.mri, "Busy").await;
    let mark = b.mark();
    a.report(OwnStatus::Manual(ManualAvailability::DoNotDisturb)).await;
    eprintln!("after a Redis that kept nothing, b was told {:?}", b.told(mark, &a.mri));
    b.sees(mark, &a.mri, &Availability::DoNotDisturb).await;
    drop((a, b, c));
    rig.stop().await;
}

/// The Redis of `redis` keeps nothing over its next restart.
fn redis_forgets_on_restart(redis: &OwnRedis) {
    let container = redis.container();
    for args in [
        vec!["exec", container, "redis-cli", "CONFIG", "SET", "save", ""],
        vec!["exec", container, "rm", "-f", "/data/dump.rdb"],
    ] {
        let done = std::process::Command::new("podman")
            .args(&args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(done.success(), "podman {args:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_set_by_hand_outlasts_a_restart_of_a_single_node() {
    let mut rig = Rig::start_with(Backend::Sqlite, SHORT_PRESENCE).await;
    let (mut a, mut b, c) = busy_and_watched(&rig).await;
    let (a_mark, b_mark) = (a.mark(), b.mark());
    rig.first.stop().await;
    rig.first.start().await;
    a.online(a_mark).await;
    b.online(b_mark).await;
    // the app's refresh reports Available again; the status a chose is what Teams goes on showing
    b.idle_for(Duration::from_secs(3)).await;
    let shown = c.reads(&a.mri).await;
    assert_eq!(shown.as_deref(), Some("Busy"), "a's Busy was forgotten by the restart");
    let told = b.told(b_mark, &a.mri);
    assert!(!told.contains(&Availability::Available), "b was told {told:?}");
    a.close();
    drop((a, b, c));
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_set_by_hand_ends_when_it_expires_and_watchers_are_told() {
    let rig = Rig::start_with(Backend::Sqlite, SHORT_PRESENCE).await;
    let (mut a, mut b, c) = busy_and_watched(&rig).await;
    let (_, expires) = manual_row(&rig.database, a.oid()).await.unwrap();
    let set = chrono::Utc::now().timestamp();
    assert!(
        (set + 30 * 86_400 - 5..=set + 30 * 86_400).contains(&expires),
        "{expires}"
    );
    // the 30 days run out while a's client keeps reporting Available
    let mark = b.mark();
    manual_expires(&rig.database, a.oid(), chrono::Utc::now().timestamp() + 2).await;
    b.sees(mark, &a.mri, &Availability::Available).await;
    reads_as(&c, &a.mri, "Available").await;
    a.close();
    drop((a, b, c));
    rig.stop().await;
}
