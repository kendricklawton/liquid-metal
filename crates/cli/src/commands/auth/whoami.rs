use anyhow::Result;

use common::contract::{UserResponse, WorkspaceResponse};

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_data};

pub async fn run(config: &Config, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;

    let user: UserResponse = match ctx.client.get("/users/me").await {
        Ok(u) => u,
        Err(e) if e.to_string().contains("401") => {
            anyhow::bail!("not logged in — run: flux login");
        }
        Err(e) => return Err(e),
    };

    let workspaces: Vec<WorkspaceResponse> = ctx.client
        .get("/workspaces")
        .await
        .unwrap_or_default();

    #[derive(serde::Serialize)]
    struct WhoamiOutput {
        user: UserResponse,
        workspaces: Vec<WorkspaceResponse>,
    }

    let data = WhoamiOutput { user, workspaces };

    print_data(ctx.output, &data, |data| {
        println!("name:  {}", data.user.name);
        println!("email: {}", data.user.email);
        println!("id:    {}", data.user.id);

        if data.workspaces.is_empty() {
            println!("  (could not fetch workspaces)");
            return;
        }

        let active = config.workspace_id.as_deref().unwrap_or("");
        for ws in &data.workspaces {
            let marker = if ws.id == active { "* " } else { "  " };
            println!("{}workspace: {}  tier: {}", marker, ws.slug, ws.tier);
        }
    });

    Ok(())
}
