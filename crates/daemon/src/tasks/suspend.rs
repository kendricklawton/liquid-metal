//! Suspend consumer.
//!
//! Listens for `SuspendEvent` (workspace balance depleted) and deprovisions
//! all running services for that workspace on this node.
//!
//! Graceful drain: routes are evicted FIRST (proxy stops sending new
//! requests), then we wait `SUSPEND_DRAIN_SECS` for in-flight requests to
//! complete before killing VMs / removing Wasm handlers.

use std::sync::Arc;

use common::config::env_or;
use common::events::{
    RouteRemovedEvent, SuspendEvent, SUBJECT_ROUTE_REMOVED, SUBJECT_SUSPEND,
};
use crate::{deprovision, provision};
use futures::StreamExt;

pub fn spawn(
    pool: Arc<deadpool_postgres::Pool>,
    node_id: String,
    nats: Arc<async_nats::Client>,
    registry: deprovision::VmRegistry,
    liquid_registry: deprovision::LiquidRegistry,
    cfg: Arc<provision::ProvisionConfig>,
) {
    let drain_secs: u64 = env_or("SUSPEND_DRAIN_SECS", "30").parse().unwrap_or(30);
    tokio::spawn(async move {
        let mut sub = match nats.subscribe(SUBJECT_SUSPEND).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "suspend subscriber setup failed");
                return;
            }
        };
        tracing::info!(drain_secs, "suspend subscriber ready");
        while let Some(msg) = sub.next().await {
            let event: SuspendEvent = match serde_json::from_slice(&msg.payload) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to parse SuspendEvent");
                    continue;
                }
            };
            tracing::warn!(workspace_id = event.workspace_id, reason = event.reason, "suspending workspace services");
            let db = match pool.get().await {
                Ok(d) => d,
                Err(_) => continue,
            };
            let wid: uuid::Uuid = match event.workspace_id.parse() {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid workspace_id in SuspendEvent");
                    continue;
                }
            };

            // Phase 1: Mark as draining and evict routes.
            let rows = match db
                .query(
                    "UPDATE services SET status = 'draining' \
                     WHERE workspace_id = $1 AND node_id = $2 \
                       AND status = 'running' AND deleted_at IS NULL \
                     RETURNING id::text, slug, engine",
                    &[&wid, &node_id],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "suspend drain query failed");
                    continue;
                }
            };

            // Evict routes immediately — proxy drops new requests.
            for row in &rows {
                let slug: String = row.get("slug");
                if let Ok(payload) = serde_json::to_vec(&RouteRemovedEvent { slug }) {
                    if let Err(e) = nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                        tracing::warn!(error = %e, "NATS publish route_removed failed (suspend handler)");
                    }
                }
            }

            if rows.is_empty() {
                continue;
            }

            // Phase 2: Drain — wait for in-flight requests to finish.
            tracing::info!(
                workspace_id = event.workspace_id,
                count = rows.len(),
                drain_secs,
                "draining in-flight requests before suspend"
            );
            tokio::time::sleep(std::time::Duration::from_secs(drain_secs)).await;

            // Phase 3: Hard suspend — kill VMs, remove Wasm handlers, clear DB.
            if let Err(e) = db
                .execute(
                    "UPDATE services SET status = 'suspended', upstream_addr = NULL \
                     WHERE workspace_id = $1 AND node_id = $2 \
                       AND status = 'draining' AND deleted_at IS NULL",
                    &[&wid, &node_id],
                )
                .await
            {
                tracing::error!(error = %e, "suspend finalize query failed");
            }

            for row in &rows {
                let sid: String = row.get("id");
                let eng: String = row.get("engine");

                if eng == "metal" {
                    let handle = registry.lock().await.remove(&sid);
                    if let Some(h) = handle {
                        deprovision::metal(&sid, h, &cfg.artifact_dir).await;
                    }
                } else if eng == "liquid" {
                    liquid_registry.lock().await.remove(&sid);
                }
            }

            tracing::warn!(workspace_id = event.workspace_id, count = rows.len(), "suspended services (drain complete)");
        }
    });
}
