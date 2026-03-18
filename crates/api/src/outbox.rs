//! Transactional outbox poller.
//!
//! Spawned as a background Tokio task on API startup. Polls the `outbox` table
//! every second, publishes each row to NATS JetStream, and deletes on ack.
//!
//! Because the deploy handler inserts into `outbox` in the same DB transaction
//! as the `services` row, publish and insert are atomic — either both land or
//! neither does. NATS failures no longer leave orphaned `provisioning` services.

use std::time::Duration;

use anyhow::Result;
use async_nats::jetstream;
use deadpool_postgres::Pool;
use tokio::time::sleep;

pub async fn run(pool: Pool, js: jetstream::Context) {
    let poll_secs: u64 = common::config::env_or("OUTBOX_POLL_SECS", "1").parse().unwrap_or(1);
    let batch_size: i64 = common::config::env_or("OUTBOX_BATCH_SIZE", "50").parse().unwrap_or(50);
    let stale_mins: i32 = common::config::env_or("OUTBOX_STALE_MINS", "30").parse().unwrap_or(30);
    let cleanup_every = if poll_secs > 0 { (60 / poll_secs).max(1) as u32 } else { 60 };

    tracing::info!(poll_secs, batch_size, stale_mins, "outbox poller configured");

    let poll_interval = Duration::from_secs(poll_secs);
    let mut cycles_since_cleanup: u32 = 0;
    loop {
        if let Err(e) = poll_once(&pool, &js, batch_size).await {
            tracing::error!(error = %e, "outbox poll failed");
        }

        cycles_since_cleanup += 1;
        if cycles_since_cleanup >= cleanup_every {
            cycles_since_cleanup = 0;
            if let Err(e) = cleanup_stale(&pool, stale_mins).await {
                tracing::error!(error = %e, "outbox stale cleanup failed");
            }
            // Emit outbox lag metrics — early warning for NATS outages.
            if let Err(e) = emit_lag_metrics(&pool).await {
                tracing::debug!(error = %e, "outbox lag metrics failed");
            }
        }

        sleep(poll_interval).await;
    }
}

/// Delete outbox rows older than STALE_THRESHOLD. These are events that
/// could never be published (e.g. extended NATS outage). The corresponding
/// service may have been deleted by the user in the meantime — replaying
/// these stale events would provision ghost services.
async fn cleanup_stale(pool: &Pool, stale_mins: i32) -> Result<()> {
    let db = pool.get().await?;
    let n = db.execute(
        "DELETE FROM outbox WHERE created_at < NOW() - ($1 || ' minutes')::interval",
        &[&stale_mins.to_string()],
    ).await?;
    if n > 0 {
        tracing::warn!(count = n, stale_mins, "outbox: purged stale rows");
    }
    Ok(())
}

async fn poll_once(pool: &Pool, js: &jetstream::Context, batch_size: i64) -> Result<()> {
    let mut db = pool.get().await?;

    // Use a transaction so FOR UPDATE SKIP LOCKED works correctly across
    // multiple API instances. The lock on each row is held for the duration
    // of the transaction — a second poller will skip locked rows entirely
    // rather than re-publishing the same event to NATS.
    let txn = db.build_transaction().start().await?;

    let rows = txn
        .query(
            "SELECT id, subject, payload \
             FROM outbox \
             ORDER BY created_at ASC \
             LIMIT $1 \
             FOR UPDATE SKIP LOCKED",
            &[&batch_size],
        )
        .await?;

    if rows.is_empty() {
        txn.rollback().await?;
        return Ok(());
    }

    let mut published_ids: Vec<uuid::Uuid> = Vec::with_capacity(rows.len());

    for row in &rows {
        let id:      uuid::Uuid        = row.get("id");
        let subject: String            = row.get("subject");
        let payload: serde_json::Value = row.get("payload");

        let bytes = serde_json::to_vec(&payload)?;

        // Publish and await JetStream ack. If NATS is down this returns an
        // error — we bail out of the loop, the transaction is rolled back,
        // locks are released, and the rows are retried next poll.
        js.publish(subject.clone(), bytes.into())
            .await?
            .await?;

        published_ids.push(id);
        tracing::debug!(subject, %id, "outbox event published");
    }

    // Batch delete all published rows in a single round-trip.
    txn.execute(
        "DELETE FROM outbox WHERE id = ANY($1)",
        &[&published_ids],
    ).await?;

    txn.commit().await?;
    Ok(())
}

/// Publish Prometheus gauges for outbox depth and oldest row age.
/// Runs on the same cadence as stale cleanup (~once per minute).
async fn emit_lag_metrics(pool: &Pool) -> Result<()> {
    let db = pool.get().await?;
    let row = db
        .query_one(
            "SELECT COUNT(*)::bigint AS depth, \
             COALESCE(EXTRACT(EPOCH FROM (NOW() - MIN(created_at)))::bigint, 0) AS oldest_age_secs \
             FROM outbox",
            &[],
        )
        .await?;

    let depth: i64 = row.get("depth");
    let oldest_age_secs: i64 = row.get("oldest_age_secs");

    metrics::gauge!("outbox_depth").set(depth as f64);
    metrics::gauge!("outbox_oldest_age_seconds").set(oldest_age_secs as f64);

    Ok(())
}
