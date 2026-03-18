use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use pingora::prelude::*;
use pingora::listeners::tls::TlsSettings;

use common::config::{env_or, require_env};
use proxy::{cache, db, router::{self, MachineRouter}, tls};

fn main() -> Result<()> {
    // Proxy main() is synchronous (Pingora). A tokio runtime is needed for
    // OTel batch exporter and NATS connect.
    let otel_rt = tokio::runtime::Runtime::new()?;
    let _rt_guard = otel_rt.enter();
    let _tracer_provider = common::config::init_tracing("proxy");
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("rustls CryptoProvider already installed");

    let db_url            = require_env("DATABASE_URL")?;
    let nats_url          = env_or("NATS_URL",     "nats://127.0.0.1:4222");
    let api_upstream      = env_or("API_UPSTREAM", "127.0.0.1:3000");
    let tls_bind_addr     = env_or("BIND_ADDR",    "0.0.0.0:8443");
    let platform_domain   = env_or("PLATFORM_DOMAIN", "liquidmetal.dev");
    let wildcard_cert     = env_or("PLATFORM_WILDCARD_CERT",
                                   &format!("/etc/letsencrypt/live/{platform_domain}/fullchain.pem"));
    let wildcard_key      = env_or("PLATFORM_WILDCARD_KEY",
                                   &format!("/etc/letsencrypt/live/{platform_domain}/privkey.pem"));

    // ── Platform cert DEK (KMS-wrapped) ──────────────────────────────────────
    // Unwrapped once at startup via GCP KMS; used synchronously for all cert
    // cache operations. If unset, the proxy runs in wildcard-only mode.
    let cert_key: [u8; 32] = match std::env::var("CERT_DEK_WRAPPED").ok() {
        Some(ref wrapped) => {
            otel_rt.block_on(tls::unwrap_cert_dek(wrapped))
                .context("unwrapping platform cert DEK — check GCP_KMS_KEY + GCP_KMS_CREDENTIALS")?
        }
        None => {
            tracing::warn!("CERT_DEK_WRAPPED not set — custom domain certs will not be loaded");
            [0u8; 32]
        }
    };

    let pool        = Arc::new(db::build_pool(&db_url)?);
    let route_cache = cache::new();

    // Bulk-populate route cache before accepting traffic.
    cache::warm(&route_cache, &pool);

    // Warm custom domain → slug cache from DB.
    let domain_cache = cache::new_domain_cache();
    cache::warm_domains(&domain_cache, &pool);

    // ── TLS cert cache ──────────────────────────────────────────────────────
    let cert_cache = tls::new_cert_cache();
    if cert_key != [0u8; 32] {
        tls::warm_cert_cache(&cert_cache, &pool, &cert_key);
    }

    // NATS subscriber keeps the route cache warm and handles route eviction.
    cache::start_subscriber(route_cache.clone(), nats_url.clone());

    // Periodic DB reconciliation catches routes missed by NATS.
    cache::start_reconciler(route_cache.clone(), pool.clone());

    // Connect NATS for outbound traffic pulse publishes + cert hot-reload.
    let nats_client = {
        otel_rt.block_on(async { common::config::nats_connect(&nats_url).await.ok() })
            .map(Arc::new)
    };
    if nats_client.is_none() {
        tracing::warn!(%nats_url, "proxy NATS connect failed — idle timeout pulses and cert hot-reload disabled");
    }

    // Subscribe to cert_provisioned events to hot-reload custom domain certs.
    if let Some(ref nats) = nats_client {
        let cert_cache2 = cert_cache.clone();
        let pool2       = pool.clone();
        let nats2       = nats.clone();
        let cert_key2   = cert_key;
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio rt for cert reload");
            rt.block_on(async move {
                let mut sub = match nats2.subscribe(common::events::SUBJECT_CERT_PROVISIONED).await {
                    Ok(s) => s,
                    Err(e) => { tracing::warn!(error = %e, "cert reload: NATS subscribe failed"); return; }
                };
                use futures::StreamExt as _;
                while let Some(msg) = sub.next().await {
                    if let Ok(ev) = serde_json::from_slice::<common::events::CertProvisionedEvent>(&msg.payload) {
                        // Fetch the new cert from DB and hot-load it.
                        if let Ok(db) = pool2.get().await {
                            let row = db.query_opt(
                                "SELECT dc.cert_pem_enc, dc.key_pem_enc
                                 FROM domain_certs dc
                                 JOIN domains d ON d.id = dc.domain_id
                                 WHERE d.domain = $1 AND dc.expires_at > NOW()",
                                &[&ev.domain],
                            ).await;
                            if let Ok(Some(row)) = row {
                                let cert_enc: Vec<u8> = row.get("cert_pem_enc");
                                let key_enc:  Vec<u8> = row.get("key_pem_enc");
                                match (tls::decrypt_blob(&cert_key2, &cert_enc), tls::decrypt_blob(&cert_key2, &key_enc)) {
                                    (Ok(cert), Ok(key)) => {
                                        tls::insert_cert(&cert_cache2, &ev.domain, &cert, &key);
                                    }
                                    (Err(e), _) | (_, Err(e)) => {
                                        tracing::warn!(domain = %ev.domain, error = %e, "cert hot-reload: decrypt failed");
                                    }
                                }
                            }
                        }
                    }
                }
            });
        });
    }

    let pulse_cache = Arc::new(RwLock::new(HashMap::new()));
    router::start_pulse_sweeper(pulse_cache.clone());

    let mut server = Server::new(None).context("creating Pingora server")?;
    server.bootstrap();

    // ── TLS service (port 8443) ─────────────────────────────────────────────
    // HAProxy forwards :443 here as TCP pass-through — Pingora terminates TLS.
    let wildcard_ctx = tls::load_wildcard_context(&wildcard_cert, &wildcard_key)
        .context("loading wildcard TLS cert — set PLATFORM_WILDCARD_CERT / PLATFORM_WILDCARD_KEY")?;

    // Start domain + cert cache reconciler before caches are moved.
    cache::start_domain_reconciler(domain_cache.clone(), cert_cache.clone(), pool.clone());

    let sni_selector = tls::SniSelector { wildcard_ctx, cert_cache, platform_domain };
    let tls_settings = TlsSettings::with_callbacks(Box::new(sni_selector))
        .map_err(|e| anyhow::anyhow!("TLS settings: {e}"))?;

    let r = MachineRouter {
        pool,
        api_upstream,
        cache: route_cache,
        domain_cache,
        nats:  nats_client,
        pulse_cache,
    };

    let mut tls_svc = http_proxy_service(&server.configuration, r);
    tls_svc.add_tls_with_settings(&tls_bind_addr, None, tls_settings);
    server.add_service(tls_svc);

    tracing::info!(%tls_bind_addr, "liquid-metal-proxy listening");
    server.run_forever();
}
