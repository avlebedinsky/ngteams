//! The S3 store the tests run against.

use std::time::Duration;

use object_store::aws::AmazonS3Builder;
use object_store::path::Path;
use object_store::signer::Signer as _;

/// Makes `bucket` on the S3 server when it isn't there, with a signed `PUT` of the bucket itself.
pub async fn ensure_bucket(endpoint: &str, access: &str, secret: &str, bucket: &str) {
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_region("us-east-1")
        .with_bucket_name(bucket)
        .with_access_key_id(access)
        .with_secret_access_key(secret)
        .with_allow_http(true)
        .with_virtual_hosted_style_request(false)
        .build()
        .unwrap();
    let url = store
        .signed_url(reqwest::Method::PUT, &Path::from(""), Duration::from_secs(60))
        .await
        .unwrap();
    let answer = reqwest::Client::new().put(url).send().await.unwrap();
    assert!(
        answer.status().is_success() || answer.status() == 409,
        "the test bucket: HTTP {}",
        answer.status()
    );
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::{StreamExt as _, TryStreamExt as _, stream};
    use ngteams_server::config::Config;
    use ngteams_server::storage::{ObjectStorage, Storage as _, StorageError};

    async fn store() -> (ObjectStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        super::super::configure_storage(&mut config, dir.path()).await;
        (
            ObjectStorage::open_with(&config.storage_url, &config.storage_options, &config.storage).unwrap(),
            dir,
        )
    }

    fn body(parts: &[&'static [u8]]) -> ngteams_server::storage::Incoming {
        let parts: Vec<_> = parts.iter().map(|part| Ok(Bytes::from_static(part))).collect();
        stream::iter(parts).boxed()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_is_stored_read_back_in_ranges_and_dropped_when_over_its_limit() {
        let (store, _dir) = store().await;
        let size = store
            .put_stream("a/b", body(&[b"0123", b"4567", b"89"]), 10)
            .await
            .unwrap();
        assert_eq!(size, 10);
        let whole = store.open("a/b", None).await.unwrap().unwrap();
        assert_eq!((whole.size, whole.range.clone()), (10, 0..10));
        assert_eq!(format!("{whole:?}"), "Blob { size: 10, range: 0..10, .. }");
        let read: Vec<Bytes> = whole.bytes.try_collect().await.unwrap();
        assert_eq!(read.concat(), b"0123456789");
        let part = store.open("a/b", Some(3..7)).await.unwrap().unwrap();
        assert_eq!((part.size, part.range.clone()), (10, 3..7));
        let read: Vec<Bytes> = part.bytes.try_collect().await.unwrap();
        assert_eq!(read.concat(), b"3456");

        let over = store.put_stream("a/over", body(&[b"0123", b"4567"]), 7).await;
        assert!(matches!(over, Err(StorageError::TooLarge)), "{over:?}");
        assert!(store.open("a/over", None).await.unwrap().is_none());
        let broken: ngteams_server::storage::Incoming = stream::iter(vec![
            Ok(Bytes::from_static(b"01")),
            Err(std::io::Error::other("the client went away")),
        ])
        .boxed();
        let failed = store.put_stream("a/broken", broken, 10).await;
        assert!(matches!(failed, Err(StorageError::Body(_))), "{failed:?}");
        assert!(store.get("a/broken").await.unwrap().is_none());

        store.delete("a/b").await.unwrap();
        store.delete("a/b").await.unwrap();
        assert!(store.open("a/b", None).await.unwrap().is_none());
        assert!(matches!(store.open("", None).await, Err(StorageError::Key)));
    }
}
