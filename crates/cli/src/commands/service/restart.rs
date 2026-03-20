use anyhow::Result;

use common::contract::DeployResponse;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_ok};

pub async fn run(config: &Config, service_ref: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: DeployResponse = ctx.client
        .post(&format!("/services/{}/restart", service_id), &serde_json::Value::Null)
        .await?;

    print_ok(output, &format!(
        "Restarting {} — status: {}",
        resp.service.slug, resp.service.status
    ));
    Ok(())
}
