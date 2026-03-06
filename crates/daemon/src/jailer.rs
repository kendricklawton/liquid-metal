//! Firecracker Jailer integration — the Execution Jailer security boundary.
//!
//! The jailer wraps Firecracker in a hard chain of OS isolation before the
//! VMM starts accepting any configuration:
//!
//!   ┌─────────────────────────────────────────────────────────┐
//!   │  jailer binary  (runs as root, drops privileges inside) │
//!   │                                                         │
//!   │  1. PID namespace  — FC sees no other host processes   │
//!   │  2. Mount namespace + chroot — FC's / is an empty dir  │
//!   │     No access to /etc, /home, /proc of the host        │
//!   │  3. uid/gid remap — FC runs as unprivileged user       │
//!   │  4. Seccomp-BPF  — ~20 syscalls allowed; everything    │
//!   │     else → SIGKILL. Kernel exploits have no syscall    │
//!   │     surface to reach.                                  │
//!   └─────────────────────────────────────────────────────────┘
//!
//! # File staging
//!
//! The jailer chroot lives at `{chroot_base}/firecracker/{vm_id}/root/`.
//! Artifacts (rootfs + kernel) are hard-linked into that directory so FC
//! can access them at `/rootfs.ext4` and `/vmlinux` inside the chroot.
//! Hard links share the inode — no disk copy, instant staging.
//!
//! # Socket path
//!
//! Inside the chroot FC binds `/run/api.sock`.
//! From the host the same socket appears at:
//!   `{chroot_base}/firecracker/{vm_id}/root/run/api.sock`
//! `JailerPaths::socket` holds the host-side path used by `firecracker::start_vm`.
#![cfg(target_os = "linux")]

use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::fs;

pub const DEFAULT_BIN:         &str = "/usr/local/bin/jailer";
pub const DEFAULT_CHROOT_BASE: &str = "/srv/jailer";
pub const DEFAULT_UID:         u32  = 10000;
pub const DEFAULT_GID:         u32  = 10000;

pub struct JailerConfig<'a> {
    pub vm_id:       &'a str,
    pub jailer_bin:  &'a str,
    pub fc_bin:      &'a str,
    pub uid:         u32,
    pub gid:         u32,
    pub chroot_base: &'a str,
    pub rootfs_src:  &'a str,   // host path to the ext4 rootfs image
    pub kernel_src:  &'a str,   // host path to the vmlinux kernel
}

/// Paths the caller needs after the jailer is running.
pub struct JailerPaths {
    /// Host-side path to the Firecracker API socket.
    pub socket: String,
    /// rootfs path as FC sees it *inside* the chroot (use in VmConfig).
    pub rootfs_guest: &'static str,
    /// kernel path as FC sees it *inside* the chroot (use in VmConfig).
    pub kernel_guest: &'static str,
}

/// Launch Firecracker under the jailer. Returns `(jailer_pid, paths)`.
///
/// After this returns the Firecracker API socket is ready to accept PUT
/// requests. The caller should then call `firecracker::start_vm` with
/// `sock_path = paths.socket` and the guest-relative artifact paths.
pub async fn spawn(cfg: &JailerConfig<'_>) -> Result<(u32, JailerPaths)> {
    let chroot_root = format!(
        "{}/firecracker/{}/root",
        cfg.chroot_base, cfg.vm_id
    );

    // Prepare chroot directory structure
    fs::create_dir_all(format!("{}/run", chroot_root))
        .await
        .context("creating chroot/run dir")?;

    // Stage artifacts — hard link (same device) or copy (cross-device)
    stage_artifact(cfg.rootfs_src, &format!("{}/rootfs.ext4", chroot_root)).await
        .context("staging rootfs")?;
    stage_artifact(cfg.kernel_src, &format!("{}/vmlinux", chroot_root)).await
        .context("staging kernel")?;

    let mut cmd = tokio::process::Command::new(cfg.jailer_bin);
    cmd.args([
        "--id",              cfg.vm_id,
        "--exec-file",       cfg.fc_bin,
        "--uid",             &cfg.uid.to_string(),
        "--gid",             &cfg.gid.to_string(),
        "--chroot-base-dir", cfg.chroot_base,
        "--cgroup-version",  "2",
        "--",                          // arguments forwarded to Firecracker
        "--api-sock",        "/run/api.sock",
        "--id",              cfg.vm_id,
    ]);
    cmd.stdout(std::process::Stdio::null())
       .stderr(std::process::Stdio::null());

    let child = cmd.spawn().context("spawning jailer")?;
    let pid = child.id().context("jailer exited immediately after spawn")?;

    let socket = format!("{}/run/api.sock", chroot_root);
    wait_for_socket(&socket).await?;

    tracing::info!(
        vm_id = cfg.vm_id,
        pid,
        chroot = chroot_root,
        "jailer running — namespaces + chroot + seccomp active"
    );

    Ok((pid, JailerPaths {
        socket,
        rootfs_guest: "/rootfs.ext4",
        kernel_guest:  "/vmlinux",
    }))
}

/// Clean up jailer chroot directory after the VM has stopped.
pub async fn cleanup(chroot_base: &str, vm_id: &str) {
    let chroot = format!("{}/firecracker/{}", chroot_base, vm_id);
    if let Err(e) = fs::remove_dir_all(&chroot).await {
        tracing::warn!(vm_id, error = %e, "jailer chroot cleanup failed");
    } else {
        tracing::debug!(vm_id, "jailer chroot removed");
    }
}

/// Hard-link `src` to `dst`. Falls back to copy on cross-device filesystems.
/// Skips if `dst` already exists (idempotent restarts).
async fn stage_artifact(src: &str, dst: &str) -> Result<()> {
    if fs::metadata(dst).await.is_ok() {
        return Ok(());
    }
    match fs::hard_link(src, dst).await {
        Ok(_) => tracing::debug!(src, dst, "artifact hard-linked"),
        Err(_) => {
            // Cross-device (e.g. tmpfs chroot on a different mount) → copy
            fs::copy(src, dst)
                .await
                .with_context(|| format!("copying artifact {} → {}", src, dst))?;
            tracing::debug!(src, dst, "artifact copied (cross-device)");
        }
    }
    Ok(())
}

/// Poll until the Firecracker API socket appears (jailer + FC ready).
async fn wait_for_socket(path: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if fs::metadata(path).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("jailer API socket {} not ready within 5s", path);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
