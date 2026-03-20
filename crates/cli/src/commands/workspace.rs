use anyhow::Result;

use common::contract::WorkspaceResponse;

use crate::context::CommandContext;
use crate::config::Config;
use crate::output::{OutputMode, print_data, print_ok};
use crate::table::print_table;

pub async fn run_list(config: &Config, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let workspaces: Vec<WorkspaceResponse> = ctx.client.get("/workspaces").await?;

    print_data(ctx.output, &workspaces, |workspaces| {
        if workspaces.is_empty() {
            println!("No workspaces found.");
            return;
        }

        let active = config.workspace_id.as_deref().unwrap_or("");
        let rows: Vec<Vec<String>> = workspaces
            .iter()
            .map(|ws| vec![ws.slug.clone(), ws.name.clone(), ws.tier.clone(), ws.id.clone()])
            .collect();
        let markers: Vec<&str> = workspaces
            .iter()
            .map(|ws| if ws.id == active { "* " } else { "  " })
            .collect();
        print_table(
            &["SLUG", "NAME", "TIER", "ID"],
            &rows,
            &[],
            Some(&markers),
        );
    });
    Ok(())
}

pub async fn run_use(config: &mut Config, slug_or_id: &str, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let workspaces: Vec<WorkspaceResponse> = ctx.client.get("/workspaces").await?;

    let ws = workspaces
        .iter()
        .find(|w| w.id == slug_or_id || w.slug == slug_or_id)
        .ok_or_else(|| anyhow::anyhow!("workspace {:?} not found", slug_or_id))?;

    config.workspace_id = Some(ws.id.clone());
    config.save()?;
    print_ok(output, &format!("Switched to workspace: {}", ws.slug));
    Ok(())
}
