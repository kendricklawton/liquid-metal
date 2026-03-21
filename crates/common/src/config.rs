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
        (Some(_), None) => {
            anyhow::bail!("NATS_USER is set but NATS_PASSWORD is missing — set both or neither");
        }
        (None, Some(_)) => {
            anyhow::bail!("NATS_PASSWORD is set but NATS_USER is missing — set both or neither");
        }
        (None, None) => {
            tracing::info!("NATS: connecting without auth (NATS_USER/NATS_PASSWORD not set)");
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

/// Initialize tracing with an optional OpenTelemetry layer.
///
/// When `OTEL_EXPORTER_OTLP_ENDPOINT` is set, spans are exported via OTLP/gRPC
/// to the configured collector (e.g., Grafana Tempo). When unset, only the
/// standard `fmt` subscriber is used — zero overhead, no network calls.
///
/// Returns the tracer provider handle for graceful shutdown. Drop it at exit.
pub fn init_tracing(service_name: &str) -> Option<opentelemetry_sdk::trace::SdkTracerProvider> {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| format!("{service_name}=info").into());
    let fmt_layer = tracing_subscriber::fmt::layer();

    if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_otlp::WithExportConfig;
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.clone())
            .build()
            .expect("OTLP span exporter");

        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(
                opentelemetry_sdk::Resource::builder()
                    .with_service_name(service_name.to_string())
                    .build(),
            )
            .build();

        let tracer = provider.tracer(service_name.to_string());
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(otel_layer)
            .init();

        tracing::info!(%endpoint, "OpenTelemetry tracing enabled");
        Some(provider)
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .init();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn require_env_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("TEST_REQUIRE_ENV", "hello"); }
        assert_eq!(require_env("TEST_REQUIRE_ENV").unwrap(), "hello");
        unsafe { std::env::remove_var("TEST_REQUIRE_ENV"); }
    }

    #[test]
    fn require_env_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("TEST_REQUIRE_ENV_MISSING"); }
        let err = require_env("TEST_REQUIRE_ENV_MISSING").unwrap_err();
        assert!(err.to_string().contains("TEST_REQUIRE_ENV_MISSING"));
    }

    #[test]
    fn env_or_present() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("TEST_ENV_OR", "custom"); }
        assert_eq!(env_or("TEST_ENV_OR", "default"), "custom");
        unsafe { std::env::remove_var("TEST_ENV_OR"); }
    }

    #[test]
    fn env_or_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("TEST_ENV_OR_MISSING"); }
        assert_eq!(env_or("TEST_ENV_OR_MISSING", "fallback"), "fallback");
    }

    #[test]
    fn env_or_empty_string() {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("TEST_ENV_OR_EMPTY", ""); }
        assert_eq!(env_or("TEST_ENV_OR_EMPTY", "default"), "");
        unsafe { std::env::remove_var("TEST_ENV_OR_EMPTY"); }
    }
}
