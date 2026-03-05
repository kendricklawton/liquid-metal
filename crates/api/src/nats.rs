use anyhow::{Context, Result};
use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;
use common::events::{ProvisionEvent, STREAM_NAME, SUBJECT_PROVISION};

pub async fn ensure_stream(js: &jetstream::Context) -> Result<()> {
    js.get_or_create_stream(StreamConfig {
        name: STREAM_NAME.to_string(),
        subjects: vec!["platform.*".to_string()],
        ..Default::default()
    })
    .await
    .context("ensuring NATS stream")?;
    Ok(())
}

pub async fn publish_provision(js: &jetstream::Context, event: &ProvisionEvent) -> Result<()> {
    let payload = serde_json::to_vec(event).context("serializing provision event")?;
    js.publish(SUBJECT_PROVISION, payload.into())
        .await
        .context("publishing provision event")?
        .await
        .context("awaiting publish ack")?;
    Ok(())
}
