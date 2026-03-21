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
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dockerfile: Option<toml::Value>,
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
            dockerfile: build.dockerfile.as_ref().map(|p| toml::Value::String(p.clone())),
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
    if let Some(dockerfile) = &build.dockerfile {
        msg.push_str(&format!("\n  docker:  {}\n  output:  {}\n\nRun `flux deploy` when ready.", dockerfile, build.output));
    } else if let Some(command) = &build.command {
        msg.push_str(&format!("\n  build:   {}\n  output:  {}\n\nRun `flux deploy` when ready.", command, build.output));
    }
    print_ok(output, &msg);
    Ok(())
}

struct DetectedBuild {
    command: Option<String>,
    output: String,
    dockerfile: Option<String>,
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
    let has_dockerfile = std::path::Path::new("Dockerfile").exists();

    if std::path::Path::new("go.mod").exists() {
        if has_dockerfile {
            println!("Detected Go project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("GOOS=wasip1 GOARCH=wasm go build -o main.wasm .".to_string()),
            output: "main.wasm".to_string(),
            dockerfile: None,
        });
    }

    if std::path::Path::new("Cargo.toml").exists() {
        let bin_name = parse_rust_bin_name()?;
        if has_dockerfile {
            println!("Detected Rust project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("cargo build --target wasm32-wasip1 --release".to_string()),
            output: format!("target/wasm32-wasip1/release/{bin_name}.wasm"),
            dockerfile: None,
        });
    }

    if std::path::Path::new("build.zig").exists() {
        if has_dockerfile {
            println!("Detected Zig project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("zig build -Dtarget=wasm32-wasi".to_string()),
            output: "zig-out/bin/main.wasm".to_string(),
            dockerfile: None,
        });
    }

    if has_dockerfile {
        return prompt_dockerfile_build();
    }

    bail!(
        "could not detect language — no go.mod, Cargo.toml, build.zig, or Dockerfile found.\n\
         Create one of those files or edit liquid-metal.toml manually."
    )
}

fn detect_build_metal() -> Result<DetectedBuild> {
    let has_dockerfile = std::path::Path::new("Dockerfile").exists();

    if std::path::Path::new("go.mod").exists() {
        if has_dockerfile {
            println!("Detected Go project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -o app .".to_string()),
            output: "app".to_string(),
            dockerfile: None,
        });
    }

    if std::path::Path::new("Cargo.toml").exists() {
        let bin_name = parse_rust_bin_name()?;
        if has_dockerfile {
            println!("Detected Rust project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("cargo build --target x86_64-unknown-linux-musl --release".to_string()),
            output: format!("target/x86_64-unknown-linux-musl/release/{bin_name}"),
            dockerfile: None,
        });
    }

    if std::path::Path::new("build.zig").exists() {
        if has_dockerfile {
            println!("Detected Zig project. Using native build command.");
            println!("Tip: Dockerfile found. To use it instead, set dockerfile = true in liquid-metal.toml\n");
        }
        return Ok(DetectedBuild {
            command: Some("zig build -Dtarget=x86_64-linux-musl -Doptimize=ReleaseSafe".to_string()),
            output: "zig-out/bin/app".to_string(),
            dockerfile: None,
        });
    }

    if has_dockerfile {
        return prompt_dockerfile_build();
    }

    bail!(
        "could not detect language — no go.mod, Cargo.toml, build.zig, or Dockerfile found.\n\
         Create one of those files or edit liquid-metal.toml manually."
    )
}

fn prompt_dockerfile_build() -> Result<DetectedBuild> {
    println!("Found Dockerfile but no go.mod, Cargo.toml, or build.zig.");
    println!("Use Dockerfile as build tool? (Docker builds your binary, no containers at runtime)\n");
    print!("Enter y/n: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if input.trim().to_lowercase() != "y" {
        bail!(
            "Dockerfile build declined.\n\
             Add a go.mod, Cargo.toml, or build.zig, or edit liquid-metal.toml manually."
        );
    }

    print!("Path to compiled binary inside the container (e.g. /app/myapp): ");
    io::stdout().flush()?;

    let mut output_path = String::new();
    io::stdin().read_line(&mut output_path)?;
    let output_path = output_path.trim().to_string();
    if output_path.is_empty() {
        bail!("binary path is required");
    }

    Ok(DetectedBuild {
        command: None,
        output: output_path,
        dockerfile: Some("Dockerfile".to_string()),
    })
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

