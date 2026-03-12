use anyhow::{bail, Context, Result};

refinery::embed_migrations!("../../migrations");

/// The highest migration version embedded in this binary.
/// Used by `verify()` to ensure the DB schema matches the code.
pub fn expected_version() -> u32 {
    migrations::runner()
        .get_migrations()
        .iter()
        .map(|m| m.version())
        .max()
        .unwrap_or(0)
}

/// Run migrations using a dedicated connection URL.
///
/// Called via `--migrate` flag or the `migrate` Nomad batch job.
/// In production, `MIGRATIONS_DATABASE_URL` points to an owner-privilege
/// connection so that DDL never runs under the restricted `lm_app` role.
pub async fn run_with_url(url: &str) -> Result<()> {
    let mut client = if let Some(tls) = common::config::pg_tls()? {
        let (client, conn) = tokio_postgres::connect(url, tls)
            .await
            .context("postgres TLS connect for migrations")?;
        tokio::spawn(async move { if let Err(e) = conn.await { tracing::error!(error = %e, "migration conn error"); } });
        client
    } else {
        let (client, conn) = tokio_postgres::connect(url, tokio_postgres::NoTls)
            .await
            .context("postgres connect for migrations")?;
        tokio::spawn(async move { if let Err(e) = conn.await { tracing::error!(error = %e, "migration conn error"); } });
        client
    };

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

/// Verify that the DB schema version matches this binary's embedded migrations.
/// Returns Ok if up to date, Err if migrations are pending.
///
/// This replaces running migrations on every API startup — migrations should
/// be applied by the `migrate` Nomad batch job (or `--migrate` flag) before
/// the API service starts.
pub async fn verify(pool: &deadpool_postgres::Pool) -> Result<()> {
    let expected = expected_version();

    let db = pool.get().await.context("db pool for migration verify")?;

    // refinery_schema_history may not exist if migrations have never run.
    let row = db
        .query_opt(
            "SELECT version FROM refinery_schema_history ORDER BY version DESC LIMIT 1",
            &[],
        )
        .await;

    let db_version: u32 = match row {
        Ok(Some(r)) => {
            let v: i32 = r.get(0);
            v as u32
        }
        Ok(None) => 0,
        Err(_) => {
            // Table doesn't exist — no migrations have ever run.
            0
        }
    };

    if db_version < expected {
        bail!(
            "database schema is at V{} but this binary expects V{}. \
             Run migrations first: `nomad job dispatch migrate` or `api --migrate`",
            db_version,
            expected,
        );
    }

    tracing::info!(db_version, expected, "migration version check passed");
    Ok(())
}
