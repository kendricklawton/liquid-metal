//! Kernel resource enforcement via cgroup v2.
//!
//! Each Firecracker process is moved into its own cgroup under
//! `/sys/fs/cgroup/liquid-metal/{service_id}` immediately after spawn.
//!
//! Enforced limits:
//!   - `memory.max`  — hard memory ceiling (guest RAM + headroom)
//!   - `pids.max`    — fork-bomb protection
//!   - `io.max`      — per-device disk bandwidth/IOPS caps
//!
//! Requires: cgroup v2 unified hierarchy mounted at /sys/fs/cgroup (default
//! on Linux 5.14+ and any distro with systemd ≥ 244 in unified mode).
#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use common::events::ResourceQuota;
use tokio::fs;

const CGROUP_BASE: &str = "/sys/fs/cgroup/liquid-metal";

/// Extra percentage above guest RAM to allow for Firecracker process overhead.
/// Override via `CGROUP_MEMORY_HEADROOM_PCT` (default 10%).
static MEMORY_HEADROOM_PCT: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    common::config::env_or("CGROUP_MEMORY_HEADROOM_PCT", "10")
        .parse()
        .unwrap_or(10)
});

/// Maximum number of PIDs per VM cgroup. Override via `CGROUP_PIDS_MAX`.
static PIDS_MAX: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
    common::config::env_or("CGROUP_PIDS_MAX", "512")
        .parse()
        .unwrap_or(512)
});

/// Move `fc_pid` into a new cgroup for `service_id` and apply resource limits.
/// `rootfs_dev` is the block device identifier string, e.g. "8:0".
/// `memory_mb` is the guest RAM allocation from MetalSpec.
pub async fn apply(
    service_id: &str,
    fc_pid: u32,
    rootfs_dev: &str,
    quota: &ResourceQuota,
    memory_mb: u32,
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

    // ── memory.max ─────────────────────────────────────────────────────────
    // Hard ceiling = guest RAM + headroom for FC process overhead.
    // Exceeding this triggers the cgroup OOM killer (only this VM, not the host).
    let memory_bytes = (memory_mb as u64) * 1024 * 1024 * (100 + *MEMORY_HEADROOM_PCT) / 100;
    if let Err(e) = fs::write(format!("{}/memory.max", cg), memory_bytes.to_string()).await {
        tracing::warn!(service_id, error = %e, "failed to set memory.max");
    } else {
        tracing::debug!(service_id, memory_bytes, "memory.max applied");
    }

    // ── pids.max ───────────────────────────────────────────────────────────
    // Fork-bomb protection. Firecracker spawns a small fixed number of threads;
    // 512 is generous but prevents escalation via any future guest escape.
    if let Err(e) = fs::write(format!("{}/pids.max", cg), PIDS_MAX.to_string()).await {
        tracing::warn!(service_id, error = %e, "failed to set pids.max");
    } else {
        tracing::debug!(service_id, pids_max = *PIDS_MAX, "pids.max applied");
    }

    // ── io.max ─────────────────────────────────────────────────────────────
    if let Some(io_max) = build_io_max(rootfs_dev, quota) {
        fs::write(format!("{}/io.max", cg), &io_max)
            .await
            .context("writing io.max")?;
        tracing::debug!(service_id, io_max, "io.max applied");
    }

    tracing::info!(service_id, fc_pid, memory_bytes, pids_max = *PIDS_MAX, "cgroup v2 limits applied");
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
