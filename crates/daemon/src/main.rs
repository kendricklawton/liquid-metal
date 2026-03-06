use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use common::config::{env_or, require_env};
use common::events::{DeprovisionEvent, ProvisionEvent, STREAM_NAME, SUBJECT_DEPROVISION, SUBJECT_PROVISION};
use daemon::deprovision;
use daemon::provision::{self, ProvisionConfig, ProvisionCtx};
use daemon::storage;
use futures::StreamExt;
use std::sync::Arc;
use tokio::task::JoinSet;

const CONSUMER_PROVISION:   &str = "daemon-provision-1";
const CONSUMER_DEPROVISION: &str = "daemon-deprovision-1";

/// Maximum concurrent provisions per node.
/// Each Metal VM is CPU/RAM-intensive; cap prevents OOM on burst deploys.
const MAX_CONCURRENT: usize = 16;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "daemon=info".into()),
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
    let mgr  = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = Arc::new(
        deadpool_postgres::Pool::builder(mgr)
            .max_size(8)
            .build()
            .context("building postgres pool")?,
    );

    // S3 client
    let s3 = Arc::new(storage::build_client());

    // VM registry — rebuilt from DB on startup, updated on each provision/deprovision
    let registry = deprovision::new_registry();

    // Initialize TAP counter from DB to avoid collisions after restart
    provision::init_tap_counter(&pool, &cfg.node_id).await;

    let ctx = Arc::new(ProvisionCtx {
        pool:     pool.clone(),
        cfg:      cfg.clone(),
        s3:       s3.clone(),
        bucket:   bucket.clone(),
        registry: registry.clone(),
    });

    tracing::info!(
        %nats_url,
        node_id   = cfg.node_id,
        use_jailer = cfg.use_jailer,
        "daemon starting"
    );

    // NATS
    let nc = async_nats::connect(&nats_url).await.context("NATS connect")?;
    let js = async_nats::jetstream::new(nc);

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

    let mut provision_msgs   = provision_consumer.messages().await.context("subscribe provision")?;
    let mut deprovision_msgs = deprovision_consumer.messages().await.context("subscribe deprovision")?;

    tracing::info!("daemon ready");

    // Semaphore caps concurrent in-flight provisions
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT));

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

                let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
                let ctx2   = ctx.clone();

                tasks.spawn(async move {
                    let _permit = permit;
                    match provision::provision(&ctx2, &event).await {
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

                let registry2    = registry.clone();
                let cfg2         = cfg.clone();
                let artifact_dir = cfg.artifact_dir.clone();

                tasks.spawn(async move {
                    let handle = registry2.lock().await.remove(&event.service_id);
                    match handle {
                        Some(h) => {
                            deprovision::metal(
                                &event.service_id,
                                h,
                                cfg2.physical_cores,
                                &artifact_dir,
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
