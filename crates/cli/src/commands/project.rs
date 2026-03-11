use anyhow::Result;
use serde::Deserialize;

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct Project {
    id: String,
    name: String,
    slug: String,
}

pub async fn run_list(config: &Config) -> Result<()> {
    let token = config.require_token()?;
    let workspace_id = config
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no active workspace — run `flux workspace use <slug>` first"))?;

    let client = ApiClient::new(config.api_url(), Some(token));
    let projects: Vec<Project> = client
        .get(&format!("/projects?workspace_id={}", workspace_id))
        .await?;

    if projects.is_empty() {
        println!("No projects found in this workspace.");
        return Ok(());
    }

    let active_project = read_project_toml_id();
    println!("  {:<20} {:<20} {}", "SLUG", "NAME", "ID");
    for p in &projects {
        let marker = if active_project.as_deref() == Some(&p.id) { "* " } else { "  " };
        println!("{}{:<20} {:<20} {}", marker, p.slug, p.name, p.id);
    }
    Ok(())
}

pub async fn run_use(config: &Config, slug_or_id: &str) -> Result<()> {
    let token = config.require_token()?;
    let workspace_id = config
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no active workspace — run `flux workspace use <slug>` first"))?;

    let client = ApiClient::new(config.api_url(), Some(token));
    let projects: Vec<Project> = client
        .get(&format!("/projects?workspace_id={}", workspace_id))
        .await?;

    let project = projects
        .iter()
        .find(|p| p.id == slug_or_id || p.slug == slug_or_id)
        .ok_or_else(|| anyhow::anyhow!("project {:?} not found in active workspace", slug_or_id))?;

    set_project_in_toml(&project.id)?;
    println!("Set project_id in liquid-metal.toml: {}", project.id);
    Ok(())
}

fn read_project_toml_id() -> Option<String> {
    let contents = std::fs::read_to_string("liquid-metal.toml").ok()?;
    let val: toml::Value = toml::from_str(&contents).ok()?;
    val.get("service")?.get("project_id")?.as_str().map(|s| s.to_string())
}

fn set_project_in_toml(project_id: &str) -> Result<()> {
    let mut val: toml::Value = if let Ok(contents) = std::fs::read_to_string("liquid-metal.toml") {
        toml::from_str(&contents)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    val.as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid liquid-metal.toml"))?
        .entry("service")
        .or_insert(toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("invalid [service] section"))?
        .insert("project_id".to_string(), toml::Value::String(project_id.to_string()));

    std::fs::write("liquid-metal.toml", toml::to_string(&val)?)?;
    Ok(())
}
