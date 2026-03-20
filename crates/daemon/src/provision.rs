use crate::{deprovision, storage, verify, wasm_http};
use anyhow::{Context, Result};
use common::events::{
    DeployProgressEvent, DeployStep, Engine, EngineSpec, FailureKind, ProvisionEvent,
    RouteUpdatedEvent, SUBJECT_DEPLOY_PROGRESS, SUBJECT_ROUTE_UPDATED,
};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use crate::{cgroup, ebpf, firecracker, jailer, netlink, rootfs, tc};

/// How long to wait for the guest binary to bind its port after VM boot.
/// Override via `STARTUP_PROBE_TIMEOUT_SECS` (default 30s).
#[cfg(target_os = "linux")]
static STARTUP_PROBE_TIMEOUT: std::sync::LazyLock<Duration> = std::sync::LazyLock::new(|| {
    let secs: u64 = common::config::env_or("STARTUP_PROBE_TIMEOUT_SECS", "30")
        .parse().unwrap_or(30);
    Duration::from_secs(secs)
});

/// How many lines of serial console output to include in startup failure errors.
#[cfg(target_os = "linux")]
static STARTUP_PROBE_LOG_LINES: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
    common::config::env_or("STARTUP_PROBE_LOG_LINES", "50")
        .parse().unwrap_or(50)
});

/// Tracks in-use TAP indices so freed indices can be reclaimed.
/// Without this, the old monotonic counter would exhaust the IP address space
/// (3969 addresses per node) after enough create/delete cycles.
static TAP_INDICES: std::sync::OnceLock<Mutex<BTreeSet<u32>>> = std::sync::OnceLock::new();

fn tap_indices() -> &'static Mutex<BTreeSet<u32>> {
    TAP_INDICES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Runtime configuration — all values sourced from env vars at startup.
/// No hardcoded paths anywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProvisionConfig {
    // Firecracker binaries + sockets
    pub fc_bin:         String,   // FC_BIN,          default /usr/local/bin/firecracker
    pub kernel_path:    String,   // FC_KERNEL_PATH,  default /opt/firecracker/vmlinux
    pub sock_dir:       String,   // FC_SOCK_DIR,     default /run/firecracker
    pub bridge:         String,   // BRIDGE,          default br0
    // cgroup v2 IO limits
    pub rootfs_dev:     String,   // ROOTFS_DEVICE,   default 8:0
    // Jailer (production isolation)
    pub use_jailer:     bool,     // USE_JAILER,      default false
    pub jailer_bin:     String,   // JAILER_BIN,      default /usr/local/bin/jailer
    pub jailer_uid:     u32,      // JAILER_UID,      default 10000
    pub jailer_gid:     u32,      // JAILER_GID,      default 10000
    pub chroot_base:    String,   // JAILER_CHROOT_BASE, default /srv/jailer
    // Node identity + artifact cache
    pub node_id:        String,   // NODE_ID,         default node-a
    pub artifact_dir:   String,   // ARTIFACT_DIR,    default /var/lib/liquid-metal/artifacts
    // Base Alpine template for rootfs assembly
    pub base_image_key:    String,          // BASE_IMAGE_KEY,    default templates/base-alpine-v1.ext4
    pub base_image_sha256: Option<String>,  // BASE_IMAGE_SHA256, optional integrity check
}

pub struct ProvisionedService {
    pub id:            Uuid,
    pub engine:        Engine,
    pub upstream_addr: Option<String>,
    /// S3 key prefix for snapshot files (Metal only, set after snapshot create).
    /// Format: `snapshots/{service_id}/{deploy_id}/`
    pub snapshot_key:  Option<String>,
    /// Metal dedicated VM tracking — written to DB for post-restart recovery.
    pub tap_name:      Option<String>,
    pub fc_pid:        Option<u32>,
}

/// Shared context passed into each provision task.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProvisionCtx {
    pub pool:     Arc<deadpool_postgres::Pool>,
    pub cfg:      Arc<ProvisionConfig>,
    pub s3:       Arc<aws_sdk_s3::Client>,
    pub bucket:   Arc<String>,
    pub registry:        deprovision::VmRegistry,
    pub liquid_registry: deprovision::LiquidRegistry,
    /// Plain NATS client for fire-and-forget RouteUpdated publishes.
    pub nats:     Arc<async_nats::Client>,
}

/// Populate the in-use TAP index set from the DB on daemon startup.
/// This allows `allocate_tap_index` to reclaim gaps left by deleted services
/// instead of monotonically increasing until the IP space is exhausted.
pub async fn init_tap_indices(pool: &deadpool_postgres::Pool, node_id: &str) {
    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "could not init TAP indices from DB — starting empty");
            return;
        }
    };

    let rows = db
        .query(
            "SELECT tap_name FROM services \
             WHERE node_id = $1 AND status IN ('running', 'provisioning') \
               AND engine = 'metal' AND deleted_at IS NULL AND tap_name IS NOT NULL",
            &[&node_id],
        )
        .await;

    match rows {
        Ok(rows) => {
            let indices: BTreeSet<u32> = rows
                .iter()
                .filter_map(|r| {
                    let name: String = r.get(0);
                    name.strip_prefix("tap")
                        .and_then(|n| n.parse::<u32>().ok())
                })
                .collect();

            let count = indices.len();
            *tap_indices().lock().await = indices;
            tracing::info!(node_id, in_use = count, "TAP index set initialized from DB");
        }
        Err(e) => tracing::warn!(error = %e, "TAP index DB query failed — starting empty"),
    }
}

/// Allocate the smallest available TAP index. Returns the index and marks it
/// as in-use. The caller must call `release_tap_index` on deprovision.
pub async fn allocate_tap_index() -> anyhow::Result<u32> {
    let mut set = tap_indices().lock().await;
    // Find the first gap: 0, 1, 2, ... that isn't in the set.
    let mut idx = 0u32;
    for &used in set.iter() {
        if idx < used { break; }
        idx = used + 1;
    }
    anyhow::ensure!(
        idx <= common::networking::MAX_TAP_INDEX,
        "node at capacity: all {} TAP indices in use — cannot provision more Metal VMs",
        common::networking::MAX_TAP_INDEX + 1
    );
    set.insert(idx);
    Ok(idx)
}

/// Release a TAP index back to the pool so it can be reused.
pub async fn release_tap_index(tap_name: &str) {
    if let Some(idx) = tap_name.strip_prefix("tap").and_then(|n| n.parse::<u32>().ok()) {
        tap_indices().lock().await.remove(&idx);
    }
}

/// On daemon startup, enumerate TAP devices attached to the bridge and delete
/// any that are not tracked in the DB. These are orphans left by a previous
/// daemon crash between TAP creation and the `services.tap_name` DB write.
///
/// Only runs on Linux — TAP/bridge primitives don't exist on macOS dev boxes.
#[cfg(target_os = "linux")]
pub async fn cleanup_orphaned_taps(
    pool: &deadpool_postgres::Pool,
    node_id: &str,
    bridge: &str,
) {
    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "orphan TAP cleanup: db pool error — skipping");
            return;
        }
    };

    // Collect all tap_names the DB knows about for this node.
    let rows = db
        .query(
            "SELECT tap_name FROM services \
             WHERE node_id = $1 \
               AND status IN ('running', 'provisioning') \
               AND engine = 'metal' \
               AND deleted_at IS NULL \
               AND tap_name IS NOT NULL",
            &[&node_id],
        )
        .await
        .unwrap_or_default();

    let known: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|r| r.get::<_, Option<String>>(0))
        .collect();

    // Read bridge member interfaces from the kernel's sysfs.
    // /sys/class/net/{bridge}/brif/ contains one directory entry per member.
    let brif_path = format!("/sys/class/net/{bridge}/brif");
    let entries = match std::fs::read_dir(&brif_path) {
        Ok(e)  => e,
        Err(e) => {
            tracing::warn!(error = %e, bridge, "could not read bridge interfaces — skipping orphan cleanup");
            return;
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Only touch tap* devices — skip veth pairs, the bridge itself, etc.
        if !name.starts_with("tap") {
            continue;
        }
        if known.contains(&name) {
            continue;
        }
        tracing::warn!(tap = name, bridge, "orphaned TAP device found — deleting");
        let status = tokio::process::Command::new("ip")
            .args(["link", "del", &name])
            .status()
            .await;
        match status {
            Ok(s) if s.success() => tracing::info!(tap = name, "orphaned TAP deleted"),
            Ok(s)  => tracing::warn!(tap = name, code = ?s.code(), "ip link del returned non-zero"),
            Err(e) => tracing::warn!(tap = name, error = %e, "ip link del failed"),
        }
    }
}

/// On daemon startup, delete artifact directories that don't belong to any
/// running or provisioning service on this node. These are orphans from
/// services that were deleted/stopped while the daemon was down, or from
/// provisions that failed after the download but before cleanup.
pub async fn cleanup_orphaned_artifacts(
    pool: &deadpool_postgres::Pool,
    node_id: &str,
    artifact_dir: &str,
) {
    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "orphan artifact cleanup: db pool error — skipping");
            return;
        }
    };

    let rows = db
        .query(
            "SELECT id::text FROM services \
             WHERE node_id = $1 \
               AND status IN ('running', 'provisioning') \
               AND deleted_at IS NULL",
            &[&node_id],
        )
        .await
        .unwrap_or_default();

    let known: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|r| r.get::<_, Option<String>>(0))
        .collect();

    let entries = match std::fs::read_dir(artifact_dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, artifact_dir, "could not read artifact dir — skipping cleanup");
            return;
        }
    };

    let mut removed = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if known.contains(&name) {
            continue;
        }
        let path = entry.path();
        if let Err(e) = tokio::fs::remove_dir_all(&path).await {
            tracing::debug!(path = %path.display(), error = %e, "orphan artifact removal failed");
        } else {
            removed += 1;
        }
    }

    if removed > 0 {
        tracing::info!(removed, artifact_dir, "cleaned up orphaned artifact directories");
    }
}

/// On daemon startup, remove cgroup directories under /sys/fs/cgroup/liquid-metal/
/// that don't belong to any running or provisioning service on this node.
/// Prevents accumulation of empty cgroup dirs from crash/restart cycles.
#[cfg(target_os = "linux")]
pub async fn cleanup_orphaned_cgroups(
    pool: &deadpool_postgres::Pool,
    node_id: &str,
) {
    const CGROUP_BASE: &str = "/sys/fs/cgroup/liquid-metal";

    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "orphan cgroup cleanup: db pool error — skipping");
            return;
        }
    };

    let rows = db
        .query(
            "SELECT id::text FROM services \
             WHERE node_id = $1 \
               AND status IN ('running', 'provisioning') \
               AND deleted_at IS NULL",
            &[&node_id],
        )
        .await
        .unwrap_or_default();

    let known: std::collections::HashSet<String> = rows
        .iter()
        .filter_map(|r| r.get::<_, Option<String>>(0))
        .collect();

    let entries = match std::fs::read_dir(CGROUP_BASE) {
        Ok(e)  => e,
        Err(_) => return, // cgroup base doesn't exist — nothing to clean
    };

    let mut removed = 0u32;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if known.contains(&name) {
            continue;
        }
        // rmdir (not remove_dir_all) — cgroup dirs must be empty.
        // If still populated, the rmdir fails harmlessly.
        if tokio::fs::remove_dir(entry.path()).await.is_ok() {
            removed += 1;
        }
    }

    if removed > 0 {
        tracing::info!(removed, "cleaned up orphaned cgroup directories");
    }
}

/// Log a warning if the partition containing `artifact_dir` exceeds 80% usage.
/// Helps operators catch disk pressure before provisions start failing with
/// opaque I/O errors. Linux-only (uses libc::statvfs).
#[cfg(target_os = "linux")]
pub fn check_disk_space(artifact_dir: &str) {
    use std::ffi::CString;

    let Ok(c_path) = CString::new(artifact_dir) else { return };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };

    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        tracing::debug!(artifact_dir, "statvfs failed — skipping disk check");
        return;
    }

    let total = stat.f_blocks as u64 * stat.f_frsize as u64;
    let avail = stat.f_bavail as u64 * stat.f_frsize as u64;
    if total == 0 { return; }

    let used_pct = ((total - avail) as f64 / total as f64) * 100.0;
    if used_pct > 80.0 {
        tracing::warn!(
            artifact_dir,
            used_pct = format!("{used_pct:.1}%"),
            avail_mb = avail / (1024 * 1024),
            "artifact partition above 80% — deploys may fail if disk fills"
        );
    } else {
        tracing::info!(
            artifact_dir,
            used_pct = format!("{used_pct:.1}%"),
            avail_mb = avail / (1024 * 1024),
            "artifact partition disk check OK"
        );
    }
}

/// No-op on non-Linux (macOS dev).
#[cfg(not(target_os = "linux"))]
pub fn check_disk_space(_artifact_dir: &str) {}

/// Fire-and-forget progress publish. Failures are silently dropped — a missed
/// progress event degrades CLI UX but never affects provisioning correctness.
async fn publish_progress(nats: &async_nats::Client, service_id: &str, step: DeployStep, message: &str) {
    if let Ok(data) = serde_json::to_vec(&DeployProgressEvent {
        service_id: service_id.to_string(),
        step,
        message: message.to_string(),
    }) {
        let subject = format!("{}.{}", SUBJECT_DEPLOY_PROGRESS, service_id);
        let _ = nats.publish(subject, data.into()).await;
    }
}

pub async fn provision(ctx: &ProvisionCtx, event: &ProvisionEvent) -> Result<ProvisionedService> {
    publish_progress(&ctx.nats, &event.service_id, DeployStep::Queued, "Picked up by daemon").await;

    let svc = match &event.spec {
        EngineSpec::Metal(spec)  => provision_metal(ctx, event, spec).await?,
        EngineSpec::Liquid(spec) => provision_liquid(ctx, event, spec).await?,
    };

    let svc_id: Uuid = event
        .service_id
        .parse()
        .context("invalid service_id UUID in event")?;

    // Write final status to the DB.
    //
    // Metal (dedicated VM): status='running', upstream_addr set, tap_name/fc_pid/vm_id
    //   persisted for post-restart registry recovery. VM stays alive permanently.
    //
    // Liquid: status='running', upstream_addr set, snapshot_key=NULL.
    //   The Wasm module is loaded in-process and actively serving.
    let (status, db_result) = if svc.snapshot_key.is_some() {
        // Snapshot path (unused in current model — kept for compatibility)
        let result = async {
            let db = ctx.pool.get().await.context("db pool")?;
            db.execute(
                "UPDATE services SET status = 'ready', node_id = $1, snapshot_key = $2 WHERE id = $3",
                &[&ctx.cfg.node_id, &svc.snapshot_key, &svc_id],
            )
            .await
            .context("writing snapshot_key + node_id to services")
        }
        .await;
        ("ready", result)
    } else {
        // Metal dedicated VM or Liquid — service is running.
        // For Metal: also persist tap_name, fc_pid, vm_id so the startup
        // registry re-population (main.rs) works after a daemon restart.
        let result = async {
            let db = ctx.pool.get().await.context("db pool")?;
            let vm_id_str = if svc.engine == Engine::Metal {
                Some(svc.id.to_string())
            } else {
                None
            };
            db.execute(
                "UPDATE services SET upstream_addr = $1, status = 'running', node_id = $2, \
                 tap_name = $3, fc_pid = $4, vm_id = $5 WHERE id = $6",
                &[&svc.upstream_addr, &ctx.cfg.node_id,
                  &svc.tap_name, &svc.fc_pid.map(|p| p as i32), &vm_id_str,
                  &svc_id],
            )
            .await
            .context("writing upstream_addr + node_id to services")
        }
        .await;
        ("running", result)
    };

    if let Err(e) = db_result {
        tracing::error!(
            service_id = event.service_id,
            %status,
            error = %e,
            "DB write failed after provision — tearing down to avoid resource leak"
        );
        rollback_provision(ctx, event, &e).await;
        return Err(e);
    }

    // Publish RouteUpdated so Pingora caches update without a DB round-trip.
    // Only published for running services (Liquid) — Metal services have no
    // upstream_addr until woken from snapshot (Phase 2).
    if let Some(ref addr) = svc.upstream_addr {
        if !event.slug.is_empty() {
            let payload = serde_json::to_vec(&RouteUpdatedEvent {
                slug:          event.slug.clone(),
                upstream_addr: addr.clone(),
            })
            .unwrap_or_default();
            if let Err(e) = ctx
                .nats
                .publish(SUBJECT_ROUTE_UPDATED, payload.into())
                .await
            {
                tracing::warn!(error = %e, slug = event.slug, "failed to publish RouteUpdated — proxy will fall back to DB");
            }
        }
    }

    Ok(svc)
}

/// Tear down resources allocated during provision when the final DB write fails.
///
/// The critical invariant: the Firecracker process must be killed *synchronously*
/// before any async cleanup. If the daemon crashes mid-rollback, the FC process
/// must already be dead. The startup sweep (`cleanup_stale_provisioning`) handles
/// any remaining kernel resources (TAP, cgroup, etc.) on next boot.
/// Inspect the error chain and decide whether the failure is worth retrying.
fn classify_error(err: &anyhow::Error) -> FailureKind {
    let msg = format!("{err:#}").to_lowercase();

    // Permanent failures — no point retrying
    if msg.contains("artifact integrity failed")
        || msg.contains("startup probe failed")
        || msg.contains("readiness probe failed")
        || msg.contains("wasm http readiness probe failed")
        || msg.contains("invalid elf")
        || msg.contains("wasm compile")
        || msg.contains("not a valid wasm module")
    {
        return FailureKind::Permanent;
    }

    // Everything else is transient by default — safe to retry
    FailureKind::Transient
}

pub async fn rollback_provision(ctx: &ProvisionCtx, event: &ProvisionEvent, err: &anyhow::Error) -> FailureKind {
    let reason = format!("{err:#}");
    let kind = classify_error(err);

    publish_progress(&ctx.nats, &event.service_id, DeployStep::Failed, &reason).await;

    // Best-effort: mark the service as 'failed' in the DB *before* cleanup.
    // Persist the failure reason and bump the attempt counter so the API/CLI
    // can show the user what went wrong.
    if let Ok(db) = ctx.pool.get().await {
        if let Ok(svc_id) = event.service_id.parse::<uuid::Uuid>() {
            let _ = db
                .execute(
                    "UPDATE services SET status = 'failed', failure_reason = $1, \
                     provision_attempts = provision_attempts + 1 \
                     WHERE id = $2 AND deleted_at IS NULL",
                    &[&reason, &svc_id],
                )
                .await;
        }
    }

    match &event.spec {
        EngineSpec::Metal(_) => {
            if let Some(handle) = ctx.registry.lock().await.remove(&event.service_id) {
                // SIGKILL immediately — this is the crash-safe part. Even if the
                // daemon dies on the next line, the FC process is already dead.
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::kill(handle.fc_pid as libc::pid_t, libc::SIGKILL);
                }

                release_tap_index(&handle.tap_name).await;
                deprovision::metal(
                    &event.service_id,
                    handle,
                    &ctx.cfg.artifact_dir,
                )
                .await;
            }
        }
        EngineSpec::Liquid(_) => {
            // Remove from billing registry. The wasm HTTP listener task continues
            // on localhost until the daemon restarts — harmless and unreachable
            // since no upstream_addr was written to the DB.
            ctx.liquid_registry.lock().await.remove(&event.service_id);
        }
    }

    kind
}

/// On daemon startup, find services stuck in `provisioning` or `failed` on this
/// node and clean them up. These are orphans from a previous daemon crash during
/// provision or rollback — the FC process may still be alive and kernel resources
/// (TAP, cgroup, eBPF) may still be allocated.
#[cfg(target_os = "linux")]
pub async fn cleanup_stale_provisioning(
    pool: &deadpool_postgres::Pool,
    node_id: &str,
    artifact_dir: &str,
) {
    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "stale provision cleanup: db pool error — skipping");
            return;
        }
    };

    // Find services stuck in provisioning/failed with VM metadata on this node.
    let rows = db
        .query(
            "SELECT id::text, tap_name, fc_pid, vm_id \
             FROM services \
             WHERE node_id = $1 \
               AND status IN ('provisioning', 'failed') \
               AND engine = 'metal' \
               AND deleted_at IS NULL \
               AND tap_name IS NOT NULL",
            &[&node_id],
        )
        .await
        .unwrap_or_default();

    if rows.is_empty() {
        return;
    }

    tracing::warn!(count = rows.len(), "found stale provisioning/failed services — cleaning up");

    for row in &rows {
        let service_id: String          = row.get(0);
        let tap_name: String            = row.get(1);
        let fc_pid: Option<i32>         = row.get(2);
        let vm_id: Option<String>       = row.get(3);

        // Kill the FC process if it's still alive.
        if let Some(pid) = fc_pid {
            if std::path::Path::new(&format!("/proc/{pid}")).exists() {
                tracing::warn!(service_id, fc_pid = pid, "killing orphaned Firecracker process");
                unsafe { libc::kill(pid, libc::SIGKILL); }
            }
        }

        // Mark as crashed in DB.
        if let Ok(svc_id) = service_id.parse::<uuid::Uuid>() {
            let _ = db
                .execute(
                    "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                     WHERE id = $1 AND deleted_at IS NULL",
                    &[&svc_id],
                )
                .await;
        }

        // Clean up kernel resources via the normal deprovision path.
        let handle = deprovision::VmHandle {
            tap_name:    tap_name.clone(),
            fc_pid:      fc_pid.unwrap_or(0) as u32,
            vm_id:       vm_id.unwrap_or_default(),
            use_jailer:  false,
            chroot_base: String::new(),
        };

        release_tap_index(&tap_name).await;
        deprovision::metal(&service_id, handle, artifact_dir).await;

        tracing::info!(service_id, "stale provisioning service cleaned up");
    }
}

async fn provision_metal(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
) -> Result<ProvisionedService> {
    let vm_id   = Uuid::now_v7();
    let tap_idx = allocate_tap_index().await?;
    let tap     = common::networking::tap_name(tap_idx);
    let ip      = common::networking::guest_ip(tap_idx)
        .context("TAP index pool exhausted")?;
    let upstream_addr = format!("{}:{}", ip, spec.port);

    tracing::info!(
        vm_id = %vm_id, tap, app = event.app_name,
        %upstream_addr, use_jailer = ctx.cfg.use_jailer,
        "provisioning metal VM"
    );

    #[cfg(target_os = "linux")]
    {
        // Ensure the base Alpine template is cached locally (downloads from S3 on first call).
        let template_path = rootfs::ensure_template(
            &ctx.s3, &ctx.bucket, &ctx.cfg.base_image_key,
            &ctx.cfg.artifact_dir, ctx.cfg.base_image_sha256.as_deref(),
        ).await.context("ensuring base Alpine template")?;

        // Build a bootable rootfs: copy template, download + inject user binary + env vars.
        publish_progress(&ctx.nats, &event.service_id, DeployStep::Downloading, "Downloading user binary from object storage").await;
        publish_progress(&ctx.nats, &event.service_id, DeployStep::Building, "Assembling rootfs image").await;
        let local_rootfs = rootfs::build_rootfs(
            &ctx.s3, &ctx.bucket, &spec.artifact_key, spec.artifact_sha256.as_deref(),
            &event.service_id, &ctx.cfg.artifact_dir, &event.env_vars,
            &template_path, spec.port, &ctx.cfg.node_id,
        ).await.context("building rootfs")?;

        let rootfs_path = local_rootfs.to_string_lossy().into_owned();
        let mac         = format!("AA:FC:00:00:{:02X}:{:02X}", tap_idx >> 8, tap_idx & 0xFF);
        let serial_log  = PathBuf::from(&ctx.cfg.artifact_dir)
            .join(&event.service_id)
            .join("serial.log");
        let serial_log_str = serial_log.to_string_lossy().into_owned();

        netlink::create_tap(&tap).context("create TAP")?;

        publish_progress(&ctx.nats, &event.service_id, DeployStep::Booting, "Booting Firecracker VM").await;

        // Everything after TAP creation must clean up the TAP device + index
        // on failure, otherwise they leak until the next daemon restart.
        let result = provision_metal_after_tap(
            ctx, event, spec, &tap, &rootfs_path, &mac, vm_id, &serial_log_str, &ip,
        ).await;

        if let Err(ref e) = result {
            tracing::warn!(tap = tap, error = %e, "metal provision failed after TAP creation — cleaning up");
            release_tap_index(&tap).await;
            if let Err(del_err) = netlink::delete_tap(&tap).await {
                tracing::warn!(tap = tap, error = %del_err, "failed to delete leaked TAP device");
            }
            return Err(result.unwrap_err());
        }

        let fc_pid = result.unwrap();

        // Startup readiness probe: HTTP GET / to confirm the binary is listening
        // and speaking HTTP. Without this, a binary that boots and immediately
        // crashes leaves the service stuck in "running" state returning errors.
        publish_progress(&ctx.nats, &event.service_id, DeployStep::HealthCheck, "Waiting for service to respond to HTTP requests").await;
        if let Err(e) = startup_probe(&upstream_addr, Some(&serial_log_str)).await {
            tracing::error!(
                service_id = event.service_id,
                %upstream_addr,
                error = %e,
                "startup probe failed — binary did not bind its port"
            );
            publish_progress(&ctx.nats, &event.service_id, DeployStep::Failed, &format!("Startup probe failed: {e}")).await;
            // Kill the VM and clean up — don't leave a ghost running.
            unsafe { libc::kill(fc_pid as libc::pid_t, libc::SIGKILL); }
            release_tap_index(&tap).await;
            deprovision::metal(
                &event.service_id,
                deprovision::VmHandle {
                    tap_name:    tap.clone(),
                    fc_pid,
                    vm_id:       vm_id.to_string(),
                    use_jailer:  ctx.cfg.use_jailer,
                    chroot_base: ctx.cfg.chroot_base.clone(),
                },
                &ctx.cfg.artifact_dir,
            ).await;
            return Err(e);
        }

        // ── Dedicated VM — register handle and stay running ────────────────
        // The startup probe passed — the VM is healthy. For dedicated Metal VMs,
        // we keep the VM running permanently (no snapshot, no scale-to-zero).
        // Register the handle so deprovision and crash-watcher can tear it down later.
        ctx.registry.lock().await.insert(
            event.service_id.clone(),
            deprovision::VmHandle {
                tap_name:    tap.clone(),
                fc_pid,
                vm_id:       vm_id.to_string(),
                use_jailer:  ctx.cfg.use_jailer,
                chroot_base: ctx.cfg.chroot_base.clone(),
            },
        );

        tracing::info!(
            vm_id = %vm_id, %upstream_addr, service_id = event.service_id,
            "metal VM running — dedicated, always on"
        );
        publish_progress(&ctx.nats, &event.service_id, DeployStep::Running,
            "VM is live — dedicated Metal service running").await;

        return Ok(ProvisionedService {
            id: vm_id,
            engine: Engine::Metal,
            upstream_addr: Some(upstream_addr),
            snapshot_key: None,
            tap_name: Some(tap),
            fc_pid: Some(fc_pid),
        });
    }

    // Non-Linux dev
    #[allow(unreachable_code)]
    Ok(ProvisionedService {
        id: vm_id,
        engine: Engine::Metal,
        upstream_addr: None,
        snapshot_key: None,
        tap_name: None,
        fc_pid: None,
    })
}

/// Inner helper for Metal provisioning steps that occur after TAP creation.
/// Returns `fc_pid` on success. Separated so `provision_metal`
/// can clean up the TAP device + index if any step here fails.
#[cfg(target_os = "linux")]
async fn provision_metal_after_tap(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
    tap: &str,
    rootfs_path: &str,
    mac: &str,
    vm_id: Uuid,
    serial_log: &str,
    guest_ip: &str,
) -> Result<u32> {
    netlink::attach_to_bridge(tap, &ctx.cfg.bridge).await.context("attach TAP to bridge")?;
    tc::apply(tap, &spec.quota).await.context("tc bandwidth")?;
    ebpf::attach(tap, &event.service_id).context("eBPF TC attach")?;

    let (fc_pid, sock_path, rootfs_guest, kernel_guest) = if ctx.cfg.use_jailer {
        let (pid, paths) = jailer::spawn(&jailer::JailerConfig {
            vm_id:       &vm_id.to_string(),
            jailer_bin:  &ctx.cfg.jailer_bin,
            fc_bin:      &ctx.cfg.fc_bin,
            uid:         ctx.cfg.jailer_uid,
            gid:         ctx.cfg.jailer_gid,
            chroot_base: &ctx.cfg.chroot_base,
            rootfs_src:  rootfs_path,
            kernel_src:  &ctx.cfg.kernel_path,
        }, serial_log)
        .await
        .context("jailer spawn")?;
        (pid, paths.socket, paths.rootfs_guest.to_string(), paths.kernel_guest.to_string())
    } else {
        let sock = format!("{}/{}.sock", ctx.cfg.sock_dir, vm_id);
        let pid = spawn_firecracker_direct(&ctx.cfg.fc_bin, &vm_id.to_string(), &sock, serial_log).await?;
        (pid, sock, rootfs_path.to_string(), ctx.cfg.kernel_path.clone())
    };

    cgroup::apply(&event.service_id, fc_pid, &ctx.cfg.rootfs_dev, &spec.quota, spec.memory_mb)
        .await
        .context("cgroup limits")?;

    // No CPU pinning — the Linux CFS scheduler shares cores across all VMs.
    // This allows overselling: 20 VMs can timeshare 6 cores because most are
    // idle most of the time. Performance degrades gracefully under contention.

    firecracker::start_vm(&firecracker::VmConfig {
        sock_path:   &sock_path,
        vcpu:        spec.vcpu,
        memory_mb:   spec.memory_mb,
        kernel_path: &kernel_guest,
        rootfs_path: &rootfs_guest,
        tap_name:    tap,
        guest_mac:   mac,
        guest_ip,
        gateway:     common::networking::GATEWAY,
    })
    .await
    .context("start VM")?;

    Ok(fc_pid)
}

async fn provision_liquid(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::LiquidSpec,
) -> Result<ProvisionedService> {
    let id = Uuid::now_v7();

    let local_wasm = PathBuf::from(&ctx.cfg.artifact_dir)
        .join(&event.service_id)
        .join("main.wasm");

    publish_progress(&ctx.nats, &event.service_id, DeployStep::Downloading, "Downloading Wasm module from object storage").await;
    storage::download(&ctx.s3, &ctx.bucket, &spec.artifact_key, &local_wasm)
        .await
        .context("downloading wasm from Object Storage")?;

    let wasm_path = local_wasm.to_string_lossy().into_owned();

    if let Some(expected) = &spec.artifact_sha256 {
        publish_progress(&ctx.nats, &event.service_id, DeployStep::Verifying, "Verifying artifact integrity").await;
        verify::artifact(&wasm_path, expected)
            .await
            .context("wasm integrity check")?;
    } else {
        tracing::warn!(service_id = %event.service_id, "no artifact_sha256 provided — skipping wasm integrity check");
    }

    tracing::info!(service_id = %id, app = event.app_name, "provisioning liquid (wasm)");

    // Persist metadata for serverless wake — env vars and app name are needed
    // to re-start the Wasm shim after scale-to-zero idle teardown.
    let metadata = serde_json::json!({
        "app_name": event.app_name,
        "env_vars": event.env_vars,
        "tenant_id": event.tenant_id,
    });
    let metadata_path = PathBuf::from(&ctx.cfg.artifact_dir)
        .join(&event.service_id)
        .join("metadata.json");
    tokio::fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap_or_default())
        .await
        .context("writing liquid metadata for wake recovery")?;

    // Invocation counter — shared with wasm_http::dispatch (writes) and usage reporter (reads).
    let invocations = Arc::new(AtomicU64::new(0));

    publish_progress(&ctx.nats, &event.service_id, DeployStep::Starting, "Starting Wasmtime runtime").await;
    // Compile the module once and start a per-request HTTP shim on a free port.
    // Pingora routes to 127.0.0.1:{port} as upstream_addr.
    let port = wasm_http::serve(wasm_path, event.app_name.clone(), invocations.clone(), event.env_vars.clone())
        .await
        .context("starting wasm HTTP shim")?;

    // Probe: confirm the first Wasm invocation succeeds — not just that the port is bound.
    // wasm_http::serve() spawns the accept loop in a background task, so the port may be
    // returned before the listener is ready. Any HTTP response (200/404/500) proves the
    // shim is alive and the module compiled and dispatches without panicking.
    let upstream_addr = format!("127.0.0.1:{port}");
    publish_progress(&ctx.nats, &event.service_id, DeployStep::HealthCheck, "Verifying Wasm module responds to requests").await;
    startup_probe(&upstream_addr, None)
        .await
        .context("wasm HTTP readiness probe failed")?;

    // Register for billing usage reporting.
    ctx.liquid_registry.lock().await.insert(
        event.service_id.clone(),
        deprovision::LiquidHandle {
            workspace_id: event.tenant_id.clone(),
            invocations,
        },
    );

    tracing::info!(service_id = %id, %upstream_addr, "liquid wasm ready");
    publish_progress(&ctx.nats, &event.service_id, DeployStep::Running, "Wasm module is live").await;

    Ok(ProvisionedService { id, engine: Engine::Liquid, upstream_addr: Some(upstream_addr), snapshot_key: None, tap_name: None, fc_pid: None })
}

/// HTTP startup probe: poll upstream_addr with `GET /` until any HTTP response is received
/// (alive = HTTP layer responded, even 4xx/5xx) or `STARTUP_PROBE_TIMEOUT` expires.
/// `serial_log` is read on timeout for Metal VMs; pass `None` for Liquid (no serial console).
async fn startup_probe(upstream_addr: &str, serial_log: Option<&str>) -> Result<()> {
    use hyper::Request;
    use hyper::client::conn::http1;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpStream;

    let deadline = tokio::time::Instant::now() + *STARTUP_PROBE_TIMEOUT;
    let mut interval = tokio::time::interval(Duration::from_millis(500));

    tracing::info!(upstream_addr, timeout_secs = STARTUP_PROBE_TIMEOUT.as_secs(), "startup probe: waiting for HTTP response");

    loop {
        interval.tick().await;

        // Attempt HTTP GET / — any response (200, 404, 500) proves HTTP layer is alive.
        let probe_result: Result<()> = async {
            let stream = TcpStream::connect(upstream_addr).await?;
            let io = TokioIo::new(stream);
            let (mut sender, conn) = http1::handshake(io).await?;
            tokio::spawn(conn);
            let req = Request::get("/")
                .header("host", upstream_addr)
                .body(http_body_util::Empty::<hyper::body::Bytes>::new())?;
            sender.send_request(req).await?;
            Ok(())
        }
        .await;

        match probe_result {
            Ok(()) => {
                tracing::info!(upstream_addr, "startup probe: HTTP layer responding");
                return Ok(());
            }
            Err(_) if tokio::time::Instant::now() >= deadline => {
                let timeout_secs = STARTUP_PROBE_TIMEOUT.as_secs();
                let serial_section = if let Some(log_path) = serial_log {
                    let serial_output = read_tail(log_path, *STARTUP_PROBE_LOG_LINES).await;
                    format!(
                        "\n\nLast {lines} lines of serial console (kernel + app output):\n\
                         ───────────────────────────────────────\n\
                         {serial_output}\n\
                         ───────────────────────────────────────",
                        lines = STARTUP_PROBE_LOG_LINES.min(serial_output.lines().count()),
                    )
                } else {
                    String::new()
                };
                anyhow::bail!(
                    "startup probe failed: service did not respond to HTTP GET / on {upstream_addr} within {timeout_secs}s\n\n\
                     Common causes:\n\
                     \x20 - Application crashed on startup (panic, missing env var, segfault)\n\
                     \x20 - Application listening on wrong port (check [service].port in liquid-metal.toml)\n\
                     \x20 - HTTP server not started — application bound TCP but never spoke HTTP\n\
                     \x20 - Binary is glibc-linked on a musl rootfs (should have been caught by ELF check){serial_section}"
                );
            }
            Err(_) => {
                // Not yet — keep polling
            }
        }
    }
}

/// Read the last `n` lines from a file. Best-effort — returns empty string on error.
async fn read_tail(path: &str, n: usize) -> String {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].join("\n")
        }
        Err(_) => "(serial log not available)".to_string(),
    }
}

#[cfg(target_os = "linux")]
pub async fn spawn_firecracker_direct(fc_bin: &str, vm_id: &str, sock_path: &str, serial_log: &str) -> Result<u32> {
    let sock_dir = sock_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/run/firecracker");
    tokio::fs::create_dir_all(sock_dir).await.context("mkdir sock dir")?;

    // Capture serial console (ttyS0) output for debugging startup failures.
    let serial_file = std::fs::File::create(serial_log)
        .with_context(|| format!("creating serial log: {serial_log}"))?;
    let stderr_file = serial_file.try_clone()
        .context("cloning serial log file for stderr")?;

    let mut child = tokio::process::Command::new(fc_bin)
        .args(["--api-sock", sock_path, "--id", vm_id])
        .stdout(serial_file)
        .stderr(stderr_file)
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawning {}", fc_bin))?;

    let pid = child.id().context("FC process exited immediately")?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if tokio::fs::metadata(sock_path).await.is_ok() { return Ok(pid); }
        if tokio::time::Instant::now() >= deadline {
            // Kill the entire process group to avoid leaking the Firecracker process.
            unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL); }
            let _ = child.wait().await;
            anyhow::bail!("firecracker socket {} not ready within 3s — process killed", sock_path);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

