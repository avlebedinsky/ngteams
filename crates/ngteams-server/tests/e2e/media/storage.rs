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
