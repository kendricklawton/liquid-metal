//! Object Storage client builder (S3-compatible — MinIO locally, Vultr in prod).
//!
//! Required env vars (no defaults — fail fast per 12-factor III):
//!   OBJECT_STORAGE_ENDPOINT    — e.g. http://localhost:9000 or https://ord1.vultrobjects.com
//!   OBJECT_STORAGE_ACCESS_KEY
//!   OBJECT_STORAGE_SECRET_KEY
//!
//! Optional:
//!   OBJECT_STORAGE_REGION      — defaults to "us-east-1"

use anyhow::Result;
use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder;
use common::config::{env_or, require_env};

pub fn build_client() -> Result<aws_sdk_s3::Client> {
    let endpoint = require_env("OBJECT_STORAGE_ENDPOINT")?;
    let access   = require_env("OBJECT_STORAGE_ACCESS_KEY")?;
    let secret   = require_env("OBJECT_STORAGE_SECRET_KEY")?;
    let region   = env_or("OBJECT_STORAGE_REGION", "us-east-1");

    let creds = Credentials::new(access, secret, None, None, "env");

    let cfg = Builder::new()
        .endpoint_url(endpoint)
        .region(Region::new(region))
        .credentials_provider(creds)
        .force_path_style(true)
        .behavior_version_latest()
        .build();

    Ok(aws_sdk_s3::Client::from_conf(cfg))
}

/// S3 download deadline. Override via S3_DOWNLOAD_TIMEOUT_SECS.
static DOWNLOAD_TIMEOUT: std::sync::LazyLock<std::time::Duration> = std::sync::LazyLock::new(|| {
    let secs: u64 = env_or("S3_DOWNLOAD_TIMEOUT_SECS", "300").parse().unwrap_or(300);
    std::time::Duration::from_secs(secs)
});

/// Download `key` from Object Storage to `local_path`.
/// Creates parent directories as needed. Uses atomic write-then-rename
/// to prevent partial/corrupt artifacts on disk if the process crashes
/// or the write is interrupted.
///
/// Times out after `S3_DOWNLOAD_TIMEOUT_SECS` (default 300s) to prevent
/// a hung S3 connection from blocking the provision semaphore indefinitely.
pub async fn download(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    local_path: &std::path::Path,
) -> anyhow::Result<()> {
    tokio::time::timeout(*DOWNLOAD_TIMEOUT, download_inner(s3, bucket, key, local_path))
        .await
        .map_err(|_| anyhow::anyhow!(
            "S3 download timed out after {}s for {key}",
            DOWNLOAD_TIMEOUT.as_secs()
        ))?
}

async fn download_inner(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    local_path: &std::path::Path,
) -> anyhow::Result<()> {
    use anyhow::Context;

    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .context("creating artifact dir")?;
    }

    let output = s3
        .get_object()
        .bucket(bucket)
        .key(key)
        .send()
        .await
        .with_context(|| format!("downloading artifact {key} from Object Storage"))?;

    let bytes = output
        .body
        .collect()
        .await
        .context("reading artifact body")?
        .into_bytes();

    // Write to a temp file first, then atomically rename into place.
    // If we crash mid-write, the .tmp file is harmless and gets overwritten
    // on next attempt — the final path never holds partial data.
    let tmp_path = local_path.with_extension("tmp");
    tokio::fs::write(&tmp_path, &bytes)
        .await
        .with_context(|| format!("writing artifact to {}", tmp_path.display()))?;
    tokio::fs::rename(&tmp_path, local_path)
        .await
        .with_context(|| format!("renaming artifact to {}", local_path.display()))?;

    tracing::info!(
        key,
        path = %local_path.display(),
        bytes = bytes.len(),
        "artifact downloaded"
    );
    Ok(())
}
