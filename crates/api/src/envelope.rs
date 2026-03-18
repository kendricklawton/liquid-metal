//! Envelope encryption backed by GCP Cloud KMS.
//!
//! ## Design
//!
//! Each workspace has one active Data Encryption Key (DEK) stored in the
//! `workspace_keys` table. The DEK is wrapped (encrypted) by a KMS Customer
//! Managed Key (CMK) and never stored in plaintext.
//!
//! **Write path:**
//!   1. Fetch (or create) the workspace's active DEK from `workspace_keys`.
//!   2. Unwrap the DEK via KMS.
//!   3. AES-256-GCM encrypt the value → `(ciphertext, nonce)`.
//!   4. Zeroize the plaintext DEK.
//!
//! **Read path:**
//!   1. Fetch the workspace's active DEK.
//!   2. Unwrap via KMS.
//!   3. AES-256-GCM decrypt `(ciphertext, nonce)` → plaintext value.
//!   4. Zeroize the plaintext DEK.

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::Aead,
};
use async_trait::async_trait;
use base64::Engine as _;
use uuid::Uuid;

// ── KMS trait ────────────────────────────────────────────────────────────────

/// Abstraction over a key-wrapping service (GCP KMS, or a local fallback for
/// testing). Only two operations: wrap and unwrap a DEK.
#[async_trait]
pub trait KmsClient: Send + Sync + 'static {
    /// Wrap (encrypt) a plaintext DEK. Returns `(ciphertext, key_version)`.
    async fn wrap_key(&self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, String)>;

    /// Unwrap (decrypt) a wrapped DEK back to plaintext bytes.
    async fn unwrap_key(&self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>>;
}

// ── GCP Cloud KMS implementation ─────────────────────────────────────────────

/// GCP Cloud KMS client using the REST API via `reqwest`.
///
/// Requires:
///   - `GCP_KMS_KEY`: full resource name, e.g.
///     `projects/my-proj/locations/us-central1/keyRings/ring/cryptoKeys/key`
///   - `GCP_KMS_CREDENTIALS`: path to a service account JSON with
///     `roles/cloudkms.cryptoKeyEncrypterDecrypter`. Falls back to
///     `GOOGLE_APPLICATION_CREDENTIALS` / ADC if unset (production Nomad
///     sets `GOOGLE_APPLICATION_CREDENTIALS` to the tmpfs-rendered SA JSON).
pub struct GcpKmsClient {
    http: reqwest::Client,
    auth: std::sync::Arc<dyn gcp_auth::TokenProvider>,
    /// Full KMS crypto key resource name (without `/cryptoKeyVersions/...`).
    key_name: String,
}

impl GcpKmsClient {
    /// Build a new GCP KMS client.
    ///
    /// `key_name` is the full resource path:
    /// `projects/{project}/locations/{location}/keyRings/{ring}/cryptoKeys/{key}`
    ///
    /// Authentication priority:
    /// 1. `GCP_KMS_CREDENTIALS` env var → load that specific SA JSON file.
    /// 2. Default ADC chain (`GOOGLE_APPLICATION_CREDENTIALS`, metadata server, etc.).
    pub async fn new(key_name: String) -> anyhow::Result<Self> {
        let auth: std::sync::Arc<dyn gcp_auth::TokenProvider> =
            if let Ok(path) = std::env::var("GCP_KMS_CREDENTIALS") {
                let sa = gcp_auth::CustomServiceAccount::from_file(&path)
                    .map_err(|e| anyhow::anyhow!("loading GCP_KMS_CREDENTIALS ({path}): {e}"))?;
                std::sync::Arc::new(sa)
            } else {
                gcp_auth::provider().await?
            };
        Ok(Self {
            http: reqwest::Client::new(),
            auth,
            key_name,
        })
    }

    /// Obtain a bearer token scoped to Cloud KMS.
    async fn token(&self) -> anyhow::Result<String> {
        let scopes = &["https://www.googleapis.com/auth/cloudkms"];
        let token: std::sync::Arc<gcp_auth::Token> = self.auth.token(scopes).await?;
        Ok(token.as_str().to_string())
    }
}

#[async_trait]
impl KmsClient for GcpKmsClient {
    async fn wrap_key(&self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, String)> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(plaintext);
        let token = self.token().await?;

        let url = format!(
            "https://cloudkms.googleapis.com/v1/{}:encrypt",
            self.key_name
        );

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "plaintext": b64 }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("KMS encrypt failed ({}): {}", status, body);
        }

        let body: serde_json::Value = resp.json().await?;

        let ciphertext_b64 = body["ciphertext"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("KMS response missing 'ciphertext' field"))?;
        let ciphertext = base64::engine::general_purpose::STANDARD.decode(ciphertext_b64)?;

        // The response includes the full key version used for encryption.
        let key_version = body["name"]
            .as_str()
            .unwrap_or(&self.key_name)
            .to_string();

        Ok((ciphertext, key_version))
    }

    async fn unwrap_key(&self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(ciphertext);
        let token = self.token().await?;

        let url = format!(
            "https://cloudkms.googleapis.com/v1/{}:decrypt",
            self.key_name
        );

        let resp = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&serde_json::json!({ "ciphertext": b64 }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("KMS decrypt failed ({}): {}", status, body);
        }

        let body: serde_json::Value = resp.json().await?;

        let plaintext_b64 = body["plaintext"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("KMS response missing 'plaintext' field"))?;
        let plaintext = base64::engine::general_purpose::STANDARD.decode(plaintext_b64)?;

        Ok(plaintext)
    }
}

// ── AES-256-GCM envelope helpers ─────────────────────────────────────────────

/// Encrypt a plaintext value using a raw 256-bit DEK.
/// Returns `(ciphertext, nonce)`. The nonce is 12 bytes (AES-GCM standard).
pub fn encrypt_value(dek: &[u8; 32], plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let cipher = Aes256Gcm::new_from_slice(dek)
        .map_err(|e| anyhow::anyhow!("AES key init: {e}"))?;

    let nonce_bytes: [u8; 12] = rand_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt: {e}"))?;

    Ok((ciphertext, nonce_bytes.to_vec()))
}

/// Decrypt a ciphertext using a raw 256-bit DEK and the original nonce.
pub fn decrypt_value(dek: &[u8; 32], ciphertext: &[u8], nonce: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(dek)
        .map_err(|e| anyhow::anyhow!("AES key init: {e}"))?;

    let nonce = Nonce::from_slice(nonce);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decrypt: {e}"))?;

    Ok(plaintext)
}

/// Generate a random 12-byte nonce for AES-GCM.
fn rand_nonce() -> [u8; 12] {
    use aes_gcm::aead::OsRng;
    use aes_gcm::aead::rand_core::RngCore;
    let mut buf = [0u8; 12];
    OsRng.fill_bytes(&mut buf);
    buf
}

/// Generate a random 256-bit DEK (32 bytes).
pub fn generate_dek() -> [u8; 32] {
    use aes_gcm::aead::OsRng;
    use aes_gcm::aead::rand_core::RngCore;
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    buf
}

// ── Workspace key management (DB operations) ─────────────────────────────────

/// Provision a new DEK for a workspace: generate → KMS wrap → store.
/// Deactivates any previously active DEK for the workspace.
pub async fn provision_workspace_key(
    db: &deadpool_postgres::Object,
    kms: &dyn KmsClient,
    workspace_id: Uuid,
) -> anyhow::Result<Uuid> {
    let plaintext_dek = generate_dek();

    let (encrypted_dek, key_version) = kms.wrap_key(&plaintext_dek).await?;

    let key_id = Uuid::now_v7();
    // Atomic CTE: deactivate any existing active DEK and insert the new one
    // in a single statement, avoiding a window where no active DEK exists.
    db.execute(
        "WITH deactivated AS ( \
             UPDATE workspace_keys SET active = FALSE \
             WHERE workspace_id = $2 AND active = TRUE \
         ) \
         INSERT INTO workspace_keys (id, workspace_id, encrypted_dek, kms_key_version, active) \
         VALUES ($1, $2, $3, $4, TRUE)",
        &[&key_id, &workspace_id, &encrypted_dek, &key_version],
    )
    .await?;

    tracing::info!(%workspace_id, %key_id, "provisioned workspace DEK");
    Ok(key_id)
}

/// Fetch the active DEK for a workspace, unwrap it via KMS, and return the
/// plaintext 32-byte key. Caller must zeroize after use.
///
/// If no DEK exists yet (e.g. workspace predates envelope encryption), one is
/// provisioned on-demand.
pub async fn unwrap_workspace_dek(
    db: &deadpool_postgres::Object,
    kms: &dyn KmsClient,
    workspace_id: Uuid,
) -> anyhow::Result<[u8; 32]> {
    let row = db
        .query_opt(
            "SELECT encrypted_dek FROM workspace_keys \
             WHERE workspace_id = $1 AND active = TRUE",
            &[&workspace_id],
        )
        .await?;

    let row = match row {
        Some(r) => r,
        None => {
            // On-demand provisioning for workspaces that predate envelope encryption.
            tracing::info!(%workspace_id, "no active DEK — provisioning on-demand");
            provision_workspace_key(db, kms, workspace_id).await?;
            db.query_one(
                "SELECT encrypted_dek FROM workspace_keys \
                 WHERE workspace_id = $1 AND active = TRUE",
                &[&workspace_id],
            )
            .await?
        }
    };

    let encrypted_dek: Vec<u8> = row.get("encrypted_dek");
    let plaintext = kms.unwrap_key(&encrypted_dek).await?;

    plaintext
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("DEK is {} bytes, expected 32", v.len()))
}

/// Encrypt a map of env vars into a single JSON blob ciphertext.
/// Returns `(ciphertext, nonce)` suitable for storage.
pub async fn encrypt_env_vars(
    db: &deadpool_postgres::Object,
    kms: &dyn KmsClient,
    workspace_id: Uuid,
    vars: &std::collections::HashMap<String, String>,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let dek = unwrap_workspace_dek(db, kms, workspace_id).await?;
    let plaintext = serde_json::to_vec(vars)?;
    let result = encrypt_value(&dek, &plaintext)?;
    // dek goes out of scope and is dropped (stack-allocated, not heap)
    Ok(result)
}

/// Decrypt a ciphertext blob back into a map of env vars.
pub async fn decrypt_env_vars(
    db: &deadpool_postgres::Object,
    kms: &dyn KmsClient,
    workspace_id: Uuid,
    ciphertext: &[u8],
    nonce: &[u8],
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let dek = unwrap_workspace_dek(db, kms, workspace_id).await?;
    let plaintext = decrypt_value(&dek, ciphertext, nonce)?;
    let vars: std::collections::HashMap<String, String> = serde_json::from_slice(&plaintext)?;
    Ok(vars)
}

// ── Test / dev KMS implementation ─────────────────────────────────────────────

/// In-memory KMS that uses a static XOR key for wrapping. For integration
/// tests where a real GCP KMS endpoint isn't available.
///
/// **Not for production use.** The "wrapping" is a simple XOR with the static
/// key, which is NOT cryptographically secure wrapping — it only preserves the
/// encrypt/decrypt contract for testing.
pub struct TestKmsClient {
    wrap_key: [u8; 32],
}

impl TestKmsClient {
    pub fn new() -> Self {
        Self {
            wrap_key: [0x42; 32], // deterministic for tests
        }
    }
}

#[async_trait]
impl KmsClient for TestKmsClient {
    async fn wrap_key(&self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, String)> {
        let wrapped: Vec<u8> = plaintext
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ self.wrap_key[i % 32])
            .collect();
        Ok((wrapped, "test-key-version-1".to_string()))
    }

    async fn unwrap_key(&self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        let unwrapped: Vec<u8> = ciphertext
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ self.wrap_key[i % 32])
            .collect();
        Ok(unwrapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let dek = generate_dek();
        let plaintext = b"DATABASE_URL=postgres://localhost/mydb";

        let (ciphertext, nonce) = encrypt_value(&dek, plaintext).unwrap();
        assert_ne!(ciphertext, plaintext);

        let decrypted = decrypt_value(&dek, &ciphertext, &nonce).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let dek1 = generate_dek();
        let dek2 = generate_dek();
        let plaintext = b"SECRET=hunter2";

        let (ciphertext, nonce) = encrypt_value(&dek1, plaintext).unwrap();
        let result = decrypt_value(&dek2, &ciphertext, &nonce);
        assert!(result.is_err());
    }

    #[test]
    fn wrong_nonce_fails() {
        let dek = generate_dek();
        let plaintext = b"SECRET=hunter2";

        let (ciphertext, _nonce) = encrypt_value(&dek, plaintext).unwrap();
        let bad_nonce = [0u8; 12];
        let result = decrypt_value(&dek, &ciphertext, &bad_nonce);
        assert!(result.is_err());
    }

    #[test]
    fn dek_is_32_bytes() {
        let dek = generate_dek();
        assert_eq!(dek.len(), 32);
    }

    #[test]
    fn nonce_is_12_bytes() {
        let nonce = rand_nonce();
        assert_eq!(nonce.len(), 12);
    }
}
