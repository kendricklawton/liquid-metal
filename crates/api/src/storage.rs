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
