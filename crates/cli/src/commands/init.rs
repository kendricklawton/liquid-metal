use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Serialize)]
struct CreateProjectRequest<'a> {
    workspace_id: &'a str,
    name: &'a str,
    slug: &'a str,
}

#[derive(Deserialize)]
struct Project {
    id: String,
}

#[derive(Deserialize)]
struct CreateProjectResponse {
    project: Project,
}

#[derive(Serialize)]
struct ServiceSection<'a> {
    name: &'a str,
    engine: &'a str,
    project_id: &'a str,
}

#[derive(Serialize)]
struct BuildSection {
    command: String,
    output: String,
}

#[derive(Serialize)]
struct LiquidMetalConfig<'a> {
    service: ServiceSection<'a>,
    build: BuildSection,
}

pub async fn run(config: &Config) -> Result<()> {
    let token = config.require_token()?;
    let workspace_id = config
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no workspace found — run `flux login` first"))?;

    if std::path::Path::new("liquid-metal.toml").exists() {
        bail!("liquid-metal.toml already exists — edit it directly or delete it to re-init");
    }

    let cwd = std::env::current_dir()?;
    let name = to_slug(
        cwd.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("service"),
    );

    println!(
        "Initializing service {:?} in workspace {}...\n",
        name, workspace_id
    );

    let client = ApiClient::new(config.api_url(), Some(token));
    let resp: CreateProjectResponse = client
        .post(
            "/projects",
            &CreateProjectRequest {
                workspace_id,
                name: &name,
                slug: &name,
            },
        )
        .await?;

    let project_id = resp.project.id;

    let cfg = LiquidMetalConfig {
        service: ServiceSection {
            name: &name,
            engine: "liquid",
            project_id: &project_id,
        },
        build: BuildSection {
            command: "cargo build --target wasm32-wasip1 --release".to_string(),
            output: "target/wasm32-wasip1/release/main.wasm".to_string(),
        },
    };

    std::fs::write("liquid-metal.toml", toml::to_string(&cfg)?)?;

    println!("Created liquid-metal.toml");
    println!("  service: {}", name);
    println!("  project: {}", project_id);
    println!("  engine:  liquid\n");
    println!("Edit liquid-metal.toml if needed, then run:\n\n  flux deploy");
    Ok(())
}

fn to_slug(s: &str) -> String {
    let s = s.to_lowercase();
    let s: String = s
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    s.trim_matches('-').to_string()
}
