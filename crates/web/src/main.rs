use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use common::{
    Features,
    config::{env_or, require_env},
};
use web::{AppState, build_router};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "web=info".into()))
        .init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("rustls CryptoProvider already installed");

    let features = Features::from_env();
    features.log_summary();

    let api_url = env_or("API_URL", "http://localhost:7070");
    let bind = env_or("WEB_BIND_ADDR", "0.0.0.0:3000");

    let internal_secret = require_env("INTERNAL_SECRET")?
        .split(',')
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("INTERNAL_SECRET must contain at least one non-empty value"))?;

    // ── OIDC configuration (Zitadel discovery) ────────────────────────────────
    // OIDC_WEB_CLIENT_ID is a separate Zitadel application from the CLI's
    // OIDC_CLI_CLIENT_ID — configured for Authorization Code + PKCE.
    let oidc_issuer = require_env("OIDC_ISSUER")?;
    let oidc_client_id = require_env("OIDC_WEB_CLIENT_ID")?;

    let disc = oidc_discover(&oidc_issuer)
        .await
        .with_context(|| format!("OIDC discovery failed for {oidc_issuer}"))?;
    tracing::info!(%oidc_issuer, "OIDC endpoints discovered (browser flow)");

    let oidc_auth_url    = disc.authorization_endpoint;
    let oidc_token_url   = disc.token_endpoint;
    let oidc_userinfo_url = disc.userinfo_endpoint;
    let oidc_revoke_url  = disc.revocation_endpoint;

    // ── Cookie encryption key ────────────────────────────────────────────────
    let cookie_key = match std::env::var("COOKIE_SECRET") {
        Ok(hex_key) => {
            let bytes = hex::decode(&hex_key)
                .context("COOKIE_SECRET must be valid hex (64 chars = 32 bytes)")?;
            axum_extra::extract::cookie::Key::from(&bytes)
        }
        Err(_) => {
            tracing::warn!("COOKIE_SECRET not set — generating ephemeral key (sessions won't survive restart)");
            axum_extra::extract::cookie::Key::generate()
        }
    };

    let oidc_redirect_uri = format!(
        "{}/auth/callback",
        env_or("WEB_PUBLIC_URL", &format!("http://localhost:{}", bind.split(':').last().unwrap_or("3000")))
    );

    // ── Shared HTTP client ───────────────────────────────────────────────────
    let http_client = reqwest::Client::new();

    // ── Dev auth bypass ─────────────────────────────────────────────────────
    let disable_auth = std::env::var("DISABLE_AUTH").as_deref() == Ok("1");
    if disable_auth {
        tracing::warn!("DISABLE_AUTH=1 — dashboard auth bypass active (dev only)");
    }

    // ── AppState ─────────────────────────────────────────────────────────────
    let state = Arc::new(AppState {
        api_url,
        internal_secret,
        http_client,
        oidc_client_id,
        oidc_auth_url,
        oidc_token_url,
        oidc_userinfo_url,
        oidc_revoke_url,
        oidc_redirect_uri,
        cookie_key,
        features,
        disable_auth,
    });

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    let port = listener.local_addr()?.port();
    tracing::info!(%bind, "web listening");
    tracing::info!("http://localhost:{port}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("web exited cleanly");
    Ok(())
}

// ── OIDC discovery ───────────────────────────────────────────────────────────

async fn oidc_discover(issuer: &str) -> Result<OidcDiscovery> {
    let issuer = issuer.trim_end_matches('/');
    let issuer = if issuer.starts_with("http://") || issuer.starts_with("https://") {
        issuer.to_string()
    } else {
        format!("https://{issuer}")
    };
    let url = format!("{issuer}/.well-known/openid-configuration");

    let disc = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .context("HTTP request to OIDC discovery URL")?
        .error_for_status()
        .context("OIDC discovery returned non-2xx")?
        .json::<OidcDiscovery>()
        .await
        .context("deserializing OIDC discovery document")?;

    validate_oidc_url(&disc.authorization_endpoint, "authorization_endpoint")?;
    validate_oidc_url(&disc.token_endpoint, "token_endpoint")?;
    validate_oidc_url(&disc.userinfo_endpoint, "userinfo_endpoint")?;
    if let Some(rev) = &disc.revocation_endpoint {
        validate_oidc_url(rev, "revocation_endpoint")?;
    }

    Ok(disc)
}

fn validate_oidc_url(raw: &str, field: &str) -> Result<()> {
    let parsed = url::Url::parse(raw)
        .with_context(|| format!("OIDC {field} is not a valid URL: {raw}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => anyhow::bail!("OIDC {field} has unsupported scheme \"{other}\": {raw}"),
    }
}

#[derive(serde::Deserialize)]
struct OidcDiscovery {
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    #[serde(default)]
    revocation_endpoint: Option<String>,
}

// ── Graceful shutdown ────────────────────────────────────────────────────────

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = sigterm.recv()          => tracing::info!("SIGTERM received"),
            _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C received"),
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.expect("Ctrl-C handler");
}
