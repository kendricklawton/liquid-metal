use std::sync::Arc;

use anyhow::{Context, Result};

use api::{AppState, RateLimitConfig, build_router};
use common::{
    Features,
    config::{env_or, require_env},
};

#[tokio::main]
async fn main() -> Result<()> {
    let startup = std::time::Instant::now();
    let _tracer_provider = common::config::init_tracing("api");
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("rustls CryptoProvider already installed");

    // ── Prometheus metrics server ─────────────────────────────────────────────
    let metrics_bind = env_or("METRICS_BIND_ADDR", "0.0.0.0:9090");
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("installing metrics recorder")?;
    {
        let listener = tokio::net::TcpListener::bind(&metrics_bind).await?;
        tracing::info!(%metrics_bind, "metrics server listening");
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/metrics",
                axum::routing::get(move || {
                    let h = metrics_handle.clone();
                    async move { h.render() }
                }),
            );
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!(error = %e, "metrics server exited with error");
            }
        });
    }

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

    // ── OIDC configuration (Zitadel discovery) ────────────────────────────────
    let oidc_issuer = require_env("OIDC_ISSUER")?;
    let oidc_cli_client_id = require_env("OIDC_CLI_CLIENT_ID")?;

    let disc = oidc_discover(&oidc_issuer)
        .await
        .with_context(|| format!("OIDC discovery failed for {oidc_issuer}"))?;
    tracing::info!(%oidc_issuer, "OIDC endpoints discovered");

    let oidc_device_auth_url = disc.device_authorization_endpoint;
    let oidc_token_url       = disc.token_endpoint;
    let oidc_userinfo_url    = disc.userinfo_endpoint;
    let oidc_revoke_url      = disc.revocation_endpoint;

    let nats_url    = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let bind        = env_or("BIND_ADDR", "0.0.0.0:7070");
    let bucket      = env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts");

    // ── Postgres pool ────────────────────────────────────────────────────────
    let pool_size: usize = env_or("DATABASE_POOL_SIZE", "16").parse().unwrap_or(16);
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let pool = if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
        deadpool_postgres::Pool::builder(mgr).max_size(pool_size).build()
            .context("building postgres pool (TLS)")?
    } else {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(mgr).max_size(pool_size).build()
            .context("building postgres pool")?
    };
    tracing::info!(pool_size, "postgres pool configured");

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
    tracing::info!(%nats_url, "NATS JetStream connected");

    // ── S3 client ────────────────────────────────────────────────────────────
    let s3 = api::storage::build_client()?;
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

    // ── Resource quotas ─────────────────────────────────────────────────────
    let default_quota = common::events::ResourceQuota::from_env();
    tracing::info!(
        disk_read_bps  = ?default_quota.disk_read_bps,
        disk_write_bps = ?default_quota.disk_write_bps,
        disk_read_iops = ?default_quota.disk_read_iops,
        disk_write_iops = ?default_quota.disk_write_iops,
        net_ingress_kbps = ?default_quota.net_ingress_kbps,
        net_egress_kbps  = ?default_quota.net_egress_kbps,
        "default resource quotas"
    );

    // ── Vault (secret management) ────────────────────────────────────────────
    let vault = Arc::new(
        common::vault::VaultClient::from_env()
            .context("initializing Vault client — check VAULT_ADDR and VAULT_TOKEN")?,
    );
    vault.health_check().await.context("vault health check failed — is Vault running and unsealed?")?;

    // ── AppState ─────────────────────────────────────────────────────────────
    let state = Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secrets,
        oidc_cli_client_id,
        oidc_device_auth_url,
        oidc_token_url,
        oidc_userinfo_url,
        oidc_revoke_url,
        default_quota,
        features,
        http_client,
        victorialogs_url: env_or("VICTORIALOGS_URL", ""),
        vault,
        stripe,
        stripe_webhook_secret,
        stripe_price_pro,
        stripe_price_team,
    });

    // ── Outbox poller ────────────────────────────────────────────────────────
    tokio::spawn(api::outbox::run(state.db.clone(), state.nats.clone()));

    // ── TLS cert manager (ACME HTTP-01, renewal) ─────────────────────────────
    tokio::spawn(api::cert_manager::run(state.clone()));

    // ── Billing background tasks ───────────────────────────────────────────
    tokio::spawn(api::billing::usage_subscriber(state.clone()));
    tokio::spawn(api::billing::billing_aggregator(state.clone()));
    tokio::spawn(api::billing::monthly_credit_reset(state.clone()));

    // ── Stuck provisioning watchdog ──────────────────────────────────────────
    // Marks services stuck in 'provisioning' for longer than the threshold as 'failed'.
    {
        let watchdog_interval_secs: u64 = env_or("WATCHDOG_INTERVAL_SECS", "60").parse().unwrap_or(60);
        let watchdog_timeout_mins: i32  = env_or("PROVISIONING_TIMEOUT_MINS", "10").parse().unwrap_or(10);
        tracing::info!(watchdog_interval_secs, watchdog_timeout_mins, "watchdog configured");

        let pool = state.db.clone();
        tokio::spawn(async move {
            let timeout_str = watchdog_timeout_mins.to_string();
            let query = "UPDATE services SET status = 'failed' \
                 WHERE status = 'provisioning' \
                   AND created_at < NOW() - ($1 || ' minutes')::interval \
                   AND deleted_at IS NULL";
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(watchdog_interval_secs));
            loop {
                interval.tick().await;
                match pool.get().await {
                    Ok(db) => {
                        match db.execute(query, &[&timeout_str]).await {
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

    let version = env!("CARGO_PKG_VERSION");
    let git_sha = env!("GIT_SHA");
    let startup_ms = startup.elapsed().as_millis();
    tracing::info!(%bind, %version, %git_sha, startup_ms, "api listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;

    let shutdown_instant = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
    let shutdown_instant_clone = shutdown_instant.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            *shutdown_instant_clone.lock().unwrap() = Some(std::time::Instant::now());
        })
        .await?;
    let drain_ms = shutdown_instant
        .lock()
        .unwrap()
        .map(|t| t.elapsed().as_millis())
        .unwrap_or(0);
    tracing::info!(drain_ms, "api exited cleanly — in-flight requests drained");
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

    // Validate discovered endpoints are well-formed URLs.
    validate_oidc_url(&disc.device_authorization_endpoint, "device_authorization_endpoint")?;
    validate_oidc_url(&disc.token_endpoint, "token_endpoint")?;
    validate_oidc_url(&disc.userinfo_endpoint, "userinfo_endpoint")?;
    if let Some(rev) = &disc.revocation_endpoint {
        validate_oidc_url(rev, "revocation_endpoint")?;
    }

    Ok(disc)
}

/// Validate that `raw` is a well-formed HTTP(S) URL. Fails fast at startup
/// rather than letting a bad URL surface as a confusing reqwest error when a
/// user tries to log in.
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
