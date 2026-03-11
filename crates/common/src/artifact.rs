//! Artifact integrity utilities shared by the CLI, API, and daemon.
//!
//! The integrity chain works as follows:
//!   1. Whoever produces the artifact (CLI local build, or API server build)
//!      calls `sha256_hex()` or `sha256_file()` to compute the digest.
//!   2. The digest travels with the deploy request and into the NATS ProvisionEvent.
//!   3. The daemon calls `verify()` after downloading the artifact from S3 —
//!      any tampering in transit or storage is caught before the VM boots or
//!      the Wasm module executes.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Compute the SHA-256 hex digest of an in-memory byte slice.
/// Used by the CLI after reading the built artifact into memory.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Compute the SHA-256 hex digest of a file on disk.
/// Used by the API build runner after a server-side build completes.
pub async fn sha256_file(path: &str) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading artifact for hashing: {path}"))?;
    Ok(sha256_hex(&bytes))
}

/// Verify that `path` hashes to `expected_hex` (lowercase SHA-256).
/// Called by the daemon before booting a VM or executing a Wasm module.
pub async fn verify(path: &str, expected_hex: &str) -> Result<()> {
    let actual = sha256_file(path).await?;
    if actual != expected_hex.to_lowercase() {
        bail!(
            "artifact integrity FAILED for {path}\n  expected: {expected_hex}\n  computed: {actual}"
        );
    }
    tracing::debug!(path, "artifact SHA-256 verified");
    Ok(())
}
