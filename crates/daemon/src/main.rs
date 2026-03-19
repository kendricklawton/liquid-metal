use std::sync::Arc;

use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use futures::StreamExt;
use tokio::task::JoinSet;

use common::events::{
    DeprovisionEvent, Engine, LiquidUsageEvent, ProvisionEvent,
    RouteRemovedEvent, RouteUpdatedEvent, SuspendEvent, TrafficPulseEvent, WakeEvent,
    STREAM_NAME, SUBJECT_DEPROVISION, SUBJECT_PROVISION, SUBJECT_ROUTE_REMOVED,
    SUBJECT_ROUTE_UPDATED, SUBJECT_SUSPEND, SUBJECT_TRAFFIC_PULSE, SUBJECT_WAKE,
    SUBJECT_USAGE_LIQUID,
};
#[cfg(target_os = "linux")]
use common::events::{ServiceCrashedEvent, SUBJECT_SERVICE_CRASHED};
use common::{Features, config::{env_or, require_env}};
use daemon::deprovision;
use daemon::provision::{self, ProvisionConfig, ProvisionCtx};
use daemon::storage;

const CONSUMER_PROVISION:   &str = "daemon-provision-1";
const CONSUMER_DEPROVISION: &str = "daemon-deprovision-1";
const CONSUMER_WAKE:        &str = "daemon-wake-1";

/// Default maximum concurrent provisions per node.
/// Each Metal VM is CPU/RAM-intensive; cap prevents OOM on burst deploys.
/// Override via NODE_MAX_CONCURRENT_PROVISIONS env var.
const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Default idle timeout in seconds. Services with no traffic for this duration
/// are stopped automatically (serverless scale-to-zero). Set to 0 to disable.
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 300;

#[tokio::main]
async fn main() -> Result<()> {
    let _tracer_provider = common::config::init_tracing("daemon");

    // ── PID file lock — prevent two daemons on the same node ────────────────
    let pid_file_path = env_or("DAEMON_PID_FILE", "/run/liquid-metal-daemon.pid");
    #[cfg(target_os = "linux")]
    let _pid_lock = {
        use std::io::Write;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&pid_file_path)
            .with_context(|| format!("opening PID file {pid_file_path}"))?;
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let lock_result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if lock_result != 0 {
            anyhow::bail!(
                "another daemon is already running (PID file {pid_file_path} is locked). \
                 If this is stale, delete the file and retry."
            );
        }
        // Write our PID for debugging.
        let mut file = file;
        writeln!(file, "{}", std::process::id())?;
        file // keep the File alive (holds the lock) for the process lifetime
    };

    let nats_url = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let db_url   = require_env("DATABASE_URL")?;

    let cfg = Arc::new(ProvisionConfig {
        // Firecracker
        fc_bin:      env_or("FC_BIN",         "/usr/local/bin/firecracker"),
        kernel_path: env_or("FC_KERNEL_PATH", "/opt/firecracker/vmlinux"),
        sock_dir:    env_or("FC_SOCK_DIR",    "/run/firecracker"),
        bridge:      env_or("BRIDGE",         "br0"),
        // cgroup v2
        rootfs_dev:     env_or("ROOTFS_DEVICE",  "8:0"),
        // Jailer
        use_jailer:  env_or("USE_JAILER",          "false") == "true",
        jailer_bin:  env_or("JAILER_BIN",           "/usr/local/bin/jailer"),
        jailer_uid:  env_or("JAILER_UID",           "10000").parse().unwrap_or(10000),
        jailer_gid:  env_or("JAILER_GID",           "10000").parse().unwrap_or(10000),
        chroot_base: env_or("JAILER_CHROOT_BASE",   "/srv/jailer"),
        // Node identity + artifacts
        node_id:      env_or("NODE_ID",      "node-a"),
        artifact_dir: env_or("ARTIFACT_DIR", "/var/lib/liquid-metal/artifacts"),
        // Base Alpine template for Metal rootfs assembly
        base_image_key:    env_or("BASE_IMAGE_KEY", "templates/base-alpine-v1.ext4"),
        base_image_sha256: std::env::var("BASE_IMAGE_SHA256").ok(),
    });

    let bucket = Arc::new(env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts"));

    // Postgres pool
    let pool_size: usize = env_or("DATABASE_POOL_SIZE", "8").parse().unwrap_or(8);
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let pool = Arc::new(if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
        deadpool_postgres::Pool::builder(mgr).max_size(pool_size).build()
            .context("building postgres pool (TLS)")?
    } else {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(mgr).max_size(pool_size).build()
            .context("building postgres pool")?
    });
    tracing::info!(pool_size, "postgres pool configured");

    // S3 client
    let s3 = Arc::new(storage::build_client()?);

    // VM registry — rebuilt from DB on startup, updated on each provision/deprovision
    let registry = deprovision::new_registry();
    let liquid_registry = deprovision::new_liquid_registry();

    // Mark Liquid (Wasm) services as stopped on restart. Unlike Metal (Firecracker
    // runs as a separate process), Wasm listeners run in-process — they die with the
    // daemon. Any `running` Liquid service from a previous instance is unreachable.
    //
    // Also finalize any 'draining' services left by a daemon crash mid-suspend.
    // These are already route-evicted; just need their status moved to 'suspended'.
    let stale_liquid_slugs: Vec<String> = {
        if let Ok(db) = pool.get().await {
            // Finalize orphaned drains — daemon crashed between drain start and suspend.
            let draining = db.execute(
                "UPDATE services SET status = 'suspended', upstream_addr = NULL \
                 WHERE node_id = $1 AND status = 'draining' AND deleted_at IS NULL",
                &[&cfg.node_id],
            ).await.unwrap_or(0);
            if draining > 0 {
                tracing::info!(count = draining, "finalized orphaned draining services → suspended");
            }

            let rows = db.query(
                "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                 WHERE node_id = $1 AND engine = 'liquid' AND status = 'running' AND deleted_at IS NULL \
                 RETURNING slug",
                &[&cfg.node_id],
            ).await.unwrap_or_default();
            let slugs: Vec<String> = rows.iter().map(|r| r.get::<_, String>(0)).collect();
            if !slugs.is_empty() {
                tracing::info!(count = slugs.len(), "marked stale Liquid services as stopped (listeners died with previous daemon)");
            }
            slugs
        } else {
            vec![]
        }
    };

    // Initialize in-use TAP index set from DB to avoid collisions after restart
    provision::init_tap_indices(&pool, &cfg.node_id).await;

    // Delete orphaned TAP devices from the bridge — left by a previous daemon
    // crash between TAP creation and the services.tap_name DB write.
    #[cfg(target_os = "linux")]
    provision::cleanup_orphaned_taps(&pool, &cfg.node_id, &cfg.bridge).await;

    // Delete orphaned artifact directories — services deleted while daemon was down.
    provision::cleanup_orphaned_artifacts(&pool, &cfg.node_id, &cfg.artifact_dir).await;

    // Delete orphaned cgroup directories from crash/restart cycles.
    #[cfg(target_os = "linux")]
    provision::cleanup_orphaned_cgroups(&pool, &cfg.node_id).await;

    // Kill orphaned Firecracker processes and clean up kernel resources for
    // services stuck in 'provisioning' or 'failed' — left by a daemon crash
    // during provision or rollback.
    #[cfg(target_os = "linux")]
    provision::cleanup_stale_provisioning(
        &pool, &cfg.node_id, &cfg.artifact_dir,
    ).await;

    // Warn if artifact partition is running low on disk space.
    provision::check_disk_space(&cfg.artifact_dir);

    // Clock drift check — compare local time against Postgres NOW().
    // A skew > 5s can cause premature idle timeouts; > 60s is dangerous.
    if let Ok(db) = pool.get().await {
        if let Ok(row) = db.query_one(
            "SELECT EXTRACT(EPOCH FROM NOW())::float8", &[]
        ).await {
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

    // Re-attach eBPF isolation filters for any VMs still running from a
    // previous daemon instance (filters are unloaded when the process exits).
    #[cfg(target_os = "linux")]
    {
        use daemon::ebpf;
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
                        ).await.ok();
                    }
                    // Kernel cleanup happens below via the registry rebuild + watchdog.
                    // For immediate safety, SIGKILL the FC process now.
                    if let Some(row) = db.query_opt(
                        "SELECT fc_pid FROM services WHERE id::text = $1 AND fc_pid IS NOT NULL",
                        &[service_id],
                    ).await.ok().flatten() {
                        let pid: i32 = row.get(0);
                        unsafe { libc::kill(pid, libc::SIGKILL); }
                    }
                }
            }
        }
    }


    // ── eBPF isolation health checker (Linux only) ──────────────────────────
    // Runs every 30s: asks the kernel whether each active TAP still has a BPF
    // TC egress classifier attached. If a filter is missing, the VM is running
    // without tenant isolation — kill it immediately, no second chances.
    //
    // This catches scenarios that reattach_all doesn't:
    //   - Manual `tc filter del` by an operator or rogue script
    //   - TAP device deleted by an external process
    //   - Kernel module reload clearing TC state
    //   - Bug in our code that drops the Ebpf handle
    #[cfg(target_os = "linux")]
    {
        let pool_ebpf    = pool.clone();
        let registry_ebpf = registry.clone();
        let node_id_ebpf = cfg.node_id.clone();
        let ebpf_check_secs: u64 = env_or("EBPF_CHECK_INTERVAL_SECS", "30").parse().unwrap_or(30);
        tokio::spawn(async move {
            use daemon::ebpf;
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(ebpf_check_secs));
            tracing::info!(interval_secs = ebpf_check_secs, "eBPF isolation health checker started");

            loop {
                ticker.tick().await;

                let missing = ebpf::audit_filters().await;
                if missing.is_empty() {
                    continue;
                }

                tracing::error!(
                    count = missing.len(),
                    taps  = ?missing,
                    "CRITICAL: eBPF isolation breach detected — killing unisolated VMs"
                );

                let db = match pool_ebpf.get().await {
                    Ok(d)  => d,
                    Err(e) => {
                        tracing::error!(error = %e, "cannot get DB connection to handle isolation breach");
                        continue;
                    }
                };

                for tap_name in &missing {
                    // Look up the service by TAP name on this node.
                    let row = db.query_opt(
                        "SELECT id, fc_pid FROM services \
                         WHERE tap_name = $1 AND node_id = $2 \
                           AND status = 'running' AND deleted_at IS NULL",
                        &[tap_name, &node_id_ebpf],
                    ).await.ok().flatten();

                    let Some(row) = row else {
                        // TAP is in our active map but no matching running service in DB.
                        // Detach the stale entry.
                        ebpf::detach(tap_name);
                        continue;
                    };

                    let svc_id: uuid::Uuid = row.get("id");
                    let fc_pid: Option<i32> = row.get("fc_pid");

                    // SIGKILL the Firecracker process immediately — every packet
                    // from this VM is a potential isolation violation.
                    if let Some(pid) = fc_pid {
                        unsafe { libc::kill(pid, libc::SIGKILL); }
                        tracing::error!(
                            service_id = %svc_id,
                            tap = tap_name.as_str(),
                            fc_pid = pid,
                            "SIGKILL sent to unisolated VM"
                        );
                    }

                    // Mark crashed in DB and clear upstream so proxy stops routing.
                    db.execute(
                        "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                         WHERE id = $1 AND deleted_at IS NULL",
                        &[&svc_id],
                    ).await.ok();

                    // Remove from registry so the crash watcher doesn't double-process.
                    registry_ebpf.lock().await.remove(&svc_id.to_string());

                    // Clean up our eBPF tracking entry.
                    ebpf::detach(tap_name);
                }
            }
        });
    }

    // Rebuild VM registry from DB — services provisioned by a previous daemon
    // instance need handles so deprovision events can clean them up.
    {
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
                let svc_id: String   = row.get(0);
                let tap_name: String = row.get(1);
                let fc_pid: i32      = row.get(2);
                let vm_id: String    = row.get(3);

                reg.insert(svc_id, deprovision::VmHandle {
                    tap_name,
                    fc_pid:      fc_pid as u32,
                    vm_id,
                    use_jailer:  cfg.use_jailer,
                    chroot_base: cfg.chroot_base.clone(),
                });
            }

            if !rows.is_empty() {
                tracing::info!(count = rows.len(), "rebuilt VM registry from DB");
            }
        }
    }


    // NATS — connect here so nc can be cloned into ProvisionCtx for plain publishes.
    // js (JetStream) gets nc.clone() and is used for durable consumers below.
    let nc = common::config::nats_connect(&nats_url).await?;
    let js = async_nats::jetstream::new(nc.clone());

    // Evict stale Liquid routes from the proxy cache now that NATS is connected.
    // The proxy reconciler would catch these within 60s, but publishing immediately
    // prevents 502s for the first minute after daemon restart.
    if !stale_liquid_slugs.is_empty() {
        use common::events::{RouteRemovedEvent, SUBJECT_ROUTE_REMOVED};
        for slug in &stale_liquid_slugs {
            let event = RouteRemovedEvent { slug: slug.clone() };
            if let Ok(payload) = serde_json::to_vec(&event) {
                if let Err(e) = nc.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                    tracing::warn!(error = %e, "NATS publish route_removed failed (stale liquid cleanup)");
                }
            }
        }
        tracing::info!(count = stale_liquid_slugs.len(), "published route evictions for stale Liquid services");
    }

    // Save plain-client clones for the pulse subscriber and idle checker tasks
    // before nc is moved into Arc inside ProvisionCtx.
    let nc_pulse = nc.clone();
    let js_idle  = js.clone();

    let ctx = Arc::new(ProvisionCtx {
        pool:     pool.clone(),
        cfg:      cfg.clone(),
        s3:       s3.clone(),
        bucket:   bucket.clone(),
        registry: registry.clone(),
        liquid_registry: liquid_registry.clone(),
        nats:     Arc::new(nc),
    });

    let features = Features::from_env();
    features.log_summary();

    // ── Idle timeout configuration ────────────────────────────────────────────
    // Set IDLE_TIMEOUT_SECS=0 to disable idle timeout (always-on services).
    // Default: 300s (5 minutes) — standard serverless scale-to-zero behaviour.
    let idle_timeout_secs: i64 = env_or("IDLE_TIMEOUT_SECS", &DEFAULT_IDLE_TIMEOUT_SECS.to_string())
        .parse()
        .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);

    // ── Traffic pulse subscriber (batched) ─────────────────────────────────
    // Listens for platform.traffic_pulse events published by Pingora on every
    // proxied request (debounced 30s per slug). Accumulates slugs over a 5s
    // window and batch-updates services.last_request_at in a single query.
    // This prevents a NATS flood on proxy restart from hammering Postgres
    // with 10k individual UPDATEs.
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            use futures::StreamExt;
            use std::collections::HashSet;
            use tokio::time::{Duration, interval};

            let mut sub = match nc_pulse.subscribe(SUBJECT_TRAFFIC_PULSE).await {
                Ok(s)  => s,
                Err(e) => { tracing::warn!(error = %e, "pulse subscriber setup failed"); return; }
            };
            let pulse_window: u64 = env_or("PULSE_BATCH_WINDOW_SECS", "5").parse().unwrap_or(5);
            tracing::info!(pulse_window, "traffic pulse subscriber ready (batched)");

            // Cap prevents memory exhaustion if slugs arrive faster than flush windows.
            // A single node cannot run more services than this, so legitimate traffic
            // will never hit the ceiling. Missing a pulse is safe — the batch window
            // is 5s and idle timeout is 5 minutes+.
            const PULSE_PENDING_MAX: usize = 10_000;

            let mut pending: HashSet<String> = HashSet::new();
            let mut flush_tick = interval(Duration::from_secs(pulse_window));

            loop {
                tokio::select! {
                    Some(msg) = sub.next() => {
                        if let Ok(event) = serde_json::from_slice::<TrafficPulseEvent>(&msg.payload) {
                            if pending.len() < PULSE_PENDING_MAX {
                                pending.insert(event.slug);
                            }
                        }
                    }
                    _ = flush_tick.tick() => {
                        if pending.is_empty() { continue; }
                        let slugs: Vec<String> = pending.drain().collect();
                        if let Ok(db) = pool.get().await {
                            let result = db.execute(
                                "UPDATE services SET last_request_at = NOW() \
                                 WHERE slug = ANY($1) AND status = 'running' AND deleted_at IS NULL",
                                &[&slugs],
                            ).await;
                            match result {
                                Ok(n) => tracing::debug!(batch_size = slugs.len(), updated = n, "pulse batch flush"),
                                Err(e) => tracing::warn!(error = %e, "pulse batch flush failed"),
                            }
                        }
                    }
                }
            }
        });
    }

    // ── Idle checker ────────────────────────────────────────────────────────
    // Runs every 60s: finds Metal services idle longer than IDLE_TIMEOUT_SECS
    // and deprovisions them via the normal DeprovisionEvent flow.
    if idle_timeout_secs > 0 {
        let pool    = pool.clone();
        let node_id = cfg.node_id.clone();
        let idle_check_secs: u64 = env_or("IDLE_CHECK_INTERVAL_SECS", "60").parse().unwrap_or(60);
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(idle_check_secs));

            loop {
                ticker.tick().await;

                let db = match pool.get().await {
                    Ok(d)  => d,
                    Err(_) => continue,
                };

                let rows = match db.query(
                    "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                     WHERE engine = 'metal' AND status = 'running' \
                       AND node_id = $1 AND deleted_at IS NULL \
                       AND COALESCE(last_request_at, created_at) \
                           < NOW() - interval '1 second' * $2 \
                     RETURNING id::text, slug",
                    &[&node_id, &idle_timeout_secs],
                ).await {
                    Ok(r)  => r,
                    Err(e) => { tracing::warn!(error = %e, "idle checker query failed"); continue; }
                };

                for row in rows {
                    let sid:  String = row.get("id");
                    let slug: String = row.get("slug");
                    tracing::info!(
                        service_id = sid,
                        slug,
                        idle_timeout_secs,
                        "idle timeout — deprovisioning"
                    );
                    let event = DeprovisionEvent {
                        service_id: sid.clone(),
                        slug,
                        engine: Engine::Metal,
                    };
                    let payload = match serde_json::to_vec(&event) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::error!(service_id = sid, error = %e, "failed to serialize DeprovisionEvent");
                            continue;
                        }
                    };

                    // Retry NATS publish up to 3 times. If all retries fail,
                    // revert the service back to 'running' so the next idle
                    // check or crash watchdog can pick it up — prevents ghost VMs.
                    let mut published = false;
                    for attempt in 1..=3u32 {
                        match js_idle.publish(SUBJECT_DEPROVISION, payload.clone().into()).await {
                            Ok(_) => { published = true; break; }
                            Err(e) => {
                                tracing::warn!(
                                    service_id = sid, attempt,
                                    error = %e, "idle deprovision publish failed — retrying"
                                );
                                tokio::time::sleep(Duration::from_millis(500 * attempt as u64)).await;
                            }
                        }
                    }
                    if !published {
                        tracing::error!(
                            service_id = sid,
                            "idle deprovision publish failed after 3 attempts — reverting to running"
                        );
                        let _ = db.execute(
                            "UPDATE services SET status = 'running' \
                             WHERE id = $1::uuid AND status = 'stopped' AND deleted_at IS NULL",
                            &[&sid],
                        ).await;
                    }
                }
            }
        });
    }

    // ── Deleted-workspace orphan sweep ──────────────────────────────────────
    // Runs every 60s: finds services still running on this node whose workspace
    // has been soft-deleted. This is a safety net — the API's delete_workspace
    // handler publishes DeprovisionEvent for each service, but if that fails
    // (NATS down, partial publish) this sweep catches the stragglers.
    {
        let pool    = pool.clone();
        let node_id = cfg.node_id.clone();
        let js_orphan = js.clone();
        let orphan_sweep_secs: u64 = env_or("ORPHAN_SWEEP_INTERVAL_SECS", "60").parse().unwrap_or(60);
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(orphan_sweep_secs));

            loop {
                ticker.tick().await;

                let db = match pool.get().await {
                    Ok(d)  => d,
                    Err(_) => continue,
                };

                let rows = match db.query(
                    "UPDATE services s SET status = 'stopped', upstream_addr = NULL \
                     FROM workspaces w \
                     WHERE s.workspace_id = w.id \
                       AND w.deleted_at IS NOT NULL \
                       AND s.node_id = $1 \
                       AND s.status IN ('running', 'provisioning') \
                       AND s.deleted_at IS NULL \
                     RETURNING s.id::text, s.slug, s.engine",
                    &[&node_id],
                ).await {
                    Ok(r)  => r,
                    Err(e) => { tracing::warn!(error = %e, "deleted-workspace orphan sweep failed"); continue; }
                };

                for row in &rows {
                    let sid:  String = row.get("id");
                    let slug: String = row.get("slug");
                    let eng:  String = row.get("engine");

                    let engine: Engine = match eng.parse() {
                        Ok(e)  => e,
                        Err(_) => { tracing::error!(engine = eng, service_id = sid, "unknown engine in orphan sweep"); continue; }
                    };

                    tracing::info!(service_id = sid, slug, "orphan sweep — workspace deleted, deprovisioning");

                    let event = DeprovisionEvent { service_id: sid, slug, engine };
                    if let Ok(payload) = serde_json::to_vec(&event) {
                        if let Err(e) = js_orphan.publish(SUBJECT_DEPROVISION, payload.into()).await {
                            tracing::warn!(error = %e, "NATS publish deprovision failed (orphan sweep)");
                        }
                    }
                }
            }
        });
    }

    // ── VM crash watcher (Linux only) ───────────────────────────────────────
    // Every 10s, checks if tracked Firecracker PIDs are still alive via
    // kill(pid, 0). If a process has exited, marks the service as 'crashed',
    // clears upstream_addr, publishes RouteRemovedEvent + ServiceCrashedEvent,
    // and triggers cleanup.
    #[cfg(target_os = "linux")]
    {
        let registry    = registry.clone();
        let pool        = pool.clone();
        let node_id     = cfg.node_id.clone();
        let nats_crash  = ctx.nats.clone();
        let cfg_crash   = cfg.clone();
        let crash_check_secs: u64 = env_or("VM_CRASH_CHECK_INTERVAL_SECS", "10").parse().unwrap_or(10);
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(crash_check_secs));
            loop {
                ticker.tick().await;
                // Collect crashed VMs and remove from registry atomically.
                // This prevents a race where a new provision for the same
                // service_id completes between detection and cleanup — the
                // new handle would have a different fc_pid and must not be
                // touched by this watchdog cycle.
                // Check each tracked VM: try waitpid (works for direct children),
                // fall back to /proc/{pid} existence (works for jailer grandchildren).
                let crashed: Vec<(String, deprovision::VmHandle, Option<i32>)> = {
                    let mut reg = registry.lock().await;
                    let mut dead: Vec<(String, Option<i32>)> = Vec::new();
                    for (id, h) in reg.iter() {
                        let pid = h.fc_pid as libc::pid_t;
                        let mut status: libc::c_int = 0;
                        let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                        if ret > 0 {
                            // Direct child exited — extract exit code.
                            let code = if libc::WIFEXITED(status) {
                                Some(libc::WEXITSTATUS(status))
                            } else if libc::WIFSIGNALED(status) {
                                Some(128 + libc::WTERMSIG(status))
                            } else {
                                None
                            };
                            dead.push((id.clone(), code));
                        } else if ret == 0 {
                            // Still running (direct child). Skip.
                        } else {
                            // waitpid failed (ECHILD) — not our direct child (jailer case).
                            // Fall back to /proc existence check.
                            if !std::path::Path::new(&format!("/proc/{}", h.fc_pid)).exists() {
                                dead.push((id.clone(), None));
                            }
                        }
                    }
                    dead.into_iter()
                        .filter_map(|(id, code)| reg.remove(&id).map(|h| (id, h, code)))
                        .collect()
                };
                for (service_id, handle, exit_code) in &crashed {
                    tracing::error!(
                        service_id, fc_pid = handle.fc_pid,
                        exit_code = ?exit_code,
                        "VM crash detected — process no longer alive"
                    );
                    // Update DB: mark crashed + clear upstream.
                    if let Ok(db) = pool.get().await {
                        let svc_id: uuid::Uuid = match service_id.parse() {
                            Ok(id) => id,
                            Err(_) => continue,
                        };
                        let row = db.query_opt(
                            "UPDATE services SET status = 'crashed', upstream_addr = NULL \
                             WHERE id = $1 AND node_id = $2 AND status = 'running' AND deleted_at IS NULL \
                             RETURNING slug",
                            &[&svc_id, &node_id],
                        ).await.ok().flatten();

                        if let Some(row) = row {
                            let slug: String = row.get("slug");
                            // Evict from proxy cache.
                            let removed = RouteRemovedEvent { slug: slug.clone() };
                            if let Ok(payload) = serde_json::to_vec(&removed) {
                                if let Err(e) = nats_crash.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                                    tracing::warn!(error = %e, "NATS publish route_removed failed (crash watcher)");
                                }
                            }
                            // Publish crash event for observability.
                            let crash = ServiceCrashedEvent {
                                service_id: service_id.clone(),
                                slug,
                                exit_code: *exit_code,
                            };
                            if let Ok(payload) = serde_json::to_vec(&crash) {
                                if let Err(e) = nats_crash.publish(SUBJECT_SERVICE_CRASHED, payload.into()).await {
                                    tracing::warn!(error = %e, "NATS publish service_crashed failed");
                                }
                            }
                        }
                    }
                }
                // Clean up kernel resources (TAP, cgroup, CPU pin).
                // Registry entries were already removed above to prevent
                // the stale-handle race with concurrent provisions.
                for (service_id, handle, _) in crashed {
                    provision::release_tap_index(&handle.tap_name).await;
                    deprovision::metal(
                        &service_id, handle, &cfg_crash.artifact_dir,
                    ).await;
                }
            }
        });
    }

    // ── Metal usage reporter ─────────────────────────────────────────────────
    // Metal usage reporting has moved to the proxy (per-invocation billing).
    // The daemon no longer reports time-based Metal usage. The proxy counts
    // requests per slug and publishes MetalUsageEvent with invocation counts.

    // ── Liquid usage reporter ─────────────────────────────────────────────
    // Drains invocation counters from the liquid registry and publishes
    // LiquidUsageEvent for each active Wasm service.
    //
    // Critical difference from Metal: the atomic counter is swapped to zero
    // BEFORE publish. If the publish fails, those invocations must not be
    // lost — they're accumulated into a per-service backlog and retried on
    // the next tick.
    {
        let liquid_registry = liquid_registry.clone();
        let nats = ctx.nats.clone();
        let liquid_usage_secs: u64 = env_or("USAGE_REPORT_INTERVAL_SECS", "60").parse().unwrap_or(60);
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(liquid_usage_secs));
            // service_id → accumulated invocations that failed to publish.
            let mut backlog: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            loop {
                ticker.tick().await;
                let reg = liquid_registry.lock().await;
                let mut reported = 0u32;
                for (service_id, handle) in reg.iter() {
                    let fresh = handle.invocations.swap(0, Ordering::Relaxed);
                    // Merge fresh count with any previously failed backlog.
                    let total = backlog.remove(service_id).unwrap_or(0) + fresh;
                    if total == 0 {
                        continue;
                    }
                    let event = LiquidUsageEvent {
                        workspace_id: handle.workspace_id.clone(),
                        service_id:   service_id.clone(),
                        invocations:  total,
                    };
                    if let Ok(payload) = serde_json::to_vec(&event) {
                        match nats.publish(SUBJECT_USAGE_LIQUID, payload.into()).await {
                            Ok(_)  => { reported += 1; }
                            Err(_) => {
                                // Put back — will be retried next tick.
                                backlog.insert(service_id.clone(), total);
                            }
                        }
                    }
                }
                // Prune backlog entries for services that have been deprovisioned.
                // Without this, a service that fails to publish and is then removed
                // from the registry leaves an orphan entry that grows forever.
                backlog.retain(|sid, _| reg.contains_key(sid));

                // Safety valve: cap backlog to prevent unbounded growth if NATS
                // is down and many services are active simultaneously.
                const LIQUID_BACKLOG_MAX: usize = 10_000;
                if backlog.len() > LIQUID_BACKLOG_MAX {
                    tracing::error!(
                        backlog = backlog.len(),
                        "usage: liquid backlog exceeded {} — clearing",
                        LIQUID_BACKLOG_MAX,
                    );
                    backlog.clear();
                }

                // Drop the lock before logging.
                drop(reg);
                if reported > 0 {
                    tracing::debug!(count = reported, "usage: reported Liquid invocation ticks");
                }
                if !backlog.is_empty() {
                    tracing::warn!(backlog = backlog.len(), "usage: liquid events buffered (NATS unreachable)");
                }
            }
        });
    }

    // ── Suspend consumer ──────────────────────────────────────────────────
    // Listens for SuspendEvent (workspace balance depleted) and deprovisions
    // all running services for that workspace on this node.
    //
    // Graceful drain: routes are evicted FIRST (proxy stops sending new
    // requests), then we wait SUSPEND_DRAIN_SECS for in-flight requests to
    // complete before killing VMs / removing Wasm handlers.
    //
    // This lets the last request finish cleanly. The overage is small:
    //   max_overage = billing_tick (60s) + drain (30s) = 90s of compute
    //   Pro tier (2 vCPU, 512 MB): ~5,100 µcr ≈ $0.005
    // The negative balance is tracked honestly (deduct_credits allows
    // topup_balance to go negative) and recovered on next top-up.
    {
        let pool            = pool.clone();
        let node_id         = cfg.node_id.clone();
        let nats            = ctx.nats.clone();
        let registry        = registry.clone();
        let liquid_registry = liquid_registry.clone();
        let cfg             = cfg.clone();
        let drain_secs: u64 = env_or("SUSPEND_DRAIN_SECS", "30").parse().unwrap_or(30);
        tokio::spawn(async move {
            let mut sub = match nats.subscribe(SUBJECT_SUSPEND).await {
                Ok(s)  => s,
                Err(e) => { tracing::warn!(error = %e, "suspend subscriber setup failed"); return; }
            };
            tracing::info!(drain_secs, "suspend subscriber ready");
            while let Some(msg) = sub.next().await {
                let event: SuspendEvent = match serde_json::from_slice(&msg.payload) {
                    Ok(e)  => e,
                    Err(e) => { tracing::warn!(error = %e, "failed to parse SuspendEvent"); continue; }
                };
                tracing::warn!(workspace_id = event.workspace_id, reason = event.reason, "suspending workspace services");
                let db = match pool.get().await {
                    Ok(d)  => d,
                    Err(_) => continue,
                };
                let wid: uuid::Uuid = match event.workspace_id.parse() {
                    Ok(id) => id,
                    Err(e) => { tracing::warn!(error = %e, "invalid workspace_id in SuspendEvent"); continue; }
                };

                // Phase 1: Mark as draining and evict routes. The proxy stops
                // sending new requests, but existing TCP connections (in-flight
                // requests) continue until their response completes.
                let rows = match db.query(
                    "UPDATE services SET status = 'draining' \
                     WHERE workspace_id = $1 AND node_id = $2 \
                       AND status = 'running' AND deleted_at IS NULL \
                     RETURNING id::text, slug, engine",
                    &[&wid, &node_id],
                ).await {
                    Ok(r)  => r,
                    Err(e) => { tracing::error!(error = %e, "suspend drain query failed"); continue; }
                };

                // Evict routes immediately — proxy drops new requests.
                for row in &rows {
                    let slug: String = row.get("slug");
                    if let Ok(payload) = serde_json::to_vec(&RouteRemovedEvent { slug }) {
                        if let Err(e) = nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                            tracing::warn!(error = %e, "NATS publish route_removed failed (suspend handler)");
                        }
                    }
                }

                if rows.is_empty() {
                    continue;
                }

                // Phase 2: Drain — wait for in-flight requests to finish.
                // For Liquid (Wasm): dispatch() holds an Arc<WasmService>, so
                // in-flight requests complete even after we remove the handle.
                // For Metal: the VM stays alive, so proxied TCP connections
                // finish naturally before we SIGTERM.
                tracing::info!(
                    workspace_id = event.workspace_id,
                    count = rows.len(),
                    drain_secs,
                    "draining in-flight requests before suspend"
                );
                tokio::time::sleep(std::time::Duration::from_secs(drain_secs)).await;

                // Phase 3: Hard suspend — kill VMs, remove Wasm handlers, clear DB.
                if let Err(e) = db.execute(
                    "UPDATE services SET status = 'suspended', upstream_addr = NULL \
                     WHERE workspace_id = $1 AND node_id = $2 \
                       AND status = 'draining' AND deleted_at IS NULL",
                    &[&wid, &node_id],
                ).await {
                    tracing::error!(error = %e, "suspend finalize query failed");
                }

                for row in &rows {
                    let sid: String = row.get("id");
                    let eng: String = row.get("engine");

                    if eng == "metal" {
                        let handle = registry.lock().await.remove(&sid);
                        if let Some(h) = handle {
                            deprovision::metal(&sid, h, &cfg.artifact_dir).await;
                        }
                    } else if eng == "liquid" {
                        liquid_registry.lock().await.remove(&sid);
                    }
                }

                tracing::warn!(workspace_id = event.workspace_id, count = rows.len(), "suspended services (drain complete)");
            }
        });
    }

    tracing::info!(
        %nats_url,
        node_id    = cfg.node_id,
        use_jailer = cfg.use_jailer,
        "daemon starting"
    );

    js.get_or_create_stream(StreamConfig {
        name:     STREAM_NAME.to_string(),
        subjects: vec!["platform.*".to_string()],
        ..Default::default()
    })
    .await
    .context("ensure stream")?;

    // ── Provision consumer ────────────────────────────────────────────────────
    let provision_consumer = js
        .get_stream(STREAM_NAME).await.context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name:   Some(CONSUMER_PROVISION.to_string()),
            filter_subject: SUBJECT_PROVISION.to_string(),
            // Wasm compilation (Module::new) can take 60s+ in debug builds for
            // large binaries. Default ack_wait is 30s — too short, causes
            // redelivery loops that pile up competing compilations.
            ack_wait: std::time::Duration::from_secs(
                env_or("PROVISION_ACK_WAIT_SECS", "300").parse().unwrap_or(300)
            ),
            // Cap redelivery to avoid infinite retry loops on transient failures.
            // Permanent failures ACK immediately (see error handler below).
            max_deliver: 3,
            ..Default::default()
        })
        .await
        .context("create provision consumer")?;

    // ── Deprovision consumer ──────────────────────────────────────────────────
    let deprovision_consumer = js
        .get_stream(STREAM_NAME).await.context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name:   Some(CONSUMER_DEPROVISION.to_string()),
            filter_subject: SUBJECT_DEPROVISION.to_string(),
            ..Default::default()
        })
        .await
        .context("create deprovision consumer")?;

    // ── Wake consumer (snapshot restore on request) ────────────────────────
    let wake_consumer = js
        .get_stream(STREAM_NAME).await.context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name:   Some(CONSUMER_WAKE.to_string()),
            filter_subject: SUBJECT_WAKE.to_string(),
            ack_wait: std::time::Duration::from_secs(30),
            max_deliver: 3,
            ..Default::default()
        })
        .await
        .context("create wake consumer")?;

    // ── Consumer lag monitor ─────────────────────────────────────────────────
    // Periodically check JetStream consumer lag and warn when it exceeds 50
    // pending messages — indicates the daemon can't keep up with deploy volume.
    {
        let mut prov_consumer = provision_consumer.clone();
        let lag_interval_secs: u64 = env_or("LAG_MONITOR_INTERVAL_SECS", "30").parse().unwrap_or(30);
        let lag_threshold: u64 = env_or("PROVISION_LAG_THRESHOLD", "50").parse().unwrap_or(50);
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(lag_interval_secs));
            loop {
                ticker.tick().await;
                if let Ok(info) = prov_consumer.info().await {
                    let pending = info.num_pending;
                    if pending > lag_threshold {
                        tracing::warn!(
                            pending,
                            "provision consumer lag > 50 — consider increasing NODE_MAX_CONCURRENT_PROVISIONS or adding nodes"
                        );
                    } else if pending > 0 {
                        tracing::debug!(pending, "provision consumer lag");
                    }
                }
            }
        });
    }

    // ── Health check endpoint ────────────────────────────────────────────────
    {
        let health_port = env_or("HEALTH_PORT", "9090");
        let health_bind = format!("0.0.0.0:{health_port}");
        let node_id = cfg.node_id.clone();
        let registry_health = registry.clone();
        let liquid_health = liquid_registry.clone();
        let started = std::time::Instant::now();

        let listener = tokio::net::TcpListener::bind(&health_bind)
            .await
            .context("binding health check listener")?;
        tracing::info!(%health_bind, "health check server listening");

        tokio::spawn(async move {
            use hyper::server::conn::http1;
            use hyper::service::service_fn;
            use hyper::{Request, Response, StatusCode};
            use http_body_util::Full;
            use hyper::body::Bytes;

            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(x)  => x,
                    Err(e) => { tracing::debug!(error = %e, "health accept error"); continue; }
                };
                let node_id = node_id.clone();
                let registry = registry_health.clone();
                let liquid = liquid_health.clone();
                let started = started;

                tokio::spawn(async move {
                    let io = hyper_util::rt::TokioIo::new(stream);
                    let handler = service_fn(move |_req: Request<hyper::body::Incoming>| {
                        let node_id = node_id.clone();
                        let registry = registry.clone();
                        let liquid = liquid.clone();
                        async move {
                            let vm_count = registry.lock().await.len();
                            let wasm_count = liquid.lock().await.len();
                            let uptime = started.elapsed().as_secs();
                            let body = serde_json::json!({
                                "status": "ok",
                                "node_id": node_id,
                                "uptime_secs": uptime,
                                "metal_vms": vm_count,
                                "liquid_services": wasm_count,
                            });
                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", "application/json")
                                    .body(Full::new(Bytes::from(body.to_string())))
                                    .unwrap()
                            )
                        }
                    });
                    let _ = http1::Builder::new().serve_connection(io, handler).await;
                });
            }
        });
    }

    let mut provision_msgs   = provision_consumer.messages().await.context("subscribe provision")?;
    let mut deprovision_msgs = deprovision_consumer.messages().await.context("subscribe deprovision")?;
    let mut wake_msgs        = wake_consumer.messages().await.context("subscribe wake")?;

    tracing::info!("daemon ready");

    // Semaphore caps concurrent in-flight provisions. Tunable per-node to
    // match hardware — a 32-core node can handle more parallel FC boots than a 4-core.
    let max_concurrent: usize = env_or("NODE_MAX_CONCURRENT_PROVISIONS", &DEFAULT_MAX_CONCURRENT.to_string())
        .parse()
        .unwrap_or(DEFAULT_MAX_CONCURRENT);
    tracing::info!(max_concurrent, "provision concurrency cap");
    let sem = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

    // JoinSet tracks all spawned tasks for graceful drain
    let mut tasks: JoinSet<()> = JoinSet::new();

    // Shutdown signal (SIGTERM or Ctrl-C)
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = sigterm.recv()          => tracing::info!("SIGTERM received"),
                _ = tokio::signal::ctrl_c() => tracing::info!("Ctrl-C received"),
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.expect("Ctrl-C handler");
    };
    let mut shutdown = std::pin::pin!(shutdown);

    loop {
        tokio::select! {
            // ── Provision message ─────────────────────────────────────────
            Some(msg_result) = provision_msgs.next() => {
                let msg = match msg_result {
                    Err(e) => { tracing::error!(error = %e, "NATS provision error"); continue; }
                    Ok(m)  => m,
                };

                let event: ProvisionEvent = match serde_json::from_slice(&msg.payload) {
                    Err(e) => {
                        tracing::error!(error = %e, "parse error — ACKing to avoid poison pill");
                        msg.ack().await.ok();
                        continue;
                    }
                    Ok(e) => e,
                };

                tracing::info!(
                    app    = event.app_name,
                    engine = ?event.engine,
                    "provision event received"
                );

                let engine_enabled = match event.engine {
                    Engine::Metal  => features.enable_metal,
                    Engine::Liquid => features.enable_liquid,
                };
                if !engine_enabled || features.maintenance_mode {
                    tracing::warn!(
                        app    = event.app_name,
                        engine = ?event.engine,
                        maintenance_mode = features.maintenance_mode,
                        "provision rejected by feature flag — NACKing"
                    );
                    msg.ack_with(AckKind::Nak(None)).await.ok();
                    continue;
                }

                let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
                let ctx    = ctx.clone();

                let provision_timeout_secs: u64 = env_or("PROVISION_TIMEOUT_SECS", "600")
                    .parse().unwrap_or(600);

                tasks.spawn(async move {
                    let _permit = permit;
                    let timeout = std::time::Duration::from_secs(provision_timeout_secs);
                    let result = tokio::time::timeout(
                        timeout,
                        provision::provision(&ctx, &event),
                    ).await;

                    match result {
                        Ok(Ok(svc)) => {
                            tracing::info!(
                                id            = %svc.id,
                                engine        = ?svc.engine,
                                upstream_addr = ?svc.upstream_addr,
                                app           = event.app_name,
                                "provision succeeded"
                            );
                            msg.ack().await.ok();
                        }
                        Ok(Err(e)) => {
                            let kind = provision::rollback_provision(&ctx, &event, &e).await;
                            match kind {
                                common::events::FailureKind::Permanent => {
                                    tracing::error!(error = %e, app = event.app_name, "provision permanently failed — not retrying");
                                    msg.ack().await.ok();
                                }
                                common::events::FailureKind::Transient => {
                                    let attempt = msg.info().map(|i| i.delivered as u64).unwrap_or(1);
                                    let backoff = std::time::Duration::from_secs(15 * attempt);
                                    tracing::warn!(
                                        error = %e,
                                        app = event.app_name,
                                        attempt,
                                        backoff_secs = backoff.as_secs(),
                                        "transient provision failure — retrying"
                                    );
                                    // Reset status so the retry attempt picks it up as provisioning
                                    if let Ok(db) = ctx.pool.get().await {
                                        if let Ok(sid) = event.service_id.parse::<uuid::Uuid>() {
                                            let _ = db.execute(
                                                "UPDATE services SET status = 'provisioning' WHERE id = $1",
                                                &[&sid],
                                            ).await;
                                        }
                                    }
                                    msg.ack_with(AckKind::Nak(Some(backoff))).await.ok();
                                }
                            }
                        }
                        Err(_) => {
                            let timeout_err = anyhow::anyhow!(
                                "provision timed out after {provision_timeout_secs}s"
                            );
                            let kind = provision::rollback_provision(&ctx, &event, &timeout_err).await;
                            match kind {
                                common::events::FailureKind::Permanent => {
                                    tracing::error!(app = event.app_name, timeout_secs = provision_timeout_secs, "provision timed out — permanent failure");
                                    msg.ack().await.ok();
                                }
                                common::events::FailureKind::Transient => {
                                    let attempt = msg.info().map(|i| i.delivered as u64).unwrap_or(1);
                                    let backoff = std::time::Duration::from_secs(15 * attempt);
                                    tracing::warn!(app = event.app_name, attempt, backoff_secs = backoff.as_secs(), "provision timed out — retrying");
                                    if let Ok(db) = ctx.pool.get().await {
                                        if let Ok(sid) = event.service_id.parse::<uuid::Uuid>() {
                                            let _ = db.execute(
                                                "UPDATE services SET status = 'provisioning' WHERE id = $1",
                                                &[&sid],
                                            ).await;
                                        }
                                    }
                                    msg.ack_with(AckKind::Nak(Some(backoff))).await.ok();
                                }
                            }
                        }
                    }
                });
            }

            // ── Wake message (snapshot restore) ──────────────────────────
            Some(msg_result) = wake_msgs.next() => {
                let msg = match msg_result {
                    Err(e) => { tracing::error!(error = %e, "NATS wake error"); continue; }
                    Ok(m)  => m,
                };

                let event: WakeEvent = match serde_json::from_slice(&msg.payload) {
                    Err(e) => {
                        tracing::error!(error = %e, "wake parse error — ACKing");
                        msg.ack().await.ok();
                        continue;
                    }
                    Ok(e) => e,
                };

                // Dedup: if the service is already running (another wake beat us),
                // or is already being restored, ACK and skip.
                if registry.lock().await.contains_key(&event.service_id) {
                    tracing::debug!(service_id = event.service_id, "wake: already running — skipping");
                    msg.ack().await.ok();
                    continue;
                }

                tracing::info!(service_id = event.service_id, slug = event.slug, "wake event — restoring from snapshot");

                let ctx     = ctx.clone();
                let cfg     = cfg.clone();
                let registry = registry.clone();
                let sem     = sem.clone();

                tasks.spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => { msg.ack().await.ok(); return; }
                    };

                    let result = wake_from_snapshot(&ctx, &cfg, &event, &registry).await;

                    match result {
                        Ok(()) => {
                            tracing::info!(service_id = event.service_id, "wake complete");
                            msg.ack().await.ok();
                        }
                        Err(e) => {
                            tracing::error!(service_id = event.service_id, error = %e, "wake failed");
                            // NAK with backoff for retry
                            let attempt = msg.info().map(|i| i.delivered as u64).unwrap_or(1);
                            let backoff = std::time::Duration::from_secs(5 * attempt);
                            msg.ack_with(AckKind::Nak(Some(backoff))).await.ok();
                        }
                    }
                });
            }

            // ── Deprovision message ───────────────────────────────────────
            Some(msg_result) = deprovision_msgs.next() => {
                let msg = match msg_result {
                    Err(e) => { tracing::error!(error = %e, "NATS deprovision error"); continue; }
                    Ok(m)  => m,
                };

                let event: DeprovisionEvent = match serde_json::from_slice(&msg.payload) {
                    Err(e) => {
                        tracing::error!(error = %e, "deprovision parse error — ACKing");
                        msg.ack().await.ok();
                        continue;
                    }
                    Ok(e) => e,
                };

                tracing::info!(service_id = event.service_id, "deprovision event received");

                let registry        = registry.clone();
                let liquid_registry = liquid_registry.clone();
                let cfg             = cfg.clone();
                let ctx             = ctx.clone();

                tasks.spawn(async move {
                    match event.engine {
                        Engine::Metal => {
                            let handle = registry.lock().await.remove(&event.service_id);
                            match handle {
                                Some(h) => {
                                    provision::release_tap_index(&h.tap_name).await;
                                    deprovision::metal(
                                        &event.service_id,
                                        h,
                                        &cfg.artifact_dir,
                                    )
                                    .await;
                                }
                                None => {
                                    tracing::warn!(
                                        service_id = event.service_id,
                                        "deprovision: no in-memory handle (VM may have been provisioned \
                                         before this daemon instance started)"
                                    );
                                }
                            }
                        }
                        Engine::Liquid => {
                            liquid_registry.lock().await.remove(&event.service_id);

                            // Delete local artifact cache (wasm module).
                            let artifact_path = format!("{}/{}", cfg.artifact_dir, event.service_id);
                            if let Err(e) = tokio::fs::remove_dir_all(&artifact_path).await {
                                tracing::debug!(service_id = event.service_id, error = %e, "artifact cache cleanup (may not exist)");
                            }

                            tracing::info!(service_id = event.service_id, "liquid service deprovisioned");
                        }
                    }

                    // Evict the slug from every Pingora instance's route cache.
                    if let Ok(payload) = serde_json::to_vec(&RouteRemovedEvent { slug: event.slug.clone() }) {
                        if let Err(e) = ctx.nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                            tracing::warn!(error = %e, "NATS publish route_removed failed (deprovision)");
                        }
                    }

                    msg.ack().await.ok();
                });
            }

            // ── Completed task ────────────────────────────────────────────
            Some(result) = tasks.join_next() => {
                if let Err(e) = result {
                    tracing::error!(error = ?e, "task panicked");
                }
            }

            // ── Shutdown ──────────────────────────────────────────────────
            _ = &mut shutdown => {
                tracing::info!(
                    in_flight = tasks.len(),
                    "shutdown signal — draining in-flight tasks"
                );
                break;
            }
        }
    }

    // Drain all in-flight tasks before exiting (with timeout to prevent hang)
    let drain_timeout_secs: u64 = env_or("SHUTDOWN_DRAIN_TIMEOUT_SECS", "30")
        .parse().unwrap_or(30);
    let drain_deadline = std::time::Duration::from_secs(drain_timeout_secs);

    if tokio::time::timeout(drain_deadline, async {
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result {
                tracing::error!(error = ?e, "task panicked during drain");
            }
        }
    }).await.is_err() {
        tracing::error!(
            remaining = tasks.len(),
            timeout_secs = drain_timeout_secs,
            "drain timeout exceeded — aborting remaining tasks"
        );
        tasks.abort_all();
    }

    // ── Stop all running VMs on this node ───────────────────────────────────
    // Prevents ghost Firecracker processes surviving a daemon restart.
    // Nomad's stagger (30s) gives the new daemon time to re-provision.
    #[cfg(target_os = "linux")]
    {
        let vms: Vec<(String, deprovision::VmHandle)> = {
            let mut reg = registry.lock().await;
            reg.drain().collect()
        };
        if !vms.is_empty() {
            tracing::info!(count = vms.len(), "shutting down running VMs before exit");
            if let Ok(db) = pool.get().await {
                for (service_id, _) in &vms {
                    let svc_id: uuid::Uuid = match service_id.parse() {
                        Ok(id) => id,
                        Err(_) => continue,
                    };
                    let row = db.query_opt(
                        "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                         WHERE id = $1 AND node_id = $2 AND status = 'running' AND deleted_at IS NULL \
                         RETURNING slug",
                        &[&svc_id, &cfg.node_id],
                    ).await.ok().flatten();

                    if let Some(row) = row {
                        let slug: String = row.get("slug");
                        if let Ok(payload) = serde_json::to_vec(&RouteRemovedEvent { slug }) {
                            if let Err(e) = ctx.nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await {
                                tracing::warn!(error = %e, "NATS publish route_removed failed (shutdown cleanup)");
                            }
                        }
                    }
                }
            }
            for (service_id, handle) in vms {
                provision::release_tap_index(&handle.tap_name).await;
                deprovision::metal(&service_id, handle, &cfg.artifact_dir).await;
            }
            tracing::info!("all VMs stopped");
        }
    }

    tracing::info!("daemon exited cleanly");
    Ok(())
}

/// Restore a Metal service from a Firecracker snapshot.
///
/// Called when the daemon receives a WakeEvent (proxy detected a cold
/// service with a snapshot). Downloads snapshot from S3, creates TAP,
/// spawns Firecracker, loads snapshot, runs a quick health check, then
/// publishes RouteUpdatedEvent so the proxy can forward the held request.
#[cfg(target_os = "linux")]
async fn wake_from_snapshot(
    ctx: &Arc<ProvisionCtx>,
    cfg: &Arc<ProvisionConfig>,
    event: &WakeEvent,
    registry: &deprovision::VmRegistry,
) -> anyhow::Result<()> {
    use common::networking;
    use daemon::{ebpf, netlink, snapshot, tc, cgroup};
    use uuid::Uuid;

    // Download snapshot from S3 (or use local cache)
    let snap = snapshot::ensure_snapshot(
        &ctx.s3, &ctx.bucket, &event.snapshot_key, &cfg.artifact_dir, &event.service_id,
    ).await.context("downloading snapshot")?;

    // Allocate TAP + network identity
    let tap_idx = provision::allocate_tap_index().await?;
    let tap     = networking::tap_name(tap_idx);
    let ip      = networking::guest_ip(tap_idx).context("TAP index pool exhausted")?;
    let vm_id   = Uuid::now_v7();

    // Read the port from the DB so we can build upstream_addr.
    let port: i32 = {
        let db = ctx.pool.get().await.context("db pool")?;
        let svc_id: Uuid = event.service_id.parse().context("invalid service_id")?;
        let row = db.query_one(
            "SELECT port FROM services WHERE id = $1",
            &[&svc_id],
        ).await.context("reading service port")?;
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
        netlink::attach_to_bridge(&tap, &cfg.bridge).await.context("attach TAP")?;
        tc::apply(&tap, &common::events::ResourceQuota::default()).await.context("tc")?;
        ebpf::attach(&tap, &event.service_id).context("eBPF")?;

        // Spawn Firecracker and load snapshot (VM resumes immediately)
        let (fc_pid, _sock) = snapshot::restore_vm(
            &cfg.fc_bin, &cfg.sock_dir, &vm_id.to_string(), &snap, &serial_log,
        ).await.context("restoring VM from snapshot")?;

        // Apply cgroup limits (memory + IO)
        cgroup::apply(&event.service_id, fc_pid, &cfg.rootfs_dev, &common::events::ResourceQuota::default(), 128)
            .await
            .context("cgroup limits")?;

        // Quick health check — app was already running at snapshot time,
        // should respond almost immediately after restore.
        // Use a TCP connect probe (not HTTP) to avoid pulling in reqwest.
        let probe_timeout = std::time::Duration::from_secs(5);
        if tokio::time::timeout(probe_timeout, async {
            loop {
                if tokio::net::TcpStream::connect(&upstream_addr).await.is_ok() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }).await.is_err() {
            anyhow::bail!("wake health check timed out after {}s — app not listening on {upstream_addr}", probe_timeout.as_secs());
        }

        // Register in-memory for future deprovision
        registry.lock().await.insert(
            event.service_id.clone(),
            deprovision::VmHandle {
                tap_name:    tap.clone(),
                fc_pid,
                vm_id:       vm_id.to_string(),
                use_jailer:  cfg.use_jailer,
                chroot_base: cfg.chroot_base.clone(),
            },
        );

        // Update DB
        let db = ctx.pool.get().await.context("db pool")?;
        let svc_id: Uuid = event.service_id.parse()?;
        db.execute(
            "UPDATE services SET status = 'running', upstream_addr = $1, node_id = $2 WHERE id = $3",
            &[&Some(&upstream_addr), &cfg.node_id, &svc_id],
        ).await.context("updating service status to running")?;

        // Publish RouteUpdatedEvent — proxy unblocks the held request
        let payload = serde_json::to_vec(&RouteUpdatedEvent {
            slug:          event.slug.clone(),
            upstream_addr: upstream_addr.clone(),
        })?;
        ctx.nats.publish(SUBJECT_ROUTE_UPDATED, payload.into()).await.ok();

        tracing::info!(
            service_id = event.service_id,
            slug = event.slug,
            %upstream_addr,
            fc_pid,
            "service woke from snapshot"
        );

        Ok::<_, anyhow::Error>(())
    }.await {
        cleanup_on_err().await;
        return Err(e);
    }

    Ok(())
}

/// Non-Linux stub — wake is a no-op outside Linux.
#[cfg(not(target_os = "linux"))]
async fn wake_from_snapshot(
    _ctx: &Arc<ProvisionCtx>,
    _cfg: &Arc<ProvisionConfig>,
    _event: &WakeEvent,
    _registry: &deprovision::VmRegistry,
) -> anyhow::Result<()> {
    tracing::warn!("wake_from_snapshot called on non-Linux — no-op");
    Ok(())
}
