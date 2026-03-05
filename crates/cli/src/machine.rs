use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

const CONFIG_FILE: &str = "machine.toml";

#[derive(Debug, Serialize, Deserialize)]
pub struct MachineConfig {
    pub service: ServiceConfig,
    #[serde(default)]
    pub metal: Option<MetalConfig>,
    #[serde(default)]
    pub flash: Option<FlashConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub name: String,
    /// "metal" | "flash"
    pub engine: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MetalConfig {
    #[serde(default = "default_vcpu")]
    pub vcpu: u32,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FlashConfig {
    /// Path to the compiled .wasm binary relative to project root
    pub wasm: String,
}

fn default_port()      -> u16 { 8080 }
fn default_vcpu()      -> u32 { 1 }
fn default_memory_mb() -> u32 { 128 }

fn load_config() -> Result<MachineConfig> {
    let raw = std::fs::read_to_string(CONFIG_FILE)
        .with_context(|| format!("reading {}  — run `plat init` first", CONFIG_FILE))?;
    toml::from_str(&raw).context("parsing machine.toml")
}

// ── Commands ─────────────────────────────────────────────────────────────────

pub fn init(engine: &str) -> Result<()> {
    if Path::new(CONFIG_FILE).exists() {
        bail!("{} already exists", CONFIG_FILE);
    }
    let content = match engine {
        "flash" => r#"[service]
name      = "my-app"
engine    = "flash"
port      = 8080

[flash]
wasm = "main.wasm"
"#,
        _ => r#"[service]
name      = "my-app"
engine    = "metal"
port      = 8080

[metal]
vcpu      = 1
memory_mb = 128
"#,
    };
    std::fs::write(CONFIG_FILE, content)?;
    println!("created {}", CONFIG_FILE);
    Ok(())
}

pub async fn deploy(api: &str) -> Result<()> {
    let cfg = load_config()?;

    // Build the request body from machine.toml
    let mut body = serde_json::json!({
        "workspace_id": std::env::var("MACHINENAME_WORKSPACE").unwrap_or_else(|_| "local".into()),
        "name":   cfg.service.name,
        "engine": cfg.service.engine,
    });

    match cfg.service.engine.as_str() {
        "metal" => {
            let m = cfg.metal.unwrap_or(MetalConfig { vcpu: 1, memory_mb: 128 });
            body["vcpu"]       = serde_json::json!(m.vcpu);
            body["memory_mb"]  = serde_json::json!(m.memory_mb);
            // rootfs_path is resolved server-side after build step (TODO: build + upload)
            body["rootfs_path"] = serde_json::json!("");
        }
        "flash" => {
            let f = cfg.flash.context("missing [flash] section in machine.toml")?;
            // wasm_path resolved server-side after upload (TODO: upload)
            body["wasm_path"] = serde_json::json!(f.wasm);
        }
        e => bail!("unknown engine: {}", e),
    }

    let client = reqwest_or_hyper_post(api, &body).await?;
    println!("{}", client);
    Ok(())
}

pub async fn status(api: &str) -> Result<()> {
    let cfg = load_config()?;
    println!("service: {}", cfg.service.name);
    println!("engine:  {}", cfg.service.engine);
    println!("api:     {}", api);
    // TODO: GET /services/<id>
    Ok(())
}

pub async fn logs(_api: &str) -> Result<()> {
    println!("log streaming coming soon");
    Ok(())
}

/// Minimal POST using std-lib + tokio (avoids pulling reqwest into the workspace).
async fn reqwest_or_hyper_post(api: &str, body: &serde_json::Value) -> Result<String> {
    use http_body_util::{BodyExt, Full};
    use hyper::Request;
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use tokio::net::TcpStream;

    // Strip scheme
    let host_path = api
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (host, _path) = host_path.split_once('/').unwrap_or((host_path, ""));
    let url = format!("{}/services", api.trim_end_matches('/'));
    let uri: hyper::Uri = url.parse().context("invalid API URL")?;

    let stream = TcpStream::connect(host).await.context("connecting to API")?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("HTTP handshake")?;
    tokio::spawn(async move { conn.await.ok(); });

    let payload = serde_json::to_vec(body)?;
    let req = Request::builder()
        .method("POST")
        .uri(uri.path_and_query().map(|p| p.as_str()).unwrap_or("/services"))
        .header("Host", host)
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(payload)))
        .context("building request")?;

    let resp  = sender.send_request(req).await.context("send request")?;
    let bytes = resp.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
