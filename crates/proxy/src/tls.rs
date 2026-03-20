/// SNI-based TLS cert selection for Pingora.
///
/// On startup, `warm_cert_cache` loads all active custom-domain certs from
/// Postgres (decrypted) and builds per-domain `SslContext` objects. The
/// `SniSelector` SNI callback swaps the SSL context based on the client's
/// server name indicator:
///   - `*.liquidmetal.dev` → wildcard cert from disk (certbot-managed)
///   - `<custom-domain>` → per-domain cert from the in-memory `CertCache`
///
/// Hot-reload: the NATS `platform.cert_provisioned` subscriber calls
/// `insert_cert` to add new certs without proxy restart.
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use openssl::pkey::PKey;
use openssl::ssl::{SslContext, SslContextBuilder, SslMethod, SslFiletype, SslRef, NameType, SslVerifyMode};
use openssl::x509::X509;
use deadpool_postgres::Pool;

/// Domain → compiled SslContext. Written by warm-up + NATS reload; read by
/// the SNI callback on every TLS handshake. Arc<SslContext> is Send + Sync.
pub type CertCache = Arc<RwLock<HashMap<String, Arc<SslContext>>>>;

pub fn new_cert_cache() -> CertCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Build an `SslContext` from PEM bytes (cert chain + private key).
pub fn build_ssl_context(cert_pem: &[u8], key_pem: &[u8]) -> Result<SslContext> {
    let mut builder = SslContextBuilder::new(SslMethod::tls_server())
        .context("creating SSL context builder")?;
    builder.set_verify(SslVerifyMode::NONE);

    // Parse the PEM chain: first cert is the leaf, rest are intermediates.
    let certs = X509::stack_from_pem(cert_pem).context("parsing cert chain PEM")?;
    if certs.is_empty() {
        anyhow::bail!("cert PEM contains no certificates");
    }
    builder.set_certificate(&certs[0]).context("loading leaf cert")?;
    for intermediate in &certs[1..] {
        builder.add_extra_chain_cert(intermediate.clone()).context("loading intermediate cert")?;
    }

    let pkey = PKey::private_key_from_pem(key_pem).context("parsing private key PEM")?;
    builder.set_private_key(&pkey).context("loading private key")?;
    builder.check_private_key()
        .context("private key does not match certificate")?;
    Ok(builder.build())
}

/// Load the platform wildcard cert from disk (certbot-managed).
/// Paths come from `PLATFORM_WILDCARD_CERT` and `PLATFORM_WILDCARD_KEY` env vars.
pub fn load_wildcard_context(cert_path: &str, key_path: &str) -> Result<Arc<SslContext>> {
    let mut builder = SslContextBuilder::new(SslMethod::tls_server())
        .context("creating wildcard SSL context")?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_certificate_chain_file(cert_path)
        .with_context(|| format!("loading wildcard cert from {cert_path}"))?;
    builder.set_private_key_file(key_path, SslFiletype::PEM)
        .with_context(|| format!("loading wildcard key from {key_path}"))?;
    builder.check_private_key()
        .context("wildcard private key does not match certificate")?;
    Ok(Arc::new(builder.build()))
}

/// Insert or replace a cert in the cache (called on NATS cert_provisioned event).
pub fn insert_cert(cache: &CertCache, domain: &str, cert_pem: &[u8], key_pem: &[u8]) {
    match build_ssl_context(cert_pem, key_pem) {
        Ok(ctx) => {
            cache.write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(domain.to_string(), Arc::new(ctx));
            tracing::info!(domain, "cert cache: cert loaded");
        }
        Err(e) => {
            tracing::error!(domain, error = %e, "cert cache: failed to build SslContext");
        }
    }
}

/// Bulk-load active custom-domain certs from Postgres into the cert cache.
/// Called once at startup. Non-fatal — errors are logged and the cache is left empty.
pub fn warm_cert_cache(
    cache: &CertCache,
    pool: &Arc<Pool>,
    cert_key: &[u8; 32],
) {
    let cache = cache.clone();
    let pool  = pool.clone();
    let key   = *cert_key;

    // Pingora main() is sync; use a blocking thread for the async DB call.
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio rt for cert warm-up");

        rt.block_on(async move {
            let db = match pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "cert warm-up: failed to get DB conn");
                    return;
                }
            };

            let rows = match db.query(
                "SELECT d.domain, dc.cert_pem_enc, dc.key_pem_enc
                 FROM domain_certs dc
                 JOIN domains d ON d.id = dc.domain_id
                 WHERE d.is_verified = true
                   AND d.deleted_at IS NULL
                   AND dc.expires_at > NOW()",
                &[],
            ).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "cert warm-up: query failed");
                    return;
                }
            };

            let mut loaded = 0usize;
            for row in &rows {
                let domain:   String = row.get("domain");
                let cert_enc: Vec<u8> = row.get("cert_pem_enc");
                let key_enc:  Vec<u8> = row.get("key_pem_enc");

                let cert_pem = match decrypt_blob(&key, &cert_enc) {
                    Ok(v) => v,
                    Err(e) => { tracing::warn!(domain, error = %e, "cert warm-up: decrypt cert failed"); continue; }
                };
                let key_pem = match decrypt_blob(&key, &key_enc) {
                    Ok(v) => v,
                    Err(e) => { tracing::warn!(domain, error = %e, "cert warm-up: decrypt key failed"); continue; }
                };

                insert_cert(&cache, &domain, &cert_pem, &key_pem);
                loaded += 1;
            }
            tracing::info!(loaded, "cert warm-up complete");
        });
    });
}

/// AES-GCM decrypt using nonce-prepended format `[12-byte nonce || ciphertext]`.
pub fn decrypt_blob(key: &[u8; 32], blob: &[u8]) -> Result<Vec<u8>> {
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
    if blob.len() < 12 {
        anyhow::bail!("blob too short");
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow::anyhow!("{e}"))?;
    let nonce  = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ciphertext).map_err(|e| anyhow::anyhow!("AES-GCM: {e}"))
}

/// Unwrap the platform cert DEK via GCP KMS. Called once at proxy startup.
///
/// Uses the same `GCP_KMS_KEY` + `GCP_KMS_CREDENTIALS` env vars as the API.
/// The returned key is used for all cert decryption during warm-up and hot-reload.
/// The plaintext DEK lives in memory only — never logged or persisted.
pub async fn unwrap_cert_dek(wrapped_b64: &str) -> anyhow::Result<[u8; 32]> {
    use base64::Engine as _;

    let kms_key = std::env::var("GCP_KMS_KEY")
        .map_err(|_| anyhow::anyhow!("GCP_KMS_KEY is required for cert DEK unwrap"))?;

    let auth: std::sync::Arc<dyn gcp_auth::TokenProvider> =
        if let Ok(path) = std::env::var("GCP_KMS_CREDENTIALS") {
            let sa = gcp_auth::CustomServiceAccount::from_file(&path)
                .map_err(|e| anyhow::anyhow!("loading GCP_KMS_CREDENTIALS ({path}): {e}"))?;
            std::sync::Arc::new(sa)
        } else {
            gcp_auth::provider().await?
        };

    let token = auth
        .token(&["https://www.googleapis.com/auth/cloudkms"])
        .await
        .context("obtaining GCP auth token for KMS")?;

    let wrapped = base64::engine::general_purpose::STANDARD
        .decode(wrapped_b64.trim())
        .context("CERT_DEK_WRAPPED is not valid base64")?;

    let resp = reqwest::Client::new()
        .post(format!("https://cloudkms.googleapis.com/v1/{kms_key}:decrypt"))
        .bearer_auth(token.as_str())
        .json(&serde_json::json!({
            "ciphertext": base64::engine::general_purpose::STANDARD.encode(&wrapped)
        }))
        .send()
        .await
        .context("KMS decrypt HTTP request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("KMS decrypt failed ({status}): {body}");
    }

    let body: serde_json::Value = resp.json().await.context("parsing KMS decrypt response")?;
    let plaintext_b64 = body["plaintext"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("KMS response missing 'plaintext' field"))?;

    let plaintext = base64::engine::general_purpose::STANDARD
        .decode(plaintext_b64)
        .context("decoding KMS plaintext")?;

    plaintext
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("cert DEK is {} bytes, expected 32", v.len()))
}

/// Pingora TLS accept callbacks — swaps the SSL context based on SNI.
pub struct SniSelector {
    /// Pre-built SslContext for *.liquidmetal.dev (loaded from disk at startup).
    pub wildcard_ctx:    Arc<SslContext>,
    /// Per-custom-domain SslContext map. Hot-reloaded via NATS.
    pub cert_cache:      CertCache,
    /// Platform domain suffix, e.g. "liquidmetal.dev".
    pub platform_domain: String,
}

#[async_trait]
impl pingora::listeners::TlsAccept for SniSelector {
    async fn certificate_callback(&self, ssl: &mut SslRef) {
        // Own the server name to release the immutable borrow on `ssl`
        // before the mutable `set_ssl_context` calls below.
        let server_name = ssl.servername(NameType::HOST_NAME)
            .unwrap_or("")
            .to_string();

        let dotted_suffix = format!(".{}", self.platform_domain);
        if server_name.is_empty()
            || server_name == self.platform_domain
            || server_name.ends_with(&dotted_suffix)
        {
            // Platform subdomain or no SNI — use the wildcard cert.
            if let Err(e) = ssl.set_ssl_context(&self.wildcard_ctx) {
                tracing::error!(error = %e, server_name, "SNI: failed to set wildcard context");
            }
        } else {
            // Custom domain — look up in cert cache.
            let cache = self.cert_cache.read().unwrap_or_else(|e| e.into_inner());
            if let Some(ctx) = cache.get(server_name.as_str()) {
                if let Err(e) = ssl.set_ssl_context(ctx) {
                    tracing::error!(error = %e, server_name, "SNI: failed to set custom domain context");
                }
            }
            // No cert found → handshake will fail with no shared cipher.
            // This is the correct behavior — don't serve the wildcard for unknown domains.
        }
    }
}
