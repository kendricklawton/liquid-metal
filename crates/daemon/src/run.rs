//! Daemon orchestrator.
//!
//! `run()` is the single entry point called by `main()`. It handles startup,
//! spawns all background tasks, runs the main event loop, and performs
//! graceful shutdown.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use common::config::{env_or, require_env};
use common::events::{
    DeprovisionEvent, Engine, ProvisionEvent, RouteRemovedEvent, WakeEvent,
    STREAM_NAME, SUBJECT_DEPROVISION, SUBJECT_PROVISION, SUBJECT_ROUTE_REMOVED,
    SUBJECT_WAKE,
};
use common::Features;
use futures::StreamExt;
use tokio::task::JoinSet;

use crate::{deprovision, provision::{self, ProvisionCtx}, startup, storage, tasks, wake};

const CONSUMER_PROVISION: &str = "daemon-provision-1";
const CONSUMER_DEPROVISION: &str = "daemon-deprovision-1";
const CONSUMER_WAKE: &str = "daemon-wake-1";

/// Default maximum concurrent provisions per node.
const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Default idle timeout in seconds. Set to 0 to disable (Metal dedicated VMs
/// are always-on — they run until explicitly deleted by the user).
const DEFAULT_IDLE_TIMEOUT_SECS: i64 = 0;

pub async fn run() -> Result<()> {
    // ── PID file lock — prevent two daemons on the same node ────────────────
    let pid_file_path = env_or("DAEMON_PID_FILE", "/run/liquid-metal-daemon.pid");
    let _pid_lock = startup::acquire_pid_lock(&pid_file_path)?;

    let nats_url = env_or("NATS_URL", "nats://127.0.0.1:4222");
    let db_url = require_env("DATABASE_URL")?;

    let cfg = startup::build_config();
    let bucket = Arc::new(env_or("OBJECT_STORAGE_BUCKET", "liquid-metal-artifacts"));
    let pool = startup::build_pool(&db_url).await?;
    let s3 = Arc::new(storage::build_client()?);

    // VM registry — rebuilt from DB on startup, updated on each provision/deprovision
    let registry = deprovision::new_registry();
    let liquid_registry = deprovision::new_liquid_registry();

    // ── Startup cleanup + rebuild ───────────────────────────────────────────
    let stale_liquid_slugs = startup::run_cleanup(&pool, &cfg).await;
    startup::check_clock_drift(&pool).await;
    startup::reattach_ebpf(&pool, &cfg).await;
    startup::rebuild_registry(&pool, &cfg, &registry).await;

    // ── NATS connect ────────────────────────────────────────────────────────
    let nc = startup::connect_nats_and_evict(&nats_url, &stale_liquid_slugs).await?;
    let js = async_nats::jetstream::new(nc.clone());

    // Save plain-client clone for the pulse subscriber before nc is moved into Arc.
    let nc_pulse = nc.clone();

    let ctx = Arc::new(ProvisionCtx {
        pool: pool.clone(),
        cfg: cfg.clone(),
        s3: s3.clone(),
        bucket: bucket.clone(),
        registry: registry.clone(),
        liquid_registry: liquid_registry.clone(),
        nats: Arc::new(nc),
    });

    let features = Features::from_env();
    features.log_summary();

    let idle_timeout_secs: i64 =
        env_or("IDLE_TIMEOUT_SECS", &DEFAULT_IDLE_TIMEOUT_SECS.to_string())
            .parse()
            .unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS);

    // ── Spawn background tasks ──────────────────────────────────────────────
    tasks::pulse::spawn(pool.clone(), nc_pulse);

    // Idle checker runs unconditionally — Liquid has its own timeout
    // (LIQUID_IDLE_TIMEOUT_SECS, default 300s). Metal idle is controlled
    // by IDLE_TIMEOUT_SECS (default 0 = disabled for dedicated VMs).
    tasks::idle::spawn(
        pool.clone(),
        cfg.node_id.clone(),
        js.clone(),
        ctx.nats.clone(),
        liquid_registry.clone(),
        idle_timeout_secs,
    );

    tasks::orphan_sweep::spawn(pool.clone(), cfg.node_id.clone(), js.clone());

    #[cfg(target_os = "linux")]
    tasks::crash_watcher::spawn(
        registry.clone(),
        pool.clone(),
        cfg.node_id.clone(),
        ctx.nats.clone(),
        cfg.clone(),
    );

    #[cfg(target_os = "linux")]
    tasks::ebpf_audit::spawn(pool.clone(), registry.clone(), cfg.node_id.clone());

    tasks::usage::spawn(liquid_registry.clone(), ctx.nats.clone());

    tasks::suspend::spawn(
        pool.clone(),
        cfg.node_id.clone(),
        ctx.nats.clone(),
        registry.clone(),
        liquid_registry.clone(),
        cfg.clone(),
    );

    tracing::info!(
        %nats_url,
        node_id    = cfg.node_id,
        use_jailer = cfg.use_jailer,
        "daemon starting"
    );

    js.get_or_create_stream(StreamConfig {
        name: STREAM_NAME.to_string(),
        subjects: vec!["platform.*".to_string()],
        ..Default::default()
    })
    .await
    .context("ensure stream")?;

    // ── JetStream consumers ─────────────────────────────────────────────────
    let provision_consumer = js
        .get_stream(STREAM_NAME)
        .await
        .context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(CONSUMER_PROVISION.to_string()),
            filter_subject: SUBJECT_PROVISION.to_string(),
            ack_wait: std::time::Duration::from_secs(
                env_or("PROVISION_ACK_WAIT_SECS", "300")
                    .parse()
                    .unwrap_or(300),
            ),
            max_deliver: 3,
            ..Default::default()
        })
        .await
        .context("create provision consumer")?;

    let deprovision_consumer = js
        .get_stream(STREAM_NAME)
        .await
        .context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(CONSUMER_DEPROVISION.to_string()),
            filter_subject: SUBJECT_DEPROVISION.to_string(),
            ..Default::default()
        })
        .await
        .context("create deprovision consumer")?;

    let wake_consumer = js
        .get_stream(STREAM_NAME)
        .await
        .context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some(CONSUMER_WAKE.to_string()),
            filter_subject: SUBJECT_WAKE.to_string(),
            ack_wait: std::time::Duration::from_secs(30),
            max_deliver: 3,
            ..Default::default()
        })
        .await
        .context("create wake consumer")?;

    tasks::lag_monitor::spawn(provision_consumer.clone());

    // ── Health check endpoint ───────────────────────────────────────────────
    let health_port = env_or("HEALTH_PORT", "9090");
    let health_bind = format!("0.0.0.0:{health_port}");
    let listener = tokio::net::TcpListener::bind(&health_bind)
        .await
        .context("binding health check listener")?;
    tracing::info!(%health_bind, "health check server listening");
    tasks::health::spawn(
        cfg.node_id.clone(),
        registry.clone(),
        liquid_registry.clone(),
        listener,
    );

    // ── Main event loop ─────────────────────────────────────────────────────
    let mut provision_msgs = provision_consumer
        .messages()
        .await
        .context("subscribe provision")?;
    let mut deprovision_msgs = deprovision_consumer
        .messages()
        .await
        .context("subscribe deprovision")?;
    let mut wake_msgs = wake_consumer
        .messages()
        .await
        .context("subscribe wake")?;

    tracing::info!("daemon ready");

    let max_concurrent: usize = env_or(
        "NODE_MAX_CONCURRENT_PROVISIONS",
        &DEFAULT_MAX_CONCURRENT.to_string(),
    )
    .parse()
    .unwrap_or(DEFAULT_MAX_CONCURRENT);
    tracing::info!(max_concurrent, "provision concurrency cap");
    let sem = Arc::new(tokio::sync::Semaphore::new(max_concurrent));

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

                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        // Semaphore closed — shutdown in progress. NAK so NATS retries after restart.
                        msg.ack_with(AckKind::Nak(None)).await.ok();
                        continue;
                    }
                };
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

            // ── Wake message (Metal snapshot restore / Liquid scale-from-zero) ──
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

                // Dedup: if the service is already running (Metal in registry,
                // Liquid in liquid_registry), ACK and skip.
                let already_running = match event.engine {
                    Engine::Metal  => registry.lock().await.contains_key(&event.service_id),
                    Engine::Liquid => liquid_registry.lock().await.contains_key(&event.service_id),
                };
                if already_running {
                    tracing::debug!(service_id = event.service_id, engine = ?event.engine, "wake: already running — skipping");
                    msg.ack().await.ok();
                    continue;
                }

                tracing::info!(service_id = event.service_id, slug = event.slug, engine = ?event.engine, "wake event received");

                let ctx      = ctx.clone();
                let cfg      = cfg.clone();
                let registry = registry.clone();
                let sem      = sem.clone();

                tasks.spawn(async move {
                    let _permit = match sem.acquire().await {
                        Ok(p) => p,
                        Err(_) => { msg.ack().await.ok(); return; }
                    };

                    let result = match event.engine {
                        Engine::Metal  => wake::wake_from_snapshot(&ctx, &cfg, &event, &registry).await,
                        Engine::Liquid => wake::wake_liquid(&ctx, &cfg, &event).await,
                    };

                    match result {
                        Ok(()) => {
                            tracing::info!(service_id = event.service_id, engine = ?event.engine, "wake complete");
                            msg.ack().await.ok();
                        }
                        Err(e) => {
                            tracing::error!(service_id = event.service_id, engine = ?event.engine, error = %e, "wake failed");
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

    // ── Drain in-flight tasks ───────────────────────────────────────────────
    let drain_timeout_secs: u64 = env_or("SHUTDOWN_DRAIN_TIMEOUT_SECS", "30")
        .parse()
        .unwrap_or(30);
    let drain_deadline = std::time::Duration::from_secs(drain_timeout_secs);

    if tokio::time::timeout(drain_deadline, async {
        while let Some(result) = tasks.join_next().await {
            if let Err(e) = result {
                tracing::error!(error = ?e, "task panicked during drain");
            }
        }
    })
    .await
    .is_err()
    {
        tracing::error!(
            remaining = tasks.len(),
            timeout_secs = drain_timeout_secs,
            "drain timeout exceeded — aborting remaining tasks"
        );
        tasks.abort_all();
    }

    // ── Stop all running VMs on this node ───────────────────────────────────
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
                    let row = db
                        .query_opt(
                            "UPDATE services SET status = 'stopped', upstream_addr = NULL \
                             WHERE id = $1 AND node_id = $2 AND status = 'running' AND deleted_at IS NULL \
                             RETURNING slug",
                            &[&svc_id, &cfg.node_id],
                        )
                        .await
                        .ok()
                        .flatten();

                    if let Some(row) = row {
                        let slug: String = row.get("slug");
                        if let Ok(payload) =
                            serde_json::to_vec(&RouteRemovedEvent { slug })
                        {
                            if let Err(e) = ctx
                                .nats
                                .publish(SUBJECT_ROUTE_REMOVED, payload.into())
                                .await
                            {
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
