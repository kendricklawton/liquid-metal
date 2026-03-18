use anyhow::Result;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_ok};

pub async fn run(config: &Config, service_ref: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let svc = ctx.client.resolve_service_full(service_ref).await?;
    let url = format!("https://{}.{}", svc.slug, ctx.config.platform_domain());
    print_ok(output, &format!("Opening {}", url));
    open::that(&url)?;
    Ok(())
}
