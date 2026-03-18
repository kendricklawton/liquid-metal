use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct LiquidMetalConfig {
    pub service: ServiceConfig,
    pub build: Option<BuildConfig>,
}

#[derive(Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    pub engine: String,
    #[serde(default)]
    pub project_id: Option<String>,
    pub port: Option<u32>,
}

#[derive(Deserialize)]
pub struct BuildConfig {
    pub command: Option<String>,
    pub output: Option<String>,
}

pub struct BuildResult {
    pub artifact_path: String,
    pub sha256_hex: String,
    pub file_bytes: Vec<u8>,
}

/// Load and parse liquid-metal.toml from the current directory.
pub fn load_config() -> Result<LiquidMetalConfig> {
    let raw = std::fs::read_to_string("liquid-metal.toml").map_err(|_| {
        anyhow::anyhow!(
            "no liquid-metal.toml found\n\nRun `flux init` to set up this directory as a Liquid Metal service."
        )
    })?;
    Ok(toml::from_str(&raw)?)
}

/// Validate engine, run build command, read artifact, compute SHA256.
pub fn run_build(cfg: &LiquidMetalConfig) -> Result<BuildResult> {
    let engine = cfg.service.engine.to_lowercase();
    match engine.as_str() {
        "liquid" | "metal" => {}
        other => bail!("unknown engine {:?} — expected \"liquid\" or \"metal\"", other),
    }

    let build = cfg.build.as_ref();
    let build_cmd = build.and_then(|b| b.command.as_deref());
    let artifact_path = build
        .and_then(|b| b.output.as_deref())
        .unwrap_or(if engine == "metal" { "app" } else { "main.wasm" })
        .to_string();

    let engine_display = if engine == "metal" { "Metal" } else { "Liquid" };
    println!(
        "=> Building {} (Engine: {})...",
        cfg.service.name, engine_display
    );

    if let Some(cmd) = build_cmd {
        println!("=> Running: {}", cmd);
        let status = std::process::Command::new("sh")
            .args(["-c", cmd])
            .status()?;
        if !status.success() {
            bail!("build failed");
        }
    } else if engine == "liquid" {
        bail!(
            "liquid deploys require an explicit [build].command in liquid-metal.toml\n\n\
             Run `flux init --engine liquid` to generate one, or add it manually:\n\n\
             [build]\n\
             command = \"cargo build --target wasm32-wasip1 --release\"\n\
             output  = \"target/wasm32-wasip1/release/your-binary.wasm\""
        );
    } else {
        bail!(
            "metal deploys require an explicit [build].command in liquid-metal.toml\n\n\
             Run `flux init --engine metal` to generate one, or add it manually:\n\n\
             [build]\n\
             command = \"cargo build --target x86_64-unknown-linux-musl --release\"\n\
             output  = \"target/x86_64-unknown-linux-musl/release/your-binary\""
        );
    }

    let file_bytes = if std::path::Path::new(&artifact_path).exists() {
        std::fs::read(&artifact_path)?
    } else {
        bail!("build succeeded but artifact not found at: {}", artifact_path);
    };

    let sha256_hex = common::artifact::sha256_hex(&file_bytes);

    println!(
        "=> Artifact: {} (SHA256: {}...)",
        artifact_path,
        &sha256_hex[..8]
    );

    Ok(BuildResult {
        artifact_path,
        sha256_hex,
        file_bytes,
    })
}
