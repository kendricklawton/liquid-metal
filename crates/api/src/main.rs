use std::sync::Arc;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use api::{AppState, RateLimitConfig, build_router};
use common::{
    Features,
    config::{env_or, require_env},
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "api=info".into()))
        .init();

    // ── Migrate-only mode ────────────────────────────────────────────────────
    if std::env::args().any(|a| a == "--migrate") {
        let db_url = require_env("DATABASE_URL")?;
        let migrate_url = env_or("MIGRATIONS_DATABASE_URL", &db_url);

        api::migrations::run_with_url(&migrate_url)
            .await
            .context("running migrations")?;

        tracing::info!("migrations complete — exiting");
        return Ok(());
    }

    let features = Features::from_env();
    features.log_summary();

    let db_url = require_env("DATABASE_URL")?;
    let internal_secrets: Vec<String> = require_env("INTERNAL_SECRET")?
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if internal_secrets.is_empty() {
        anyhow::bail!("INTERNAL_SECRET must contain at least one non-empty value");
    }

    // ── OIDC configuration ───────────────────────────────────────────────────
    let (oidc_client_id, oidc_device_auth_url, oidc_token_url, oidc_userinfo_url, oidc_revoke_url) = {
        let issuer = std::env::var("OIDC_ISSUER")
            .or_else(|_| std::env::var("ZITADEL_DOMAIN"))
            .ok();

        let client_id = std::env::var("OIDC_CLIENT_ID")
            .or_else(|_| std::env::var("ZITADEL_CLIENT_ID"))
            .ok();

        if let Some(issuer) = issuer {
            let client_id = client_id.context(
                "OIDC_CLIENT_ID (or ZITADEL_CLIENT_ID) is required when using OIDC_ISSUER",
            )?;

            let disc = oidc_discover(&issuer)
                .await
                .with_context(|| format!("OIDC discovery failed for {issuer}"))?;

            tracing::info!(issuer, "OIDC endpoints discovered");
            (
                client_id,
                disc.device_authorization_endpoint,
                disc.token_endpoint,
                disc.userinfo_endpoint,
                disc.revocation_endpoint,
            )
        } else {
            (
                require_env("OIDC_CLIENT_ID")?,
                require_env("OIDC_DEVICE_AUTH_URL")?,
                require_env("OIDC_TOKEN_URL")?,
                require_env("OIDC_USERINFO_URL")?,
                std::env::var("OIDC_REVOKE_URL").ok(),
            )
        }
    };

    let nats_url    = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let bind        = env_or("BIND_ADDR", "0.0.0.0:7070");
    let bucket      = env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts");

    // ── Postgres pool ────────────────────────────────────────────────────────
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let pool = if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
        deadpool_postgres::Pool::builder(mgr).max_size(16).build()
            .context("building postgres pool (TLS)")?
    } else {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(mgr).max_size(16).build()
            .context("building postgres pool")?
    };

    // ── Verify migrations are up to date ─────────────────────────────────────
    // Migrations are applied by `nomad job dispatch migrate` (or `api --migrate`)
    // before the API service starts. The API only verifies — never runs DDL.
    api::migrations::verify(&pool)
        .await
        .context("migration version check")?;

    // ── NATS JetStream ───────────────────────────────────────────────────────
    let nc = common::config::nats_connect(&nats_url).await?;
    let js = async_nats::jetstream::new(nc.clone());
    api::nats::ensure_stream(&js).await?;

    // ── S3 client ────────────────────────────────────────────────────────────
    let s3 = api::storage::build_client().await;
    api::storage::ensure_bucket(&s3, &bucket).await;

    // ── Shared HTTP client ───────────────────────────────────────────────────
    let http_client = reqwest::Client::new();

    // ── Stripe (optional — billing disabled when STRIPE_SECRET_KEY is unset) ─
    let stripe = std::env::var("STRIPE_SECRET_KEY").ok()
        .map(|key| api::stripe::StripeClient::new(key, http_client.clone()));
    let stripe_webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET").ok();
    let stripe_price_pro  = std::env::var("STRIPE_PRICE_ID_PRO").ok();
    let stripe_price_team = std::env::var("STRIPE_PRICE_ID_TEAM").ok();

    if stripe.is_some() {
        tracing::info!("Stripe billing enabled");
    } else {
        tracing::info!("Stripe billing disabled (STRIPE_SECRET_KEY not set)");
    }

    // ── AppState ─────────────────────────────────────────────────────────────
    let metal_capacity_mb: i64 = env_or("METAL_CAPACITY_MB", "0")
        .parse()
        .unwrap_or(0);

    let state = Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secrets,
        oidc_client_id,
        oidc_device_auth_url,
        oidc_token_url,
        oidc_userinfo_url,
        oidc_revoke_url,
        features,
        metal_capacity_mb,
        http_client,
        victorialogs_url: env_or("VICTORIALOGS_URL", ""),
        stripe,
        stripe_webhook_secret,
        stripe_price_pro,
        stripe_price_team,
    });

    // ── Outbox poller ────────────────────────────────────────────────────────
    tokio::spawn(api::outbox::run(state.db.clone(), state.nats.clone()));

    // ── Billing background tasks ───────────────────────────────────────────
    tokio::spawn(api::billing::usage_subscriber(state.clone()));
    tokio::spawn(api::billing::billing_aggregator(state.clone()));
    tokio::spawn(api::billing::monthly_credit_reset(state.clone()));

    // ── Stuck provisioning watchdog ──────────────────────────────────────────
    // Marks services stuck in 'provisioning' for >10 minutes as 'failed'.
    {
        let pool = state.db.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                match pool.get().await {
                    Ok(db) => {
                        match db
                            .execute(
                                "UPDATE services SET status = 'failed' \
                                 WHERE status = 'provisioning' \
                                   AND created_at < NOW() - INTERVAL '10 minutes' \
                                   AND deleted_at IS NULL",
                                &[],
                            )
                            .await
                        {
                            Ok(n) if n > 0 => tracing::warn!(
                                count = n,
                                "watchdog: marked stuck provisioning services as failed"
                            ),
                            Ok(_)  => {}
                            Err(e) => tracing::error!(error = %e, "watchdog query failed"),
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "watchdog: db pool error"),
                }
            }
        });
    }

    // ── Rate limits ──────────────────────────────────────────────────────────
    let auth_rpm: u32 = env_or("RATE_LIMIT_AUTH_RPM", "10").parse().unwrap_or(10);
    let api_rpm: u32  = env_or("RATE_LIMIT_API_RPM", "60").parse().unwrap_or(60);

    let rate_limits = RateLimitConfig {
        auth:      api::rate_limit::RateLimit::per_minute(auth_rpm),
        protected: api::rate_limit::RateLimit::per_minute(api_rpm),
    };

    tracing::info!(auth_rpm, api_rpm, "rate limits configured");

    let app = build_router(state, rate_limits);

    tracing::info!(%bind, "api listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("api exited cleanly");
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

    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .context("HTTP request to OIDC discovery URL")?
        .error_for_status()
        .context("OIDC discovery returned non-2xx")?
        .json::<OidcDiscovery>()
        .await
        .context("deserializing OIDC discovery document")?;

    Ok(resp)
}

#[derive(serde::Deserialize)]
struct OidcDiscovery {
    device_authorization_endpoint: String,
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
