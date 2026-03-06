//! Artifact integrity verification via SHA-256.
//!
//! Prevents "man-in-the-middle" attacks between the API and daemon:
//! the API computes the hash at upload time, embeds it in the NATS event,
//! and the daemon re-hashes the file before booting or executing it.
//! Any tampering of the artifact in transit or in storage is caught here.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Verify that `path` hashes to `expected_hex` (lowercase SHA-256).
/// Returns an error if the file cannot be read or the digest does not match.
pub async fn artifact(path: &str, expected_hex: &str) -> Result<()> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading artifact for integrity check: {}", path))?;

    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != expected_hex.to_lowercase() {
        bail!(
            "artifact integrity FAILED for {}\n  expected: {}\n  computed: {}",
            path, expected_hex, actual
        );
    }
    tracing::debug!(path, "artifact SHA-256 verified");
    Ok(())
}

/// Compute the SHA-256 hex digest of `path`. Utility for the API upload path.
pub async fn compute(path: &str) -> Result<String> {
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading artifact: {}", path))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}
