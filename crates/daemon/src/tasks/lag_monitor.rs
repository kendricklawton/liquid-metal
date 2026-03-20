//! Consumer lag monitor.
//!
//! Periodically checks JetStream consumer lag and warns when it exceeds 50
//! pending messages — indicates the daemon can't keep up with deploy volume.

use common::config::env_or;
use tokio::time::{Duration, interval};

pub fn spawn(
    mut consumer: async_nats::jetstream::consumer::Consumer<async_nats::jetstream::consumer::pull::Config>,
) {
    let lag_interval_secs: u64 = env_or("LAG_MONITOR_INTERVAL_SECS", "30")
        .parse()
        .unwrap_or(30);
    let lag_threshold: u64 = env_or("PROVISION_LAG_THRESHOLD", "50")
        .parse()
        .unwrap_or(50);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(lag_interval_secs));
        loop {
            ticker.tick().await;
            if let Ok(info) = consumer.info().await {
                let pending = info.num_pending;
                if pending > lag_threshold {
                    tracing::warn!(
                        pending,
                        "provision consumer lag > 50 — consider increasing NODE_MAX_CONCURRENT_PROVISIONS or adding nodes"
                    );
                } else if pending > 0 {
                    tracing::debug!(pending, "provision consumer lag");
                }
            }
        }
    });
}
