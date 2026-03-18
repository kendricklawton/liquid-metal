use std::io::{self, Write};

use anyhow::{bail, Result};
use serde::Serialize;

use common::contract::{CreateProjectRequest, CreateProjectResponse, ProjectResponse};

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::{OutputMode, print_ok};

#[derive(Serialize)]
struct ServiceSection<'a> {
    name: &'a str,
    engine: &'a str,
    project_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u32>,
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

pub async fn run(config: &Config, name_override: Option<String>, engine_override: Option<String>, output: OutputMode) -> Result<()> {
    let ctx = CommandContext::new(config, output)?;
    let workspace_id = ctx.config
        .workspace_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("no workspace found — run `flux login` first"))?;

    if std::path::Path::new("liquid-metal.toml").exists() {
        bail!("liquid-metal.toml already exists — edit it directly or delete it to re-initialize");
    }

    let cwd = std::env::current_dir()?;
    let name = name_override.unwrap_or_else(|| {
        common::slugify(
            cwd.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("service"),
        )
    });

    let engine = match engine_override {
        Some(e) if e == "liquid" || e == "metal" => e,
        Some(e) => bail!("invalid engine {:?} — expected \"liquid\" or \"metal\"", e),
        None => prompt_engine()?,
    };
    let build = detect_build(&engine)?;

    println!(
        "Initializing service {:?} in workspace {}...\n",
        name, workspace_id
    );

    let project_id = match ctx.client
        .post::<_, CreateProjectResponse>(
            "/projects",
            &CreateProjectRequest {
                workspace_id: workspace_id.to_string(),
                name: name.clone(),
                slug: name.clone(),
            },
        )
        .await
    {
        Ok(r) => r.project.id,
        Err(e) if e.to_string().contains("409") => {
            // Project already exists — look it up and reuse it.
            let projects: Vec<ProjectResponse> = ctx.client
                .get(&format!("/projects?workspace_id={}", workspace_id))
                .await?;
            let existing = projects
                .iter()
                .find(|p| p.slug == name)
                .ok_or_else(|| anyhow::anyhow!(
                    "project \"{name}\" reported as existing but not found in project list"
                ))?;
            println!("Project \"{name}\" already exists — reusing (id: {})\n", existing.id);
            existing.id.clone()
        }
        Err(e) => return Err(e),
    };

    let is_metal = engine == "metal";
    let cfg = LiquidMetalConfig {
        service: ServiceSection {
            name: &name,
            engine: &engine,
            project_id: &project_id,
            port: if is_metal { Some(8080) } else { None },
        },
        build: BuildSection {
            command: build.command.clone(),
            output: build.output.clone(),
        },
    };

    std::fs::write("liquid-metal.toml", toml::to_string(&cfg)?)?;

    let mut msg = format!(
        "Created liquid-metal.toml\n  service: {}\n  project: {}\n  engine:  {}",
        name, project_id, engine
    );
    if is_metal {
        msg.push_str("\n  port:    8080");
    }
    msg.push_str(&format!("\n  build:   {}\n  output:  {}\n\nRun `flux deploy` when ready.", build.command, build.output));
    print_ok(output, &msg);
    Ok(())
}

struct DetectedBuild {
    command: String,
    output: String,
}

fn prompt_engine() -> Result<String> {
    println!("Select engine:\n");
    println!("  1) liquid  — Best for request-driven workloads (APIs, webhooks, functions).");
    println!("               Compiles to WebAssembly. Sub-millisecond cold starts. No VM overhead.\n");
    println!("  2) metal   — Best for long-running or stateful workloads (servers, daemons, databases).");
    println!("               Runs in a Firecracker microVM with dedicated vCPU, RAM, and rootfs.\n");
    print!("Enter 1 or 2: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    match input.trim() {
        "1" | "liquid" => Ok("liquid".to_string()),
        "2" | "metal" => Ok("metal".to_string()),
        other => bail!("invalid engine choice: {:?} — expected 1 (liquid) or 2 (metal)", other),
    }
}

fn detect_build(engine: &str) -> Result<DetectedBuild> {
    match engine {
        "liquid" => detect_build_liquid(),
        "metal" => detect_build_metal(),
        _ => bail!("unknown engine: {engine}"),
    }
}

fn detect_build_liquid() -> Result<DetectedBuild> {
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

fn detect_build_metal() -> Result<DetectedBuild> {
    if std::path::Path::new("go.mod").exists() {
        return Ok(DetectedBuild {
            command: "GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -o app .".to_string(),
            output: "app".to_string(),
        });
    }

    if std::path::Path::new("Cargo.toml").exists() {
        let bin_name = parse_rust_bin_name()?;
        return Ok(DetectedBuild {
            command: "cargo build --target x86_64-unknown-linux-musl --release".to_string(),
            output: format!("target/x86_64-unknown-linux-musl/release/{bin_name}"),
        });
    }

    if std::path::Path::new("build.zig").exists() {
        return Ok(DetectedBuild {
            command: "zig build -Dtarget=x86_64-linux-musl -Doptimize=ReleaseSafe".to_string(),
            output: "zig-out/bin/app".to_string(),
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

