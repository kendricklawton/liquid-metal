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

/// Poll interval — short enough to feel instant, long enough not to hammer the DB.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum rows to publish in a single poll pass.
const BATCH_SIZE: i64 = 50;

pub async fn run(pool: Pool, js: jetstream::Context) {
    let mut cycles_since_cleanup: u32 = 0;
    loop {
        if let Err(e) = poll_once(&pool, &js).await {
            tracing::error!(error = %e, "outbox poll failed");
        }

        // Run stale cleanup every ~60 polls (~60s at 1s interval).
        cycles_since_cleanup += 1;
        if cycles_since_cleanup >= 60 {
            cycles_since_cleanup = 0;
            if let Err(e) = cleanup_stale(&pool).await {
                tracing::error!(error = %e, "outbox stale cleanup failed");
            }
        }

        sleep(POLL_INTERVAL).await;
    }
}

/// Delete outbox rows older than STALE_THRESHOLD. These are events that
/// could never be published (e.g. extended NATS outage). The corresponding
/// service may have been deleted by the user in the meantime — replaying
/// these stale events would provision ghost services.
async fn cleanup_stale(pool: &Pool) -> Result<()> {
    let db = pool.get().await?;
    let n = db
        .execute(
            "DELETE FROM outbox WHERE created_at < NOW() - interval '30 minutes'",
            &[],
        )
        .await?;
    if n > 0 {
        tracing::warn!(count = n, "outbox: purged stale rows older than 30m");
    }
    Ok(())
}

async fn poll_once(pool: &Pool, js: &jetstream::Context) -> Result<()> {
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
            &[&BATCH_SIZE],
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
