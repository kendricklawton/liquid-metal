//! Deleted-workspace orphan sweep.
//!
//! Runs every 60s: finds services still running on this node whose workspace
//! has been soft-deleted. This is a safety net — the API's `delete_workspace`
//! handler publishes `DeprovisionEvent` for each service, but if that fails
//! (NATS down, partial publish) this sweep catches the stragglers.

use std::sync::Arc;

use common::config::env_or;
use common::events::{DeprovisionEvent, Engine, SUBJECT_DEPROVISION};
use tokio::time::{Duration, interval};

pub fn spawn(
    pool: Arc<deadpool_postgres::Pool>,
    node_id: String,
    js: async_nats::jetstream::Context,
) {
    let orphan_sweep_secs: u64 = env_or("ORPHAN_SWEEP_INTERVAL_SECS", "60")
        .parse()
        .unwrap_or(60);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(orphan_sweep_secs));

        loop {
            ticker.tick().await;

            let db = match pool.get().await {
                Ok(d) => d,
                Err(_) => continue,
            };

            let rows = match db
                .query(
                    "UPDATE services s SET status = 'stopped', upstream_addr = NULL \
                     FROM workspaces w \
                     WHERE s.workspace_id = w.id \
                       AND w.deleted_at IS NOT NULL \
                       AND s.node_id = $1 \
                       AND s.status IN ('running', 'provisioning') \
                       AND s.deleted_at IS NULL \
                     RETURNING s.id::text, s.slug, s.engine",
                    &[&node_id],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "deleted-workspace orphan sweep failed");
                    continue;
                }
            };

            for row in &rows {
                let sid: String = row.get("id");
                let slug: String = row.get("slug");
                let eng: String = row.get("engine");

                let engine: Engine = match eng.parse() {
                    Ok(e) => e,
                    Err(_) => {
                        tracing::error!(engine = eng, service_id = sid, "unknown engine in orphan sweep");
                        continue;
                    }
                };

                tracing::info!(service_id = sid, slug, "orphan sweep — workspace deleted, deprovisioning");

                let event = DeprovisionEvent {
                    service_id: sid,
                    slug,
                    engine,
                };
                if let Ok(payload) = serde_json::to_vec(&event) {
                    if let Err(e) = js.publish(SUBJECT_DEPROVISION, payload.into()).await {
                        tracing::warn!(error = %e, "NATS publish deprovision failed (orphan sweep)");
                    }
                }
            }
        }
    });
}
