use anyhow::Result;

use common::contract::{ScaleRequest, ScaleResponse};

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, confirm, print_ok};

pub async fn run(config: &Config, service_ref: &str, mode: &str, skip_confirm: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    if !skip_confirm {
        confirm(output, &format!("Change \"{}\" to {} mode?", service_ref, mode))?;
    }

    let resp: ScaleResponse = ctx.client
        .post(
            &format!("/services/{}/scale", service_id),
            &ScaleRequest { mode: mode.to_string() },
        )
        .await?;

    print_ok(output, &format!("Service \"{}\" set to {} mode.", resp.slug, resp.run_mode));
    Ok(())
}
