//! Server deployments that share one Redis, as an operator runs them: real server processes, two nodes per
//! deployment, each deployment on a Postgres database of its own, operator-created synthetic accounts with the same
//! names in every deployment, and the real client's session, engine and push channel. Each deployment's sign-in
//! holds, setup holds, pushes, replay and operator commands stay in its own Redis namespace.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::join_all;
use serde_json::json;
use teams_core::engine::{Event, MessageChange, Source};
use teams_trouter::Down;
use tokio::time::Instant;

use super::push::support::{Link, User, brief, options};
use super::support::{Deployment, LOGIN_WINDOW_SECONDS, Node};
use crate::support::{Backend, PASSWORD, PG_ENV, REDIS_ENV, redis_url};

/// The accounts every deployment here has, by the same names.
const USERS: [&str; 5] = [
    "a@own.test",
    "b@own.test",
    "c@own.test",
    "d@own.test",
    "Mixed.Case@own.test",
];
/// Synthetic accounts added beside them, the same names in each deployment.
const MANY: usize = 10;
const SETUP: &str = "/api/admin/v1/setup";
/// The old server binary an upgrade test starts from (a build of the version before the namespaces).
const OLD_SERVER_ENV: &str = "NGTS_QA_OLD_SERVER";

fn many() -> Vec<String> {
    (1..=MANY).map(|n| format!("qa-{n:02}@own.test")).collect()
}

/// The shared Redis, when Postgres and Redis are both given; else the test says it skipped.
fn shared_redis(test: &str) -> Option<String> {
    if std::env::var_os(PG_ENV).is_none() || redis_url().is_none() {
        eprintln!("SKIP {test}: set {PG_ENV} and {REDIS_ENV}");
        return None;
    }
    redis_url()
}

/// Root keys can't follow the configuration's tables, so they go before them.
fn prepend(config: &Path, keys: &str) {
    let rest = std::fs::read_to_string(config).unwrap();
    std::fs::write(config, format!("{keys}{rest}")).unwrap();
}

fn append(config: &Path, tables: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(file, "{tables}").unwrap();
}

/// The operator adds `upn` with [`PASSWORD`]; the command prints the UPN as stored: lowercased.
fn add(node: &Node, upn: &str) {
    let added = node.cli(&["user", "add", upn, "--name", "QA Operator"], &format!("{PASSWORD}\n"));
    assert!(added.starts_with(&format!("added {} (", upn.to_lowercase())), "{added}");
}

/// A copy of `site`'s database, as a restore of its dump makes one: the same tenant id and accounts. The site's
/// nodes start and stop first, as a deployment that ran does: the tenant id is made by the first node to start.
async fn restored_copy(site: &mut Site) -> String {
    site.start().await;
    site.stop().await;
    copy_database(&site.database).await
}

/// A copy of the Postgres database `url` (nothing may be connected to it).
async fn copy_database(url: &str) -> String {
    use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
    let base = std::env::var(PG_ENV).unwrap();
    let source = url::Url::parse(url).unwrap().path().trim_start_matches('/').to_owned();
    let name = format!("ngts_{}", uuid::Uuid::new_v4().simple());
    let mut admin = AsyncPgConnection::establish(&base).await.unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {name} TEMPLATE {source}"))
        .await
        .unwrap();
    let mut copy = url::Url::parse(&base).unwrap();
    copy.set_path(&format!("/{name}"));
    copy.to_string()
}

/// One deployment: two nodes on its own database and the shared Redis.
struct Site {
    name: &'static str,
    /// The sign-in hold's window, in seconds.
    window: u64,
    origin: String,
    database: String,
    nodes: [Node; 2],
    deployment: Deployment,
}

impl Site {
    /// A deployment on a new database, its nodes configured (`keys`: root keys, `tables`: TOML tables), not started.
    async fn new(name: &'static str, redis: &str, keys: &str, tables: &str) -> Self {
        Self::on(name, redis, None, keys, tables).await
    }

    /// As [`Site::new`], on the database `database` when given.
    async fn on(name: &'static str, redis: &str, database: Option<String>, keys: &str, tables: &str) -> Self {
        let mut deployment = Deployment::new(Backend::Postgres, false, Some(redis.to_owned())).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        if let Some(database) = database {
            deployment.database = database;
        }
        let storage = format!(
            "storage_url = \"file://{}\"\n{keys}",
            deployment.dir.path().join("blobs").display()
        );
        let nodes = ["a", "b"].map(|node| {
            let node = deployment.node(node);
            prepend(&node.config, &storage);
            append(&node.config, tables);
            node
        });
        Self {
            name,
            window: LOGIN_WINDOW_SECONDS,
            origin: deployment.origin.clone(),
            database: deployment.database.clone(),
            nodes,
            deployment,
        }
    }

    /// Migrates the database and adds `upns` (all with [`PASSWORD`]) through the operator's command line.
    fn provision(&self, upns: &[String]) {
        self.nodes[0].cli(&["migrate"], "");
        let node = &self.nodes[0];
        std::thread::scope(|scope| {
            for chunk in upns.chunks(4) {
                scope.spawn(move || chunk.iter().for_each(|upn| add(node, upn)));
            }
        });
    }

    /// The sign-in hold lasts `seconds` from the first wrong password.
    fn login_window(&mut self, seconds: u64) {
        for node in &self.nodes {
            let text = std::fs::read_to_string(&node.config).unwrap();
            let from = format!("window = {LOGIN_WINDOW_SECONDS}\n");
            assert!(text.contains(&from));
            std::fs::write(&node.config, text.replacen(&from, &format!("window = {seconds}\n"), 1)).unwrap();
        }
        self.window = seconds;
    }

    async fn start(&mut self) {
        for (n, node) in self.nodes.iter_mut().enumerate() {
            let log = self.deployment.dir.path().join(format!("{}-{n}.log", self.name));
            node.start_logged(&log).await;
        }
    }

    async fn stop(&mut self) {
        for node in &mut self.nodes {
            node.stop().await;
        }
    }

    async fn user(&self, node: usize, upn: &str) -> User {
        User::sign_in(&self.nodes[node], &self.origin, upn, options()).await
    }

    /// The status of a password sign-in at node `node`.
    async fn password(&self, node: usize, upn: &str, password: &str) -> u16 {
        self.nodes[node].password(upn, password).await.status().as_u16()
    }
}

/// The tenant id `site`'s tokens name (`tid`), from a sign-in of `upn`.
async fn tenant(site: &Site, upn: &str) -> String {
    use base64::Engine as _;
    let answer: serde_json::Value = site.nodes[0].password(upn, PASSWORD).await.json().await.unwrap();
    let token = answer["access_token"].as_str().unwrap();
    let claims = token.split('.').nth(1).unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
    claims["tid"].as_str().unwrap().to_owned()
}

/// The original and its copy name one tenant, as a restored database does.
async fn one_tenant(x: &Site, y: &Site, upn: &str) {
    let (x_tenant, y_tenant) = (tenant(x, upn).await, tenant(y, upn).await);
    assert_eq!(x_tenant, y_tenant, "the copy is not a restore of the original");
}

fn all_users() -> Vec<String> {
    USERS.iter().map(|upn| (*upn).to_owned()).chain(many()).collect()
}

/// Three wrong passwords for `upn` at alternating nodes of `site`, then the right one at each node: held off at
/// both. Returns when the first wrong password was sent (the window starts there).
async fn hold_off(site: &Site, upn: &str) -> Instant {
    let started = Instant::now();
    for node in [0, 1, 0] {
        assert_eq!(
            site.password(node, upn, "wrong password").await,
            400,
            "{}: {upn}",
            site.name
        );
    }
    for node in [1, 0] {
        assert_eq!(
            site.password(node, upn, PASSWORD).await,
            429,
            "{}: {upn} at node {node}",
            site.name
        );
    }
    let window = Duration::from_secs(site.window);
    assert!(
        started.elapsed() < window,
        "{upn}: the attempts took {:?}, longer than the window; the host is too slow for this check",
        started.elapsed()
    );
    started
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_passwords_on_one_deployment_hold_off_the_account_on_both_its_nodes_and_never_on_another() {
    let Some(redis) = shared_redis("namespaces: sign-in holds") else {
        return;
    };
    let mut x = Site::new("x", &redis, "", "").await;
    let mut y = Site::new("y", &redis, "", "").await;
    // long enough for the checks below on a loaded host
    x.login_window(60);
    y.login_window(60);
    let users = all_users();
    x.provision(&users);
    y.provision(&users);
    x.start().await;
    y.start().await;

    // X's operator sees a storm of wrong passwords on some accounts (one of them typed in another case)
    let held = [
        "a@own.test",
        "mixed.case@own.test",
        "qa-01@own.test",
        "qa-02@own.test",
        "qa-03@own.test",
    ];
    let started = join_all(held.iter().map(|upn| hold_off(&x, upn))).await;
    // X's other accounts are not held
    for upn in ["b@own.test", "qa-04@own.test", "qa-10@own.test"] {
        for node in [0, 1] {
            assert_eq!(x.password(node, upn, PASSWORD).await, 200, "x: {upn} at node {node}");
        }
    }
    // Y's accounts of the same names, at both its nodes, sign in, the client's way too
    for upn in held.iter().chain(&["Mixed.Case@own.test"]) {
        for node in [0, 1] {
            assert_eq!(y.password(node, upn, PASSWORD).await, 200, "y: {upn} at node {node}");
        }
    }
    let session = y.nodes[1].sign_in("a@own.test", PASSWORD).await;
    session.skypetoken().await.unwrap();
    // and Y's own storm holds off Y's b and not X's b
    hold_off(&y, "b@own.test").await;
    assert_eq!(x.password(1, "b@own.test", PASSWORD).await, 200);
    // X's holds still stand: no sign-in on Y cleared them
    for (upn, started) in held.iter().zip(&started) {
        let status = x.password(0, upn, PASSWORD).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(x.window),
            "the checks took {elapsed:?}, past the window; the host is too slow for this check"
        );
        assert_eq!(status, 429, "x: {upn}, {elapsed:?} after its first wrong password");
    }

    // the hold ends with the window
    let last = *started.iter().max().unwrap();
    tokio::time::sleep_until(last + Duration::from_secs(x.window + 2)).await;
    for upn in held {
        for node in [1, 0] {
            assert_eq!(
                x.password(node, upn, PASSWORD).await,
                200,
                "x: {upn} at node {node} after the window"
            );
        }
    }
    drop(session);
    y.stop().await;
    x.stop().await;
}

async fn setup(node: &Node, code: &str) -> u16 {
    let body = json!({"upn": "root@own.test", "display_name": "Root", "password": PASSWORD, "code": code});
    let response = node
        .http
        .post(format!("{}{SETUP}", node.endpoint))
        .json(&body)
        .send()
        .await
        .unwrap();
    response.status().as_u16()
}

const WRONG_CODE: &str = "AAAAA-AAAAA-AAAAA-AAAAA-AAAAA";

/// Setup attempts with a wrong code on `x` hold off setup on both its nodes, even with the right code.
async fn setup_held_off(x: &Site) {
    for node in [0, 1] {
        assert_eq!(setup(&x.nodes[node], WRONG_CODE).await, 403, "x node {node}");
    }
    let code = x.nodes[0].cli(&["setup-code"], "");
    for node in [0, 1] {
        assert_eq!(
            setup(&x.nodes[node], code.lines().next().unwrap()).await,
            429,
            "x node {node}"
        );
    }
}

/// Setup attempts with a wrong code on `x` hold off setup on both its nodes; `y` takes a wrong code as wrong, and
/// its own code makes its admin, who signs in; `x`'s accounts still sign in.
async fn setup_holds_stay_in_their_deployment(x: &Site, y: &Site) {
    setup_held_off(x).await;
    assert_eq!(setup(&y.nodes[0], WRONG_CODE).await, 403, "y: not held");
    let code = y.nodes[1].cli(&["setup-code"], "");
    assert_eq!(setup(&y.nodes[1], code.lines().next().unwrap()).await, 201);
    assert_eq!(y.password(0, "root@own.test", PASSWORD).await, 200);
    for (site, node) in [(x, 0), (x, 1), (y, 0), (y, 1)] {
        assert_eq!(
            site.password(node, "a@own.test", PASSWORD).await,
            200,
            "{}: a at node {node}",
            site.name
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_setup_codes_on_one_deployment_hold_off_neither_setup_nor_sign_in_on_another() {
    let Some(redis) = shared_redis("namespaces: setup holds") else {
        return;
    };
    let tables = "[setup]\nmax_attempts = 2\nwindow = 300";
    let mut x = Site::new("x", &redis, "", tables).await;
    let mut y = Site::new("y", &redis, "", tables).await;
    x.provision(&["a@own.test".to_owned()]);
    y.provision(&["a@own.test".to_owned()]);
    x.start().await;
    y.start().await;
    setup_holds_stay_in_their_deployment(&x, &y).await;
    y.stop().await;
    x.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_setup_codes_on_a_restored_copy_hold_off_nothing_on_the_original_once_it_has_its_namespace() {
    let Some(redis) = shared_redis("namespaces: setup holds of a restored copy") else {
        return;
    };
    let tables = "[setup]\nmax_attempts = 2\nwindow = 300";
    let mut x = Site::new("x", &redis, "", tables).await;
    x.provision(&["a@own.test".to_owned()]);
    let copy = restored_copy(&mut x).await;
    let keys = format!("redis_namespace = \"restored-{}\"\n", uuid::Uuid::new_v4().simple());
    let mut y = Site::on("y", &redis, Some(copy), &keys, tables).await;
    x.start().await;
    y.start().await;
    one_tenant(&x, &y, "a@own.test").await;
    setup_holds_stay_in_their_deployment(&x, &y).await;
    y.stop().await;
    x.stop().await;
}

/// Message pushes (live or replayed) the client got since `mark`, of any chat.
fn heard(user: &User, mark: usize) -> Vec<String> {
    user.log[mark..]
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::Message {
                    source: Source::Delivery | Source::Replay,
                    ..
                }
            )
        })
        .map(brief)
        .collect()
}

fn replays(user: &User, mark: usize) -> usize {
    user.count(mark, |event| {
        matches!(
            event,
            Event::Message {
                source: Source::Replay,
                ..
            }
        )
    })
}

/// A deployment's clients: a on the first node, b on the second, and b's second client on the first node through a
/// link that can drop.
struct Clients {
    a: User,
    b: User,
    b2: User,
    link: Link,
    /// Where b's and b2's logs stood once all were online.
    marks: (usize, usize),
}

impl Clients {
    async fn of(site: &Site) -> Self {
        let a = site.user(0, "a@own.test").await;
        let b = site.user(1, "b@own.test").await;
        let link = Link::to(&site.nodes[0]).await;
        let mut b2 = User::signed_in(&site.nodes[0], &site.origin, "b@own.test", options()).await;
        b2.through(&link);
        b2.start();
        b2.online(0).await;
        let marks = (b.mark(), b2.mark());
        Self { a, b, b2, link, marks }
    }

    /// b2's network drops: its socket is down.
    async fn cut(&mut self) {
        let dropped = self.b2.mark();
        self.link.cut();
        self.b2
            .since(dropped, "disconnect", |event| {
                matches!(event, Event::Disconnected { .. })
            })
            .await;
    }

    /// b2's network is back: its socket is up and registered. Where its log stood before.
    async fn restore(&mut self) -> usize {
        let back = self.b2.mark();
        self.link.restore();
        self.b2.online(back).await;
        back
    }

    async fn settle(&mut self) {
        for user in [&mut self.a, &mut self.b, &mut self.b2] {
            user.settle().await;
        }
    }
}

/// x's a posts while x's b2 is offline: x's b gets each post live. The ids of the chat's posts.
async fn posts_while_b2_is_offline(x: &mut Clients, chat: &str) -> Vec<String> {
    let mut ids = vec![x.a.send(chat, "x: first").await];
    x.b.pushed(0, chat, &ids[0], MessageChange::New).await;
    x.b2.pushed(0, chat, &ids[0], MessageChange::New).await;
    x.cut().await;
    for n in 1..=3 {
        let id = x.a.send(chat, &format!("x: while b2 is offline {n}")).await;
        x.b.pushed(0, chat, &id, MessageChange::New).await;
        ids.push(id);
    }
    ids
}

/// x's posts reach both nodes, and x's b2 gets each it missed once when its link is back. The ids of the posts.
async fn posts_reach_both_nodes_and_an_offline_client_at_reconnect(x: &mut Clients, chat: &str) -> Vec<String> {
    let ids = posts_while_b2_is_offline(x, chat).await;
    let back = x.restore().await;
    for id in &ids[1..] {
        x.b2.eventually_stored(chat, id).await;
    }
    x.b2.settle().await;
    let log = heard(&x.b2, back);
    eprintln!(
        "x's b2 after its link was back: {} replayed; {log:?}",
        replays(&x.b2, back)
    );
    for id in &ids[1..] {
        assert_eq!(x.b2.pushes(back, chat, id, MessageChange::New), 1, "{id}: {log:?}");
    }
    assert_eq!(x.b2.stored(chat, "x: ").await, ids);
    ids
}

/// y's clients heard no push since they were online, and hold none of x's chat's messages `ids`.
async fn heard_nothing(y: &mut Clients, chat: &str, ids: &[String]) {
    y.settle().await;
    for (user, mark) in [(&y.a, 0), (&y.b, y.marks.0), (&y.b2, y.marks.1)] {
        assert_eq!(heard(user, mark), Vec::<String>::new(), "y's {}", user.upn);
        for id in ids {
            assert!(
                user.engine().message(chat, id).await.unwrap().is_none(),
                "y's {}",
                user.upn
            );
        }
    }
}

/// `x`'s a posts to its b (on the other node, and b's second client through a link that drops); `y`'s a and b,
/// whose accounts have the same names (and, on a restored copy, the same ids), hear nothing of it, even across their
/// own reconnect; `x`'s b's second client gets what it missed when the link is back; then `y`'s own post reaches
/// `y`'s b and none of `x`'s clients.
async fn pushes_stay_in_their_deployment(x: &Site, y: &Site) {
    let mut on_x = Clients::of(x).await;
    let mut on_y = Clients::of(y).await;
    let chat = on_x.a.create(&[&on_x.b], None).await;
    on_y.cut().await;
    let ids = posts_reach_both_nodes_and_an_offline_client_at_reconnect(&mut on_x, &chat).await;
    let y_back = on_y.restore().await;
    heard_nothing(&mut on_y, &chat, &ids).await;

    // Y's own chat stays in Y
    let quiet_from = (on_x.b.mark(), on_x.b2.mark());
    let y_chat = on_y.a.create(&[&on_y.b], None).await;
    let y_first = on_y.a.send(&y_chat, "y: first").await;
    on_y.b.pushed(on_y.marks.0, &y_chat, &y_first, MessageChange::New).await;
    on_y.b2.pushed(y_back, &y_chat, &y_first, MessageChange::New).await;
    on_x.settle().await;
    assert_eq!(heard(&on_x.b, quiet_from.0), Vec::<String>::new());
    assert_eq!(heard(&on_x.b2, quiet_from.1), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_reaches_its_deployments_clients_on_both_nodes_and_no_client_of_another_deployment() {
    let Some(redis) = shared_redis("namespaces: pushes") else {
        return;
    };
    let mut x = Site::new("x", &redis, "", "").await;
    let mut y = Site::new("y", &redis, "", "").await;
    x.provision(&all_users());
    y.provision(&all_users());
    x.start().await;
    y.start().await;
    pushes_stay_in_their_deployment(&x, &y).await;
    y.stop().await;
    x.stop().await;
}

/// `x` and a deployment `y` made from a copy of `x`'s database: the same tenant id, account ids and names. `y` set
/// `redis_namespace`; `x` keeps the default.
async fn original_and_restored_copy(redis: &str) -> (Site, Site) {
    let mut x = Site::new("x", redis, "", "").await;
    x.provision(&all_users());
    let copy = restored_copy(&mut x).await;
    let keys = format!("redis_namespace = \"restored-{}\"\n", uuid::Uuid::new_v4().simple());
    let y = Site::on("y", redis, Some(copy), &keys, "").await;
    (x, y)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restored_copy_with_its_own_namespace_shares_no_push_hold_or_replay_with_the_original() {
    let Some(redis) = shared_redis("namespaces: restored copy") else {
        return;
    };
    let (mut x, mut y) = original_and_restored_copy(&redis).await;
    x.start().await;
    y.start().await;
    one_tenant(&x, &y, "qa-10@own.test").await;
    pushes_stay_in_their_deployment(&x, &y).await;
    hold_off(&x, "c@own.test").await;
    for node in [0, 1] {
        assert_eq!(
            y.password(node, "c@own.test", PASSWORD).await,
            200,
            "y: c at node {node}"
        );
    }
    y.stop().await;
    x.stop().await;
}

/// Since `mark` the service ended `user`'s push within seconds of `returned` and the client was signed out.
async fn signed_out_at_once(user: &mut User, mark: usize, returned: Instant) {
    user.since(mark, "the service's disconnect", |event| {
        matches!(event, Event::Disconnected { down: Down::ServiceDisconnect(Some(reason)), .. }
            if reason == "unauthorized")
    })
    .await;
    assert!(returned.elapsed() < Duration::from_secs(5), "{:?}", returned.elapsed());
    user.until_signed_out().await;
}

/// Since `mark` `user`'s push never dropped, and it got `id` once; its session still gets tokens.
async fn untouched(user: &mut User, mark: usize, chat: &str, id: &str) {
    user.pushed(mark, chat, id, MessageChange::New).await;
    user.settle().await;
    let log: Vec<String> = user.log[mark..].iter().map(brief).collect();
    let dropped = user.count(mark, |event| {
        matches!(event, Event::Disconnected { .. } | Event::PushStopped(_))
    });
    assert_eq!(dropped, 0, "{}: {log:?}", user.upn);
    assert_eq!(user.pushes(mark, chat, id, MessageChange::New), 1, "{log:?}");
    assert!(user.session().signed_out().is_none());
    user.session()
        .access_token(teams_auth::token::SPACES_RESOURCE)
        .await
        .unwrap();
}

/// a's client and `upn`'s client at the second node of `site`, in a chat whose first post `upn` got.
async fn pair(site: &Site, upn: &str) -> (User, User, String) {
    let a = site.user(0, "a@own.test").await;
    let mut target = site.user(1, upn).await;
    let chat = a.create(&[&target], None).await;
    let first = a.send(&chat, "operator, first").await;
    target.pushed(0, &chat, &first, MessageChange::New).await;
    (a, target, chat)
}

/// The operator's `command` on `x`'s account `upn`: `x`'s client of it loses its push within seconds and is signed
/// out, and nothing posted afterwards reaches it; `y`'s client of the same account (same id on a restored copy) keeps
/// its push, gets a post after the command once, and can still get tokens.
async fn operator_command_stays_in_its_deployment(x: &Site, y: &Site, upn: &str, command: &[&str], input: &str) {
    let (x_a, mut x_target, x_chat) = pair(x, upn).await;
    let (y_a, mut y_target, y_chat) = pair(y, upn).await;
    let (x_mark, y_mark) = (x_target.mark(), y_target.mark());
    x.nodes[0].cli(command, input);
    signed_out_at_once(&mut x_target, x_mark, Instant::now()).await;
    let unseen = x_a.send(&x_chat, "operator, x after the command").await;
    let y_after = y_a.send(&y_chat, "operator, y after the command").await;
    untouched(&mut y_target, y_mark, &y_chat, &y_after).await;
    x_target.settle().await;
    assert_eq!(x_target.pushes(x_mark, &x_chat, &unseen, MessageChange::New), 0);
}

async fn operator_commands_stay_in_their_deployment(x: &Site, y: &Site) {
    operator_command_stays_in_its_deployment(x, y, "b@own.test", &["user", "disable", "b@own.test"], "").await;
    for node in [0, 1] {
        assert_ne!(x.password(node, "b@own.test", PASSWORD).await, 200, "x: b is disabled");
        assert_eq!(y.password(node, "b@own.test", PASSWORD).await, 200, "y: b is untouched");
    }
    let reset = ["user", "reset-password", "d@own.test"];
    operator_command_stays_in_its_deployment(x, y, "d@own.test", &reset, "another horse battery\n").await;
    for node in [0, 1] {
        assert_ne!(
            x.password(node, "d@own.test", PASSWORD).await,
            200,
            "x: d has a new password"
        );
        assert_eq!(x.password(node, "d@own.test", "another horse battery").await, 200);
        assert_eq!(
            y.password(node, "d@own.test", PASSWORD).await,
            200,
            "y: d keeps its password"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operators_disable_and_reset_on_one_deployment_leave_another_deployments_same_named_users_alone() {
    let Some(redis) = shared_redis("namespaces: operator commands") else {
        return;
    };
    let mut x = Site::new("x", &redis, "", "").await;
    let mut y = Site::new("y", &redis, "", "").await;
    x.provision(&all_users());
    y.provision(&all_users());
    x.start().await;
    y.start().await;
    operator_commands_stay_in_their_deployment(&x, &y).await;
    y.stop().await;
    x.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_operators_disable_and_reset_on_the_original_leave_a_restored_copy_with_its_namespace_alone() {
    let Some(redis) = shared_redis("namespaces: operator commands on a restored copy") else {
        return;
    };
    let (mut x, mut y) = original_and_restored_copy(&redis).await;
    x.start().await;
    y.start().await;
    one_tenant(&x, &y, "qa-10@own.test").await;
    operator_commands_stay_in_their_deployment(&x, &y).await;
    y.stop().await;
    x.stop().await;
}

/// What `redis_namespace` set to one value on two deployments means (docs/server.md): they share every Redis name,
/// so a hold on an account name holds it off in both. And two deployments from one database copy, with no
/// namespace set, share the tenant id's namespace: a push for an account reaches the copy's client of that account.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deployments_in_one_namespace_share_holds_and_a_copy_without_its_own_namespace_hears_the_originals_pushes() {
    let Some(redis) = shared_redis("namespaces: one namespace shared") else {
        return;
    };
    let keys = format!("redis_namespace = \"shared-{}\"\n", uuid::Uuid::new_v4().simple());
    let mut x = Site::new("x", &redis, &keys, "").await;
    let mut w = Site::new("w", &redis, &keys, "").await;
    x.provision(&all_users());
    w.provision(&all_users());
    x.start().await;
    w.start().await;
    hold_off(&x, "c@own.test").await;
    for node in [0, 1] {
        assert_eq!(
            w.password(node, "c@own.test", PASSWORD).await,
            429,
            "w shares x's hold on c"
        );
    }
    w.stop().await;
    x.stop().await;

    let mut x = Site::new("x", &redis, "", "").await;
    x.provision(&all_users());
    let copy = restored_copy(&mut x).await;
    let mut z = Site::on("z", &redis, Some(copy), "", "").await;
    x.start().await;
    z.start().await;
    one_tenant(&x, &z, "qa-10@own.test").await;
    let x_a = x.user(0, "a@own.test").await;
    let x_b = x.user(1, "b@own.test").await;
    let mut z_b = z.user(1, "b@own.test").await;
    assert_eq!(x_b.mri, z_b.mri);
    let mark = z_b.mark();
    let chat = x_a.create(&[&x_b], None).await;
    let id = x_a.send(&chat, "shared namespace").await;
    // z's client is pushed x's message, for a chat its own server doesn't have
    z_b.since(mark, "the original's push", |event| {
        matches!(event, Event::Message { conversation, source: Source::Delivery, .. } if *conversation == chat)
            || brief(event) == "Failed(adding a delivery's conversation)"
    })
    .await;
    eprintln!(
        "z's b heard x's push (a shared namespace): {:?}",
        z_b.log[mark..].iter().map(brief).collect::<Vec<_>>()
    );
    let _ = id;
    drop((x_a, x_b, z_b));
    z.stop().await;
    x.stop().await;
}

/// The namespace check as the operator meets it: a bad `redis_namespace` stops `serve` and every command at start
/// with the documented message, and the longest and shortest good ones start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_redis_namespace_stops_the_server_at_start_and_good_ones_start() {
    const SAID: &str = "redis_namespace must be 1 to 64 letters, digits, '.', '_' or '-'";
    let redis = redis_url();
    let deployment = Deployment::new(Backend::Sqlite, false, redis).await;
    let node = deployment.node("a");
    node.cli(&["migrate"], "");
    let original = std::fs::read_to_string(&node.config).unwrap();
    let too_long = "n".repeat(65);
    for bad in [
        "",
        "has space",
        "a:b",
        "ngts:x",
        "zoë",
        "a/b",
        "*",
        too_long.as_str(),
        "line\\nbreak",
    ] {
        std::fs::write(&node.config, format!("redis_namespace = \"{bad}\"\n{original}")).unwrap();
        let output = node.cli_output(&["serve"], "");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{bad:?} started");
        assert!(said.contains(SAID), "{bad:?}: {said}");
        let output = node.cli_output(&["user", "list"], "");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success() && said.contains(SAID),
            "user list with {bad:?}: {said}"
        );
    }
    let longest = "A.b_c-9".repeat(10)[..64].to_owned();
    for good in ["x", longest.as_str()] {
        std::fs::write(&node.config, format!("redis_namespace = \"{good}\"\n{original}")).unwrap();
        let mut node = deployment.node("a");
        node.start().await;
        assert_eq!(node.status("/health/ready").await, Some(204), "{good:?}");
        node.stop().await;
    }
}

/// Every key of the shared Redis that `pattern` matches.
async fn keys(redis: &str, pattern: &str) -> Vec<String> {
    let client = redis::Client::open(redis).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("KEYS").arg(pattern).query_async(&mut conn).await.unwrap()
}

/// The seconds `key` has left (`-1`: it never expires, `-2`: there is no such key).
async fn ttl(redis: &str, key: &str) -> i64 {
    let client = redis::Client::open(redis).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    redis::cmd("TTL").arg(key).query_async(&mut conn).await.unwrap()
}

/// The keys the version before left for `mri`'s pushes: no namespace. The new version never reads them, and they
/// run out by themselves. The journal's key.
async fn old_names(redis: &str, mri: &str) -> String {
    let journal = format!("journal:push:{mri}");
    assert_eq!(
        keys(redis, &journal).await,
        vec![journal.clone()],
        "the old version kept no journal"
    );
    eprintln!(
        "old version's sign-in counters: {:?}",
        keys(redis, "ngts:login:*").await
    );
    for key in [journal.clone(), format!("cursors:push:{mri}")] {
        assert!(ttl(redis, &key).await > 0, "{key} would outlive the upgrade");
    }
    journal
}

/// The operator stops every node of `site` and starts each on `binary`; the running clients `users` reconnect.
async fn upgrade(site: &mut Site, binary: &Path, users: [&mut User; 2]) {
    let marks = users.each_ref().map(|user| user.mark());
    site.stop().await;
    for node in &mut site.nodes {
        node.binary = binary.to_owned();
    }
    site.start().await;
    for (user, mark) in users.into_iter().zip(marks) {
        user.since(mark, "disconnect", |event| matches!(event, Event::Disconnected { .. }))
            .await;
        user.online(mark).await;
    }
}

/// The upgrade an operator makes: every node of a deployment stops on the version before the namespaces, then
/// starts on this one. Holds and journals start empty (the documented upgrade note); the clients reconnect and
/// catch up, and no message is lost or shown twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_an_upgrade_onto_namespaces_holds_start_empty_and_clients_recover_every_message_once() {
    let Some(redis) = shared_redis("namespaces: upgrade") else {
        return;
    };
    let Some(old) = std::env::var_os(OLD_SERVER_ENV).map(PathBuf::from) else {
        eprintln!("SKIP namespaces: upgrade: set {OLD_SERVER_ENV} to a server binary of the version before");
        return;
    };
    let mut x = Site::new("x", &redis, "", "").await;
    let new = std::mem::replace(&mut x.nodes[0].binary, old.clone());
    x.nodes[1].binary = old;
    x.provision(&all_users());
    x.start().await;
    let mut a = x.user(0, "a@own.test").await;
    let mut b = x.user(1, "b@own.test").await;
    let mut b2 = x.user(0, "b@own.test").await;
    let chat = a.create(&[&b], None).await;
    let mut expected = vec![a.send(&chat, "upgrade, first").await];
    b.pushed(0, &chat, &expected[0], MessageChange::New).await;
    b2.pushed(0, &chat, &expected[0], MessageChange::New).await;

    // b's first client is closed; a posts; an account is held off, all on the old version
    b.close();
    for n in 1..=3 {
        expected.push(a.send(&chat, &format!("upgrade, before {n}")).await);
    }
    b2.eventually_stored(&chat, expected.last().unwrap()).await;
    hold_off(&x, "c@own.test").await;
    let journal = old_names(&redis, b2.mri.as_str()).await;

    upgrade(&mut x, &new, [&mut a, &mut b2]).await;
    // the hold the old version kept is gone: the upgrade starts with empty holds
    assert_eq!(x.password(1, "c@own.test", PASSWORD).await, 200);

    let back = b2.mark();
    expected.push(a.send(&chat, "upgrade, after").await);
    b2.pushed(back, &chat, expected.last().unwrap(), MessageChange::New)
        .await;
    let restarted = b.mark();
    b.start();
    b.online(restarted).await;
    for id in &expected {
        b.eventually_stored(&chat, id).await;
    }
    expected.push(a.send(&chat, "upgrade, live after b's restart").await);
    b.pushed(restarted, &chat, expected.last().unwrap(), MessageChange::New)
        .await;
    b2.pushed(back, &chat, expected.last().unwrap(), MessageChange::New)
        .await;
    for user in [&mut a, &mut b, &mut b2] {
        user.settle().await;
        assert_eq!(user.stored(&chat, "upgrade, ").await, expected, "{}", user.upn);
    }
    for id in &expected[expected.len() - 2..] {
        assert!(b2.pushes(back, &chat, id, MessageChange::New) <= 1, "{id}");
    }
    // the new version journals in the deployment's namespace
    let namespaced = format!("ngts:{}:{journal}", tenant(&x, "qa-10@own.test").await);
    assert_eq!(keys(&redis, &namespaced).await, vec![namespaced.clone()]);
    drop((a, b, b2));
    x.stop().await;
}
