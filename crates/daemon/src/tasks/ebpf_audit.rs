//! eBPF isolation health checker (Linux only).
//!
//! Runs every 30s: asks the kernel whether each active TAP still has a BPF
//! TC egress classifier attached. If a filter is missing, the VM is running
//! without tenant isolation — kill it immediately, no second chances.

use std::sync::Arc;

use common::config::env_or;
use crate::{deprovision, ebpf};
use tokio::time::{Duration, interval};

pub fn spawn(
    pool: Arc<deadpool_postgres::Pool>,
    registry: deprovision::VmRegistry,
    node_id: String,
) {
    let ebpf_check_secs: u64 = env_or("EBPF_CHECK_INTERVAL_SECS", "30")
        .parse()
        .unwrap_or(30);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(ebpf_check_secs));
        tracing::info!(interval_secs = ebpf_check_secs, "eBPF isolation health checker started");

        loop {
            ticker.tick().await;

            let missing = ebpf::audit_filters().await;
            if missing.is_empty() {
                continue;
            }

            tracing::error!(
                count = missing.len(),
                taps  = ?missing,
                "CRITICAL: eBPF isolation breach detected — killing unisolated VMs"
            );

            let db = match pool.get().await {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!(error = %e, "cannot get DB connection to handle isolation breach");
                    continue;
                }
            };

            for tap_name in &missing {
                // Look up the service by TAP name on this node.
                let row = db
                    .query_opt(
                        "SELECT id, fc_pid FROM services \
                         WHERE tap_name = $1 AND node_id = $2 \
                           AND status = 'running' AND deleted_at IS NULL",
                        &[tap_name, &node_id],
                    )
                    .await
                    .ok()
                    .flatten();

                let Some(row) = row else {
                    // TAP is in our active map but no matching running service in DB.
                    // Detach the stale entry.
                    ebpf::detach(tap_name);
                    continue;
                };

                let svc_id: uuid::Uuid = row.get("id");
                let fc_pid: Option<i32> = row.get("fc_pid");

                // SIGKILL the Firecracker process immediately — every packet
                // from this VM is a potential isolation violation.
                if let Some(pid) = fc_pid {
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                    tracing::error!(
                        service_id = %svc_id,
                        tap = tap_name.as_str(),
                        fc_pid = pid,
                        "SIGKILL sent to unisolated VM"
                    );
                }

                // Mark crashed in DB and clear upstream so proxy stops routing.
                db.execute(
                    "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                     WHERE id = $1 AND deleted_at IS NULL",
                    &[&svc_id],
                )
                .await
                .ok();

                // Remove from registry so the crash watcher doesn't double-process.
                registry.lock().await.remove(&svc_id.to_string());

                // Clean up our eBPF tracking entry.
                ebpf::detach(tap_name);
            }
        }
    });
}
