use std::sync::Arc;

use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use futures::StreamExt;
use tokio::task::JoinSet;

use common::events::{
    DeprovisionEvent, Engine, LiquidUsageEvent, MetalUsageEvent, ProvisionEvent,
    RouteRemovedEvent, SuspendEvent, TrafficPulseEvent,
    STREAM_NAME, SUBJECT_DEPROVISION, SUBJECT_PROVISION, SUBJECT_ROUTE_REMOVED,
    SUBJECT_SUSPEND, SUBJECT_TRAFFIC_PULSE,
    SUBJECT_USAGE_LIQUID, SUBJECT_USAGE_METAL,
};
#[cfg(target_os = "linux")]
use common::events::{ServiceCrashedEvent, SUBJECT_SERVICE_CRASHED};
use common::{Features, config::{env_or, require_env}};
use daemon::deprovision;
use daemon::provision::{self, ProvisionConfig, ProvisionCtx};
use daemon::storage;

const CONSUMER_PROVISION:   &str = "daemon-provision-1";
const CONSUMER_DEPROVISION: &str = "daemon-deprovision-1";

/// Default maximum concurrent provisions per node.
/// Each Metal VM is CPU/RAM-intensive; cap prevents OOM on burst deploys.
/// Override via NODE_MAX_CONCURRENT_PROVISIONS env var.
const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Default idle timeout in seconds. Services with no traffic for this duration
/// are stopped automatically (serverless scale-to-zero). Set to 0 to disable.
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 300;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "daemon=info,liquid_metal_daemon=info".into()),
        )
        .init();

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
        physical_cores: env_or("PHYSICAL_CORES", "4").parse().unwrap_or(4),
        // Jailer
        use_jailer:  env_or("USE_JAILER",          "false") == "true",
        jailer_bin:  env_or("JAILER_BIN",           "/usr/local/bin/jailer"),
        jailer_uid:  env_or("JAILER_UID",           "10000").parse().unwrap_or(10000),
        jailer_gid:  env_or("JAILER_GID",           "10000").parse().unwrap_or(10000),
        chroot_base: env_or("JAILER_CHROOT_BASE",   "/srv/jailer"),
        // Node identity + artifacts
        node_id:      env_or("NODE_ID",      "node-a"),
        artifact_dir: env_or("ARTIFACT_DIR", "/var/lib/liquid-metal/artifacts"),
    });

    let bucket = Arc::new(env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts"));

    // Postgres pool
    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let pool = Arc::new(if let Some(tls) = common::config::pg_tls()? {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tls);
        deadpool_postgres::Pool::builder(mgr).max_size(8).build()
            .context("building postgres pool (TLS)")?
    } else {
        let mgr = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
        deadpool_postgres::Pool::builder(mgr).max_size(8).build()
            .context("building postgres pool")?
    });

    // S3 client
    let s3 = Arc::new(storage::build_client());

    // VM registry — rebuilt from DB on startup, updated on each provision/deprovision
    let registry = deprovision::new_registry();
    let liquid_registry = deprovision::new_liquid_registry();

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
                ebpf::reattach_all(&taps);
            }
        }
    }


    // Rebuild VM registry from DB — services provisioned by a previous daemon
    // instance need handles so deprovision events can clean them up.
    {
        if let Ok(db) = pool.get().await {
            let rows = db
                .query(
                    "SELECT id::text, tap_name, fc_pid, cpu_core, vm_id \
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
                let cpu_core: i32    = row.get(3);
                let vm_id: String    = row.get(4);

                reg.insert(svc_id, deprovision::VmHandle {
                    tap_name,
                    fc_pid:      fc_pid as u32,
                    cpu_core:    cpu_core as u32,
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
            tracing::info!("traffic pulse subscriber ready (batched, 5s window)");

            let mut pending: HashSet<String> = HashSet::new();
            let mut flush_tick = interval(Duration::from_secs(5));

            loop {
                tokio::select! {
                    Some(msg) = sub.next() => {
                        if let Ok(event) = serde_json::from_slice::<TrafficPulseEvent>(&msg.payload) {
                            pending.insert(event.slug);
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
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(60));

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
                        service_id: sid,
                        slug,
                        engine: Engine::Metal,
                    };
                    if let Ok(payload) = serde_json::to_vec(&event) {
                        js_idle.publish(SUBJECT_DEPROVISION, payload.into()).await.ok();
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
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(60));

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
                        js_orphan.publish(SUBJECT_DEPROVISION, payload.into()).await.ok();
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
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(10));
            loop {
                ticker.tick().await;
                let crashed: Vec<(String, deprovision::VmHandle)> = {
                    let reg = registry.lock().await;
                    reg.iter()
                        .filter(|(_, h)| {
                            // /proc/{pid} exists iff the process is alive.
                            !std::path::Path::new(&format!("/proc/{}", h.fc_pid)).exists()
                        })
                        .map(|(id, h)| (id.clone(), h.clone()))
                        .collect()
                };
                for (service_id, handle) in crashed {
                    tracing::error!(service_id, fc_pid = handle.fc_pid, "VM crash detected — process no longer alive");
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
                                nats_crash.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await.ok();
                            }
                            // Publish crash event for observability.
                            let crash = ServiceCrashedEvent {
                                service_id: service_id.clone(),
                                slug,
                                exit_code: None,
                            };
                            if let Ok(payload) = serde_json::to_vec(&crash) {
                                nats_crash.publish(SUBJECT_SERVICE_CRASHED, payload.into()).await.ok();
                            }
                        }
                    }
                    // Clean up kernel resources (TAP, cgroup, CPU pin).
                    provision::release_tap_index(&handle.tap_name).await;
                    deprovision::metal(
                        &service_id, handle, cfg_crash.physical_cores, &cfg_crash.artifact_dir,
                    ).await;
                    registry.lock().await.remove(&service_id);
                }
            }
        });
    }

    // ── Metal usage reporter ─────────────────────────────────────────────────
    // Every 60s, publishes MetalUsageEvent for each running Metal service
    // on this node. Consumed by the API billing aggregator.
    {
        let pool    = pool.clone();
        let node_id = cfg.node_id.clone();
        let nats    = ctx.nats.clone();
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let db = match pool.get().await {
                    Ok(d)  => d,
                    Err(_) => continue,
                };
                let rows = match db.query(
                    "SELECT id::text, workspace_id::text, vcpu, memory_mb \
                     FROM services \
                     WHERE node_id = $1 AND engine = 'metal' AND status = 'running' \
                       AND deleted_at IS NULL",
                    &[&node_id],
                ).await {
                    Ok(r)  => r,
                    Err(e) => { tracing::warn!(error = %e, "usage reporter query failed"); continue; }
                };
                for row in &rows {
                    let event = MetalUsageEvent {
                        service_id:    row.get::<_, String>("id"),
                        workspace_id:  row.get::<_, String>("workspace_id"),
                        duration_secs: 60,
                        vcpu:          row.get::<_, i32>("vcpu") as u32,
                        memory_mb:     row.get::<_, i32>("memory_mb") as u32,
                    };
                    if let Ok(payload) = serde_json::to_vec(&event) {
                        nats.publish(SUBJECT_USAGE_METAL, payload.into()).await.ok();
                    }
                }
                if !rows.is_empty() {
                    tracing::debug!(count = rows.len(), "usage: reported Metal compute ticks");
                }
            }
        });
    }

    // ── Liquid usage reporter ─────────────────────────────────────────────
    // Every 60s, drains invocation counters from the liquid registry and
    // publishes LiquidUsageEvent for each active Wasm service.
    {
        let liquid_registry = liquid_registry.clone();
        let nats = ctx.nats.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let reg = liquid_registry.lock().await;
                let mut reported = 0u32;
                for (service_id, handle) in reg.iter() {
                    let invocations = handle.invocations.swap(0, Ordering::Relaxed);
                    if invocations == 0 {
                        continue;
                    }
                    let event = LiquidUsageEvent {
                        workspace_id: handle.workspace_id.clone(),
                        service_id:   service_id.clone(),
                        invocations,
                    };
                    if let Ok(payload) = serde_json::to_vec(&event) {
                        nats.publish(SUBJECT_USAGE_LIQUID, payload.into()).await.ok();
                    }
                    reported += 1;
                }
                if reported > 0 {
                    tracing::debug!(count = reported, "usage: reported Liquid invocation ticks");
                }
            }
        });
    }

    // ── Suspend consumer ──────────────────────────────────────────────────
    // Listens for SuspendEvent (workspace balance depleted) and deprovisions
    // all running services for that workspace on this node.
    {
        let pool            = pool.clone();
        let node_id         = cfg.node_id.clone();
        let nats            = ctx.nats.clone();
        let registry        = registry.clone();
        let liquid_registry = liquid_registry.clone();
        let cfg             = cfg.clone();
        tokio::spawn(async move {
            let mut sub = match nats.subscribe(SUBJECT_SUSPEND).await {
                Ok(s)  => s,
                Err(e) => { tracing::warn!(error = %e, "suspend subscriber setup failed"); return; }
            };
            tracing::info!("suspend subscriber ready");
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
                let rows = match db.query(
                    "UPDATE services SET status = 'suspended', upstream_addr = NULL \
                     WHERE workspace_id = $1 AND node_id = $2 \
                       AND status = 'running' AND deleted_at IS NULL \
                     RETURNING id::text, slug, engine",
                    &[&wid, &node_id],
                ).await {
                    Ok(r)  => r,
                    Err(e) => { tracing::error!(error = %e, "suspend query failed"); continue; }
                };
                for row in &rows {
                    let sid:  String = row.get("id");
                    let slug: String = row.get("slug");
                    let eng:  String = row.get("engine");

                    if eng == "metal" {
                        let handle = registry.lock().await.remove(&sid);
                        if let Some(h) = handle {
                            deprovision::metal(&sid, h, cfg.physical_cores, &cfg.artifact_dir).await;
                        }
                    } else if eng == "liquid" {
                        liquid_registry.lock().await.remove(&sid);
                    }

                    // Evict from route cache.
                    if let Ok(payload) = serde_json::to_vec(&RouteRemovedEvent { slug }) {
                        nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await.ok();
                    }
                }
                if !rows.is_empty() {
                    tracing::warn!(workspace_id = event.workspace_id, count = rows.len(), "suspended services");
                }
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
            ack_wait: std::time::Duration::from_secs(300),
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

    // ── Consumer lag monitor ─────────────────────────────────────────────────
    // Periodically check JetStream consumer lag and warn when it exceeds 50
    // pending messages — indicates the daemon can't keep up with deploy volume.
    {
        let mut prov_consumer = provision_consumer.clone();
        tokio::spawn(async move {
            use tokio::time::{Duration, interval};
            let mut ticker = interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                if let Ok(info) = prov_consumer.info().await {
                    let pending = info.num_pending;
                    if pending > 50 {
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

    let mut provision_msgs   = provision_consumer.messages().await.context("subscribe provision")?;
    let mut deprovision_msgs = deprovision_consumer.messages().await.context("subscribe deprovision")?;

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

                tasks.spawn(async move {
                    let _permit = permit;
                    match provision::provision(&ctx, &event).await {
                        Ok(svc) => {
                            tracing::info!(
                                id            = %svc.id,
                                engine        = ?svc.engine,
                                upstream_addr = ?svc.upstream_addr,
                                app           = event.app_name,
                                "provision succeeded"
                            );
                            msg.ack().await.ok();
                        }
                        Err(e) => {
                            tracing::error!(error = %e, app = event.app_name, "provision failed — NACKing");
                            msg.ack_with(AckKind::Nak(None)).await.ok();
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
                                        cfg.physical_cores,
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
                        ctx.nats.publish(SUBJECT_ROUTE_REMOVED, payload.into()).await.ok();
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

    // Drain all in-flight tasks before exiting
    while let Some(result) = tasks.join_next().await {
        if let Err(e) = result {
            tracing::error!(error = ?e, "task panicked during drain");
        }
    }

    tracing::info!("daemon exited cleanly");
    Ok(())
}
