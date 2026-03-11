use anyhow::{Context, Result};
use api::{AppState, build_router};
use common::{Features, config::{env_or, require_env}};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "api=info".into()))
        .init();

    // ── Feature flags ────────────────────────────────────────────────────────
    let features = Features::from_env();
    features.log_summary();

    // ── Required configuration ───────────────────────────────────────────────
    let db_url            = require_env("DATABASE_URL")?;
    let internal_secret   = require_env("INTERNAL_SECRET")?;
    let zitadel_domain    = require_env("ZITADEL_DOMAIN")?;
    let zitadel_client_id = require_env("ZITADEL_CLIENT_ID")?;

    // ── Optional configuration (with sane defaults) ──────────────────────────
    let migrate_url = env_or("MIGRATIONS_DATABASE_URL", &db_url);
    let nats_url    = env_or("NATS_URL",   "nats://127.0.0.1:4222");
    let bind        = env_or("BIND_ADDR",  "0.0.0.0:7070");
    let bucket      = env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts");

    // ── Run migrations (owner-privilege connection) ──────────────────────────
    api::migrations::run_with_url(&migrate_url)
        .await
        .context("running migrations")?;

    // ── App Postgres pool (limited-privilege connection) ─────────────────────
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(mgr)
        .max_size(16)
        .build()
        .context("building postgres pool")?;

    // ── NATS JetStream ────────────────────────────────────────────────────────
    let nc = async_nats::connect(&nats_url)
        .await
        .context("connecting to NATS")?;
    let js = async_nats::jetstream::new(nc.clone());
    api::nats::ensure_stream(&js).await?;

    // ── Object Storage (S3-compatible) ────────────────────────────────────────
    let s3 = api::storage::build_client().await;

    let state = Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secret,
        zitadel_domain,
        zitadel_client_id,
        features,
    });

    let app = build_router(state);

    tracing::info!(%bind, "api listening (REST + ConnectRPC)");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
