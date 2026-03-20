//! Platform-scoped AES-GCM encryption for TLS cert storage.
//!
//! # Why platform-scoped, not workspace-scoped
//! TLS private keys are infrastructure credentials, not tenant secrets.
//! Unlike env vars (which have direct value if extracted), a private key is only
//! exploitable if the attacker can also intercept live traffic to that domain —
//! a much harder compound attack. A platform DEK is appropriate; workspace
//! isolation would add proxy KMS complexity for no meaningful security gain.
//!
//! # Key management
//! The DEK is a random 32-byte key that is KMS-wrapped at rest using the same
//! GCP Cloud KMS key as envelope encryption (`GCP_KMS_KEY`). Neither the API
//! nor the proxy ever stores or logs the plaintext DEK — it lives in memory only
//! for the lifetime of the process. The wrapped DEK is stored in `CERT_DEK_WRAPPED`
//! (base64-encoded KMS ciphertext).
//!
//! # Storage format
//! Encrypted blobs stored in `domain_certs`: `[12-byte nonce || ciphertext]`
//! as a single BYTEA column. This matches the nonce-prepended format used by the
//! web crate's cookie encryption, keeping the schema to one column per secret.

use anyhow::{Context, Result};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::Engine as _;

use crate::envelope::KmsClient;

/// Unwrap the platform cert DEK via KMS. Called once at API startup.
/// The returned key is stored in `AppState` and used for all cert encrypt/decrypt.
///
/// `wrapped_b64` is the base64-encoded KMS ciphertext from `CERT_DEK_WRAPPED`.
pub async fn unwrap_cert_dek(kms: &dyn KmsClient, wrapped_b64: &str) -> Result<[u8; 32]> {
    let wrapped = base64::engine::general_purpose::STANDARD
        .decode(wrapped_b64.trim())
        .context("CERT_DEK_WRAPPED is not valid base64")?;

    let plaintext = kms
        .unwrap_key(&wrapped)
        .await
        .context("KMS failed to unwrap CERT_DEK_WRAPPED")?;

    plaintext
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("cert DEK is {} bytes, expected 32", v.len()))
}

/// Encrypt `plaintext` with the provided 32-byte key.
/// Returns `[nonce (12 bytes) || ciphertext]`.
pub fn encrypt_pem(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::aead::OsRng;
    use aes_gcm::aead::rand_core::RngCore;

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("AES key init: {e}"))?;

    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow::anyhow!("AES-GCM encrypt: {e}"))?;

    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt `blob` (expected format: `[nonce (12 bytes) || ciphertext]`).
pub fn decrypt_pem(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < 12 {
        anyhow::bail!("cert blob too short to contain nonce");
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| anyhow::anyhow!("AES key init: {e}"))?;
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("AES-GCM decrypt: {e}"))
}
