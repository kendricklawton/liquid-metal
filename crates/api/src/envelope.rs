//! Secret storage backed by HashiCorp Vault KV v2.
//!
//! Replaces the previous AES-256-GCM envelope encryption scheme. All secrets
//! are now stored in Vault — encryption at rest, audit logging, and key
//! management are handled by Vault transparently.
//!
//! **Vault path conventions:**
//! - `workspaces/{workspace_id}/services/{service_id}/env` — user env vars
//! - `platform/certs/{domain}` — TLS cert + key PEMs
//! - `platform/internal/{name}` — internal platform secrets

use std::collections::HashMap;
use common::vault::VaultClient;
use uuid::Uuid;

/// Vault path for a service's environment variables.
fn env_path(workspace_id: &Uuid, service_id: &Uuid) -> String {
    format!("workspaces/{workspace_id}/services/{service_id}/env")
}

/// Store environment variables for a service in Vault.
pub async fn store_env_vars(
    vault: &VaultClient,
    workspace_id: Uuid,
    service_id: Uuid,
    vars: &HashMap<String, String>,
) -> anyhow::Result<()> {
    vault.kv_put(&env_path(&workspace_id, &service_id), vars).await
}

/// Read environment variables for a service from Vault.
///
/// Returns an empty map if no env vars have been set.
pub async fn read_env_vars(
    vault: &VaultClient,
    workspace_id: Uuid,
    service_id: Uuid,
) -> anyhow::Result<HashMap<String, String>> {
    vault
        .kv_get(&env_path(&workspace_id, &service_id))
        .await
        .map(|opt| opt.unwrap_or_default())
}

/// Delete all environment variables for a service from Vault.
pub async fn delete_env_vars(
    vault: &VaultClient,
    workspace_id: Uuid,
    service_id: Uuid,
) -> anyhow::Result<()> {
    vault.kv_delete(&env_path(&workspace_id, &service_id)).await
}

/// Store a TLS certificate and private key PEM in Vault.
pub async fn store_cert(
    vault: &VaultClient,
    domain: &str,
    cert_pem: &str,
    key_pem: &str,
) -> anyhow::Result<()> {
    let mut data = HashMap::new();
    data.insert("cert_pem".to_string(), cert_pem.to_string());
    data.insert("key_pem".to_string(), key_pem.to_string());
    vault.kv_put(&format!("platform/certs/{domain}"), &data).await
}

/// Read a TLS certificate and private key PEM from Vault.
///
/// Returns `None` if no cert has been stored for this domain.
pub async fn read_cert(
    vault: &VaultClient,
    domain: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let data = vault.kv_get(&format!("platform/certs/{domain}")).await?;
    match data {
        Some(map) => {
            let cert = map
                .get("cert_pem")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("cert_pem missing from vault for {domain}"))?;
            let key = map
                .get("key_pem")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("key_pem missing from vault for {domain}"))?;
            Ok(Some((cert, key)))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_path_format() {
        let wid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let sid = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
        let path = env_path(&wid, &sid);
        assert_eq!(
            path,
            "workspaces/550e8400-e29b-41d4-a716-446655440000/services/6ba7b810-9dad-11d1-80b4-00c04fd430c8/env"
        );
    }

    #[test]
    fn env_path_different_ids_different_paths() {
        let wid1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let wid2 = Uuid::parse_str("660e8400-e29b-41d4-a716-446655440000").unwrap();
        let sid = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
        assert_ne!(env_path(&wid1, &sid), env_path(&wid2, &sid));
    }
}
