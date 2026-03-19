//! Snapshot restore for serverless Metal (Firecracker) services.
//!
//! When a cold service receives its first request, the proxy publishes a
//! `WakeEvent`. The daemon downloads the snapshot files from S3, spawns
//! Firecracker, loads the snapshot, and resumes the VM. The restored VM
//! picks up exactly where it was when the snapshot was taken — the app is
//! already past startup, listening on its port.
//!
//! Flow:
//!   1. Download vmstate.snap + memory.snap from S3 (or use local cache)
//!   2. Create TAP device, attach to bridge, apply eBPF + tc
//!   3. Spawn Firecracker process (no boot-source — snapshot contains everything)
//!   4. PUT /snapshot/load → Resume VM from snapshot
//!   5. Quick health check (should pass in <500ms since app was already running)
//!   6. UPDATE services SET status='running', upstream_addr=...
//!   7. Publish RouteUpdatedEvent → proxy unblocks held request

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::{firecracker, storage};

/// Snapshot file paths on the local filesystem.
pub struct SnapshotPaths {
    pub vmstate: PathBuf,
    pub memory:  PathBuf,
}

/// Download snapshot files from S3 to a local cache directory.
///
/// Snapshots are cached at `{artifact_dir}/.snapshots/{service_id}/`.
/// If the files already exist locally, the download is skipped.
pub async fn ensure_snapshot(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    snapshot_key: &str,
    artifact_dir: &str,
    service_id: &str,
) -> Result<SnapshotPaths> {
    let cache_dir = PathBuf::from(artifact_dir)
        .join(".snapshots")
        .join(service_id);

    let vmstate_path = cache_dir.join("vmstate.snap");
    let memory_path  = cache_dir.join("memory.snap");

    let vmstate_key = format!("{}/vmstate.snap", snapshot_key);
    let memory_key  = format!("{}/memory.snap", snapshot_key);

    // Download if not cached locally.
    if !vmstate_path.exists() {
        tracing::info!(service_id, key = vmstate_key, "downloading vmstate snapshot");
        storage::download(s3, bucket, &vmstate_key, &vmstate_path)
            .await
            .context("downloading vmstate snapshot")?;
    }

    if !memory_path.exists() {
        tracing::info!(service_id, key = memory_key, "downloading memory snapshot");
        storage::download(s3, bucket, &memory_key, &memory_path)
            .await
            .context("downloading memory snapshot")?;
    }

    Ok(SnapshotPaths {
        vmstate: vmstate_path,
        memory:  memory_path,
    })
}

/// Restore a Firecracker VM from a snapshot.
///
/// Spawns the Firecracker process, loads the snapshot via the REST API,
/// and resumes the VM. The restored VM has its network stack already
/// configured (from the kernel boot_args at snapshot time).
///
/// Returns `(fc_pid, sock_path)` on success.
pub async fn restore_vm(
    fc_bin: &str,
    sock_dir: &str,
    vm_id: &str,
    snapshot: &SnapshotPaths,
    serial_log: &str,
) -> Result<(u32, String)> {
    let sock = format!("{}/{}.sock", sock_dir, vm_id);

    // Spawn Firecracker process. It starts with no VM — we load the snapshot
    // via the API. The --no-api flag is NOT used; we need the API socket.
    let pid = crate::provision::spawn_firecracker_direct(fc_bin, vm_id, &sock, serial_log).await?;

    // Load the snapshot. This replaces the empty VM with the snapshotted state.
    // `resume_vm: true` in the snapshot load means the VM starts running immediately.
    let vmstate_str = snapshot.vmstate.to_string_lossy();
    let memory_str  = snapshot.memory.to_string_lossy();

    firecracker::load_snapshot(&sock, &vmstate_str, &memory_str)
        .await
        .context("loading snapshot into Firecracker")?;

    tracing::info!(vm_id, fc_pid = pid, "VM restored from snapshot");
    Ok((pid, sock))
}
