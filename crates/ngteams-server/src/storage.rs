//! Where files live (uploads, pictures): any `object_store` store, by URL: `s3://<bucket>[/<prefix>]` on any
//! S3-compatible service, shared by every node (its endpoint, region, keys and addressing style are
//! `storage_options`, `docs/server.md`), `file:///<dir>` on one node, `memory:///` for tests. Only `object_store`'s
//! own calls are used, so no provider is special.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt as _, TryStreamExt as _};
use object_store::aws::{AmazonS3Builder, AmazonS3ConfigKey};
use object_store::path::Path;
use object_store::{
    ClientConfigKey, GetOptions, GetRange, ObjectStore, ObjectStoreExt as _, ObjectStoreScheme, PutPayload,
    RetryConfig, WriteMultipart,
};
use url::Url;

use crate::config::StorageConfig;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("not a storage URL the server knows: {0}")]
    Url(String),
    #[error("not a valid object key")]
    Key,
    #[error("storage failed: {0}")]
    Store(#[from] object_store::Error),
    #[error("more bytes than allowed")]
    TooLarge,
    #[error("the bytes to store couldn't be read: {0}")]
    Body(std::io::Error),
}

/// Bytes on their way in: a request's body.
pub type Incoming = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// Stored bytes on their way out: `range` of an object of `size` bytes.
pub struct Blob {
    pub size: u64,
    pub range: std::ops::Range<u64>,
    pub bytes: BoxStream<'static, Result<Bytes, StorageError>>,
}

impl std::fmt::Debug for Blob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Blob")
            .field("size", &self.size)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

/// Uploads run at most this many parts at once.
const UPLOAD_PARTS: usize = 4;

/// The files of the server, by key (`/`-separated, no `.` or `..` parts).
#[async_trait]
pub trait Storage: Send + Sync + std::fmt::Debug {
    /// Stores `bytes` under `key`, replacing what was there.
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError>;

    /// The bytes under `key`, if any.
    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError>;

    /// Removes `key`; removing a missing key is no error.
    async fn delete(&self, key: &str) -> Result<(), StorageError>;

    /// Stores `body` under `key` as it arrives, never held whole; its size. A body over `limit` bytes is dropped
    /// ([`StorageError::TooLarge`]) and nothing is stored.
    async fn put_stream(&self, key: &str, body: Incoming, limit: u64) -> Result<u64, StorageError>;

    /// The bytes under `key` as a stream, all of them or `range` (cut to the object's end), if the key is there.
    async fn open(&self, key: &str, range: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError>;
}

/// A [`Storage`] over an `object_store` store.
#[derive(Debug)]
pub struct ObjectStorage {
    store: Arc<dyn ObjectStore>,
    prefix: Path,
}

impl ObjectStorage {
    /// [`Self::open_with`] the default waits.
    ///
    /// # Errors
    /// As [`Self::open_with`].
    pub fn open<'a>(
        url: &str,
        options: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> Result<Self, StorageError> {
        Self::open_with(url, options, &StorageConfig::default())
    }

    /// The store `url` names, with `options` (S3's region, endpoint and keys, as `object_store` names them) and,
    /// for S3, the waits and retries of `client`: a store out of reach is given up on in seconds, where
    /// `object_store`'s own ten retries take much longer. A `file://` directory is made when missing.
    ///
    /// # Errors
    /// [`StorageError::Url`] for a URL `object_store` doesn't take.
    pub fn open_with<'a>(
        url: &str,
        options: impl IntoIterator<Item = (&'a String, &'a String)>,
        client: &StorageConfig,
    ) -> Result<Self, StorageError> {
        let parsed = Url::parse(url).map_err(|_| StorageError::Url(url.to_owned()))?;
        if parsed.scheme() == "file" {
            let dir = parsed.to_file_path().map_err(|()| StorageError::Url(url.to_owned()))?;
            std::fs::create_dir_all(&dir).map_err(|error| StorageError::Url(error.to_string()))?;
        }
        let options = options.into_iter().map(|(key, value)| (key.clone(), value.clone()));
        let refused = |error: object_store::Error| StorageError::Url(error.to_string());
        if let Ok((ObjectStoreScheme::AmazonS3, prefix)) = ObjectStoreScheme::parse(&parsed) {
            let store = s3(&parsed, options, client).build().map_err(refused)?;
            return Ok(Self {
                store: Arc::new(store),
                prefix: Path::parse(prefix).map_err(|_| StorageError::Url(url.to_owned()))?,
            });
        }
        let (store, prefix) = object_store::parse_url_opts(&parsed, options).map_err(refused)?;
        Ok(Self {
            store: Arc::from(store),
            prefix,
        })
    }

    fn path(&self, key: &str) -> Result<Path, StorageError> {
        let key = Path::parse(key).map_err(|_| StorageError::Key)?;
        if key.as_ref().is_empty() {
            return Err(StorageError::Key);
        }
        Ok(self.prefix.parts().chain(key.parts()).collect())
    }
}

/// The S3 store at `url` as `object_store` builds it from `options`, with `client`'s waits unless an option sets
/// its own, and `client`'s retries.
fn s3(url: &Url, options: impl Iterator<Item = (String, String)>, client: &StorageConfig) -> AmazonS3Builder {
    let waits = [
        (ClientConfigKey::ConnectTimeout, client.connect_timeout),
        (ClientConfigKey::Timeout, client.request_timeout),
    ];
    let builder = waits.into_iter().fold(
        AmazonS3Builder::new().with_url(url.as_str()),
        |builder, (key, seconds)| builder.with_config(AmazonS3ConfigKey::Client(key), format!("{seconds}s")),
    );
    options
        .fold(builder, |builder, (key, value)| {
            match key.to_ascii_lowercase().parse::<AmazonS3ConfigKey>() {
                Ok(key) => builder.with_config(key, value),
                Err(_) => builder,
            }
        })
        .with_retry(RetryConfig {
            max_retries: client.max_retries,
            retry_timeout: std::time::Duration::from_secs(client.retry_timeout),
            ..RetryConfig::default()
        })
}

#[async_trait]
impl Storage for ObjectStorage {
    async fn put(&self, key: &str, bytes: Bytes) -> Result<(), StorageError> {
        self.store.put(&self.path(key)?, PutPayload::from(bytes)).await?;
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, StorageError> {
        match self.store.get(&self.path(key)?).await {
            Ok(found) => Ok(Some(found.bytes().await?)),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        match self.store.delete(&self.path(key)?).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn put_stream(&self, key: &str, mut body: Incoming, limit: u64) -> Result<u64, StorageError> {
        let mut upload = WriteMultipart::new(self.store.put_multipart(&self.path(key)?).await?);
        let mut size = 0_u64;
        loop {
            let failed = match body.next().await {
                None => break,
                Some(Ok(bytes)) => {
                    size += bytes.len() as u64;
                    if size <= limit {
                        upload.wait_for_capacity(UPLOAD_PARTS).await?;
                        upload.put(bytes);
                        continue;
                    }
                    StorageError::TooLarge
                }
                Some(Err(error)) => StorageError::Body(error),
            };
            if let Err(error) = upload.abort().await {
                tracing::warn!(%error, "an abandoned upload wasn't cleared");
            }
            return Err(failed);
        }
        upload.finish().await?;
        Ok(size)
    }

    async fn open(&self, key: &str, range: Option<std::ops::Range<u64>>) -> Result<Option<Blob>, StorageError> {
        let options = GetOptions {
            range: range.map(GetRange::Bounded),
            ..GetOptions::default()
        };
        match self.store.get_opts(&self.path(key)?, options).await {
            Ok(found) => Ok(Some(Blob {
                size: found.meta.size,
                range: found.range.clone(),
                bytes: found.into_stream().map_err(StorageError::from).boxed(),
            })),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use super::*;

    fn options(port: u16) -> BTreeMap<String, String> {
        [
            ("aws_endpoint", format!("http://127.0.0.1:{port}")),
            ("aws_region", "us-east-1".to_owned()),
            ("aws_access_key_id", "none".to_owned()),
            ("aws_secret_access_key", "none".to_owned()),
            ("aws_allow_http", "true".to_owned()),
            ("timeout", "45s".to_owned()),
            ("not an option", "left out".to_owned()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
    }

    #[test]
    fn an_s3_store_takes_the_configured_waits_unless_an_option_sets_its_own() {
        let client = StorageConfig {
            connect_timeout: 2,
            ..StorageConfig::default()
        };
        let url = Url::parse("s3://bucket/prefix").unwrap();
        let built = s3(&url, options(9).into_iter(), &client);
        let wait = |key| built.get_config_value(&AmazonS3ConfigKey::Client(key));
        assert_eq!(wait(ClientConfigKey::ConnectTimeout).as_deref(), Some("2s"));
        assert_eq!(wait(ClientConfigKey::Timeout).as_deref(), Some("45s"));
        assert!(matches!(
            ObjectStorage::open_with("s3://", &BTreeMap::new(), &client),
            Err(StorageError::Url(_))
        ));
    }

    #[tokio::test]
    async fn an_s3_store_out_of_reach_is_given_up_on_within_the_configured_retries() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let store = ObjectStorage::open("s3://bucket/prefix", &options(port)).unwrap();
        let asked = Instant::now();
        let stored = store.put("a/b", Bytes::from_static(b"bytes")).await;
        assert!(matches!(stored, Err(StorageError::Store(_))), "{stored:?}");
        let opened = store.put_stream("a/c", futures_util::stream::empty().boxed(), 10).await;
        assert!(matches!(opened, Err(StorageError::Store(_))), "{opened:?}");
        assert!(asked.elapsed() < Duration::from_secs(4), "{:?}", asked.elapsed());
    }
}
