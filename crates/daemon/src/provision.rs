use crate::{deprovision, storage, verify, wasm_http};
use anyhow::{Context, Result};
use common::events::{Engine, EngineSpec, ProvisionEvent, RouteUpdatedEvent, SUBJECT_ROUTE_UPDATED};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use crate::{cgroup, cpu, ebpf, firecracker, jailer, netlink, tc};

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
    pub physical_cores: u32,      // PHYSICAL_CORES,  default 4
    // Jailer (production isolation)
    pub use_jailer:     bool,     // USE_JAILER,      default false
    pub jailer_bin:     String,   // JAILER_BIN,      default /usr/local/bin/jailer
    pub jailer_uid:     u32,      // JAILER_UID,      default 10000
    pub jailer_gid:     u32,      // JAILER_GID,      default 10000
    pub chroot_base:    String,   // JAILER_CHROOT_BASE, default /srv/jailer
    // Node identity + artifact cache
    pub node_id:        String,   // NODE_ID,         default node-a
    pub artifact_dir:   String,   // ARTIFACT_DIR,    default /var/lib/liquid-metal/artifacts
}

pub struct ProvisionedService {
    pub id:            Uuid,
    pub engine:        Engine,
    pub upstream_addr: Option<String>,
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
async fn allocate_tap_index() -> u32 {
    let mut set = tap_indices().lock().await;
    // Find the first gap: 0, 1, 2, ... that isn't in the set.
    let mut idx = 0u32;
    for &used in set.iter() {
        if idx < used { break; }
        idx = used + 1;
    }
    set.insert(idx);
    idx
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

pub async fn provision(ctx: &ProvisionCtx, event: &ProvisionEvent) -> Result<ProvisionedService> {
    let svc = match &event.spec {
        EngineSpec::Metal(spec)  => provision_metal(ctx, event, spec).await?,
        EngineSpec::Liquid(spec) => provision_liquid(ctx, event, spec).await?,
    };

    let svc_id: Uuid = event
        .service_id
        .parse()
        .context("invalid service_id UUID in event")?;

    // Write status='running' to the DB. If this fails, the VM/Wasm is running
    // but untracked — clean up all resources before propagating the error.
    let db_result = async {
        let db = ctx.pool.get().await.context("db pool")?;
        db.execute(
            "UPDATE services SET upstream_addr = $1, status = 'running', node_id = $2 WHERE id = $3",
            &[&svc.upstream_addr, &ctx.cfg.node_id, &svc_id],
        )
        .await
        .context("writing upstream_addr + node_id to services")
    }
    .await;

    if let Err(e) = db_result {
        tracing::error!(
            service_id = event.service_id,
            error = %e,
            "DB write failed after provision — tearing down to avoid resource leak"
        );
        rollback_provision(ctx, event).await;
        return Err(e);
    }

    // Publish RouteUpdated so Pingora caches update without a DB round-trip.
    // Fire-and-forget — a failure here is logged but does not fail the provision.
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
/// Without this, the VM/Wasm runs indefinitely but the service stays stuck in
/// `provisioning` — the watchdog marks it `failed` but can never kill the process.
async fn rollback_provision(ctx: &ProvisionCtx, event: &ProvisionEvent) {
    match &event.spec {
        EngineSpec::Metal(_) => {
            if let Some(handle) = ctx.registry.lock().await.remove(&event.service_id) {
                release_tap_index(&handle.tap_name).await;
                deprovision::metal(
                    &event.service_id,
                    handle,
                    ctx.cfg.physical_cores,
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
}

async fn provision_metal(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
) -> Result<ProvisionedService> {
    let vm_id   = Uuid::now_v7();
    let tap_idx = allocate_tap_index().await;
    let tap     = common::networking::tap_name(tap_idx);
    let ip      = common::networking::guest_ip(tap_idx);
    let upstream_addr = format!("{}:{}", ip, spec.port);

    // Download rootfs artifact from Object Storage
    let local_rootfs = PathBuf::from(&ctx.cfg.artifact_dir)
        .join(&event.service_id)
        .join("rootfs.ext4");

    storage::download(&ctx.s3, &ctx.bucket, &spec.artifact_key, &local_rootfs)
        .await
        .context("downloading rootfs from Object Storage")?;

    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(local_rootfs.to_str().unwrap_or(""), expected)
            .await
            .context("rootfs integrity check")?;
    }

    tracing::info!(
        vm_id = %vm_id, tap, app = event.app_name,
        %upstream_addr, use_jailer = ctx.cfg.use_jailer,
        "provisioning metal VM"
    );

    #[cfg(target_os = "linux")]
    {
        let rootfs_path = local_rootfs.to_string_lossy().into_owned();
        let mac         = format!("AA:FC:00:00:{:02X}:{:02X}", tap_idx >> 8, tap_idx & 0xFF);
        let cpu_core = cpu::next_core(ctx.cfg.physical_cores);

        netlink::create_tap(&tap).context("create TAP")?;
        netlink::attach_to_bridge(&tap, &ctx.cfg.bridge).await.context("attach TAP to bridge")?;
        tc::apply(&tap, &spec.quota).await.context("tc bandwidth")?;
        ebpf::attach(&tap, &event.service_id).context("eBPF TC attach")?;

        let (fc_pid, sock_path, rootfs_guest, kernel_guest) = if ctx.cfg.use_jailer {
            let (pid, paths) = jailer::spawn(&jailer::JailerConfig {
                vm_id:       &vm_id.to_string(),
                jailer_bin:  &ctx.cfg.jailer_bin,
                fc_bin:      &ctx.cfg.fc_bin,
                uid:         ctx.cfg.jailer_uid,
                gid:         ctx.cfg.jailer_gid,
                chroot_base: &ctx.cfg.chroot_base,
                rootfs_src:  &rootfs_path,
                kernel_src:  &ctx.cfg.kernel_path,
            })
            .await
            .context("jailer spawn")?;
            (pid, paths.socket, paths.rootfs_guest.to_string(), paths.kernel_guest.to_string())
        } else {
            let sock = format!("{}/{}.sock", ctx.cfg.sock_dir, vm_id);
            let pid = spawn_firecracker_direct(&ctx.cfg.fc_bin, &vm_id.to_string(), &sock).await?;
            (pid, sock, rootfs_path.clone(), ctx.cfg.kernel_path.clone())
        };

        let cg_path = format!("/sys/fs/cgroup/liquid-metal/{}", event.service_id);

        cgroup::apply(&event.service_id, fc_pid, &ctx.cfg.rootfs_dev, &spec.quota)
            .await
            .context("cgroup io.max")?;

        cpu::pin_cgroup(&cg_path, cpu_core)
            .await
            .context("cpuset pin")?;

        cpu::offline_smt_sibling(cpu_core, ctx.cfg.physical_cores)
            .await
            .context("offline SMT sibling")?;

        firecracker::start_vm(&firecracker::VmConfig {
            sock_path:   &sock_path,
            vcpu:        spec.vcpu,
            memory_mb:   spec.memory_mb,
            kernel_path: &kernel_guest,
            rootfs_path: &rootfs_guest,
            tap_name:    &tap,
            guest_mac:   &mac,
        })
        .await
        .context("start VM")?;

        // Register in-memory for deprovision
        ctx.registry.lock().await.insert(
            event.service_id.clone(),
            deprovision::VmHandle {
                tap_name:    tap.clone(),
                fc_pid,
                cpu_core,
                vm_id:       vm_id.to_string(),
                use_jailer:  ctx.cfg.use_jailer,
                chroot_base: ctx.cfg.chroot_base.clone(),
            },
        );

        // Persist VM metadata to DB for post-restart deprovision recovery.
        if let Ok(db) = ctx.pool.get().await {
            let svc_id: Uuid = event.service_id.parse().unwrap_or(Uuid::nil());
            let fc_pid_i32 = fc_pid as i32;
            let cpu_core_i32 = cpu_core as i32;
            let vm_id_str = vm_id.to_string();
            let _ = db
                .execute(
                    "UPDATE services SET tap_name = $1, fc_pid = $2, cpu_core = $3, vm_id = $4 WHERE id = $5",
                    &[&tap, &fc_pid_i32, &cpu_core_i32, &vm_id_str, &svc_id],
                )
                .await;
        }

        tracing::info!(vm_id = %vm_id, %upstream_addr, fc_pid, cpu_core, "metal VM ready");

        return Ok(ProvisionedService {
            id: vm_id,
            engine: Engine::Metal,
            upstream_addr: Some(upstream_addr),
        });
    }

    // Non-Linux dev
    #[allow(unreachable_code)]
    Ok(ProvisionedService {
        id: vm_id,
        engine: Engine::Metal,
        upstream_addr: Some(upstream_addr),
    })
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

    storage::download(&ctx.s3, &ctx.bucket, &spec.artifact_key, &local_wasm)
        .await
        .context("downloading wasm from Object Storage")?;

    let wasm_path = local_wasm.to_string_lossy().into_owned();

    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(&wasm_path, expected)
            .await
            .context("wasm integrity check")?;
    }

    tracing::info!(service_id = %id, app = event.app_name, "provisioning liquid (wasm)");

    // Invocation counter — shared with wasm_http::dispatch (writes) and usage reporter (reads).
    let invocations = Arc::new(AtomicU64::new(0));

    // Compile the module once and start a per-request HTTP shim on a free port.
    // Pingora routes to 127.0.0.1:{port} as upstream_addr.
    let port = wasm_http::serve(wasm_path, event.app_name.clone(), invocations.clone())
        .await
        .context("starting wasm HTTP shim")?;

    // Register for billing usage reporting.
    ctx.liquid_registry.lock().await.insert(
        event.service_id.clone(),
        deprovision::LiquidHandle {
            workspace_id: event.tenant_id.clone(),
            invocations,
        },
    );

    let upstream_addr = format!("127.0.0.1:{port}");
    tracing::info!(service_id = %id, %upstream_addr, "liquid wasm ready");

    Ok(ProvisionedService { id, engine: Engine::Liquid, upstream_addr: Some(upstream_addr) })
}

#[cfg(target_os = "linux")]
async fn spawn_firecracker_direct(fc_bin: &str, vm_id: &str, sock_path: &str) -> Result<u32> {
    let sock_dir = sock_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/run/firecracker");
    tokio::fs::create_dir_all(sock_dir).await.context("mkdir sock dir")?;

    let mut child = tokio::process::Command::new(fc_bin)
        .args(["--api-sock", sock_path, "--id", vm_id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

