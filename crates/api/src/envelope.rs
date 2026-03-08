//! Envelope encryption helpers for secrets at rest.
//!
//! # Scheme
//!
//! 1. Each workspace has a **Data Encryption Key** (DEK) — a 32-byte AES-256-GCM key.
//! 2. The DEK is wrapped by a **KMS Key** (CMK) and the result (EDK) is stored in
//!    `workspace_keys`.  The plaintext DEK never touches disk.
//! 3. To **write** a secret (e.g. an env var value):
//!    - Fetch the workspace's EDK from `workspace_keys`.
//!    - Decrypt the EDK via KMS to obtain the plaintext DEK.
//!    - AES-256-GCM encrypt the value with the DEK.
//!    - Store `(value_ciphertext, value_nonce)` in `project_env_vars`.
//! 4. To **read** a secret: reverse — decrypt EDK, decrypt ciphertext.
//!
//! # KMS Backends
//!
//! - [`LocalKmsClient`] — wraps the DEK with a local master key from
//!   `ENVELOPE_MASTER_KEY` (hex-encoded 32 bytes). **Dev / test only.**
//! - [`GoogleKmsClient`] — wraps the DEK via Google Cloud KMS using the key
//!   identified by `GOOGLE_KMS_KEY_NAME`. Production.
//!   Requires the `google-cloud-kms` crate (add when ready to wire up).
//!
//! # Environment Variables
//!
//! | Variable                  | Required in | Purpose                                 |
//! |---------------------------|-------------|------------------------------------------|
//! | `ENVELOPE_MASTER_KEY`     | dev         | 64 hex chars (32 bytes) local master key |
//! | `GOOGLE_KMS_KEY_NAME`     | prod        | Full KMS key resource name               |
//! | `GOOGLE_APPLICATION_CREDENTIALS` | prod | Path to service-account JSON (or use ADC)|

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, AeadCore, KeyInit, OsRng},
};
use anyhow::{Context, Result, anyhow};

/// Nonce size for AES-256-GCM (96 bits / 12 bytes).
pub const NONCE_SIZE: usize = 12;

// ─── KMS trait ───────────────────────────────────────────────────────────────

/// Abstraction over a Key Management Service.
///
/// Implementations are responsible for wrapping/unwrapping DEKs.
/// The plaintext DEK is never stored; only the wrapped form leaves this layer.
#[async_trait::async_trait]
pub trait KmsClient: Send + Sync {
    /// Wrap a plaintext DEK with the CMK.  Returns the ciphertext (EDK).
    async fn wrap_dek(&self, plaintext_dek: &[u8]) -> Result<Vec<u8>>;

    /// Unwrap an EDK with the CMK.  Returns the plaintext DEK.
    async fn unwrap_dek(&self, ciphertext_dek: &[u8]) -> Result<Vec<u8>>;
}

// ─── Local KMS client (dev / test) ───────────────────────────────────────────

/// Development-only KMS client that uses `ENVELOPE_MASTER_KEY` (64 hex chars = 32 bytes).
///
/// The DEK is wrapped with AES-256-GCM under the local master key.
/// **Never use in production** — the master key is stored in an env var.
pub struct LocalKmsClient {
    master_key: [u8; 32],
}

impl LocalKmsClient {
    /// Construct from `ENVELOPE_MASTER_KEY` env var (64 hex chars = 32 bytes).
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("ENVELOPE_MASTER_KEY")
            .context("ENVELOPE_MASTER_KEY not set")?;
        let bytes = hex::decode(raw.trim())
            .context("ENVELOPE_MASTER_KEY must be hex-encoded")?;
        let master_key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("ENVELOPE_MASTER_KEY must be exactly 32 bytes (64 hex chars)"))?;
        Ok(Self { master_key })
    }
}

#[async_trait::async_trait]
impl KmsClient for LocalKmsClient {
    async fn wrap_dek(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let (ct, nonce) = aes_gcm_encrypt(&self.master_key, plaintext)?;
        // Encode as nonce (12 bytes) || ciphertext for easy round-trip.
        let mut out = Vec::with_capacity(NONCE_SIZE + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    async fn unwrap_dek(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if ciphertext.len() <= NONCE_SIZE {
            return Err(anyhow!("wrapped DEK is too short"));
        }
        let nonce: [u8; NONCE_SIZE] = ciphertext[..NONCE_SIZE]
            .try_into()
            .expect("slice is exactly NONCE_SIZE bytes");
        aes_gcm_decrypt(&self.master_key, &ciphertext[NONCE_SIZE..], &nonce)
    }
}

// ─── Google KMS client (production) ──────────────────────────────────────────

/// Production KMS client backed by Google Cloud KMS (envelope encryption).
///
/// Set `GOOGLE_KMS_KEY_NAME` to the full key resource name:
/// ```text
/// projects/{project}/locations/{location}/keyRings/{ring}/cryptoKeys/{key}
/// ```
///
/// Authentication: Application Default Credentials (ADC) or
/// `GOOGLE_APPLICATION_CREDENTIALS` pointing to a service-account JSON file.
///
/// # TODO
/// Uncomment and wire up after adding the `google-cloud-kms` crate:
/// ```toml
/// [dependencies]
/// google-cloud-kms = "0.8"
/// ```
pub struct GoogleKmsClient {
    /// Full KMS key resource name.
    pub key_name: String,
}

impl GoogleKmsClient {
    pub fn from_env() -> Result<Self> {
        let key_name = std::env::var("GOOGLE_KMS_KEY_NAME")
            .context("GOOGLE_KMS_KEY_NAME not set")?;
        Ok(Self { key_name })
    }
}

#[async_trait::async_trait]
impl KmsClient for GoogleKmsClient {
    async fn wrap_dek(&self, _plaintext: &[u8]) -> Result<Vec<u8>> {
        // TODO: implement using google-cloud-kms crate.
        // Reference: https://cloud.google.com/kms/docs/encrypt-decrypt
        Err(anyhow!(
            "GoogleKmsClient not yet implemented — \
             add `google-cloud-kms` dependency and implement wrap_dek"
        ))
    }

    async fn unwrap_dek(&self, _ciphertext: &[u8]) -> Result<Vec<u8>> {
        Err(anyhow!(
            "GoogleKmsClient not yet implemented — \
             add `google-cloud-kms` dependency and implement unwrap_dek"
        ))
    }
}

// ─── DEK generation ──────────────────────────────────────────────────────────

/// Generate a random 256-bit AES-256-GCM Data Encryption Key.
pub fn generate_dek() -> [u8; 32] {
    Aes256Gcm::generate_key(OsRng).into()
}

// ─── AES-256-GCM primitives ──────────────────────────────────────────────────

/// Encrypt `plaintext` with AES-256-GCM under `dek`.
/// Returns `(ciphertext, nonce)`.
pub fn aes_gcm_encrypt(dek: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, [u8; NONCE_SIZE])> {
    let key    = Key::<Aes256Gcm>::from_slice(dek);
    let cipher = Aes256Gcm::new(key);
    let nonce  = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| anyhow!("AES-GCM encrypt failed: {:?}", e))?;

    Ok((ciphertext, nonce.into()))
}

/// Decrypt AES-256-GCM `ciphertext` with `dek` and `nonce`.
pub fn aes_gcm_decrypt(
    dek: &[u8; 32],
    ciphertext: &[u8],
    nonce: &[u8; NONCE_SIZE],
) -> Result<Vec<u8>> {
    let key    = Key::<Aes256Gcm>::from_slice(dek);
    let cipher = Aes256Gcm::new(key);
    let nonce  = Nonce::from_slice(nonce);

    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("AES-GCM decrypt failed: {:?}", e))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_aes_gcm() {
        let dek       = generate_dek();
        let plaintext = b"super-secret-value";
        let (ct, nonce) = aes_gcm_encrypt(&dek, plaintext).unwrap();
        let recovered   = aes_gcm_decrypt(&dek, &ct, &nonce).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn wrong_dek_fails_decrypt() {
        let dek1 = generate_dek();
        let dek2 = generate_dek();
        let (ct, nonce) = aes_gcm_encrypt(&dek1, b"secret").unwrap();
        assert!(aes_gcm_decrypt(&dek2, &ct, &nonce).is_err());
    }

    #[tokio::test]
    async fn local_kms_roundtrip() {
        // Bypass env var lookup for unit test.
        let master_key = generate_dek();
        let client     = LocalKmsClient { master_key };
        let dek        = generate_dek();
        let wrapped    = client.wrap_dek(&dek).await.unwrap();
        let unwrapped  = client.unwrap_dek(&wrapped).await.unwrap();
        assert_eq!(unwrapped, dek);
    }
}
