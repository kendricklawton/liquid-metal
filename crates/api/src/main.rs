mod migrations;
mod nats;
mod routes;

use anyhow::{Context, Result};
use axum::{Router, routing::get};
use common::config::{env_or, require_env};
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

pub struct AppState {
    pub db: deadpool_postgres::Pool,
    pub nats: async_nats::jetstream::Context,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "api=info".into()),
        )
        .init();

    let db_url  = require_env("DATABASE_URL")?;
    let nats_url = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let bind     = env_or("BIND_ADDR", "0.0.0.0:3000");

    // Postgres pool
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let mgr  = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = deadpool_postgres::Pool::builder(mgr)
        .max_size(16)
        .build()
        .context("building postgres pool")?;

    // Run migrations before accepting traffic
    migrations::run(&pool).await.context("running migrations")?;
    tracing::info!("migrations applied");

    // NATS JetStream
    let nc = async_nats::connect(&nats_url)
        .await
        .context("connecting to NATS")?;
    let js = async_nats::jetstream::new(nc);
    nats::ensure_stream(&js).await?;

    let state = Arc::new(AppState { db: pool, nats: js });

    let app = Router::new()
        .route("/healthz", get(routes::health))
        .nest("/services", routes::services_router())
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    tracing::info!(%bind, "machinename-api listening");
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
