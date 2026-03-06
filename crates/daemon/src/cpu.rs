//! Layer 0: Silicon isolation via CPU core pinning.
//!
//! Side-channel attacks (Spectre, MDS, RIDL) exploit the fact that two
//! processes sharing a physical core also share execution unit buffers,
//! L1 cache, and TLB. Pinning each Firecracker VM to its own physical core
//! eliminates the shared-silicon attack surface entirely.
//!
//! Implementation: cgroup v2 cpuset controller.
//! - `cpuset.cpus` restricts a cgroup to specific logical CPUs.
//! - On SMT (Hyper-Threading) hardware, set `cpuset.cpus` to a single
//!   thread and ensure the SMT sibling is offline or used only by the host.
//! - `cpuset.mems = "0"` locks to NUMA node 0 (single-socket machines).
//!
//! Requires: cpuset controller enabled in the parent cgroup.
//! Run `task metal:quota-setup` which enables +cpuset in subtree_control.
#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::fs;

static CORE_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Assign the next physical CPU core in round-robin fashion.
/// `physical_cores` = count of physical (not logical/HT) cores available
/// for VM workloads. Set via `PHYSICAL_CORES` env var (default: 4 for T480).
pub fn next_core(physical_cores: u32) -> u32 {
    CORE_COUNTER.fetch_add(1, Ordering::Relaxed) % physical_cores
}

/// Apply cpuset pinning to an existing cgroup directory.
///
/// Sets `cpuset.cpus = cpu_core` and `cpuset.mems = "0"`.
/// The cgroup must already exist (created by `cgroup::apply`).
pub async fn pin_cgroup(cgroup_path: &str, cpu_core: u32) -> Result<()> {
    fs::write(format!("{}/cpuset.cpus", cgroup_path), cpu_core.to_string())
        .await
        .context("writing cpuset.cpus")?;

    fs::write(format!("{}/cpuset.mems", cgroup_path), "0")
        .await
        .context("writing cpuset.mems")?;

    tracing::info!(cgroup_path, cpu_core, "VM pinned to physical core");
    Ok(())
}

/// Offline the SMT sibling of `physical_core` to eliminate cross-HT leakage.
///
/// On a 4-core/8-thread CPU (T480 i7):
///   physical core 0 → logical CPUs 0 and 4
///   physical core 1 → logical CPUs 1 and 5
///   ...
/// Taking the sibling offline means the VM has the entire physical core to
/// itself — no hyper-thread sharing with any other tenant or host process.
///
/// **Warning**: this reduces host throughput. Only use for "secure" tier VMs.
pub async fn offline_smt_sibling(physical_core: u32, total_physical_cores: u32) -> Result<()> {
    // Sibling = physical_core + total_physical_cores (for a single-socket machine)
    let sibling = physical_core + total_physical_cores;
    let path = format!("/sys/devices/system/cpu/cpu{}/online", sibling);

    if fs::metadata(&path).await.is_err() {
        tracing::debug!(sibling, "SMT sibling does not exist (SMT disabled or single-threaded)");
        return Ok(());
    }

    fs::write(&path, "0")
        .await
        .with_context(|| format!("offlining CPU {}", sibling))?;

    tracing::info!(physical_core, sibling, "SMT sibling offlined — silicon isolated");
    Ok(())
}

/// Re-online a previously offlined SMT sibling (called on VM deprovision).
pub async fn online_smt_sibling(physical_core: u32, total_physical_cores: u32) {
    let sibling = physical_core + total_physical_cores;
    let path = format!("/sys/devices/system/cpu/cpu{}/online", sibling);
    if let Err(e) = fs::write(&path, "1").await {
        tracing::warn!(sibling, error = %e, "could not re-online SMT sibling");
    } else {
        tracing::debug!(sibling, "SMT sibling re-onlined");
    }
}
