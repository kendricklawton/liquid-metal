//! Object Storage client builder (Vultr Object Storage — S3-compatible).
//!
//! Reads credentials from env vars at startup:
//!   OBJECT_STORAGE_ENDPOINT  — e.g. https://ord1.vultrobjects.com
//!   OBJECT_STORAGE_ACCESS_KEY
//!   OBJECT_STORAGE_SECRET_KEY
//!   OBJECT_STORAGE_REGION    — defaults to "us-ord" (Chicago)

use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder;
use common::config::env_or;

/// Ensure the bucket exists and has an explicit private ACL.
///
/// Creates the bucket if it doesn't exist (idempotent — ignores
/// BucketAlreadyOwnedByYou). Then enforces a private ACL on every startup
/// to guard against accidental misconfiguration. If the ACL call fails
/// (e.g. provider doesn't support PutBucketAcl), we log a warning and
/// continue — this is defense-in-depth, not a hard gate.
pub async fn ensure_bucket(client: &aws_sdk_s3::Client, bucket: &str) {
    // Create bucket if it doesn't exist.
    match client.create_bucket().bucket(bucket).send().await {
        Ok(_) => tracing::info!(bucket, "S3 bucket created"),
        Err(e) => {
            let already_exists = e
                .as_service_error()
                .map(|se| {
                    se.is_bucket_already_exists() || se.is_bucket_already_owned_by_you()
                })
                .unwrap_or(false);
            if already_exists {
                tracing::debug!(bucket, "S3 bucket already exists");
            } else {
                tracing::warn!(bucket, error = %e, "failed to create S3 bucket");
            }
        }
    }

    // Enforce private ACL.
    match client
        .put_bucket_acl()
        .bucket(bucket)
        .acl(aws_sdk_s3::types::BucketCannedAcl::Private)
        .send()
        .await
    {
        Ok(_) => tracing::info!(bucket, "S3 bucket ACL set to private"),
        Err(e) => tracing::warn!(
            bucket,
            error = %e,
            "failed to set bucket ACL to private — verify manually"
        ),
    }
}

pub async fn build_client() -> aws_sdk_s3::Client {
    let endpoint  = env_or("OBJECT_STORAGE_ENDPOINT", "https://ord1.vultrobjects.com");
    let access    = env_or("OBJECT_STORAGE_ACCESS_KEY", "");
    let secret    = env_or("OBJECT_STORAGE_SECRET_KEY", "");
    let region    = env_or("OBJECT_STORAGE_REGION", "us-ord");

    let creds = Credentials::new(access, secret, None, None, "env");

    let cfg = Builder::new()
        .endpoint_url(endpoint)
        .region(Region::new(region))
        .credentials_provider(creds)
        .force_path_style(true)
        .behavior_version_latest()
        .build();

    aws_sdk_s3::Client::from_conf(cfg)
}
