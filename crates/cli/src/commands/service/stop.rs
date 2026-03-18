use anyhow::Result;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, confirm, print_ok};

pub async fn run(config: &Config, service_ref: &str, skip_confirm: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let service_id = ctx.client.resolve_service(service_ref).await?;

    if !skip_confirm {
        confirm(output, &format!("Stop service \"{}\"?", service_ref))?;
    }

    ctx.client
        .post_no_body(&format!("/services/{}/stop", service_id))
        .await?;
    print_ok(output, &format!("Service {} stopped.", service_ref));
    Ok(())
}
