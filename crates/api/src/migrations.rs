use anyhow::{Context, Result};

refinery::embed_migrations!("../../migrations");

/// Run migrations using an existing pool (kept for internal use).
pub async fn run(pool: &deadpool_postgres::Pool) -> Result<()> {
    let mut conn = pool.get().await?;
    migrations::runner().run_async(&mut **conn).await?;
    Ok(())
}

/// Run migrations using a dedicated connection URL.
///
/// In production, `MIGRATIONS_DATABASE_URL` points to an owner-privilege
/// connection so that DDL never runs under the restricted `lm_app` role.
pub async fn run_with_url(url: &str) -> Result<()> {
    let (mut client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .context("connecting to postgres for migrations")?;

    // Drive the connection in the background; it is dropped when migrations finish.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!(error = %e, "migration connection error");
        }
    });

    let report = migrations::runner()
        .run_async(&mut client)
        .await
        .context("running migrations")?;

    let applied = report.applied_migrations();
    if applied.is_empty() {
        tracing::info!("migrations: up to date");
    } else {
        for m in applied {
            tracing::info!(version = m.version(), name = m.name(), "migration applied");
        }
    }

    Ok(())
}
