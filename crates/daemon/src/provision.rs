use crate::{verify, wasm};
use anyhow::{Context, Result};
use common::events::{Engine, EngineSpec, ProvisionEvent};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use crate::{cgroup, cilium, cpu, firecracker, jailer, netlink, tc};

static TAP_COUNTER: AtomicU32 = AtomicU32::new(0);

#[cfg(target_os = "linux")]
const BRIDGE: &str = "br0";
#[cfg(target_os = "linux")]
const SOCK_DIR: &str = "/run/firecracker";
#[cfg(target_os = "linux")]
const KERNEL_PATH: &str = "/opt/firecracker/vmlinux";
#[cfg(target_os = "linux")]
const FC_BINARY: &str = "/usr/local/bin/firecracker";
#[cfg(target_os = "linux")]
const SOCK_TIMEOUT: Duration = Duration::from_secs(3);

// Guest IPs: tap{n} → 172.16.{n/63}.{(n%63)*4+2}
fn guest_ip(tap_idx: u32) -> String {
    format!("172.16.{}.{}", tap_idx / 63, (tap_idx % 63) * 4 + 2)
}

/// Runtime configuration sourced from env vars at daemon startup.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProvisionConfig {
    pub rootfs_dev:     String,   // block device for cgroup io.max (e.g. "8:0")
    pub physical_cores: u32,      // physical CPU cores available for VM pinning
    pub use_jailer:     bool,     // true = Firecracker Jailer (production)
    pub jailer_bin:     String,
    pub jailer_uid:     u32,
    pub jailer_gid:     u32,
    pub chroot_base:    String,
}

pub struct ProvisionedService {
    pub id: Uuid,
    pub engine: Engine,
    pub upstream_addr: Option<String>,
}

pub async fn provision(
    pool: &Arc<deadpool_postgres::Pool>,
    cfg: &ProvisionConfig,
    event: &ProvisionEvent,
) -> Result<ProvisionedService> {
    let svc = match &event.spec {
        EngineSpec::Metal(spec) => provision_metal(cfg, event, spec).await?,
        EngineSpec::Flash(spec) => provision_flash(event, spec).await?,
    };

    let svc_id: Uuid = event
        .service_id
        .parse()
        .context("invalid service_id UUID in event")?;

    let db = pool.get().await.context("db pool")?;
    db.execute(
        "UPDATE services SET upstream_addr = $1, status = 'running' WHERE id = $2",
        &[&svc.upstream_addr, &svc_id],
    )
    .await
    .context("writing upstream_addr to services")?;

    Ok(svc)
}

async fn provision_metal(
    cfg: &ProvisionConfig,
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
) -> Result<ProvisionedService> {
    let vm_id   = Uuid::now_v7();
    let tap_idx = TAP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tap     = format!("tap{}", tap_idx);
    #[cfg(target_os = "linux")]
    let mac = format!("AA:FC:00:00:{:02X}:{:02X}", tap_idx >> 8, tap_idx & 0xFF);
    let ip      = guest_ip(tap_idx);
    let upstream_addr = format!("{}:{}", ip, spec.port);

    tracing::info!(
        vm_id = %vm_id, tap, app = event.app_name,
        %upstream_addr, use_jailer = cfg.use_jailer,
        "provisioning metal VM"
    );

    // ── Supply-chain integrity check ─────────────────────────────────────────
    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(&spec.rootfs_path, expected)
            .await
            .context("rootfs integrity check")?;
    }

    #[cfg(target_os = "linux")]
    {
        // ── Layer 0: Silicon — allocate a dedicated physical core ─────────────
        let cpu_core = cpu::next_core(cfg.physical_cores);

        // ── Layer 3: Network — bandwidth limits before any traffic can flow ───
        netlink::create_tap(&tap).context("create TAP")?;
        netlink::attach_to_bridge(&tap, BRIDGE).await.context("attach TAP")?;
        tc::apply(&tap, &spec.quota).await.context("tc bandwidth")?;

        // ── Layer 3 cont: Cilium identity — enforce network policy ───────────
        cilium::label_endpoint(&tap, &event.service_id)
            .await
            .context("cilium label endpoint")?;

        // ── Launch Firecracker ────────────────────────────────────────────────
        let (fc_pid, sock_path, rootfs_path, kernel_path) = if cfg.use_jailer {
            // Production path: full jailer isolation (namespaces + chroot + seccomp)
            let (pid, paths) = jailer::spawn(&jailer::JailerConfig {
                vm_id:       &vm_id.to_string(),
                jailer_bin:  &cfg.jailer_bin,
                fc_bin:      FC_BINARY,
                uid:         cfg.jailer_uid,
                gid:         cfg.jailer_gid,
                chroot_base: &cfg.chroot_base,
                rootfs_src:  &spec.rootfs_path,
                kernel_src:  KERNEL_PATH,
            })
            .await
            .context("jailer spawn")?;
            (pid, paths.socket, paths.rootfs_guest.to_string(), paths.kernel_guest.to_string())
        } else {
            // Dev path: direct Firecracker spawn (macOS / no jailer binary)
            let sock = format!("{}/{}.sock", SOCK_DIR, vm_id);
            let pid = spawn_firecracker_direct(&vm_id.to_string(), &sock).await?;
            (pid, sock, spec.rootfs_path.clone(), KERNEL_PATH.to_string())
        };

        let cg_path = format!("/sys/fs/cgroup/liquid-metal/{}", event.service_id);

        // ── Layer 2: Kernel IO — cgroup v2 io.max ────────────────────────────
        cgroup::apply(&event.service_id, fc_pid, &cfg.rootfs_dev, &spec.quota)
            .await
            .context("cgroup io.max")?;

        // ── Layer 0 cont: pin cgroup to physical core via cpuset ──────────────
        cpu::pin_cgroup(&cg_path, cpu_core)
            .await
            .context("cpuset pin")?;

        // ── Layer 1: Hypervisor — boot the VM (vcpu + memory already set) ─────
        firecracker::start_vm(&firecracker::VmConfig {
            sock_path:   &sock_path,
            vcpu:        spec.vcpu,
            memory_mb:   spec.memory_mb,
            kernel_path: &kernel_path,
            rootfs_path: &rootfs_path,
            tap_name:    &tap,
            guest_mac:   &mac,
        })
        .await
        .context("start VM")?;

        tracing::info!(
            vm_id = %vm_id, %upstream_addr, fc_pid, cpu_core,
            "metal VM ready — all security layers applied"
        );

        return Ok(ProvisionedService {
            id: vm_id,
            engine: Engine::Metal,
            upstream_addr: Some(upstream_addr),
        });
    }

    // Non-Linux (dev): no TAP, no cgroup, no jailer
    #[allow(unreachable_code)]
    Ok(ProvisionedService {
        id: vm_id,
        engine: Engine::Metal,
        upstream_addr: Some(upstream_addr),
    })
}

async fn provision_flash(
    event: &ProvisionEvent,
    spec: &common::events::FlashSpec,
) -> Result<ProvisionedService> {
    let id = Uuid::now_v7();

    // ── Supply-chain integrity check ─────────────────────────────────────────
    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(&spec.wasm_path, expected)
            .await
            .context("wasm artifact integrity check")?;
    }

    tracing::info!(service_id = %id, app = event.app_name, "provisioning flash (wasm)");

    // Wasm fuel metering is applied inside wasm::execute() — see wasm.rs
    wasm::execute(&spec.wasm_path, &event.app_name)
        .await
        .context("wasm execution")?;

    Ok(ProvisionedService { id, engine: Engine::Flash, upstream_addr: None })
}

/// Direct Firecracker spawn (dev path — no jailer).
/// Returns the process PID once the API socket is ready.
#[cfg(target_os = "linux")]
async fn spawn_firecracker_direct(vm_id: &str, sock_path: &str) -> Result<u32> {
    tokio::fs::create_dir_all(SOCK_DIR).await.context("mkdir /run/firecracker")?;

    let child = tokio::process::Command::new(FC_BINARY)
        .args(["--api-sock", sock_path, "--id", vm_id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {}", FC_BINARY))?;

    let pid = child.id().context("FC process exited immediately after spawn")?;

    let deadline = tokio::time::Instant::now() + SOCK_TIMEOUT;
    loop {
        if tokio::fs::metadata(sock_path).await.is_ok() {
            return Ok(pid);
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("firecracker socket {} not ready within {:?}", sock_path, SOCK_TIMEOUT);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
