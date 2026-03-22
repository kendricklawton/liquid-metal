//! Liquid registry sweep — safety net for stale entries.
//!
//! Runs every 5 minutes (configurable via `REGISTRY_SWEEP_INTERVAL_SECS`).
//! Compares in-memory `LiquidRegistry` entries against the DB and removes
//! any whose service is no longer `running` (e.g. stopped, suspended,
//! deleted, or missing entirely).
//!
//! This catches entries that linger if a deprovision event was lost, if
//! the idle timeout is disabled, or if a service was deleted directly in
//! the database without going through the normal deprovision flow.

use std::sync::Arc;

use common::config::env_or;
use crate::deprovision;
use tokio::time::{Duration, interval};

pub fn spawn(
    pool: Arc<deadpool_postgres::Pool>,
    node_id: String,
    liquid_registry: deprovision::LiquidRegistry,
) {
    let sweep_secs: u64 = env_or("REGISTRY_SWEEP_INTERVAL_SECS", "300")
        .parse()
        .unwrap_or(300);

    if sweep_secs == 0 {
        tracing::info!("liquid registry sweep disabled");
        return;
    }

    tracing::info!(interval_secs = sweep_secs, "liquid registry sweep configured");

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(sweep_secs));

        loop {
            ticker.tick().await;

            let service_ids: Vec<String> = {
                let reg = liquid_registry.lock().await;
                if reg.is_empty() {
                    continue;
                }
                reg.keys().cloned().collect()
            };

            let db = match pool.get().await {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Query which of these service_ids are still running on this node.
            // Any registry entry not in this result set is stale.
            let uuids: Vec<uuid::Uuid> = service_ids
                .iter()
                .filter_map(|s| s.parse().ok())
                .collect();

            let running_rows = match db
                .query(
                    "SELECT id::text AS sid FROM services \
                     WHERE id = ANY($1) \
                       AND node_id = $2 \
                       AND status = 'running' \
                       AND deleted_at IS NULL",
                    &[&uuids, &node_id],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "liquid registry sweep: DB query failed");
                    continue;
                }
            };

            let running: std::collections::HashSet<String> = running_rows
                .iter()
                .map(|r| r.get::<_, String>("sid"))
                .collect();

            // Remove stale entries.
            let mut removed = 0u32;
            {
                let mut reg = liquid_registry.lock().await;
                reg.retain(|sid, _| {
                    if running.contains(sid) {
                        true
                    } else {
                        removed += 1;
                        false
                    }
                });
            }

            if removed > 0 {
                tracing::info!(
                    removed,
                    "liquid registry sweep: removed stale entries"
                );
            }
        }
    });
}
