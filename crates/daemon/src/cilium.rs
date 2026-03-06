//! Cilium endpoint labeling — called after TAP creation so the network policy
//! in deploy/cilium/vm-isolation.yaml can enforce identity-based rules.
//!
//! Labels applied per VM:
//!   liquid-metal.io/role=vm
//!   liquid-metal.io/service-id={uuid}
//!   liquid-metal.io/engine=metal
//!
//! Linux only. Requires the `cilium` CLI binary on PATH.
#![cfg(target_os = "linux")]

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

/// Shape of the objects returned by `cilium endpoint list -o json`.
#[derive(Debug, Deserialize)]
struct Endpoint {
    id: u32,
    status: Option<EndpointStatus>,
}

#[derive(Debug, Deserialize)]
struct EndpointStatus {
    #[serde(rename = "networking")]
    networking: Option<EndpointNetworking>,
}

#[derive(Debug, Deserialize)]
struct EndpointNetworking {
    #[serde(rename = "interface-name")]
    interface_name: Option<String>,
}

/// Find the Cilium endpoint ID for `tap_name` and apply VM identity labels.
///
/// Labels let `vm-isolation.yaml` policy match this endpoint:
///   - `liquid-metal.io/role=vm`      → selects ingress/egress rules
///   - `liquid-metal.io/service-id`   → per-VM audit trail
///   - `liquid-metal.io/engine=metal` → future engine-specific policy
pub async fn label_endpoint(tap_name: &str, service_id: &str) -> Result<()> {
    let ep_id = find_endpoint_id(tap_name)
        .await
        .with_context(|| format!("finding Cilium endpoint for {}", tap_name))?;

    let out = Command::new("cilium")
        .args([
            "endpoint", "label", &ep_id.to_string(),
            &format!("liquid-metal.io/role=vm"),
            &format!("liquid-metal.io/service-id={}", service_id),
            "liquid-metal.io/engine=metal",
        ])
        .output()
        .await
        .context("cilium endpoint label")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("cilium endpoint label failed: {}", stderr.trim());
    }

    tracing::info!(
        tap = tap_name,
        ep_id,
        service_id,
        "Cilium endpoint labelled"
    );
    Ok(())
}

/// Remove VM identity labels when a service is deprovisioned.
pub async fn unlabel_endpoint(tap_name: &str) -> Result<()> {
    let ep_id = match find_endpoint_id(tap_name).await {
        Ok(id) => id,
        Err(e) => {
            // Endpoint may already be gone — log and continue teardown.
            tracing::warn!(tap = tap_name, error = %e, "Cilium endpoint not found during unlabel");
            return Ok(());
        }
    };

    let out = Command::new("cilium")
        .args([
            "endpoint", "label", &ep_id.to_string(),
            "-d", "liquid-metal.io/role",
            "-d", "liquid-metal.io/service-id",
            "-d", "liquid-metal.io/engine",
        ])
        .output()
        .await
        .context("cilium endpoint label -d")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        tracing::warn!(tap = tap_name, "cilium unlabel warning: {}", stderr.trim());
    }

    tracing::info!(tap = tap_name, ep_id, "Cilium endpoint labels removed");
    Ok(())
}

/// Ask the Cilium daemon for all endpoints and match by interface name.
async fn find_endpoint_id(tap_name: &str) -> Result<u32> {
    // Retry a few times — Cilium may not have registered the TAP yet.
    for attempt in 0..5u32 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(200 * u64::from(attempt))).await;
        }

        let out = Command::new("cilium")
            .args(["endpoint", "list", "-o", "json"])
            .output()
            .await
            .context("cilium endpoint list")?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            bail!("cilium endpoint list failed: {}", stderr.trim());
        }

        let endpoints: Vec<Endpoint> = serde_json::from_slice(&out.stdout)
            .context("parsing cilium endpoint list JSON")?;

        if let Some(ep) = endpoints.into_iter().find(|ep| {
            ep.status
                .as_ref()
                .and_then(|s| s.networking.as_ref())
                .and_then(|n| n.interface_name.as_deref())
                == Some(tap_name)
        }) {
            return Ok(ep.id);
        }
    }

    bail!("TAP '{}' not found in cilium endpoint list after retries", tap_name);
}
