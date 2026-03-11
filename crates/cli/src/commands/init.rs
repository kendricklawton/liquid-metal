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

pub async fn run(config: &Config, name_override: Option<String>) -> Result<()> {
    let token = config.require_token()?;
    let workspace_id = config
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no workspace found — run `flux login` first"))?;

    if std::path::Path::new("liquid-metal.toml").exists() {
        bail!("liquid-metal.toml already exists — edit it directly or delete it to re-init");
    }

    let cwd = std::env::current_dir()?;
    let name = name_override.unwrap_or_else(|| {
        common::slugify(
            cwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("service"),
        )
    });

    let build = detect_language()?;

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
            command: build.command.clone(),
            output: build.output.clone(),
        },
    };

    std::fs::write("liquid-metal.toml", toml::to_string(&cfg)?)?;

    println!("Created liquid-metal.toml");
    println!("  service: {}", name);
    println!("  project: {}", project_id);
    println!("  engine:  liquid");
    println!("  build:   {}", build.command);
    println!("  output:  {}\n", build.output);
    println!("Run `flux deploy` when ready.");
    Ok(())
}

struct DetectedBuild {
    command: String,
    output: String,
}

fn detect_language() -> Result<DetectedBuild> {
    if std::path::Path::new("go.mod").exists() {
        return Ok(DetectedBuild {
            command: "GOOS=wasip1 GOARCH=wasm go build -o main.wasm .".to_string(),
            output: "main.wasm".to_string(),
        });
    }

    if std::path::Path::new("Cargo.toml").exists() {
        let bin_name = parse_rust_bin_name()?;
        return Ok(DetectedBuild {
            command: "cargo build --target wasm32-wasip1 --release".to_string(),
            output: format!("target/wasm32-wasip1/release/{bin_name}.wasm"),
        });
    }

    if std::path::Path::new("build.zig").exists() {
        return Ok(DetectedBuild {
            command: "zig build -Dtarget=wasm32-wasi".to_string(),
            output: "zig-out/bin/main.wasm".to_string(),
        });
    }

    bail!(
        "could not detect language — no go.mod, Cargo.toml, or build.zig found.\n\
         Create one of those files or edit liquid-metal.toml manually."
    )
}

fn parse_rust_bin_name() -> Result<String> {
    let contents = std::fs::read_to_string("Cargo.toml")?;
    let doc: toml::Value = contents.parse()?;

    // [[bin]] name takes priority over [package] name
    if let Some(bins) = doc.get("bin").and_then(|v| v.as_array()) {
        if let Some(first_bin) = bins.first() {
            if let Some(name) = first_bin.get("name").and_then(|v| v.as_str()) {
                return Ok(name.to_string());
            }
        }
    }

    if let Some(name) = doc
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|v| v.as_str())
    {
        return Ok(name.to_string());
    }

    bail!("could not determine binary name from Cargo.toml")
}

