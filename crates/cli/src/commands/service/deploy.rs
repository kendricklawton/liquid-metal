use anyhow::bail;
use anyhow::Result;
use futures::StreamExt as _;
use reqwest::Body;

use common::contract::{DeployRequest, DeployResponse, UploadUrlRequest, UploadUrlResponse};

use crate::config::Config;
use crate::context::CommandContext;
use crate::output::OutputMode;
use crate::toml_config;

pub async fn run(config: &Config, output: OutputMode, skip_elf_check: bool) -> Result<()> {
    let started = std::time::Instant::now();
    let ctx = CommandContext::new(config, output)?;

    let cfg = toml_config::load_config()?;

    if cfg.service.name.is_empty() {
        bail!("liquid-metal.toml: [service].name is required");
    }
    let project_id = cfg.service.project_id.as_deref().unwrap_or("");
    if project_id.is_empty() {
        bail!("liquid-metal.toml: [service].project_id is required");
    }

    let engine = cfg.service.engine.to_lowercase();
    if engine == "metal" && cfg.service.port.is_none() {
        bail!("metal deploys require [service].port in liquid-metal.toml");
    }

    // ── Build ────────────────────────────────────────────────────────────────
    let build_result = toml_config::run_build(&cfg)?;

    // ELF compatibility check for Metal deploys — catch glibc binaries before upload.
    // Reads only the first 64KB (ELF headers), not the entire file.
    if engine == "metal" && !skip_elf_check {
        common::artifact::check_elf_compat_file(&build_result.artifact_path).await?;
    }

    let deploy_id = uuid::Uuid::now_v7().to_string();

    // Progress messages go to stderr in JSON mode so stdout stays clean
    let progress = |msg: &str| {
        if output == OutputMode::Human {
            println!("{}", msg);
        } else {
            eprintln!("{}", msg);
        }
    };

    // ── Upload ───────────────────────────────────────────────────────────────
    progress("=> Requesting upload destination...");
    let url_resp: UploadUrlResponse = ctx.client
        .post(
            "/deployments/upload-url",
            &UploadUrlRequest {
                engine: engine.clone(),
                deploy_id: deploy_id.clone(),
                project_id: project_id.to_string(),
            },
        )
        .await?;

    let artifact_meta = tokio::fs::metadata(&build_result.artifact_path).await?;
    let artifact_size = artifact_meta.len() as usize;
    progress(&format!("=> Uploading artifact ({})...", crate::output::human_bytes(artifact_size)));
    let file = tokio::fs::File::open(&build_result.artifact_path).await?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let upload_resp = http
        .put(&url_resp.upload_url)
        .header("content-length", artifact_size.to_string())
        .body(Body::wrap_stream(stream))
        .send()
        .await?;
    if !upload_resp.status().is_success() {
        bail!("upload failed (HTTP {})", upload_resp.status());
    }

    // ── Deploy ───────────────────────────────────────────────────────────────
    progress("=> Finalizing deployment...");
    let deploy_resp: DeployResponse = ctx.client
        .post(
            "/deployments",
            &DeployRequest {
                name: cfg.service.name.clone(),
                slug: cfg.service.name.clone(),
                engine: engine.clone(),
                project_id: project_id.to_string(),
                artifact_key: url_resp.artifact_key.clone(),
                sha256: build_result.sha256_hex.clone(),
                port: cfg.service.port,
                tier: cfg.service.tier.clone(),
            },
        )
        .await?;

    let domain = ctx.config.platform_domain().to_string();

    // JSON mode: print the provisioning response and exit — no streaming.
    if output == OutputMode::Json {
        println!("{}", serde_json::to_string(&deploy_resp)?);
        return Ok(());
    }

    // Human mode: open the SSE stream and print each step live.
    let stream_path = format!("/deployments/{}/stream", deploy_resp.service.id);
    let resp = ctx.client.get_stream(&stream_path).await?;
    let mut byte_stream = resp.bytes_stream();
    let mut buf = String::new();

    println!();
    const SSE_BUF_LIMIT: usize = 10 * 1024 * 1024; // 10 MB

    while let Some(chunk) = byte_stream.next().await {
        let chunk = chunk?;
        buf.push_str(&String::from_utf8_lossy(&chunk));
        if buf.len() > SSE_BUF_LIMIT {
            bail!("deploy event stream exceeded {}MB — connection may be corrupt", SSE_BUF_LIMIT / 1024 / 1024);
        }
        // SSE events are delimited by \n\n
        while let Some(pos) = buf.find("\n\n") {
            let raw = buf[..pos].to_string();
            buf = buf[pos + 2..].to_string();
            let data = raw.lines().find_map(|l| l.strip_prefix("data: ").map(str::to_string));
            let Some(data) = data else { continue };
            let Ok(ev) = serde_json::from_str::<serde_json::Value>(&data) else { continue };
            let step = ev["step"].as_str().unwrap_or("...");
            let msg  = ev["message"].as_str().unwrap_or("");
            match step {
                "running" | "ready" => {
                    let elapsed = started.elapsed().as_secs();
                    println!("  ✓ {}", msg);
                    println!("  → https://{}.{}", deploy_resp.service.slug, domain);
                    println!();
                    println!("  deploy:   {}", &deploy_id[..8.min(deploy_id.len())]);
                    println!("  artifact: {}", crate::output::human_bytes(artifact_size));
                    println!("  duration: {}s", elapsed);
                    return Ok(());
                }
                "failed" => {
                    eprintln!("  ✗ {}", msg);
                    std::process::exit(1);
                }
                _ => println!("  → {}", msg),
            }
        }
    }
    Ok(())
}
