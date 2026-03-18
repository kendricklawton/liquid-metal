use anyhow::Result;

use common::contract::DeploymentHistoryResponse;

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_data};
use crate::table::print_table;

pub async fn run_list(config: &Config, service_ref: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let service_id = ctx.client.resolve_service(service_ref).await?;

    let resp: DeploymentHistoryResponse = ctx.client
        .get(&format!("/services/{}/deploys", service_id))
        .await?;

    print_data(ctx.output, &resp, |resp| {
        if resp.deploys.is_empty() {
            println!("No deployment history.");
        } else {
            let rows: Vec<Vec<String>> = resp.deploys
                .iter()
                .map(|d| {
                    let short_id = if d.id.len() > 8 { &d.id[..8] } else { &d.id };
                    let short_sha = d.commit_sha.as_deref()
                        .map(|s| if s.len() > 7 { &s[..7] } else { s })
                        .unwrap_or("-");
                    vec![
                        short_id.to_string(),
                        d.engine.clone(),
                        short_sha.to_string(),
                        d.created_at.clone(),
                    ]
                })
                .collect();
            let markers: Vec<&str> = resp.deploys
                .iter()
                .map(|d| if d.is_active.unwrap_or(false) { "* " } else { "  " })
                .collect();
            print_table(&["ID", "ENGINE", "COMMIT", "DEPLOYED"], &rows, &[], Some(&markers));
        }
    });
    Ok(())
}
