mod firecracker;
#[cfg(target_os = "linux")]
mod netlink;
mod provision;
mod wasm;

use anyhow::{Context, Result};
use async_nats::jetstream::AckKind;
use async_nats::jetstream::stream::Config as StreamConfig;
use common::config::env_or;
use common::events::{ProvisionEvent, STREAM_NAME, SUBJECT_PROVISION};
use futures::StreamExt;

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
    tracing::info!(%nats_url, "daemon connecting to NATS");

    let nc = async_nats::connect(&nats_url).await.context("NATS connect")?;
    let js = async_nats::jetstream::new(nc);

    js.get_or_create_stream(StreamConfig {
        name: STREAM_NAME.to_string(),
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
                    tracing::error!(error = %e, "parse error");
                    message.ack().await.ok();
                }
                Ok(event) => {
                    tracing::info!(
                        app    = event.app_name,
                        engine = ?event.engine,
                        "received provision event"
                    );
                    match provision::provision(&event).await {
                        Ok(svc) => {
                            tracing::info!(
                                id     = %svc.id,
                                engine = ?svc.engine,
                                app    = event.app_name,
                                "provision succeeded"
                            );
                            message.ack().await.ok();
                        }
                        Err(e) => {
                            tracing::error!(error = %e, app = event.app_name, "provision failed");
                            message.ack_with(AckKind::Nak(None)).await.ok();
                        }
                    }
                }
            },
        }
    }
    Ok(())
}
