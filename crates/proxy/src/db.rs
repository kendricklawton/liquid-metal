use anyhow::{Context, Result};
use deadpool_postgres::Pool;

pub struct RouteRecord {
    pub engine: String,
    /// IP:port of the running VM/Wasm handler (None → route to API fallback)
    pub upstream_addr: Option<String>,
}

pub fn build_pool(db_url: &str) -> Result<Pool> {
    let cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(cfg, tls);
        Pool::builder(mgr).max_size(16).build().context("building postgres pool (TLS)")
    } else {
        let mgr = deadpool_postgres::Manager::new(cfg, tokio_postgres::NoTls);
        Pool::builder(mgr).max_size(16).build().context("building postgres pool")
    }
}

/// Look up the upstream address for a service slug.
pub async fn lookup_route(pool: &Pool, slug: &str) -> Result<Option<RouteRecord>> {
    let client = pool.get().await.context("acquiring db connection")?;
    let row = client
        .query_opt(
            "SELECT engine, upstream_addr FROM services WHERE slug = $1 AND deleted_at IS NULL",
            &[&slug],
        )
        .await
        .context("services lookup")?;

    Ok(row.map(|r: tokio_postgres::Row| RouteRecord {
        engine: r.get(0),
        upstream_addr: r.get(1),
    }))
}
