//! Traffic pulse subscriber (batched).
//!
//! Listens for `platform.traffic_pulse` events published by Pingora on every
//! proxied request (debounced 30s per slug). Accumulates slugs over a 5s
//! window and batch-updates `services.last_request_at` in a single query.

use std::collections::HashSet;
use std::sync::Arc;

use common::config::env_or;
use common::events::{TrafficPulseEvent, SUBJECT_TRAFFIC_PULSE};
use futures::StreamExt;
use tokio::time::{Duration, interval};

/// Cap prevents memory exhaustion if slugs arrive faster than flush windows.
const PULSE_PENDING_MAX: usize = 10_000;

pub fn spawn(pool: Arc<deadpool_postgres::Pool>, nc: async_nats::Client) {
    tokio::spawn(async move {
        let mut sub = match nc.subscribe(SUBJECT_TRAFFIC_PULSE).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "pulse subscriber setup failed");
                return;
            }
        };
        let pulse_window: u64 = env_or("PULSE_BATCH_WINDOW_SECS", "5").parse().unwrap_or(5);
        tracing::info!(pulse_window, "traffic pulse subscriber ready (batched)");

        let mut pending: HashSet<String> = HashSet::new();
        let mut flush_tick = interval(Duration::from_secs(pulse_window));

        loop {
            tokio::select! {
                Some(msg) = sub.next() => {
                    if let Ok(event) = serde_json::from_slice::<TrafficPulseEvent>(&msg.payload) {
                        if pending.len() < PULSE_PENDING_MAX {
                            pending.insert(event.slug);
                        }
                    }
                }
                _ = flush_tick.tick() => {
                    if pending.is_empty() { continue; }
                    let slugs: Vec<String> = pending.drain().collect();
                    if let Ok(db) = pool.get().await {
                        let result = db.execute(
                            "UPDATE services SET last_request_at = NOW() \
                             WHERE slug = ANY($1) AND status = 'running' AND deleted_at IS NULL",
                            &[&slugs],
                        ).await;
                        match result {
                            Ok(n) => tracing::debug!(batch_size = slugs.len(), updated = n, "pulse batch flush"),
                            Err(e) => tracing::warn!(error = %e, "pulse batch flush failed"),
                        }
                    }
                }
            }
        }
    });
}
