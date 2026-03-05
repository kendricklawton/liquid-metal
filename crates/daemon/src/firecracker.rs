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
}

/// Configure and start a Firecracker microVM.
pub async fn start_vm(cfg: &VmConfig<'_>) -> Result<()> {
    put(cfg.sock_path, "/machine-config", &serde_json::json!({
        "vcpu_count": cfg.vcpu,
        "mem_size_mib": cfg.memory_mb
    }).to_string()).await.context("PUT /machine-config")?;

    put(cfg.sock_path, "/boot-source", &serde_json::json!({
        "kernel_image_path": cfg.kernel_path,
        "boot_args": "console=ttyS0 reboot=k panic=1 pci=off"
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

async fn put(sock_path: &str, path: &str, body: &str) -> Result<()> {
    let stream = UnixStream::connect(sock_path)
        .await
        .with_context(|| format!("connecting to {}", sock_path))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("HTTP handshake")?;
    tokio::spawn(async move { conn.await.ok(); });

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
        anyhow::bail!("Firecracker PUT {} → {}", path, String::from_utf8_lossy(&bytes));
    }
    Ok(())
}
