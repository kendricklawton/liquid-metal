//! Layer 2: Kernel IO enforcement via cgroup v2 io.max.
//!
//! Each Firecracker process is moved into its own cgroup under
//! `/sys/fs/cgroup/liquid-metal/{service_id}` immediately after spawn.
//! io.max is written with per-device limits derived from ResourceQuota.
//!
//! Requires: cgroup v2 unified hierarchy mounted at /sys/fs/cgroup (default
//! on Linux 5.14+ and any distro with systemd ≥ 244 in unified mode).
#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use common::events::ResourceQuota;
use tokio::fs;

const CGROUP_BASE: &str = "/sys/fs/cgroup/liquid-metal";

/// Move `fc_pid` into a new cgroup for `service_id` and apply IO limits.
/// `rootfs_dev` is the block device identifier string, e.g. "8:0".
pub async fn apply(
    service_id: &str,
    fc_pid: u32,
    rootfs_dev: &str,
    quota: &ResourceQuota,
) -> Result<()> {
    let cg = format!("{}/{}", CGROUP_BASE, service_id);

    fs::create_dir_all(&cg)
        .await
        .context("create cgroup dir (is cgroup v2 mounted at /sys/fs/cgroup?)")?;

    // Move the Firecracker process into this leaf cgroup.
    // All threads it spawns will inherit membership automatically.
    fs::write(format!("{}/cgroup.procs", cg), fc_pid.to_string())
        .await
        .context("writing FC pid to cgroup.procs")?;

    if let Some(io_max) = build_io_max(rootfs_dev, quota) {
        fs::write(format!("{}/io.max", cg), &io_max)
            .await
            .context("writing io.max")?;
        tracing::debug!(service_id, io_max, "io.max applied");
    }

    tracing::info!(service_id, fc_pid, rootfs_dev, "cgroup v2 limits applied");
    Ok(())
}

/// Remove the cgroup when the service is deprovisioned.
/// The cgroup must be empty (FC process exited) before rmdir succeeds.
pub async fn cleanup(service_id: &str) {
    let cg = format!("{}/{}", CGROUP_BASE, service_id);
    if let Err(e) = fs::remove_dir(&cg).await {
        tracing::warn!(service_id, error = %e, "cgroup cleanup failed (VM may still be running)");
    } else {
        tracing::debug!(service_id, "cgroup removed");
    }
}

/// Build the io.max line, e.g.:
///   "8:0 rbps=52428800 wbps=52428800 riops=1000 wiops=1000"
///
/// Returns None if no limits are set (quota is fully unlimited).
fn build_io_max(device: &str, quota: &ResourceQuota) -> Option<String> {
    let mut parts: Vec<String> = vec![device.to_string()];

    if let Some(v) = quota.disk_read_bps   { parts.push(format!("rbps={}", v));  }
    if let Some(v) = quota.disk_write_bps  { parts.push(format!("wbps={}", v));  }
    if let Some(v) = quota.disk_read_iops  { parts.push(format!("riops={}", v)); }
    if let Some(v) = quota.disk_write_iops { parts.push(format!("wiops={}", v)); }

    // Only emit the line if at least one limit is set
    if parts.len() > 1 { Some(parts.join(" ")) } else { None }
}
