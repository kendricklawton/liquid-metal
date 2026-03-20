use anyhow::Result;

use common::contract::{DeployResponse, RollbackRequest};

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, confirm, print_ok};

pub async fn run(config: &Config, service_ref: &str, deploy_id: Option<&str>, skip_confirm: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    if !skip_confirm {
        let msg = match deploy_id {
            Some(id) => format!("Rollback \"{}\" to deploy {}?", service_ref, id),
            None => format!("Rollback \"{}\" to previous deployment?", service_ref),
        };
        confirm(output, &msg)?;
    }

    let resp: DeployResponse = ctx.client
        .post(
            &format!("/services/{}/rollback", service_id),
            &RollbackRequest {
                deploy_id: deploy_id.map(|s| s.to_string()),
            },
        )
        .await?;

    print_ok(output, &format!(
        "Rolling back \"{}\" — status: {}",
        resp.service.slug, resp.service.status
    ));
    Ok(())
}
