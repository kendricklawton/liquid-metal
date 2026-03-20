//! Restore a Metal service from a Firecracker snapshot.
//!
//! Called when the daemon receives a `WakeEvent` (proxy detected a cold
//! service with a snapshot). Downloads snapshot from S3, creates TAP,
//! spawns Firecracker, loads snapshot, runs a quick health check, then
//! publishes `RouteUpdatedEvent` so the proxy can forward the held request.

use std::sync::Arc;

use crate::deprovision;
use crate::provision::{self, ProvisionConfig, ProvisionCtx};
use common::events::WakeEvent;

#[cfg(target_os = "linux")]
pub async fn wake_from_snapshot(
    ctx: &Arc<ProvisionCtx>,
    cfg: &Arc<ProvisionConfig>,
    event: &WakeEvent,
    registry: &deprovision::VmRegistry,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use common::events::{RouteUpdatedEvent, SUBJECT_ROUTE_UPDATED};
    use common::networking;
    use crate::{cgroup, ebpf, netlink, snapshot, tc};
    use uuid::Uuid;

    // Download snapshot from S3 (or use local cache)
    let snap = snapshot::ensure_snapshot(
        &ctx.s3, &ctx.bucket, &event.snapshot_key, &cfg.artifact_dir, &event.service_id,
    )
    .await
    .context("downloading snapshot")?;

    // Allocate TAP + network identity
    let tap_idx = provision::allocate_tap_index().await?;
    let tap = networking::tap_name(tap_idx);
    let ip = networking::guest_ip(tap_idx).context("TAP index pool exhausted")?;
    let vm_id = Uuid::now_v7();

    // Read the port from the DB so we can build upstream_addr.
    let port: i32 = {
        let db = ctx.pool.get().await.context("db pool")?;
        let svc_id: Uuid = event.service_id.parse().context("invalid service_id")?;
        let row = db
            .query_one("SELECT port FROM services WHERE id = $1", &[&svc_id])
            .await
            .context("reading service port")?;
        row.get("port")
    };
    let upstream_addr = format!("{}:{}", ip, port);

    let serial_log = format!("{}/{}/serial.log", cfg.artifact_dir, event.service_id);

    // Create TAP, attach to bridge, apply isolation
    netlink::create_tap(&tap).context("create TAP for wake")?;

    let cleanup_on_err = || async {
        provision::release_tap_index(&tap).await;
        let _ = netlink::delete_tap(&tap).await;
    };

    if let Err(e) = async {
        netlink::attach_to_bridge(&tap, &cfg.bridge)
            .await
            .context("attach TAP")?;
        tc::apply(&tap, &common::events::ResourceQuota::default())
            .await
            .context("tc")?;
        ebpf::attach(&tap, &event.service_id).context("eBPF")?;

        // Spawn Firecracker and load snapshot (VM resumes immediately)
        let (fc_pid, _sock) = snapshot::restore_vm(
            &cfg.fc_bin,
            &cfg.sock_dir,
            &vm_id.to_string(),
            &snap,
            &serial_log,
        )
        .await
        .context("restoring VM from snapshot")?;

        // Apply cgroup limits (memory + IO)
        cgroup::apply(
            &event.service_id,
            fc_pid,
            &cfg.rootfs_dev,
            &common::events::ResourceQuota::default(),
            128,
        )
        .await
        .context("cgroup limits")?;

        // Quick health check — app was already running at snapshot time,
        // should respond almost immediately after restore.
        let probe_timeout = std::time::Duration::from_secs(5);
        if tokio::time::timeout(probe_timeout, async {
            loop {
                if tokio::net::TcpStream::connect(&upstream_addr).await.is_ok() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .is_err()
        {
            anyhow::bail!(
                "wake health check timed out after {}s — app not listening on {upstream_addr}",
                probe_timeout.as_secs()
            );
        }

        // Register in-memory for future deprovision
        registry.lock().await.insert(
            event.service_id.clone(),
            deprovision::VmHandle {
                tap_name: tap.clone(),
                fc_pid,
                vm_id: vm_id.to_string(),
                use_jailer: cfg.use_jailer,
                chroot_base: cfg.chroot_base.clone(),
            },
        );

        // Update DB
        let db = ctx.pool.get().await.context("db pool")?;
        let svc_id: Uuid = event.service_id.parse()?;
        db.execute(
            "UPDATE services SET status = 'running', upstream_addr = $1, node_id = $2 WHERE id = $3",
            &[&Some(&upstream_addr), &cfg.node_id, &svc_id],
        )
        .await
        .context("updating service status to running")?;

        // Publish RouteUpdatedEvent — proxy unblocks the held request
        let payload = serde_json::to_vec(&RouteUpdatedEvent {
            slug: event.slug.clone(),
            upstream_addr: upstream_addr.clone(),
        })?;
        ctx.nats
            .publish(SUBJECT_ROUTE_UPDATED, payload.into())
            .await
            .ok();

        tracing::info!(
            service_id = event.service_id,
            slug = event.slug,
            %upstream_addr,
            fc_pid,
            "service woke from snapshot"
        );

        Ok::<_, anyhow::Error>(())
    }
    .await
    {
        cleanup_on_err().await;
        return Err(e);
    }

    Ok(())
}

/// Non-Linux stub — wake is a no-op outside Linux.
#[cfg(not(target_os = "linux"))]
pub async fn wake_from_snapshot(
    _ctx: &Arc<ProvisionCtx>,
    _cfg: &Arc<ProvisionConfig>,
    _event: &WakeEvent,
    _registry: &deprovision::VmRegistry,
) -> anyhow::Result<()> {
    tracing::warn!("wake_from_snapshot called on non-Linux — no-op");
    Ok(())
}

/// Wake a Liquid (Wasm) service from scale-to-zero.
///
/// The Wasm module and compiled cache are still on disk from the original
/// provision. We read the saved metadata (app_name, env_vars), re-start
/// the HTTP shim, and publish RouteUpdatedEvent so the proxy can forward
/// the held request.
pub async fn wake_liquid(
    ctx: &Arc<ProvisionCtx>,
    cfg: &Arc<ProvisionConfig>,
    event: &WakeEvent,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use common::events::{RouteUpdatedEvent, SUBJECT_ROUTE_UPDATED};
    use std::sync::atomic::AtomicU64;

    let service_dir = std::path::PathBuf::from(&cfg.artifact_dir).join(&event.service_id);
    let wasm_path = service_dir.join("main.wasm");
    let metadata_path = service_dir.join("metadata.json");

    // Read metadata persisted during provision
    let metadata_bytes = tokio::fs::read(&metadata_path)
        .await
        .context("reading liquid metadata — was this service provisioned with metadata.json?")?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_bytes)
        .context("parsing liquid metadata.json")?;

    let app_name = metadata["app_name"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let env_vars: std::collections::HashMap<String, String> = metadata
        .get("env_vars")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let tenant_id = metadata["tenant_id"]
        .as_str()
        .unwrap_or("")
        .to_string();

    anyhow::ensure!(
        wasm_path.exists(),
        "wasm binary not found at {} — artifacts may have been cleaned up",
        wasm_path.display()
    );

    tracing::info!(
        service_id = event.service_id,
        app = app_name,
        "waking liquid service from scale-to-zero"
    );

    // Invocation counter for billing
    let invocations = Arc::new(AtomicU64::new(0));

    // Re-start the HTTP shim — uses the compiled cache on disk (<10ms)
    let port = crate::wasm_http::serve(
        wasm_path.to_string_lossy().into_owned(),
        app_name,
        invocations.clone(),
        env_vars,
    )
    .await
    .context("re-starting wasm HTTP shim")?;

    let upstream_addr = format!("127.0.0.1:{port}");

    // Register for billing
    ctx.liquid_registry.lock().await.insert(
        event.service_id.clone(),
        deprovision::LiquidHandle {
            workspace_id: tenant_id,
            invocations,
        },
    );

    // Update DB
    let db = ctx.pool.get().await.context("db pool")?;
    let svc_id: uuid::Uuid = event.service_id.parse().context("invalid service_id")?;
    db.execute(
        "UPDATE services SET status = 'running', upstream_addr = $1, node_id = $2 WHERE id = $3",
        &[&Some(&upstream_addr), &cfg.node_id, &svc_id],
    )
    .await
    .context("updating service status to running")?;

    // Publish RouteUpdatedEvent — proxy unblocks the held request
    let payload = serde_json::to_vec(&RouteUpdatedEvent {
        slug: event.slug.clone(),
        upstream_addr: upstream_addr.clone(),
    })?;
    ctx.nats
        .publish(SUBJECT_ROUTE_UPDATED, payload.into())
        .await
        .ok();

    tracing::info!(
        service_id = event.service_id,
        slug = event.slug,
        %upstream_addr,
        "liquid service woke from scale-to-zero"
    );

    Ok(())
}
