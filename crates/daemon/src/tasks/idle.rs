//! Idle checker.
//!
//! Runs every 60s:
//! - Metal: finds idle services and deprovisions them (full teardown).
//! - Liquid: finds idle services and stops the Wasm shim (keeps artifacts
//!   on disk for fast wake from scale-to-zero).

use std::sync::Arc;

use common::config::env_or;
use common::events::{
    DeprovisionEvent, Engine, RouteRemovedEvent, SUBJECT_DEPROVISION, SUBJECT_ROUTE_REMOVED,
};
use crate::deprovision;
use tokio::time::{Duration, interval};

pub fn spawn(
    pool: Arc<deadpool_postgres::Pool>,
    node_id: String,
    js: async_nats::jetstream::Context,
    nats: Arc<async_nats::Client>,
    liquid_registry: deprovision::LiquidRegistry,
    idle_timeout_secs: i64,
) {
    let idle_check_secs: u64 = env_or("IDLE_CHECK_INTERVAL_SECS", "60")
        .parse()
        .unwrap_or(60);

    // Liquid has its own idle timeout (default 300s = 5 min).
    // Set LIQUID_IDLE_TIMEOUT_SECS=0 to disable Liquid scale-to-zero.
    let liquid_idle_timeout_secs: i64 = env_or("LIQUID_IDLE_TIMEOUT_SECS", "300")
        .parse()
        .unwrap_or(300);

    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(idle_check_secs));

        loop {
            ticker.tick().await;

            let db = match pool.get().await {
                Ok(d) => d,
                Err(_) => continue,
            };

            // ── Metal idle (full deprovision via NATS) ──────────────────
            if idle_timeout_secs > 0 {
                let rows = match db
                    .query(
                        "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                         WHERE engine = 'metal' AND status = 'running' \
                           AND node_id = $1 AND deleted_at IS NULL \
                           AND COALESCE(last_request_at, created_at) \
                               < NOW() - interval '1 second' * $2 \
                         RETURNING id::text, slug",
                        &[&node_id, &idle_timeout_secs],
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "idle checker query failed (metal)");
                        vec![]
                    }
                };

                for row in rows {
                    let sid: String = row.get("id");
                    let slug: String = row.get("slug");
                    tracing::info!(
                        service_id = sid,
                        slug,
                        idle_timeout_secs,
                        "metal idle timeout — deprovisioning"
                    );
                    let event = DeprovisionEvent {
                        service_id: sid.clone(),
                        slug,
                        engine: Engine::Metal,
                    };
                    let payload = match serde_json::to_vec(&event) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(service_id = sid, error = %e, "failed to serialize DeprovisionEvent");
                            continue;
                        }
                    };

                    let mut published = false;
                    for attempt in 1..=3u32 {
                        match js.publish(SUBJECT_DEPROVISION, payload.clone().into()).await {
                            Ok(_) => {
                                published = true;
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(
                                    service_id = sid, attempt,
                                    error = %e, "idle deprovision publish failed — retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(500 * attempt as u64))
                                    .await;
                            }
                        }
                    }
                    if !published {
                        tracing::error!(
                            service_id = sid,
                            "idle deprovision publish failed after 3 attempts — reverting to running"
                        );
                        let _ = db
                            .execute(
                                "UPDATE services SET status = 'running' \
                                 WHERE id = $1::uuid AND status = 'stopped' AND deleted_at IS NULL",
                                &[&sid],
                            )
                            .await;
                    }
                }
            }

            // ── Liquid idle (lightweight teardown — keep artifacts) ──────
            // Liquid services scale to zero by dropping the in-process Wasm
            // shim. The compiled module + metadata stay on disk for fast wake.
            if liquid_idle_timeout_secs > 0 {
                let rows = match db
                    .query(
                        "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                         WHERE engine = 'liquid' AND status = 'running' \
                           AND node_id = $1 AND deleted_at IS NULL \
                           AND COALESCE(last_request_at, created_at) \
                               < NOW() - interval '1 second' * $2 \
                         RETURNING id::text, slug",
                        &[&node_id, &liquid_idle_timeout_secs],
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(error = %e, "idle checker query failed (liquid)");
                        vec![]
                    }
                };

                for row in &rows {
                    let sid: String = row.get("id");
                    let slug: String = row.get("slug");
                    tracing::info!(
                        service_id = sid,
                        slug,
                        liquid_idle_timeout_secs,
                        "liquid idle timeout — scaling to zero"
                    );

                    // Drop the Wasm shim from the in-process registry.
                    // The listener and compiled module handle are dropped,
                    // freeing memory. Artifacts stay on disk.
                    liquid_registry.lock().await.remove(&sid);

                    // Evict from proxy cache
                    if let Ok(payload) =
                        serde_json::to_vec(&RouteRemovedEvent { slug: slug.clone() })
                    {
                        if let Err(e) =
                            nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await
                        {
                            tracing::warn!(
                                error = %e,
                                "NATS publish route_removed failed (liquid idle)"
                            );
                        }
                    }
                }

                if !rows.is_empty() {
                    tracing::info!(
                        count = rows.len(),
                        "liquid services scaled to zero — will wake on next request"
                    );
                }
            }
        }
    });
}
