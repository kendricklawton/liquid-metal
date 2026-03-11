use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::client::ApiClient;
use crate::config::Config;

#[derive(Deserialize)]
struct ServiceConfig {
    name: String,
    engine: String,
    project_id: String,
}

#[derive(Deserialize)]
struct BuildConfig {
    command: Option<String>,
    output: Option<String>,
}

#[derive(Deserialize)]
struct LiquidMetalConfig {
    service: ServiceConfig,
    build: Option<BuildConfig>,
}

#[derive(Serialize)]
struct UploadUrlRequest<'a> {
    slug: &'a str,
    engine: &'a str,
    deploy_id: &'a str,
    project_id: &'a str,
}

#[derive(Deserialize)]
struct UploadUrlResponse {
    upload_url: String,
    artifact_key: String,
}

#[derive(Serialize)]
struct DeployRequest<'a> {
    name: &'a str,
    slug: &'a str,
    engine: &'a str,
    project_id: &'a str,
    deploy_id: &'a str,
    artifact_key: &'a str,
    sha256: &'a str,
}

#[derive(Deserialize)]
struct DeployedService {
    slug: String,
    status: String,
}

#[derive(Deserialize)]
struct DeployResponse {
    service: DeployedService,
}

pub async fn run(config: &Config) -> Result<()> {
    let token = config.require_token()?;

    let raw = std::fs::read_to_string("liquid-metal.toml")
        .map_err(|_| anyhow::anyhow!("no liquid-metal.toml found\n\nRun `flux init` to set up this directory as a Liquid Metal service."))?;
    let cfg: LiquidMetalConfig = toml::from_str(&raw)?;

    if cfg.service.name.is_empty() {
        bail!("liquid-metal.toml: [service].name is required");
    }
    if cfg.service.project_id.is_empty() {
        bail!("liquid-metal.toml: [service].project_id is required");
    }
    if cfg.service.engine.to_lowercase() != "liquid" {
        bail!("only 'liquid' engine is supported");
    }

    println!("=> Deploying {} (Engine: Liquid)...", cfg.service.name);

    let build = cfg.build.as_ref();
    let build_cmd = build.and_then(|b| b.command.as_deref());
    let wasm_file = build
        .and_then(|b| b.output.as_deref())
        .unwrap_or("main.wasm")
        .to_string();

    if let Some(cmd) = build_cmd {
        println!("=> Building ({})...", cmd);
        let status = std::process::Command::new("sh")
            .args(["-c", cmd])
            .status()?;
        if !status.success() {
            bail!("build failed");
        }
    } else {
        println!("=> Compiling Rust to WebAssembly (wasm32-wasip1)...");
        let status = std::process::Command::new("cargo")
            .args(["build", "--target", "wasm32-wasip1", "--release"])
            .status()?;
        if !status.success() {
            bail!("compilation failed");
        }
    }

    let file_bytes = std::fs::read(&wasm_file)
        .map_err(|_| anyhow::anyhow!("failed to read artifact: {}", wasm_file))?;

    let hash = Sha256::digest(&file_bytes);
    let sha256_hex = hex::encode(hash);
    let deploy_id = uuid::Uuid::new_v4().to_string();

    println!(
        "=> Artifact built: {} (SHA256: {}...)",
        wasm_file,
        &sha256_hex[..8]
    );

    let client = ApiClient::new(config.api_url(), Some(token));

    println!("=> Requesting upload destination...");
    let url_resp: UploadUrlResponse = client
        .post(
            "/deployments/upload-url",
            &UploadUrlRequest {
                slug: &cfg.service.name,
                engine: "liquid",
                deploy_id: &deploy_id,
                project_id: &cfg.service.project_id,
            },
        )
        .await?;

    println!("=> Uploading artifact to object storage...");
    let http = reqwest::Client::new();
    let upload_resp = http
        .put(&url_resp.upload_url)
        .body(file_bytes)
        .send()
        .await?;
    if !upload_resp.status().is_success() {
        bail!("upload failed (HTTP {})", upload_resp.status());
    }

    println!("=> Finalizing deployment...");
    let deploy_resp: DeployResponse = client
        .post(
            "/deployments",
            &DeployRequest {
                name: &cfg.service.name,
                slug: &cfg.service.name,
                engine: "liquid",
                project_id: &cfg.service.project_id,
                deploy_id: &deploy_id,
                artifact_key: &url_resp.artifact_key,
                sha256: &sha256_hex,
            },
        )
        .await?;

    println!("\nDeployment Successful!");
    println!("   Service: {}", deploy_resp.service.slug);
    println!("   Status:  {}", deploy_resp.service.status);
    Ok(())
}
