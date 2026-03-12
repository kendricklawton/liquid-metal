use anyhow::{Context, Result};
use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;

use common::events::{
    DeprovisionEvent, ProvisionEvent, STREAM_NAME, SUBJECT_DEPROVISION, SUBJECT_PROVISION,
};

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

pub async fn publish_deprovision(js: &jetstream::Context, event: &DeprovisionEvent) -> Result<()> {
    let payload = serde_json::to_vec(event).context("serializing deprovision event")?;
    js.publish(SUBJECT_DEPROVISION, payload.into())
        .await
        .context("publishing deprovision event")?
        .await
        .context("awaiting deprovision publish ack")?;
    Ok(())
}
