//! VM crash watcher (Linux only).
//!
//! Every 10s, checks if tracked Firecracker PIDs are still alive via
//! `kill(pid, 0)`. If a process has exited, marks the service as 'crashed',
//! clears `upstream_addr`, publishes `RouteRemovedEvent` + `ServiceCrashedEvent`,
//! and triggers cleanup.

use std::sync::Arc;

use common::config::env_or;
use common::events::{
    RouteRemovedEvent, ServiceCrashedEvent, SUBJECT_ROUTE_REMOVED, SUBJECT_SERVICE_CRASHED,
};
use crate::{deprovision, provision};
use tokio::time::{Duration, interval};

pub fn spawn(
    registry: deprovision::VmRegistry,
    pool: Arc<deadpool_postgres::Pool>,
    node_id: String,
    nats: Arc<async_nats::Client>,
    cfg: Arc<provision::ProvisionConfig>,
) {
    let crash_check_secs: u64 = env_or("VM_CRASH_CHECK_INTERVAL_SECS", "10")
        .parse()
        .unwrap_or(10);
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(crash_check_secs));
        loop {
            ticker.tick().await;
            // Collect crashed VMs and remove from registry atomically.
            // This prevents a race where a new provision for the same
            // service_id completes between detection and cleanup — the
            // new handle would have a different fc_pid and must not be
            // touched by this watchdog cycle.
            let crashed: Vec<(String, deprovision::VmHandle, Option<i32>)> = {
                let mut reg = registry.lock().await;
                let mut dead: Vec<(String, Option<i32>)> = Vec::new();
                for (id, h) in reg.iter() {
                    let pid = h.fc_pid as libc::pid_t;
                    let mut status: libc::c_int = 0;
                    let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                    if ret > 0 {
                        // Direct child exited — extract exit code.
                        let code = if libc::WIFEXITED(status) {
                            Some(libc::WEXITSTATUS(status))
                        } else if libc::WIFSIGNALED(status) {
                            Some(128 + libc::WTERMSIG(status))
                        } else {
                            None
                        };
                        dead.push((id.clone(), code));
                    } else if ret == 0 {
                        // Still running (direct child). Skip.
                    } else {
                        // waitpid failed (ECHILD) — not our direct child (jailer case).
                        // Fall back to /proc existence check.
                        if !std::path::Path::new(&format!("/proc/{}", h.fc_pid)).exists() {
                            dead.push((id.clone(), None));
                        }
                    }
                }
                dead.into_iter()
                    .filter_map(|(id, code)| reg.remove(&id).map(|h| (id, h, code)))
                    .collect()
            };
            for (service_id, handle, exit_code) in &crashed {
                tracing::error!(
                    service_id,
                    fc_pid = handle.fc_pid,
                    exit_code = ?exit_code,
                    "VM crash detected — process no longer alive"
                );
                // Update DB: mark crashed + clear upstream.
                if let Ok(db) = pool.get().await {
                    let svc_id: uuid::Uuid = match service_id.parse() {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    let row = db
                        .query_opt(
                            "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                             WHERE id = $1 AND node_id = $2 AND status = 'running' AND deleted_at IS NULL \
                             RETURNING slug",
                            &[&svc_id, &node_id],
                        )
                        .await
                        .ok()
                        .flatten();

                    if let Some(row) = row {
                        let slug: String = row.get("slug");
                        // Evict from proxy cache.
                        let removed = RouteRemovedEvent { slug: slug.clone() };
                        if let Ok(payload) = serde_json::to_vec(&removed) {
                            if let Err(e) =
                                nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await
                            {
                                tracing::warn!(error = %e, "NATS publish route_removed failed (crash watcher)");
                            }
                        }
                        // Publish crash event for observability.
                        let crash = ServiceCrashedEvent {
                            service_id: service_id.clone(),
                            slug,
                            exit_code: *exit_code,
                        };
                        if let Ok(payload) = serde_json::to_vec(&crash) {
                            if let Err(e) =
                                nats.publish(SUBJECT_SERVICE_CRASHED, payload.into()).await
                            {
                                tracing::warn!(error = %e, "NATS publish service_crashed failed");
                            }
                        }
                    }
                }
            }
            // Clean up kernel resources (TAP, cgroup, CPU pin).
            for (service_id, handle, _) in crashed {
                provision::release_tap_index(&handle.tap_name).await;
                deprovision::metal(&service_id, handle, &cfg.artifact_dir).await;
            }
        }
    });
}
