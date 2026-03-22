// Used only inside #[cfg(target_os = "linux")] blocks in provision.rs
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// Firecracker REST API client over a Unix Domain Socket.
use anyhow::{Context, Result};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

pub struct VmConfig<'a> {
    pub sock_path: &'a str,
    pub vcpu: u32,
    pub memory_mb: u32,
    pub kernel_path: &'a str,
    pub rootfs_path: &'a str,
    pub tap_name: &'a str,
    pub guest_mac: &'a str,
    /// Guest IP address (e.g. "172.16.0.2") — passed to kernel via ip= boot arg.
    pub guest_ip: &'a str,
    /// Gateway IP (br0 bridge address) — passed to kernel via ip= boot arg.
    pub gateway: &'a str,
}

/// Configure and start a Firecracker microVM.
pub async fn start_vm(cfg: &VmConfig<'_>) -> Result<()> {
    put(cfg.sock_path, "/machine-config", &serde_json::json!({
        "vcpu_count": cfg.vcpu,
        "mem_size_mib": cfg.memory_mb
    }).to_string()).await.context("PUT /machine-config")?;

    // ip= kernel parameter: ip=client::gateway:netmask::interface:autoconf
    // The kernel configures eth0 before init runs — no userspace DHCP needed.
    let boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off ip={}::{}:{}::eth0:off init=/sbin/init",
        cfg.guest_ip, cfg.gateway, common::networking::NETMASK,
    );
    put(cfg.sock_path, "/boot-source", &serde_json::json!({
        "kernel_image_path": cfg.kernel_path,
        "boot_args": boot_args
    }).to_string()).await.context("PUT /boot-source")?;

    put(cfg.sock_path, "/drives/rootfs", &serde_json::json!({
        "drive_id": "rootfs",
        "path_on_host": cfg.rootfs_path,
        "is_root_device": true,
        "is_read_only": false
    }).to_string()).await.context("PUT /drives/rootfs")?;

    put(cfg.sock_path, "/network-interfaces/eth0", &serde_json::json!({
        "iface_id": "eth0",
        "host_dev_name": cfg.tap_name,
        "guest_mac": cfg.guest_mac
    }).to_string()).await.context("PUT /network-interfaces/eth0")?;

    put(cfg.sock_path, "/actions",
        &serde_json::json!({"action_type": "InstanceStart"}).to_string())
        .await.context("PUT /actions InstanceStart")?;

    tracing::info!(sock = cfg.sock_path, "microVM started");
    Ok(())
}

/// Load a snapshot into a freshly spawned Firecracker process.
///
/// The Firecracker process must be running (API socket open) but have no
/// VM configured — the snapshot replaces everything. `resume_vm: true`
/// starts the VM immediately after loading.
pub async fn load_snapshot(sock_path: &str, vmstate_path: &str, mem_path: &str) -> Result<()> {
    put(sock_path, "/snapshot/load", &serde_json::json!({
        "snapshot_path": vmstate_path,
        "mem_backend": {
            "backend_type": "File",
            "backend_path": mem_path
        },
        "enable_diff_snapshots": false,
        "resume_vm": true
    }).to_string())
    .await
    .context("PUT /snapshot/load")
}

async fn put(sock_path: &str, path: &str, body: &str) -> Result<()> {
    let stream = UnixStream::connect(sock_path)
        .await
        .with_context(|| format!("connecting to {}", sock_path))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("HTTP handshake")?;
    // Drive the HTTP connection concurrently. The task normally completes
    // once the response is fully consumed. Abort at the end of this function
    // as a safety net if Firecracker hangs after responding.
    let conn_handle = tokio::spawn(async move { conn.await.ok(); });

    let resp = sender
        .send_request(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body.to_owned())))
                .context("building request")?,
        )
        .await
        .context("send request")?;

    if !resp.status().is_success() {
        let bytes = resp.into_body().collect().await?.to_bytes();
        conn_handle.abort();
        anyhow::bail!("Firecracker PUT {} → {}", path, String::from_utf8_lossy(&bytes));
    }
    conn_handle.abort();
    Ok(())
}
