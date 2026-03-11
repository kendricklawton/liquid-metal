//! Artifact integrity verification — thin wrapper around `common::artifact`.
//!
//! The daemon calls `artifact()` after downloading from S3, before booting
//! a VM or executing a Wasm module. Any tampering in transit or storage is
//! caught here.

use anyhow::Result;

pub async fn artifact(path: &str, expected_hex: &str) -> Result<()> {
    common::artifact::verify(path, expected_hex).await
}
