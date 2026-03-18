use anyhow::Result;

use common::contract::DeleteServiceResponse;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, confirm, print_ok};

pub async fn run(config: &Config, service_ref: &str, skip_confirm: bool, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let service_id = ctx.client.resolve_service(service_ref).await?;

    if !skip_confirm {
        confirm(output, &format!("Delete service \"{}\"? This cannot be undone.", service_ref))?;
    }

    let resp: DeleteServiceResponse = ctx.client
        .post_no_body_with_response(&format!("/services/{}/delete", service_id))
        .await?;

    print_ok(output, &format!("Deleted service \"{}\" (id: {})", resp.slug, resp.id));
    Ok(())
}
