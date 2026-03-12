//! Shared environment-based config helpers.

use anyhow::{Context, Result};

pub fn require_env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{} not set", key))
}

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Connect to NATS, optionally authenticating with `NATS_USER` + `NATS_PASSWORD`.
///
/// When both env vars are set, the connection uses user/password auth.
/// When unset, connects without credentials (local dev).
pub async fn nats_connect(url: &str) -> Result<async_nats::Client> {
    let user = std::env::var("NATS_USER").ok();
    let pass = std::env::var("NATS_PASSWORD").ok();

    let nc = match (user, pass) {
        (Some(u), Some(p)) => {
            tracing::info!("NATS: connecting with user/password auth");
            async_nats::ConnectOptions::with_user_and_password(u, p)
                .connect(url)
                .await
                .context("NATS authenticated connect")?
        }
        _ => {
            tracing::info!("NATS: connecting without auth (NATS_USER not set)");
            async_nats::connect(url)
                .await
                .context("NATS connect")?
        }
    };

    Ok(nc)
}

/// Build a rustls `MakeRustlsConnect` from a PEM CA certificate file.
///
/// Returns `None` when `POSTGRES_TLS_CA` is unset (local dev → NoTls).
pub fn pg_tls() -> Result<Option<tokio_postgres_rustls::MakeRustlsConnect>> {
    let ca_path = match std::env::var("POSTGRES_TLS_CA") {
        Ok(p) => p,
        Err(_) => {
            tracing::info!("Postgres: TLS disabled (POSTGRES_TLS_CA not set)");
            return Ok(None);
        }
    };

    let cert_pem = std::fs::read(&ca_path)
        .with_context(|| format!("reading POSTGRES_TLS_CA at {ca_path}"))?;

    let mut root_store = rustls::RootCertStore::empty();
    let certs = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parsing PEM certificates")?;

    for cert in certs {
        root_store.add(cert).context("adding CA cert to root store")?;
    }

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    tracing::info!("Postgres: TLS enabled (CA: {ca_path})");
    Ok(Some(tokio_postgres_rustls::MakeRustlsConnect::new(tls_config)))
}
