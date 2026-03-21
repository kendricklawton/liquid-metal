//! Vault KV v2 client for secret storage.
//!
//! Wraps the Vault HTTP API for reading and writing secrets. Used by the API
//! crate to store user environment variables, TLS cert PEMs, and internal
//! platform secrets.
//!
//! All secrets are stored under the `secret/` KV v2 mount (Vault default).
//!
//! **Path conventions:**
//! - `workspaces/{workspace_id}/services/{service_id}/env` — user env vars
//! - `platform/certs/{domain}` — TLS cert + key PEMs
//! - `platform/internal/{name}` — internal platform secrets

use anyhow::{Context, Result};
use std::collections::HashMap;

/// HTTP client for Vault KV v2 operations.
pub struct VaultClient {
    http: reqwest::Client,
    /// Base URL, e.g. `http://localhost:8200`.
    addr: String,
    /// Authentication token.
    token: String,
}

impl VaultClient {
    /// Build a new Vault client from environment variables.
    ///
    /// Reads `VAULT_ADDR` and `VAULT_TOKEN`. Both are required.
    pub fn from_env() -> Result<Self> {
        let addr = crate::config::require_env("VAULT_ADDR")?;
        let token = crate::config::require_env("VAULT_TOKEN")?;

        // Strip trailing slash to normalize URLs.
        let addr = addr.trim_end_matches('/').to_string();

        Ok(Self {
            http: reqwest::Client::new(),
            addr,
            token,
        })
    }

    /// Build a Vault client with explicit address and token.
    pub fn new(addr: impl Into<String>, token: impl Into<String>) -> Self {
        let addr = addr.into().trim_end_matches('/').to_string();
        Self {
            http: reqwest::Client::new(),
            addr,
            token: token.into(),
        }
    }

    /// Check that Vault is reachable and unsealed.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/v1/sys/health", self.addr);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .context("vault health check: connection failed")?;

        if resp.status().is_success() {
            tracing::info!(addr = %self.addr, "vault is healthy and unsealed");
            Ok(())
        } else {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("vault health check failed ({}): {}", status, body);
        }
    }

    // ── KV v2 Operations ────────────────────────────────────────────────────

    /// Write a map of string key-value pairs to a KV v2 path.
    ///
    /// Creates a new version of the secret. Previous versions are retained
    /// by Vault (configurable via KV v2 settings).
    pub async fn kv_put(&self, path: &str, data: &HashMap<String, String>) -> Result<()> {
        let url = format!("{}/v1/secret/data/{}", self.addr, path);
        let body = serde_json::json!({ "data": data });

        let resp = self
            .http
            .post(&url)
            .header("X-Vault-Token", &self.token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("vault kv put {path}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("vault kv put {path} failed ({}): {}", status, body);
        }

        tracing::debug!(path, "vault kv put");
        Ok(())
    }

    /// Read a map of string key-value pairs from a KV v2 path.
    ///
    /// Returns `None` if the path does not exist (404).
    pub async fn kv_get(&self, path: &str) -> Result<Option<HashMap<String, String>>> {
        let url = format!("{}/v1/secret/data/{}", self.addr, path);

        let resp = self
            .http
            .get(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .with_context(|| format!("vault kv get {path}"))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("vault kv get {path} failed ({}): {}", status, body);
        }

        // KV v2 response shape: { "data": { "data": { ... }, "metadata": { ... } } }
        let body: serde_json::Value = resp.json().await?;
        let data = body
            .get("data")
            .and_then(|d| d.get("data"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let map: HashMap<String, String> = serde_json::from_value(data)
            .context("vault kv get: failed to deserialize secret data as HashMap<String, String>")?;

        tracing::debug!(path, "vault kv get");
        Ok(Some(map))
    }

    /// Delete all versions and metadata of a secret at a KV v2 path.
    pub async fn kv_delete(&self, path: &str) -> Result<()> {
        let url = format!("{}/v1/secret/metadata/{}", self.addr, path);

        let resp = self
            .http
            .delete(&url)
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .with_context(|| format!("vault kv delete {path}"))?;

        // 404 is fine — deleting a non-existent path is a no-op.
        if !resp.status().is_success()
            && resp.status() != reqwest::StatusCode::NOT_FOUND
        {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("vault kv delete {path} failed ({}): {}", status, body);
        }

        tracing::debug!(path, "vault kv delete");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trims_trailing_slash() {
        let client = VaultClient::new("http://localhost:8200/", "token");
        assert_eq!(client.addr, "http://localhost:8200");
    }

    #[test]
    fn new_no_trailing_slash() {
        let client = VaultClient::new("http://localhost:8200", "token");
        assert_eq!(client.addr, "http://localhost:8200");
    }

    #[test]
    fn new_multiple_trailing_slashes() {
        let client = VaultClient::new("http://localhost:8200///", "token");
        assert_eq!(client.addr, "http://localhost:8200");
    }

    #[test]
    fn new_stores_token() {
        let client = VaultClient::new("http://localhost:8200", "my-secret-token");
        assert_eq!(client.token, "my-secret-token");
    }
}
