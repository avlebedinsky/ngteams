use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use ngteams_server::storage::{Blob, Incoming, Storage, StorageError};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{Crew, hooks_path, loopback};
use crate::bots::{Registry, png};
use crate::support::{Backend, TestServer, backends};

const ADA: &str = "ada@own.test";

#[derive(Debug, Default)]
struct Pause {
    armed: AtomicBool,
    entered: Notify,
    release: Notify,
}

#[derive(Debug)]
struct PausedStorage {
    inner: Arc<dyn Storage>,
    pause: Arc<Pause>,
}

#[async_trait]
impl Storage for PausedStorage {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await?;
        if self.pause.armed.swap(false, Ordering::SeqCst) {
            self.pause.entered.notify_one();
            self.pause.release.notified().await;
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError> {
        self.inner.get(key).await
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }

    async fn put_stream(&self, key: &str, body: Incoming, limit: u64) -> Result<u64, StorageError> {
        self.inner.put_stream(key, body, limit).await
    }

    async fn open(&self, key: &str, range: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError> {
        self.inner.open(key, range).await
    }
}

async fn open(backend: Backend, dir: &std::path::Path, pause: &Arc<Pause>) -> Crew {
    let server = TestServer::start_changed(
        backend,
        |config| {
            loopback(config);
            config.setup.require_code = false;
            config.storage_url = url::Url::from_directory_path(dir).unwrap().to_string();
        },
        |app| {
            app.storage = Arc::new(PausedStorage {
                inner: Arc::clone(&app.storage),
                pause: Arc::clone(pause),
            });
        },
    )
    .await;
    Crew::over(Registry::over(server).await).await
}

fn picture(width: u32) -> Value {
    use base64::Engine as _;
    json!(base64::engine::general_purpose::STANDARD.encode(png(width, width)))
}

async fn stale_put(crew: &Crew, pause: &Pause, id: &str, winner: Value) -> Value {
    let path = format!("{}/{id}", hooks_path(&crew.team));
    pause.armed.store(true, Ordering::SeqCst);
    let request = crew.mt(ADA, Method::PUT, &path, Some(json!({"picture": picture(64)})));
    tokio::pin!(request);
    tokio::select! {
        result = &mut request => panic!("the request did not pause: {result:?}"),
        () = pause.entered.notified() => {},
        () = tokio::time::sleep(std::time::Duration::from_secs(20)) => panic!("the picture did not reach storage"),
    }
    let (status, changed) = crew.mt(ADA, Method::PUT, &path, Some(winner)).await;
    pause.release.notify_one();
    let (stale_status, refused) = request.await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(stale_status, 409, "{refused}");
    assert_eq!(refused["errorCode"], "OutgoingWebhookChanged");
    changed["value"].clone()
}

async fn only_current_picture(crew: &Crew, dir: &std::path::Path, hook: &Value, width: u32) {
    let id = hook["id"].as_str().unwrap();
    let etag = hook["pictureUrl"].as_str().unwrap().split("?v=").nth(1).unwrap();
    let mut files: Vec<_> = std::fs::read_dir(dir.join(format!("outgoing-webhooks/{id}/pictures")))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
        .collect();
    files.sort();
    assert_eq!(files, [etag]);
    let path = format!("{}/{id}/picture", hooks_path(&crew.team));
    let (status, bytes) = crew.mt_raw(ADA, Method::GET, &path, None).await;
    assert_eq!(status, 200);
    assert_eq!(bytes, png(width, width));
}

async fn create(crew: &Crew) -> Value {
    let (status, answer) = crew
        .mt(
            ADA,
            Method::POST,
            &hooks_path(&crew.team),
            Some(json!({"name": "Concurrent (synthetic)", "callbackUrl": "https://example.test/a", "picture": picture(48)})),
        )
        .await;
    assert_eq!(status, 201, "{answer}");
    answer["value"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_picture_put_returns_conflict_and_removes_only_its_new_object() {
    for backend in backends() {
        let dir = tempfile::tempdir().unwrap();
        let pause = Arc::new(Pause::default());
        let crew = open(backend, dir.path(), &pause).await;
        let made = create(&crew).await;
        let id = made["id"].as_str().unwrap();
        let current = stale_put(&crew, &pause, id, json!({"picture": picture(96)})).await;
        let (_, listed) = crew.mt(ADA, Method::GET, &hooks_path(&crew.team), None).await;
        assert_eq!(listed["value"][0], current);
        only_current_picture(&crew, dir.path(), &current, 96).await;
        let path = format!("{}/{id}", hooks_path(&crew.team));
        let (status, _) = crew.mt(ADA, Method::DELETE, &path, None).await;
        assert_eq!(status, 204);
        let (status, _) = crew.mt_raw(ADA, Method::GET, &format!("{path}/picture"), None).await;
        assert_eq!(status, 404);
        assert_eq!(
            std::fs::read_dir(dir.path().join(format!("outgoing-webhooks/{id}/pictures")))
                .unwrap()
                .count(),
            0
        );
        crew.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_put_cannot_restore_a_concurrent_change_without_a_new_picture() {
    for backend in backends() {
        let dir = tempfile::tempdir().unwrap();
        let pause = Arc::new(Pause::default());
        let crew = open(backend, dir.path(), &pause).await;
        let made = create(&crew).await;
        let id = made["id"].as_str().unwrap();
        for winner in [
            json!({"name": "Current (synthetic)"}),
            json!({"description": "Current description"}),
            json!({"callbackUrl": "https://example.test/current"}),
        ] {
            let current = stale_put(&crew, &pause, id, winner.clone()).await;
            let (_, listed) = crew.mt(ADA, Method::GET, &hooks_path(&crew.team), None).await;
            assert_eq!(listed["value"][0], current);
            for (field, value) in winner.as_object().unwrap() {
                assert_eq!(&current[field], value);
            }
            only_current_picture(&crew, dir.path(), &current, 48).await;
        }
        crew.stop().await;
    }
}
