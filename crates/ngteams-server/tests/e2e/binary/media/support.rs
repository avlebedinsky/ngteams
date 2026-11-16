use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use futures_util::TryStreamExt as _;
use ngteams_server::config::Config;
use object_store::{ObjectStore, ObjectStoreExt as _};
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde_json::Value;
use teams_core::engine::{Action, MessageChange};
use teams_proto::chat::{ChatReads, MessageText, Node as TextNode, RichText, SharedFile};
use teams_proto::{Cloud, FileDownload, PastedFormat, PastedPicture, sent_picture_in};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

use super::super::push::support::{Rig, User};
use super::super::support::Node;
use crate::support::{Backend, CLIENT_ID, redis_url};

pub const IC3: &str = "https://ic3.teams.office.com";
pub const GRAPH: &str = "https://graph.microsoft.com";
pub const MIB: usize = 1024 * 1024;

/// The store the nodes keep blobs in, as their configuration names it, and the test's own view of its keys.
pub struct Blobs {
    root: String,
    tables: String,
    store: Arc<dyn ObjectStore>,
    prefix: object_store::path::Path,
    _dir: TempDir,
}

impl Blobs {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        crate::media::configure_storage(&mut config, dir.path()).await;
        if let Some(path) = config.storage_url.strip_prefix("file://") {
            std::fs::create_dir_all(path).unwrap();
        }
        let mut tables = String::from("[storage_options]\n");
        for (key, value) in &config.storage_options {
            writeln!(tables, "{key} = {value:?}").unwrap();
        }
        let url = url::Url::parse(&config.storage_url).unwrap();
        let (store, prefix) = object_store::parse_url_opts(&url, config.storage_options.clone()).unwrap();
        Self {
            root: format!("storage_url = {:?}\n", config.storage_url),
            tables,
            store: Arc::from(store),
            prefix,
            _dir: dir,
        }
    }

    /// The configuration's root key naming the store (`storage_url`).
    pub fn root(&self) -> &str {
        &self.root
    }

    /// The configuration's `[storage_options]` table.
    pub fn tables(&self) -> &str {
        &self.tables
    }

    /// The keys the store holds for this deployment, below its prefix, in order.
    pub async fn keys(&self) -> Vec<String> {
        let found: Vec<_> = self.store.list(Some(&self.prefix)).try_collect().await.unwrap();
        let prefix = format!("{}/", self.prefix);
        let mut keys: Vec<String> = found
            .into_iter()
            .map(|object| object.location.to_string())
            .map(|key| key.strip_prefix(&prefix).unwrap_or(&key).to_owned())
            .collect();
        keys.sort();
        keys
    }

    /// The bytes the store holds under `key`.
    pub async fn bytes(&self, key: &str) -> Vec<u8> {
        let path: object_store::path::Path = self
            .prefix
            .parts()
            .chain(object_store::path::Path::from(key).parts())
            .collect();
        self.store.get(&path).await.unwrap().bytes().await.unwrap().to_vec()
    }
}

/// The real server with its blobs in [`Blobs`], and the cloud its clients know it as.
pub struct Stage {
    pub rig: Rig,
    pub blobs: Blobs,
    pub cloud: Cloud,
}

impl Stage {
    /// `config`: TOML tables appended to each node's configuration (`[media]`).
    pub async fn start(backend: Backend, config: &str) -> Self {
        let blobs = Blobs::new().await;
        let redis = (backend == Backend::Postgres).then(redis_url).flatten();
        assert!(
            backend != Backend::Postgres || redis.is_some(),
            "Postgres QA requires Redis"
        );
        let rig = Rig::start_stored(backend, config, redis, Some((&blobs.root, &blobs.tables))).await;
        let cloud = Cloud::own(&rig.origin).unwrap();
        Self { rig, blobs, cloud }
    }

    pub async fn people(&self) -> (User, User, User, String) {
        people(&self.rig).await
    }

    pub async fn stop(self) {
        self.rig.stop().await;
    }
}

/// a at the first node; b and c at the other node when there are two. a and b share a chat, c is outside it.
pub async fn people(rig: &Rig) -> (User, User, User, String) {
    let a = rig.team_user(&rig.first, "a@own.test").await;
    let mut b = rig.team_user(rig.other(), "b@own.test").await;
    let c = rig.team_user(rig.other(), "c@own.test").await;
    let chat = a.create(&[&b], None).await;
    let first = a.send(&chat, "the chat's first message").await;
    b.eventually_stored(&chat, &first).await;
    (a, b, c, chat)
}

/// A token for `resource` from a new sign-in of `upn` with `password` at `node`; `None` when it is refused.
pub async fn grant(node: &Node, upn: &str, password: &str, resource: &str) -> Option<String> {
    let scope = format!("{resource}/.default openid profile offline_access");
    let form = [
        ("client_id", CLIENT_ID),
        ("grant_type", "password"),
        ("username", upn),
        ("password", password),
        ("scope", scope.as_str()),
    ];
    let answer = node
        .http
        .post(format!("{}/oauth2/v2.0/token", node.endpoint))
        .form(&form);
    let answer: Value = answer.send().await.unwrap().json().await.unwrap();
    answer["access_token"].as_str().map(str::to_owned)
}

/// The account's token for `audience`, as its client's session gets one.
pub async fn token(user: &User, audience: &str) -> String {
    let token = user.session().access_token(audience).await.unwrap();
    token.token.expose().to_owned()
}

/// A request of no client's making to `node`; the answer's status, headers and body.
pub async fn raw(
    node: &Node,
    method: Method,
    path: &str,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (u16, HeaderMap, Vec<u8>) {
    let mut request = node.http.request(method, format!("{}{path}", node.endpoint));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let answer = request.send().await.unwrap();
    let status = answer.status().as_u16();
    let headers = answer.headers().clone();
    (status, headers, answer.bytes().await.unwrap().to_vec())
}

pub async fn get(node: &Node, path: &str, token: Option<&str>) -> u16 {
    raw(node, Method::GET, path, token, &[], None).await.0
}

/// Bytes no compression shortens, the same for the same `seed`.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// Whether two byte strings are the same, said without printing them.
pub fn same(read: &[u8], sent: &[u8]) -> bool {
    read == sent
}

/// `a` pastes `bytes` (a PNG of `size`) into `chat` and sends the message; the picture's `src` and the message.
pub async fn paste(a: &User, cloud: &Cloud, chat: &str, bytes: &[u8], size: (u32, u32)) -> (String, String) {
    let picture = PastedPicture {
        format: PastedFormat::Png,
        bytes: bytes.to_vec(),
    };
    let object = a.engine().upload_picture(chat.to_owned(), picture).await.unwrap();
    let image = sent_picture_in(cloud, &object, size).unwrap();
    let src = image.src.clone();
    let text = RichText {
        nodes: vec![TextNode::Paragraph(vec![TextNode::Image(image)])],
        truncated: false,
    };
    let sent = a
        .write(Action::Send {
            conversation: chat.to_owned(),
            text: MessageText::new_in(cloud, text, []).unwrap(),
        })
        .await;
    (src, sent.message_id.unwrap())
}

/// `a` uploads `bytes` as `name` and sends it to `chat`: what the engine answered the upload with.
pub async fn try_upload(a: &User, name: &str, bytes: &[u8]) -> Result<teams_proto::UploadedFile, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    a.engine().upload_file(path).await.map_err(|error| format!("{error:?}"))
}

/// `a` uploads `bytes` as `name` and sends the file to `chat`; the message.
pub async fn send_file(a: &User, cloud: &Cloud, chat: &str, name: &str, bytes: &[u8]) -> String {
    let file = try_upload(a, name, bytes).await.unwrap();
    assert_eq!(file.size(), bytes.len() as u64);
    let text = MessageText::plain(&format!("here is {name}"))
        .unwrap()
        .with_files(vec![file])
        .for_cloud(cloud)
        .unwrap();
    let sent = a
        .write(Action::Send {
            conversation: chat.to_owned(),
            text,
        })
        .await;
    sent.message_id.unwrap()
}

/// The file card of message `id` as `reader`'s client shows it once the push brought the message: its one file.
pub async fn card(reader: &mut User, cloud: &Cloud, chat: &str, id: &str) -> SharedFile {
    reader.pushed(0, chat, id, MessageChange::New).await;
    let stored = reader.engine().message(chat, id).await.unwrap().unwrap();
    assert!(stored.properties.unwrap_or_default().contains("files"));
    let page = ChatReads::new(&reader.client()).messages(chat, 20).await.unwrap();
    let message = page.messages.iter().find(|message| message.id == id).unwrap();
    let files = message.files_in(cloud);
    assert!(files.unread.is_none(), "{:?}", files.unread);
    assert_eq!(files.files.len(), 1);
    files.files[0].clone()
}

/// The whole file from `offset` on, as the reader's engine streams it for a download.
pub async fn download(reader: &User, share: &str, offset: u64) -> Vec<u8> {
    let size = reader.engine().file_size(share.to_owned()).await.unwrap();
    let mut stream = reader
        .engine()
        .file_stream(share.to_owned(), offset, size - offset)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    while let Some(piece) = stream.next().await {
        bytes.extend_from_slice(&piece.unwrap());
    }
    bytes
}

/// The file as Graph answers `reader` through `node`: its item id and its download link's path and query.
pub async fn item(node: &Node, reader: &User, stage: &Stage, share: &str) -> Result<(String, String), u16> {
    item_as(node, &token(reader, GRAPH).await, stage, share).await
}

/// As [`item`], for the holder of the Graph token `graph`.
pub async fn item_as(node: &Node, graph: &str, stage: &Stage, share: &str) -> Result<(String, String), u16> {
    let read = FileDownload::of_in(&stage.cloud, share).unwrap();
    let path = format!("/graph/v1.0/shares/{}/driveItem", read.share());
    let (status, _, body) = raw(node, Method::GET, &path, Some(graph), &[], None).await;
    if status != 200 {
        return Err(status);
    }
    let item: Value = serde_json::from_slice(&body).unwrap();
    let link = item["@microsoft.graph.downloadUrl"].as_str().unwrap();
    Ok((
        item["sharepointIds"]["listItemUniqueId"].as_str().unwrap().to_owned(),
        link.strip_prefix(&stage.rig.origin).unwrap().to_owned(),
    ))
}

/// Where `owner`'s client uploads a file named `name`.
pub fn upload_path(owner: &User, name: &str) -> String {
    format!(
        "{}/drive/root:/Microsoft%20Teams%20Chat%20Files/{name}:/content",
        drive(owner)
    )
}

/// Starts an upload that says it has `declared` bytes, sends `sent` and goes away.
pub async fn abandon_upload(node: &Node, path: &str, token: &str, declared: usize, sent: &[u8]) {
    let address = node.endpoint.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let head = format!(
        "PUT {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: \
         application/octet-stream\r\nContent-Length: {declared}\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(sent).await.unwrap();
    stream.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(stream);
    tokio::time::sleep(Duration::from_millis(1500)).await;
}

/// Uploads `bytes` without saying how many there are (a chunked body); the answer's status.
pub async fn streamed_upload(node: &Node, path: &str, token: &str, bytes: &[u8]) -> u16 {
    let address = node.endpoint.strip_prefix("http://").unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let head = format!(
        "PUT {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Type: \
         application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut body = Vec::new();
    for chunk in bytes.chunks(64 * 1024) {
        body.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        body.extend_from_slice(chunk);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"0\r\n\r\n");
    // the server may answer and stop reading before the body ends
    let _ = stream.write_all(&body).await;
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut answer)).await;
    let line = String::from_utf8_lossy(&answer);
    let status = line
        .split(' ')
        .nth(1)
        .unwrap_or_else(|| panic!("no answer to the upload"));
    status.parse().unwrap()
}

/// The path of `owner`'s drive API.
pub fn drive(owner: &User) -> String {
    format!("/sp/personal/{}/_api/v2.0", owner.oid().replace('-', ""))
}
