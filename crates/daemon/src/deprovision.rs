//! VM and Wasm executor teardown.
//!
//! Called when a DeprovisionEvent is received from NATS. Cleans up all
//! kernel resources allocated during provisioning in reverse order:
//!   1. Terminate Firecracker process (SIGTERM → SIGKILL)
//!   2. Detach eBPF TC filter
//!   3. Remove tc qdiscs
//!   4. Delete TAP device
//!   5. Remove cgroup
//!   6. Re-online SMT sibling (if it was offlined)
//!   7. Delete local artifact cache

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::Mutex;

/// In-memory record of a running Metal VM.
/// Populated on provision, consumed on deprovision.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct VmHandle {
    pub tap_name:    String,
    pub fc_pid:      u32,
    /// UUID string used as the jailer chroot directory name.
    pub vm_id:       String,
    /// Whether this VM was launched under the jailer (needs chroot cleanup).
    pub use_jailer:  bool,
    /// JAILER_CHROOT_BASE value at provision time.
    pub chroot_base: String,
}

/// Global VM registry — service_id → VmHandle.
/// Arc<Mutex<_>> because it's shared between the provision and deprovision tasks.
pub type VmRegistry = Arc<Mutex<HashMap<String, VmHandle>>>;

pub fn new_registry() -> VmRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// In-memory record of a running Liquid (Wasm) service.
/// The invocation counter is incremented atomically by `wasm_http::dispatch`
/// and drained by the usage reporter every 60s.
pub struct LiquidHandle {
    pub workspace_id: String,
    pub invocations:  Arc<AtomicU64>,
    /// Handle to the HTTP accept loop spawned by `wasm_http::serve()`.
    /// Aborted when the service is deprovisioned or scaled to zero,
    /// preventing the accept loop from leaking as an orphaned task.
    pub accept_task:  Option<tokio::task::JoinHandle<()>>,
}

impl Drop for LiquidHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.accept_task.take() {
            handle.abort();
        }
    }
}

/// Global Liquid registry — service_id → LiquidHandle.
pub type LiquidRegistry = Arc<Mutex<HashMap<String, LiquidHandle>>>;

pub fn new_liquid_registry() -> LiquidRegistry {
    Arc::new(Mutex::new(HashMap::new()))
}

/// Tear down a Metal VM and release all associated kernel resources.
#[cfg(target_os = "linux")]
pub async fn metal(
    service_id: &str,
    handle: VmHandle,
    artifact_dir: &str,
) {
    use crate::{cgroup, ebpf, netlink, tc};
    use std::time::Duration;

    tracing::info!(service_id, tap = &handle.tap_name, fc_pid = handle.fc_pid, "deprovisioning metal VM");

    // 1. Terminate Firecracker — graceful first, then force
    unsafe {
        libc::kill(handle.fc_pid as libc::pid_t, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    unsafe {
        libc::kill(handle.fc_pid as libc::pid_t, libc::SIGKILL);
    }

    // 2. Detach eBPF TC filter
    ebpf::detach(&handle.tap_name);

    // 3. Remove tc qdiscs
    tc::remove(&handle.tap_name).await;

    // 4. Delete TAP
    if let Err(e) = netlink::delete_tap(&handle.tap_name).await {
        tracing::warn!(tap = &handle.tap_name, error = %e, "TAP deletion failed");
    }

    // 5. Cleanup cgroup
    cgroup::cleanup(service_id).await;

    // 6. Delete local artifact cache
    let artifact_path = format!("{}/{}", artifact_dir, service_id);
    if let Err(e) = tokio::fs::remove_dir_all(&artifact_path).await {
        tracing::debug!(service_id, error = %e, "artifact cache cleanup (may not exist)");
    }

    // 7. Remove jailer chroot directory (no-op if jailer was not used)
    if handle.use_jailer {
        crate::jailer::cleanup(&handle.chroot_base, &handle.vm_id).await;
    }

    tracing::info!(service_id, "metal VM deprovisioned");
}

/// No-op on non-Linux (macOS dev): Metal VMs don't actually run.
#[cfg(not(target_os = "linux"))]
pub async fn metal(
    service_id: &str,
    _handle: VmHandle,
    _artifact_dir: &str,
) {
    tracing::info!(service_id, "deprovision metal (no-op on non-Linux)");
}
