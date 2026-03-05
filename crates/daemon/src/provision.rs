use crate::{firecracker, wasm};
use anyhow::{Context, Result, bail};
use common::events::{Engine, EngineSpec, ProvisionEvent};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use crate::netlink;

static TAP_COUNTER: AtomicU32 = AtomicU32::new(0);

const BRIDGE: &str = "br0";
const SOCK_DIR: &str = "/run/firecracker";
const KERNEL_PATH: &str = "/opt/firecracker/vmlinux";
const FC_BINARY: &str = "/usr/local/bin/firecracker";
const SOCK_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ProvisionedService {
    pub id: Uuid,
    pub engine: Engine,
}

pub async fn provision(event: &ProvisionEvent) -> Result<ProvisionedService> {
    match &event.spec {
        EngineSpec::Metal(spec) => provision_metal(event, spec).await,
        EngineSpec::Flash(spec) => provision_flash(event, spec).await,
    }
}

async fn provision_metal(
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
) -> Result<ProvisionedService> {
    let vm_id   = Uuid::new_v4();
    let tap_idx = TAP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tap     = format!("tap{}", tap_idx);
    let sock    = format!("{}/{}.sock", SOCK_DIR, vm_id);
    let mac     = format!("AA:FC:00:00:{:02X}:{:02X}", tap_idx >> 8, tap_idx & 0xFF);

    tracing::info!(vm_id = %vm_id, tap, app = event.app_name, "provisioning metal VM");

    #[cfg(target_os = "linux")]
    {
        netlink::create_tap(&tap).context("create TAP")?;
        netlink::attach_to_bridge(&tap, BRIDGE).await.context("attach TAP")?;
    }

    spawn_firecracker(&vm_id.to_string(), &sock).await.context("spawn firecracker")?;

    firecracker::start_vm(&firecracker::VmConfig {
        sock_path:   &sock,
        vcpu:        spec.vcpu,
        memory_mb:   spec.memory_mb,
        kernel_path: KERNEL_PATH,
        rootfs_path: &spec.rootfs_path,
        tap_name:    &tap,
        guest_mac:   &mac,
    })
    .await
    .context("start VM")?;

    tracing::info!(vm_id = %vm_id, "metal VM ready");
    Ok(ProvisionedService { id: vm_id, engine: Engine::Metal })
}

async fn provision_flash(
    event: &ProvisionEvent,
    spec: &common::events::FlashSpec,
) -> Result<ProvisionedService> {
    let id = Uuid::new_v4();
    tracing::info!(service_id = %id, app = event.app_name, "provisioning flash (wasm)");
    wasm::execute(&spec.wasm_path, &event.app_name)
        .await
        .context("wasm execution")?;
    Ok(ProvisionedService { id, engine: Engine::Flash })
}

async fn spawn_firecracker(vm_id: &str, sock_path: &str) -> Result<()> {
    tokio::fs::create_dir_all(SOCK_DIR).await.context("mkdir /run/firecracker")?;

    tokio::process::Command::new(FC_BINARY)
        .args(["--api-sock", sock_path, "--id", vm_id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {}", FC_BINARY))?;

    let deadline = tokio::time::Instant::now() + SOCK_TIMEOUT;
    loop {
        if tokio::fs::metadata(sock_path).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("firecracker socket {} not ready within {:?}", sock_path, SOCK_TIMEOUT);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
