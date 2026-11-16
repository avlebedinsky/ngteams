use std::fmt::Write as _;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use teams_core::engine::{Action, Event, MessageChange, Source};
use teams_media::tone::Tone;
use teams_proto::chat::{ChatReads, MessageText, Node as TextNode, RichText, SharedFile};
use teams_proto::{Cloud, PastedFormat, PastedPicture, sent_picture_in};
use teams_store::OutboxState;
use tokio::net::{TcpListener, TcpStream};

use super::super::calls::group_video::SHAPES;
use super::super::calls::support::{Phone, coturn};
use super::super::media::support::Blobs;
use super::super::support::{Deployment, Node, Port};
use crate::support::{Backend, PASSWORD, PG_ENV, take_turn};

/// The three people whose clients these tests run, as `run-lab.sh`'s own-server aliases name them.
pub const SRV: [(&str, &str); 3] = [
    ("srv-a@own.test", "Ada Ł. Lovelace"),
    ("srv-b@own.test", "Bert 🎺 Bach"),
    ("srv-c@own.test", "Cora 'Cee' Nuñez"),
];
pub const TONES: [Tone; 3] = [
    Tone {
        hz: 440.0,
        amplitude: 0.5,
    },
    Tone {
        hz: 660.0,
        amplitude: 0.5,
    },
    Tone {
        hz: 880.0,
        amplitude: 0.5,
    },
];
/// A ring's length: a call nobody answers ends soon.
const RING: u64 = 15;

/// The deployments here run one at a time, also beside other tests (`--test-threads 4`): each runs several clients'
/// media and three server processes.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How the deployment runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Two API nodes (`serve --role api`) and a media node (`serve --role media`) on one Postgres, one Redis and one
    /// bucket, a balancer in front of the API nodes.
    Pair,
    /// One node (`serve`, the media node in it) on SQLite, without Redis.
    Single,
}

/// The run's deployment: its nodes, the store its blobs are in, and the front each client reaches it through.
pub struct Cluster {
    pub origin: String,
    pub database: String,
    /// The API nodes: one for [`Shape::Single`].
    pub nodes: Vec<Node>,
    pub media: Option<Node>,
    pub blobs: Blobs,
    /// A front per API node, each sending a connection to its node while it takes one, else to another.
    fronts: Vec<Front>,
    logs: Option<PathBuf>,
    starts: u32,
    _internal: Option<Port>,
    _deployment: Deployment,
    _turn: Option<tokio::sync::MutexGuard<'static, ()>>,
    _one: tokio::sync::MutexGuard<'static, ()>,
}

fn append(config: &Path, tables: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(config).unwrap();
    writeln!(file, "{tables}").unwrap();
}

/// Root keys can't follow the configuration's tables, so they go before them.
fn prepend(config: &Path, keys: &str) {
    let rest = std::fs::read_to_string(config).unwrap();
    std::fs::write(config, format!("{keys}{rest}")).unwrap();
}

/// The tables every node of the deployment takes: calls (through the run's coturn, when there is one) and the media
/// node.
fn tables(shape: Shape, turn: Option<(SocketAddr, String)>) -> String {
    let mut config = format!("[calling]\nring_timeout = {RING}\n");
    if let Some((relay, secret)) = turn {
        write!(
            config,
            "[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n"
        )
        .unwrap();
    }
    // the loopback coturn relays from 127.0.0.1: the media node's candidate and its replies are there too
    config.push_str("[mixer]\nudp_listen = \"127.0.0.1:0\"\n");
    if shape == Shape::Pair {
        config.push_str("heartbeat = 1\ndead_after = 3\ndrain_timeout = 3\n");
    }
    config
}

/// The API nodes: two (`serve --role api`) for [`Shape::Pair`], else one (`serve`).
fn api_nodes(deployment: &Deployment, shape: Shape, blobs: &Blobs, config: &str) -> Vec<Node> {
    let names: &[&str] = match shape {
        Shape::Pair => &["a", "b"],
        Shape::Single => &["a"],
    };
    let mut nodes: Vec<Node> = names.iter().map(|name| deployment.node(name)).collect();
    for node in &mut nodes {
        prepend(&node.config, blobs.root());
        append(&node.config, &format!("{}\n{config}", blobs.tables()));
        if shape == Shape::Pair {
            node.role = Some(("api", None));
        }
    }
    nodes
}

/// A front per node, each trying its own node first, then the others in turn.
async fn fronts(nodes: &[Node]) -> Vec<Front> {
    let mut fronts = Vec::new();
    for index in 0..nodes.len() {
        let order: Vec<String> = (0..nodes.len())
            .map(|step| nodes[(index + step) % nodes.len()].endpoint.clone())
            .collect();
        fronts.push(Front::open(order).await);
    }
    fronts
}

/// The media node (`serve --role media`) and the port of its internal API.
fn media_node(deployment: &Deployment, blobs: &Blobs, config: &str) -> (Node, Port) {
    let internal = Port::take();
    let mut media = deployment.node("media");
    let address = SocketAddr::from(([127, 0, 0, 1], internal.number));
    prepend(&media.config, blobs.root());
    append(
        &media.config,
        &format!("{}\n{config}internal_listen = \"{address}\"\n", blobs.tables()),
    );
    media.role = Some(("media", Some(address)));
    (media, internal)
}

impl Cluster {
    /// `None` (skipped, and said) for [`Shape::Pair`] without Postgres and Redis. The nodes relay calls through the
    /// run's coturn when it names one.
    pub async fn start(shape: Shape, flow: &str) -> Option<Self> {
        Self::start_on(shape, flow, coturn()).await
    }

    /// As [`Self::start`], and `None` (skipped, and said) without a coturn: for calls.
    pub async fn with_calls(shape: Shape, flow: &str) -> Option<Self> {
        let Some(turn) = coturn() else {
            eprintln!("skipped: calls need a coturn");
            return None;
        };
        Self::start_on(shape, flow, Some(turn)).await
    }

    async fn start_on(shape: Shape, flow: &str, turn: Option<(SocketAddr, String)>) -> Option<Self> {
        let redis = crate::support::redis_url();
        if shape == Shape::Pair && (std::env::var_os(PG_ENV).is_none() || redis.is_none()) {
            eprintln!("skipped: two nodes need Postgres and Redis");
            return None;
        }
        let one = ONE_AT_A_TIME.lock().await;
        let turn_taken = take_turn().await;
        eprintln!("cluster, {flow}: {shape:?}");
        let (backend, redis) = match shape {
            Shape::Pair => (Backend::Postgres, redis),
            Shape::Single => (Backend::Sqlite, None),
        };
        let mut deployment = Deployment::new(backend, false, redis).await;
        deployment.origin = deployment.origin.replacen("http:", "https:", 1);
        let blobs = Blobs::new().await;
        let config = tables(shape, turn);
        let nodes = api_nodes(&deployment, shape, &blobs, &config);
        let (media, internal) = if shape == Shape::Pair {
            let (media, internal) = media_node(&deployment, &blobs, &config);
            (Some(media), Some(internal))
        } else {
            (None, None)
        };
        nodes[0].cli(&["migrate"], "");
        for (upn, name) in SRV {
            nodes[0].cli(&["user", "add", upn, "--name", name], &format!("{PASSWORD}\n"));
        }
        let fronts = fronts(&nodes).await;
        let mut cluster = Self {
            origin: deployment.origin.clone(),
            database: deployment.database.clone(),
            nodes,
            media,
            blobs,
            fronts,
            logs: std::env::var_os(super::super::calls::support::LOG_DIR)
                .map(|dir| Path::new(&dir).join(format!("cluster-{flow}-{shape:?}"))),
            starts: 0,
            _internal: internal,
            _deployment: deployment,
            _turn: turn_taken,
            _one: one,
        };
        if cluster.media.is_some() {
            cluster.start_media().await;
        }
        for index in 0..cluster.nodes.len() {
            cluster.start_node(index).await;
        }
        Some(cluster)
    }

    fn log(&mut self, name: &str) -> Option<PathBuf> {
        self.starts += 1;
        let dir = self.logs.as_ref()?;
        std::fs::create_dir_all(dir).unwrap();
        Some(dir.join(format!("{name}-start-{}.log", self.starts)))
    }

    async fn start_media(&mut self) {
        let log = self.log("media");
        let media = self.media.as_mut().unwrap();
        match log {
            Some(log) => media.start_logged(&log).await,
            None => media.start().await,
        }
    }

    /// Starts the API node `index` (again).
    pub async fn start_node(&mut self, index: usize) {
        let log = self.log(&format!("api-{index}"));
        let node = &mut self.nodes[index];
        match log {
            Some(log) => node.start_logged(&log).await,
            None => node.start().await,
        }
    }

    /// Kills the API node `index` at once, as a crash or a lost machine ends it.
    pub fn kill_node(&mut self, index: usize) {
        self.nodes[index].kill();
    }

    /// The client of the `person`th of [`SRV`], signed in through the front of the API node `node` (`node` modulo
    /// the nodes there are): each of its requests and sockets goes to that node while it runs, else to the other.
    pub async fn person(&self, person: usize, node: usize) -> Phone {
        let node = node % self.nodes.len();
        let (upn, name) = SRV[person];
        let mut phone = Phone::signed_in(&self.nodes[node], &self.origin, (upn, name), TONES[person]).await;
        phone.whole();
        phone.camera = SHAPES[person];
        let front = &self.fronts[node];
        phone.connect_through(&front.url("ws"));
        phone.http_through(&front.url("http"));
        phone.start();
        phone.online(0).await;
        phone
    }

    /// The front of API node `node` (modulo the nodes there are).
    pub fn front(&self, node: usize) -> &Front {
        &self.fronts[node % self.fronts.len()]
    }

    pub async fn stop(mut self) {
        for node in &mut self.nodes {
            node.stop().await;
        }
        if let Some(media) = &mut self.media {
            media.stop().await;
        }
    }
}

/// A TCP front for the API nodes, as the deployment's reverse proxy is (`packaging/server/compose.yaml`): each
/// connection goes to the first of its nodes that takes it, in its order.
pub struct Front {
    pub port: u16,
    /// How many connections went to each node, in the front's order.
    pub sent: Arc<[std::sync::atomic::AtomicUsize]>,
    closed: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Front {
    async fn open(order: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let backends: Vec<String> = order
            .iter()
            .map(|endpoint| endpoint.trim_start_matches("http://").to_owned())
            .collect();
        let sent: Arc<[std::sync::atomic::AtomicUsize]> = backends
            .iter()
            .map(|_| std::sync::atomic::AtomicUsize::new(0))
            .collect();
        let closed = Arc::new(AtomicBool::new(false));
        let counts = Arc::clone(&sent);
        let task = tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(forward(client, backends.clone(), Arc::clone(&counts)));
            }
        });
        Self {
            port,
            sent,
            closed,
            task,
        }
    }

    pub fn url(&self, scheme: &str) -> String {
        format!("{scheme}://127.0.0.1:{}", self.port)
    }

    /// How many connections went to each node so far, in the front's order.
    pub fn counts(&self) -> Vec<usize> {
        self.sent.iter().map(|count| count.load(Ordering::Relaxed)).collect()
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

/// `client`'s connection to the first of `backends` that takes one; counted in `counts`.
async fn forward(mut client: TcpStream, backends: Vec<String>, counts: Arc<[std::sync::atomic::AtomicUsize]>) {
    for (index, backend) in backends.iter().enumerate() {
        let connect = TcpStream::connect(backend.as_str());
        if let Ok(Ok(mut upstream)) = tokio::time::timeout(Duration::from_millis(500), connect).await {
            counts[index].fetch_add(1, Ordering::Relaxed);
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            return;
        }
    }
}

/// `from` sends `text` to `chat`; the message's id.
pub async fn send(from: &Phone, chat: &str, text: &str) -> String {
    write(
        from,
        Action::Send {
            conversation: chat.into(),
            text: MessageText::plain(text).unwrap(),
        },
    )
    .await
    .message_id
    .unwrap()
}

/// `action` through `from`'s outbox, sent.
pub async fn write(from: &Phone, action: Action) -> teams_store::OutboxEntry {
    let row = from.engine().write(action).await.unwrap();
    assert_eq!(row.state, OutboxState::Sent, "{}: {row:?}", from.upn);
    row
}

/// Whether `event` is the push (or replay) of message `id` of `chat` as `wanted`.
pub fn pushed(event: &Event, chat: &str, id: &str, wanted: MessageChange) -> bool {
    matches!(event, Event::Message { conversation, id: of, change, source: Source::Delivery | Source::Replay, .. }
        if conversation == chat && of == id && *change == wanted)
}

/// Waits until `to`'s push brought message `id` of `chat` as `change` since `mark`.
pub async fn arrives(to: &mut Phone, mark: usize, chat: &str, id: &str, change: MessageChange) {
    to.since(mark, &format!("{change:?} of {id}"), |event| {
        pushed(event, chat, id, change)
    })
    .await;
}

/// The stored message's text, once `to`'s store holds message `id`.
pub async fn stored_text(to: &Phone, chat: &str, id: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        if let Some(message) = to.engine().message(chat, id).await.unwrap() {
            return message.content;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}: {id} never stored", to.upn);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A short account of what one side measured: `label` and the times, longest first.
pub fn times(label: &str, mut taken: Vec<Duration>) -> String {
    taken.sort_unstable_by(|a, b| b.cmp(a));
    let mut line = format!("{label}:");
    for time in &taken {
        let _ = write!(line, " {}ms", time.as_millis());
    }
    line
}

/// `from` pastes `bytes` (a PNG of `size`) into `chat` and sends it: the picture's `src` and the message's id.
pub async fn paste(from: &Phone, cloud: &Cloud, chat: &str, bytes: &[u8], size: (u32, u32)) -> (String, String) {
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: bytes.to_vec(),
    };
    let object = from.engine().upload_picture(chat.to_owned(), picture).await.unwrap();
    let image = sent_picture_in(cloud, &object, size).unwrap();
    let src = image.src.clone();
    let text = RichText {
        nodes: vec![TextNode::Paragraph(vec![TextNode::Image(image)])],
        truncated: false,
    };
    let id = write(
        from,
        Action::Send {
            conversation: chat.to_owned(),
            text: MessageText::new_in(cloud, text, []).unwrap(),
        },
    )
    .await
    .message_id
    .unwrap();
    (src, id)
}

/// `from` uploads `bytes` as `name` and sends the file to `chat`: the message's id.
pub async fn send_file(from: &Phone, cloud: &Cloud, chat: &str, name: &str, bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    let file = from.engine().upload_file(path).await.unwrap();
    assert_eq!(file.size(), bytes.len() as u64);
    let text = MessageText::plain(&format!("here is {name}"))
        .unwrap()
        .with_files(vec![file])
        .for_cloud(cloud)
        .unwrap();
    write(
        from,
        Action::Send {
            conversation: chat.to_owned(),
            text,
        },
    )
    .await
    .message_id
    .unwrap()
}

/// The one file of message `id` as `reader`'s client reads the chat's page.
pub async fn file_of(reader: &Phone, cloud: &Cloud, chat: &str, id: &str) -> SharedFile {
    let page = ChatReads::new(&reader.client()).messages(chat, 20).await.unwrap();
    let message = page.messages.iter().find(|message| message.id == id).unwrap();
    let files = message.files_in(cloud);
    assert_eq!(files.files.len(), 1, "{}: {:?}", reader.upn, files.unread);
    files.files[0].clone()
}

/// The whole file as `reader`'s engine streams it for a download.
pub async fn download(reader: &Phone, share: &str) -> Vec<u8> {
    let size = reader.engine().file_size(share.to_owned()).await.unwrap();
    let mut stream = reader.engine().file_stream(share.to_owned(), 0, size).await.unwrap();
    let mut bytes = Vec::new();
    while let Some(piece) = stream.next().await {
        bytes.extend_from_slice(&piece.unwrap());
    }
    bytes
}

/// The picture at `src` as `reader`'s engine reads it for the message view.
pub async fn picture(reader: &Phone, src: &str) -> Vec<u8> {
    let read = reader.engine().message_picture(src.to_owned()).await.unwrap();
    read.unwrap_or_else(|| panic!("{}: no picture at {src}", reader.upn))
        .bytes
        .clone()
}
