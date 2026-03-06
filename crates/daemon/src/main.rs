mod firecracker;
#[cfg(target_os = "linux")]
mod cgroup;
#[cfg(target_os = "linux")]
mod cilium;
#[cfg(target_os = "linux")]
mod cpu;
#[cfg(target_os = "linux")]
mod jailer;
#[cfg(target_os = "linux")]
mod netlink;
#[cfg(target_os = "linux")]
mod tc;
mod provision;
mod verify;
mod wasm;

use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use common::config::{env_or, require_env};
use common::events::{ProvisionEvent, STREAM_NAME, SUBJECT_PROVISION};
use futures::StreamExt;
use provision::ProvisionConfig;
use std::sync::Arc;

const CONSUMER: &str = "daemon-worker-1";

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

    let cfg = ProvisionConfig {
        // Block device for cgroup v2 io.max (find with: lsblk -o NAME,MAJ:MIN)
        rootfs_dev:     env_or("ROOTFS_DEVICE", "8:0"),
        // Physical (not logical/HT) cores available for VM pinning
        physical_cores: env_or("PHYSICAL_CORES", "4").parse().unwrap_or(4),
        // USE_JAILER=true enables the Firecracker jailer (namespaces + chroot + seccomp)
        // Requires /usr/local/bin/jailer — use `false` for local dev on macOS
        use_jailer:     env_or("USE_JAILER", "false") == "true",
        jailer_bin:     env_or("JAILER_BIN",  "/usr/local/bin/jailer"),
        jailer_uid:     env_or("JAILER_UID",  "10000").parse().unwrap_or(10000),
        jailer_gid:     env_or("JAILER_GID",  "10000").parse().unwrap_or(10000),
        chroot_base:    env_or("JAILER_CHROOT_BASE", "/srv/jailer"),
    };

    let pg_cfg: tokio_postgres::Config = db_url.parse().context("invalid DATABASE_URL")?;
    let mgr  = deadpool_postgres::Manager::new(pg_cfg, tokio_postgres::NoTls);
    let pool = Arc::new(
        deadpool_postgres::Pool::builder(mgr)
            .max_size(4)
            .build()
            .context("building postgres pool")?,
    );

    let cfg = Arc::new(cfg);

    tracing::info!(
        %nats_url,
        rootfs_dev   = cfg.rootfs_dev,
        physical_cores = cfg.physical_cores,
        use_jailer   = cfg.use_jailer,
        "daemon starting"
    );

    let nc = async_nats::connect(&nats_url).await.context("NATS connect")?;
    let js = async_nats::jetstream::new(nc);

    js.get_or_create_stream(StreamConfig {
        name:     STREAM_NAME.to_string(),
        subjects: vec!["platform.*".to_string()],
        ..Default::default()
    })
    .await
    .context("ensure stream")?;

    let consumer = js
        .get_stream(STREAM_NAME)
        .await
        .context("get stream")?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name:   Some(CONSUMER.to_string()),
            filter_subject: SUBJECT_PROVISION.to_string(),
            ..Default::default()
        })
        .await
        .context("create consumer")?;

    tracing::info!(subject = SUBJECT_PROVISION, "daemon ready");
    let mut messages = consumer.messages().await.context("subscribe")?;

    while let Some(msg) = messages.next().await {
        match msg {
            Err(e) => tracing::error!(error = %e, "NATS error"),
            Ok(message) => match serde_json::from_slice::<ProvisionEvent>(&message.payload) {
                Err(e) => {
                    tracing::error!(error = %e, "parse error — ACKing to avoid poison-pill loop");
                    message.ack().await.ok();
                }
                Ok(event) => {
                    tracing::info!(
                        app    = event.app_name,
                        engine = ?event.engine,
                        "received provision event"
                    );
                    match provision::provision(&pool, &cfg, &event).await {
                        Ok(svc) => {
                            tracing::info!(
                                id            = %svc.id,
                                engine        = ?svc.engine,
                                upstream_addr = ?svc.upstream_addr,
                                app           = event.app_name,
                                "provision succeeded"
                            );
                            message.ack().await.ok();
                        }
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                app   = event.app_name,
                                "provision failed — NACKing for retry"
                            );
                            message.ack_with(AckKind::Nak(None)).await.ok();
                        }
                    }
                }
            },
        }
    }
    Ok(())
}
