//! Liquid usage reporter.
//!
//! Drains invocation counters from the liquid registry and publishes
//! `LiquidUsageEvent` for each active Wasm service. Uses a per-service
//! backlog to prevent invocation loss on NATS publish failures.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::config::env_or;
use common::events::{LiquidUsageEvent, SUBJECT_USAGE_LIQUID};
use crate::deprovision;
use tokio::time::{Duration, interval};

/// Safety valve: cap backlog to prevent unbounded growth if NATS is down.
const LIQUID_BACKLOG_MAX: usize = 10_000;

pub fn spawn(liquid_registry: deprovision::LiquidRegistry, nats: Arc<async_nats::Client>) {
    let liquid_usage_secs: u64 = env_or("USAGE_REPORT_INTERVAL_SECS", "60")
        .parse()
        .unwrap_or(60);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(liquid_usage_secs));
        // service_id → accumulated invocations that failed to publish.
        let mut backlog: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        loop {
            ticker.tick().await;
            let reg = liquid_registry.lock().await;
            let mut reported = 0u32;
            for (service_id, handle) in reg.iter() {
                let fresh = handle.invocations.swap(0, Ordering::Relaxed);
                // Merge fresh count with any previously failed backlog.
                let total = backlog.remove(service_id).unwrap_or(0) + fresh;
                if total == 0 {
                    continue;
                }
                let event = LiquidUsageEvent {
                    workspace_id: handle.workspace_id.clone(),
                    service_id: service_id.clone(),
                    invocations: total,
                };
                if let Ok(payload) = serde_json::to_vec(&event) {
                    match nats.publish(SUBJECT_USAGE_LIQUID, payload.into()).await {
                        Ok(_) => {
                            reported += 1;
                        }
                        Err(_) => {
                            // Put back — will be retried next tick.
                            backlog.insert(service_id.clone(), total);
                        }
                    }
                }
            }
            // Prune backlog entries for services that have been deprovisioned.
            backlog.retain(|sid, _| reg.contains_key(sid));

            if backlog.len() > LIQUID_BACKLOG_MAX {
                tracing::error!(
                    backlog = backlog.len(),
                    "usage: liquid backlog exceeded {} — clearing",
                    LIQUID_BACKLOG_MAX,
                );
                backlog.clear();
            }

            // Drop the lock before logging.
            drop(reg);
            if reported > 0 {
                tracing::debug!(count = reported, "usage: reported Liquid invocation ticks");
            }
            if !backlog.is_empty() {
                tracing::warn!(backlog = backlog.len(), "usage: liquid events buffered (NATS unreachable)");
            }
        }
    });
}
