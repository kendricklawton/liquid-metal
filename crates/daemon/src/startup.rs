//! Daemon startup: PID lock, config, pools, cleanup, and registry rebuild.
//!
//! Everything that happens once before background tasks are spawned.

use std::sync::Arc;

use anyhow::{Context, Result};
use common::config::env_or;
use crate::{deprovision, provision::{self, ProvisionConfig}};

/// Acquire the PID file lock to prevent two daemons on the same node.
/// Returns the `File` handle — must be kept alive for the process lifetime.
#[cfg(target_os = "linux")]
pub fn acquire_pid_lock(pid_file_path: &str) -> Result<std::fs::File> {
    use std::io::Write;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(pid_file_path)
        .with_context(|| format!("opening PID file {pid_file_path}"))?;
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
    let lock_result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if lock_result != 0 {
        anyhow::bail!(
            "another daemon is already running (PID file {pid_file_path} is locked). \
             If this is stale, delete the file and retry."
        );
    }
    let mut file = file;
    writeln!(file, "{}", std::process::id())?;
    Ok(file)
}

/// Non-Linux stub — no PID locking needed outside production.
#[cfg(not(target_os = "linux"))]
pub fn acquire_pid_lock(_pid_file_path: &str) -> Result<()> {
    Ok(())
}

/// Build `ProvisionConfig` from environment variables.
pub fn build_config() -> Arc<ProvisionConfig> {
    Arc::new(ProvisionConfig {
        fc_bin:      env_or("FC_BIN",         "/usr/local/bin/firecracker"),
        kernel_path: env_or("FC_KERNEL_PATH", "/opt/firecracker/vmlinux"),
        sock_dir:    env_or("FC_SOCK_DIR",    "/run/firecracker"),
        bridge:      env_or("BRIDGE",         "br0"),
        rootfs_dev:     env_or("ROOTFS_DEVICE",  "8:0"),
        use_jailer:  env_or("USE_JAILER",          "false") == "true",
        jailer_bin:  env_or("JAILER_BIN",           "/usr/local/bin/jailer"),
        jailer_uid:  env_or("JAILER_UID",           "10000").parse().unwrap_or(10000),
        jailer_gid:  env_or("JAILER_GID",           "10000").parse().unwrap_or(10000),
        chroot_base: env_or("JAILER_CHROOT_BASE",   "/srv/jailer"),
        node_id:      env_or("NODE_ID",      "node-a"),
        artifact_dir: env_or("ARTIFACT_DIR", "/var/lib/liquid-metal/artifacts"),
        base_image_key:    env_or("BASE_IMAGE_KEY", "templates/base-alpine-v1.ext4"),
        base_image_sha256: std::env::var("BASE_IMAGE_SHA256").ok(),
    })
}

/// Create the Postgres connection pool.
pub async fn build_pool(db_url: &str) -> Result<Arc<deadpool_postgres::Pool>> {
    let pool_size: usize = env_or("DATABASE_POOL_SIZE", "8").parse().unwrap_or(8);
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let pool = if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
        deadpool_postgres::Pool::builder(mgr)
            .max_size(pool_size)
            .build()
            .context("building postgres pool (TLS)")?
    } else {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(mgr)
            .max_size(pool_size)
            .build()
            .context("building postgres pool")?
    };
    tracing::info!(pool_size, "postgres pool configured");
    Ok(Arc::new(pool))
}

/// Run all startup cleanup:
/// - Finalize orphaned draining services
/// - Mark stale Liquid services as stopped
/// - Init TAP indices
/// - Cleanup orphaned TAPs, artifacts, cgroups
/// - Cleanup stale provisioning
/// - Disk space check
///
/// Returns the slugs of stale liquid services (for route eviction after NATS connects).
pub async fn run_cleanup(
    pool: &deadpool_postgres::Pool,
    cfg: &ProvisionConfig,
) -> Vec<String> {
    let stale_liquid_slugs: Vec<String> = if let Ok(db) = pool.get().await {
        // Finalize orphaned drains — daemon crashed between drain start and suspend.
        let draining = db
            .execute(
                "UPDATE services SET status = 'suspended', upstream_addr = NULL \
                 WHERE node_id = $1 AND status = 'draining' AND deleted_at IS NULL",
                &[&cfg.node_id],
            )
            .await
            .unwrap_or(0);
        if draining > 0 {
            tracing::info!(count = draining, "finalized orphaned draining services → suspended");
        }

        let rows = db
            .query(
                "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                 WHERE node_id = $1 AND engine = 'liquid' AND status = 'running' AND deleted_at IS NULL \
                 RETURNING slug",
                &[&cfg.node_id],
            )
            .await
            .unwrap_or_default();
        let slugs: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
        if !slugs.is_empty() {
            tracing::info!(count = slugs.len(), "marked stale Liquid services as stopped (listeners died with previous daemon)");
        }
        slugs
    } else {
        vec![]
    };

    // Initialize in-use TAP index set from DB
    provision::init_tap_indices(pool, &cfg.node_id).await;

    // Delete orphaned TAP devices from the bridge
    #[cfg(target_os = "linux")]
    provision::cleanup_orphaned_taps(pool, &cfg.node_id, &cfg.bridge).await;

    // Delete orphaned artifact directories
    provision::cleanup_orphaned_artifacts(pool, &cfg.node_id, &cfg.artifact_dir).await;

    // Delete orphaned cgroup directories
    #[cfg(target_os = "linux")]
    provision::cleanup_orphaned_cgroups(pool, &cfg.node_id).await;

    // Kill orphaned Firecracker processes and clean up stale provisioning
    #[cfg(target_os = "linux")]
    provision::cleanup_stale_provisioning(pool, &cfg.node_id, &cfg.artifact_dir).await;

    // Warn if artifact partition is running low on disk space
    provision::check_disk_space(&cfg.artifact_dir);

    stale_liquid_slugs
}

/// Check clock drift between local time and Postgres `NOW()`.
pub async fn check_clock_drift(pool: &deadpool_postgres::Pool) {
    if let Ok(db) = pool.get().await {
        if let Ok(row) = db
            .query_one("SELECT EXTRACT(EPOCH FROM NOW())::float8", &[])
            .await
        {
            let pg_epoch: f64 = row.get(0);
            let local_epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64();
            let drift = (local_epoch - pg_epoch).abs() as u64;
            if drift > 60 {
                tracing::error!(drift_secs = drift, "CRITICAL clock skew between daemon and Postgres — idle timeouts will be unreliable");
            } else if drift > 5 {
                tracing::warn!(drift_secs = drift, "clock skew between daemon and Postgres exceeds 5s — check NTP/chrony");
            } else {
                tracing::info!(drift_secs = drift, "clock drift check passed");
            }
        }
    }
}

/// Re-attach eBPF isolation filters for running VMs from a previous daemon instance.
#[cfg(target_os = "linux")]
pub async fn reattach_ebpf(pool: &deadpool_postgres::Pool, cfg: &ProvisionConfig) {
    use crate::ebpf;

    if let Ok(db) = pool.get().await {
        let rows = db
            .query(
                "SELECT tap_name, id::text FROM services \
                 WHERE node_id = $1 AND status = 'running' \
                   AND engine = 'metal' AND deleted_at IS NULL \
                   AND tap_name IS NOT NULL",
                &[&cfg.node_id],
            )
            .await
            .unwrap_or_default();

        let taps: Vec<(String, String)> = rows
            .iter()
            .filter_map(|r| {
                let tap: Option<String> = r.get(0);
                let svc: Option<String> = r.get(1);
                tap.zip(svc)
            })
            .collect();

        if !taps.is_empty() {
            tracing::info!(count = taps.len(), "re-attaching eBPF filters for running VMs");
            let failed = ebpf::reattach_all(&taps);

            // Kill any VM whose eBPF filter could not be restored — running
            // without tenant isolation is not acceptable.
            for (tap_name, service_id) in &failed {
                tracing::error!(service_id, tap = tap_name.as_str(), "killing unisolated VM");
                if let Ok(svc_id) = service_id.parse::<uuid::Uuid>() {
                    db.execute(
                        "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                         WHERE id = $1 AND deleted_at IS NULL",
                        &[&svc_id],
                    )
                    .await
                    .ok();
                }
                if let Some(row) = db
                    .query_opt(
                        "SELECT fc_pid FROM services WHERE id::text = $1 AND fc_pid IS NOT NULL",
                        &[service_id],
                    )
                    .await
                    .ok()
                    .flatten()
                {
                    let pid: i32 = row.get(0);
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                }
            }
        }
    }
}

/// Non-Linux stub.
#[cfg(not(target_os = "linux"))]
pub async fn reattach_ebpf(_pool: &deadpool_postgres::Pool, _cfg: &ProvisionConfig) {}

/// Rebuild VM registry from DB — services provisioned by a previous daemon
/// instance need handles so deprovision events can clean them up.
pub async fn rebuild_registry(
    pool: &deadpool_postgres::Pool,
    cfg: &ProvisionConfig,
    registry: &deprovision::VmRegistry,
) {
    if let Ok(db) = pool.get().await {
        let rows = db
            .query(
                "SELECT id::text, tap_name, fc_pid, vm_id \
                 FROM services \
                 WHERE node_id = $1 AND status = 'running' \
                   AND engine = 'metal' AND deleted_at IS NULL \
                   AND tap_name IS NOT NULL AND fc_pid IS NOT NULL",
                &[&cfg.node_id],
            )
            .await
            .unwrap_or_default();

        let mut reg = registry.lock().await;
        for row in &rows {
            let svc_id: String = row.get(0);
            let tap_name: String = row.get(1);
            let fc_pid: i32 = row.get(2);
            let vm_id: String = row.get(3);

            reg.insert(
                svc_id,
                deprovision::VmHandle {
                    tap_name,
                    fc_pid: fc_pid as u32,
                    vm_id,
                    use_jailer: cfg.use_jailer,
                    chroot_base: cfg.chroot_base.clone(),
                },
            );
        }

        if !rows.is_empty() {
            tracing::info!(count = rows.len(), "rebuilt VM registry from DB");
        }
    }
}

/// Connect to NATS and evict stale Liquid routes.
pub async fn connect_nats_and_evict(
    nats_url: &str,
    stale_slugs: &[String],
) -> Result<async_nats::Client> {
    let nc = common::config::nats_connect(nats_url).await?;

    if !stale_slugs.is_empty() {
        use common::events::{RouteRemovedEvent, SUBJECT_ROUTE_REMOVED};
        for slug in stale_slugs {
            let event = RouteRemovedEvent { slug: slug.clone() };
            if let Ok(payload) = serde_json::to_vec(&event) {
                if let Err(e) = nc.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                    tracing::warn!(error = %e, "NATS publish route_removed failed (stale liquid cleanup)");
                }
            }
        }
        tracing::info!(count = stale_slugs.len(), "published route evictions for stale Liquid services");
    }

    Ok(nc)
}
