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

/// Bulk-load active custom-domain certs from the API (which reads from Vault).
/// Called once at startup. Non-fatal — errors are logged and the cache is left empty.
pub fn warm_cert_cache_from_api(
    cache: &CertCache,
    pool: &Arc<Pool>,
    api_url: &str,
    internal_secret: &str,
) {
    let cache = cache.clone();
    let pool  = pool.clone();
    let api   = api_url.to_string();
    let secret = internal_secret.to_string();

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

            // Get list of verified domains from DB (metadata only, no encrypted columns).
            let rows = match db.query(
                "SELECT d.domain
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

            let http = reqwest::Client::new();
            let mut loaded = 0usize;
            for row in &rows {
                let domain: String = row.get("domain");
                let url = format!("{}/internal/certs/{}", api, domain);
                match http.get(&url)
                    .header("X-Internal-Secret", &secret)
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(body) = resp.json::<std::collections::HashMap<String, String>>().await {
                            if let (Some(cert), Some(key)) = (body.get("cert_pem"), body.get("key_pem")) {
                                insert_cert(&cache, &domain, cert.as_bytes(), key.as_bytes());
                                loaded += 1;
                            }
                        }
                    }
                    Ok(resp) => {
                        tracing::warn!(domain, status = %resp.status(), "cert warm-up: API error");
                    }
                    Err(e) => {
                        tracing::warn!(domain, error = %e, "cert warm-up: API request failed");
                    }
                }
            }
            tracing::info!(loaded, "cert warm-up complete");
        });
    });
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
