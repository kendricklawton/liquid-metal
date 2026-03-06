//! Object Storage client builder (Vultr Object Storage — S3-compatible).
//!
//! Reads from env vars:
//!   OBJECT_STORAGE_ENDPOINT  — e.g. https://ord1.vultrobjects.com
//!   OBJECT_STORAGE_ACCESS_KEY
//!   OBJECT_STORAGE_SECRET_KEY
//!   OBJECT_STORAGE_REGION    — defaults to "us-ord" (Chicago)

use aws_config::Region;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder;
use common::config::env_or;

pub fn build_client() -> aws_sdk_s3::Client {
    let endpoint = env_or("OBJECT_STORAGE_ENDPOINT", "https://ord1.vultrobjects.com");
    let access   = env_or("OBJECT_STORAGE_ACCESS_KEY", "");
    let secret   = env_or("OBJECT_STORAGE_SECRET_KEY", "");
    let region   = env_or("OBJECT_STORAGE_REGION", "us-ord");

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

/// Download `key` from Object Storage to `local_path`.
/// Creates parent directories as needed.
pub async fn download(
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

    tokio::fs::write(local_path, &bytes)
        .await
        .with_context(|| format!("writing artifact to {}", local_path.display()))?;

    tracing::info!(
        key,
        path = %local_path.display(),
        bytes = bytes.len(),
        "artifact downloaded"
    );
    Ok(())
}
