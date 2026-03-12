use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct User {
    id: String,
    name: String,
    email: String,
}

#[derive(Deserialize)]
struct Workspace {
    id: String,
    slug: String,
    tier: String,
}

pub async fn run(config: &Config) -> Result<()> {
    let token = config.require_token()?;
    let client = ApiClient::new(config.api_url(), Some(token));

    let user: User = client.get("/users/me").await?;
    println!("name:  {}", user.name);
    println!("email: {}", user.email);
    println!("id:    {}", user.id);

    let workspaces: Vec<Workspace> = client.get("/workspaces").await.unwrap_or_default();
    let active = config.workspace_id.as_deref().unwrap_or("");
    for ws in &workspaces {
        let marker = if ws.id == active { "* " } else { "  " };
        println!("{}workspace: {}  tier: {}", marker, ws.slug, ws.tier);
    }

    Ok(())
}
