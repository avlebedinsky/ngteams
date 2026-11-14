//! Where files live (uploads, pictures): any `object_store` store, by URL: `s3://<bucket>[/<prefix>]` on any
//! S3-compatible service, shared by every node (its endpoint, region, keys and addressing style are
//! `storage_options`, `docs/server.md`), `file:///<dir>` on one node, `memory:///` for tests. Only `object_store`'s
//! own calls are used, so no provider is special.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{StreamExt as _, TryStreamExt as _};
use object_store::path::Path;
use object_store::{GetOptions, GetRange, ObjectStore, ObjectStoreExt as _, PutPayload, WriteMultipart};
use url::Url;

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
    /// The store `url` names, with `options` (S3's region, endpoint and keys, as `object_store` names them). A
    /// `file://` directory is made when missing.
    ///
    /// # Errors
    /// [`StorageError::Url`] for a URL `object_store` doesn't take.
    pub fn open<'a>(
        url: &str,
        options: impl IntoIterator<Item = (&'a String, &'a String)>,
    ) -> Result<Self, StorageError> {
        let parsed = Url::parse(url).map_err(|_| StorageError::Url(url.to_owned()))?;
        if parsed.scheme() == "file" {
            let dir = parsed.to_file_path().map_err(|()| StorageError::Url(url.to_owned()))?;
            std::fs::create_dir_all(&dir).map_err(|error| StorageError::Url(error.to_string()))?;
        }
        let options = options.into_iter().map(|(key, value)| (key.clone(), value.clone()));
        let (store, prefix) =
            object_store::parse_url_opts(&parsed, options).map_err(|error| StorageError::Url(error.to_string()))?;
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
