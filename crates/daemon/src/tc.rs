//! Layer 3: Network bandwidth enforcement via tc (Traffic Control).
//!
//! Applies a Token Bucket Filter (tbf) qdisc to each Firecracker VM's TAP
//! device to cap egress bandwidth. Ingress is shaped on the TAP by mirroring
//! through an IFB (Intermediate Functional Block) device.
//!
//! Bandwidth shaping (this module) and tenant isolation (ebpf.rs) are
//! independent — both attach to tap{n} and coexist without conflict.
//!
//! Requires: iproute2 (`tc` binary) installed on the host.
//! On Debian/Ubuntu: `apt install iproute2`
#![cfg(target_os = "linux")]

use anyhow::{Context, Result};
use common::events::ResourceQuota;
use tokio::process::Command;

/// Apply ingress + egress bandwidth limits to a TAP device.
/// Skips any dimension where the quota is None (unlimited).
pub async fn apply(tap: &str, quota: &ResourceQuota) -> Result<()> {
    if let Some(kbps) = quota.net_egress_kbps {
        set_egress(tap, kbps).await.context("tc egress")?;
    }
    if let Some(kbps) = quota.net_ingress_kbps {
        set_ingress(tap, kbps).await.context("tc ingress")?;
    }
    tracing::info!(
        tap,
        egress_kbps  = ?quota.net_egress_kbps,
        ingress_kbps = ?quota.net_ingress_kbps,
        "tc bandwidth applied"
    );
    Ok(())
}

/// Remove all qdiscs from a TAP device (called on deprovision).
pub async fn remove(tap: &str) {
    let _ = Command::new("tc")
        .args(["qdisc", "del", "dev", tap, "root"])
        .status()
        .await;
    let _ = Command::new("tc")
        .args(["qdisc", "del", "dev", tap, "ingress"])
        .status()
        .await;
    tracing::debug!(tap, "tc qdiscs removed");
}

/// Cap egress (VM → host/internet) with a tbf qdisc on the TAP root.
///
/// burst = rate × 10ms (reasonable default to absorb short bursts without
/// introducing latency spikes).
async fn set_egress(tap: &str, kbps: u32) -> Result<()> {
    let rate   = format!("{}kbit", kbps);
    let burst  = format!("{}kbit", (kbps / 100).max(4)); // 10ms worth, min 4kbit
    let status = Command::new("tc")
        .args([
            "qdisc", "replace", "dev", tap, "root", "tbf",
            "rate",    &rate,
            "burst",   &burst,
            "latency", "100ms",
        ])
        .status()
        .await
        .context("tc qdisc replace (egress)")?;

    if !status.success() {
        anyhow::bail!("tc qdisc replace egress failed for {}", tap);
    }
    Ok(())
}

/// Cap ingress (host/internet → VM) by redirecting through an IFB device.
///
/// Linux tc ingress is limited — to shape incoming traffic on a TAP you must:
///   1. Add an ingress qdisc to the TAP
///   2. Mirror traffic to an IFB interface
///   3. Add a tbf qdisc to the IFB interface
///
/// The IFB device name is derived from the TAP index: tap0 → ifb0, tap1 → ifb1.
async fn set_ingress(tap: &str, kbps: u32) -> Result<()> {
    // Derive IFB name from TAP number (tap0 → ifb0)
    let ifb = tap.replace("tap", "ifb");
    let rate  = format!("{}kbit", kbps);
    let burst = format!("{}kbit", (kbps / 100).max(4));

    // Ensure the IFB device exists and is up
    let _ = Command::new("ip")
        .args(["link", "add", &ifb, "type", "ifb"])
        .status()
        .await; // may already exist — ignore error
    Command::new("ip")
        .args(["link", "set", &ifb, "up"])
        .status()
        .await
        .context("ip link set ifb up")?;

    // Add ingress qdisc to TAP
    let _ = Command::new("tc")
        .args(["qdisc", "add", "dev", tap, "ingress"])
        .status()
        .await; // ignore if already exists

    // Redirect all ingress traffic from TAP → IFB
    let status = Command::new("tc")
        .args([
            "filter", "replace", "dev", tap,
            "parent", "ffff:", "protocol", "all",
            "u32", "match", "u32", "0", "0",
            "action", "mirred", "egress", "redirect", "dev", &ifb,
        ])
        .status()
        .await
        .context("tc filter redirect to IFB")?;
    if !status.success() {
        anyhow::bail!("tc filter redirect failed for {}", tap);
    }

    // Apply tbf to the IFB (this is where the shaping actually happens)
    let status = Command::new("tc")
        .args([
            "qdisc", "replace", "dev", &ifb, "root", "tbf",
            "rate",    &rate,
            "burst",   &burst,
            "latency", "100ms",
        ])
        .status()
        .await
        .context("tc qdisc replace (ingress via IFB)")?;
    if !status.success() {
        anyhow::bail!("tc ingress tbf failed for IFB {}", ifb);
    }

    Ok(())
}
