use anyhow::{Context, Result};
use api::{AppState, build_router};
use common::config::{env_or, require_env};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "api=info".into()),
        )
        .init();

    let db_url          = require_env("DATABASE_URL")?;
    let nats_url        = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let bind            = env_or("BIND_ADDR", "0.0.0.0:7070");
    let bucket          = env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts");
    let internal_secret = env_or("INTERNAL_SECRET", "");

    // Postgres pool
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let mgr  = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(mgr)
        .max_size(16)
        .build()
        .context("building postgres pool")?;

    // Run migrations before accepting traffic
    api::migrations::run(&pool).await.context("running migrations")?;
    tracing::info!("migrations applied");

    // NATS JetStream
    let nc = async_nats::connect(&nats_url)
        .await
        .context("connecting to NATS")?;
    let js = async_nats::jetstream::new(nc.clone());
    api::nats::ensure_stream(&js).await?;

    // Object Storage (S3-compatible — Vultr Object Storage)
    let s3 = api::storage::build_client().await;

    let state = Arc::new(AppState {
        db: pool,
        nats: js,
        nats_client: nc,
        s3,
        bucket,
        internal_secret,
    });

    let app = build_router(state);

    tracing::info!(%bind, "machinename-api listening (REST + ConnectRPC)");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
