use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct Workspace {
    id: String,
    name: String,
    slug: String,
    tier: String,
}

pub async fn run_list(config: &Config) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let workspaces: Vec<Workspace> = client.get("/workspaces").await?;
    if workspaces.is_empty() {
        println!("No workspaces found.");
        return Ok(());
    }

    let active = config.workspace_id.as_deref().unwrap_or("");
    println!("  {:<20} {:<20} {:<10} {}", "SLUG", "NAME", "TIER", "ID");
    for ws in &workspaces {
        let marker = if ws.id == active { "* " } else { "  " };
        println!("{}{:<20} {:<20} {:<10} {}", marker, ws.slug, ws.name, ws.tier, ws.id);
    }
    Ok(())
}

pub async fn run_use(config: &mut Config, slug_or_id: &str) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let workspaces: Vec<Workspace> = client.get("/workspaces").await?;

    let ws = workspaces
        .iter()
        .find(|w| w.id == slug_or_id || w.slug == slug_or_id)
        .ok_or_else(|| anyhow::anyhow!("workspace {:?} not found", slug_or_id))?;

    config.workspace_id = Some(ws.id.clone());
    config.save()?;
    println!("Switched to workspace: {}", ws.slug);
    Ok(())
}
