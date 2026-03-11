use crate::{deprovision, storage, verify, wasm_http};
use anyhow::{Context, Result};
use common::events::{Engine, EngineSpec, ProvisionEvent};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use uuid::Uuid;

#[cfg(target_os = "linux")]
use std::time::Duration;
#[cfg(target_os = "linux")]
use crate::{cgroup, cpu, ebpf, firecracker, jailer, netlink, tc};

static TAP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Runtime configuration — all values sourced from env vars at startup.
/// No hardcoded paths anywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProvisionConfig {
    // Firecracker binaries + sockets
    pub fc_bin:         String,   // FC_BIN,          default /usr/local/bin/firecracker
    pub kernel_path:    String,   // FC_KERNEL_PATH,  default /opt/firecracker/vmlinux
    pub sock_dir:       String,   // FC_SOCK_DIR,     default /run/firecracker
    pub bridge:         String,   // BRIDGE,          default br0
    // cgroup v2 IO limits
    pub rootfs_dev:     String,   // ROOTFS_DEVICE,   default 8:0
    pub physical_cores: u32,      // PHYSICAL_CORES,  default 4
    // Jailer (production isolation)
    pub use_jailer:     bool,     // USE_JAILER,      default false
    pub jailer_bin:     String,   // JAILER_BIN,      default /usr/local/bin/jailer
    pub jailer_uid:     u32,      // JAILER_UID,      default 10000
    pub jailer_gid:     u32,      // JAILER_GID,      default 10000
    pub chroot_base:    String,   // JAILER_CHROOT_BASE, default /srv/jailer
    // Node identity + artifact cache
    pub node_id:        String,   // NODE_ID,         default node-a
    pub artifact_dir:   String,   // ARTIFACT_DIR,    default /var/lib/liquid-metal/artifacts
}

pub struct ProvisionedService {
    pub id:            Uuid,
    pub engine:        Engine,
    pub upstream_addr: Option<String>,
}

/// Shared context passed into each provision task.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct ProvisionCtx {
    pub pool:     Arc<deadpool_postgres::Pool>,
    pub cfg:      Arc<ProvisionConfig>,
    pub s3:       Arc<aws_sdk_s3::Client>,
    pub bucket:   Arc<String>,
    pub registry: deprovision::VmRegistry,
}

/// Initialize TAP counter from the DB to avoid collisions after daemon restart.
pub async fn init_tap_counter(pool: &deadpool_postgres::Pool, node_id: &str) {
    let db = match pool.get().await {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(error = %e, "could not init TAP counter from DB — starting at 0");
            return;
        }
    };

    let row = db
        .query_opt(
            "SELECT COUNT(*) AS cnt FROM services \
             WHERE node_id = $1 AND status = 'running' AND engine = 'metal' AND deleted_at IS NULL",
            &[&node_id],
        )
        .await;

    match row {
        Ok(Some(r)) => {
            let cnt: i64 = r.get("cnt");
            TAP_COUNTER.store(cnt as u32, Ordering::Relaxed);
            tracing::info!(node_id, tap_start = cnt, "TAP counter initialized from DB");
        }
        _ => tracing::warn!("TAP counter DB query failed — starting at 0"),
    }
}

pub async fn provision(ctx: &ProvisionCtx, event: &ProvisionEvent) -> Result<ProvisionedService> {
    let svc = match &event.spec {
        EngineSpec::Metal(spec)  => provision_metal(ctx, event, spec).await?,
        EngineSpec::Liquid(spec) => provision_liquid(ctx, event, spec).await?,
    };

    let svc_id: Uuid = event
        .service_id
        .parse()
        .context("invalid service_id UUID in event")?;

    let db = ctx.pool.get().await.context("db pool")?;
    db.execute(
        "UPDATE services SET upstream_addr = $1, status = 'running', node_id = $2 WHERE id = $3",
        &[&svc.upstream_addr, &ctx.cfg.node_id, &svc_id],
    )
    .await
    .context("writing upstream_addr + node_id to services")?;

    Ok(svc)
}

async fn provision_metal(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::MetalSpec,
) -> Result<ProvisionedService> {
    let vm_id   = Uuid::now_v7();
    let tap_idx = TAP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tap     = common::networking::tap_name(tap_idx);
    let ip      = common::networking::guest_ip(tap_idx);
    let upstream_addr = format!("{}:{}", ip, spec.port);

    // Download rootfs artifact from Object Storage
    let local_rootfs = PathBuf::from(&ctx.cfg.artifact_dir)
        .join(&event.service_id)
        .join("rootfs.ext4");

    storage::download(&ctx.s3, &ctx.bucket, &spec.artifact_key, &local_rootfs)
        .await
        .context("downloading rootfs from Object Storage")?;

    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(local_rootfs.to_str().unwrap_or(""), expected)
            .await
            .context("rootfs integrity check")?;
    }

    tracing::info!(
        vm_id = %vm_id, tap, app = event.app_name,
        %upstream_addr, use_jailer = ctx.cfg.use_jailer,
        "provisioning metal VM"
    );

    #[cfg(target_os = "linux")]
    {
        let rootfs_path = local_rootfs.to_string_lossy().into_owned();
        let mac         = format!("AA:FC:00:00:{:02X}:{:02X}", tap_idx >> 8, tap_idx & 0xFF);
        let cpu_core = cpu::next_core(ctx.cfg.physical_cores);

        netlink::create_tap(&tap).context("create TAP")?;
        netlink::attach_to_bridge(&tap, &ctx.cfg.bridge).await.context("attach TAP to bridge")?;
        tc::apply(&tap, &spec.quota).await.context("tc bandwidth")?;
        ebpf::attach(&tap, &event.service_id).context("eBPF TC attach")?;

        let (fc_pid, sock_path, rootfs_guest, kernel_guest) = if ctx.cfg.use_jailer {
            let (pid, paths) = jailer::spawn(&jailer::JailerConfig {
                vm_id:       &vm_id.to_string(),
                jailer_bin:  &ctx.cfg.jailer_bin,
                fc_bin:      &ctx.cfg.fc_bin,
                uid:         ctx.cfg.jailer_uid,
                gid:         ctx.cfg.jailer_gid,
                chroot_base: &ctx.cfg.chroot_base,
                rootfs_src:  &rootfs_path,
                kernel_src:  &ctx.cfg.kernel_path,
            })
            .await
            .context("jailer spawn")?;
            (pid, paths.socket, paths.rootfs_guest.to_string(), paths.kernel_guest.to_string())
        } else {
            let sock = format!("{}/{}.sock", ctx.cfg.sock_dir, vm_id);
            let pid = spawn_firecracker_direct(&ctx.cfg.fc_bin, &vm_id.to_string(), &sock).await?;
            (pid, sock, rootfs_path.clone(), ctx.cfg.kernel_path.clone())
        };

        let cg_path = format!("/sys/fs/cgroup/liquid-metal/{}", event.service_id);

        cgroup::apply(&event.service_id, fc_pid, &ctx.cfg.rootfs_dev, &spec.quota)
            .await
            .context("cgroup io.max")?;

        cpu::pin_cgroup(&cg_path, cpu_core)
            .await
            .context("cpuset pin")?;

        cpu::offline_smt_sibling(cpu_core, ctx.cfg.physical_cores)
            .await
            .context("offline SMT sibling")?;

        firecracker::start_vm(&firecracker::VmConfig {
            sock_path:   &sock_path,
            vcpu:        spec.vcpu,
            memory_mb:   spec.memory_mb,
            kernel_path: &kernel_guest,
            rootfs_path: &rootfs_guest,
            tap_name:    &tap,
            guest_mac:   &mac,
        })
        .await
        .context("start VM")?;

        // Register in-memory for deprovision
        ctx.registry.lock().await.insert(
            event.service_id.clone(),
            deprovision::VmHandle {
                tap_name:    tap.clone(),
                fc_pid,
                cpu_core,
                vm_id:       vm_id.to_string(),
                use_jailer:  ctx.cfg.use_jailer,
                chroot_base: ctx.cfg.chroot_base.clone(),
            },
        );

        // Persist tap_name to DB for post-restart recovery
        if let Ok(db) = ctx.pool.get().await {
            let svc_id: Uuid = event.service_id.parse().unwrap_or(Uuid::nil());
            let _ = db
                .execute("UPDATE services SET tap_name = $1 WHERE id = $2", &[&tap, &svc_id])
                .await;
        }

        tracing::info!(vm_id = %vm_id, %upstream_addr, fc_pid, cpu_core, "metal VM ready");

        return Ok(ProvisionedService {
            id: vm_id,
            engine: Engine::Metal,
            upstream_addr: Some(upstream_addr),
        });
    }

    // Non-Linux dev
    #[allow(unreachable_code)]
    Ok(ProvisionedService {
        id: vm_id,
        engine: Engine::Metal,
        upstream_addr: Some(upstream_addr),
    })
}

async fn provision_liquid(
    ctx: &ProvisionCtx,
    event: &ProvisionEvent,
    spec: &common::events::LiquidSpec,
) -> Result<ProvisionedService> {
    let id = Uuid::now_v7();

    let local_wasm = PathBuf::from(&ctx.cfg.artifact_dir)
        .join(&event.service_id)
        .join("main.wasm");

    storage::download(&ctx.s3, &ctx.bucket, &spec.artifact_key, &local_wasm)
        .await
        .context("downloading wasm from Object Storage")?;

    let wasm_path = local_wasm.to_string_lossy().into_owned();

    if let Some(expected) = &spec.artifact_sha256 {
        verify::artifact(&wasm_path, expected)
            .await
            .context("wasm integrity check")?;
    }

    tracing::info!(service_id = %id, app = event.app_name, "provisioning liquid (wasm)");

    // Compile the module once and start a per-request HTTP shim on a free port.
    // Pingora routes to 127.0.0.1:{port} as upstream_addr.
    let port = wasm_http::serve(wasm_path, event.app_name.clone())
        .await
        .context("starting wasm HTTP shim")?;

    let upstream_addr = format!("127.0.0.1:{port}");
    tracing::info!(service_id = %id, %upstream_addr, "liquid wasm ready");

    Ok(ProvisionedService { id, engine: Engine::Liquid, upstream_addr: Some(upstream_addr) })
}

#[cfg(target_os = "linux")]
async fn spawn_firecracker_direct(fc_bin: &str, vm_id: &str, sock_path: &str) -> Result<u32> {
    let sock_dir = sock_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("/run/firecracker");
    tokio::fs::create_dir_all(sock_dir).await.context("mkdir sock dir")?;

    let child = tokio::process::Command::new(fc_bin)
        .args(["--api-sock", sock_path, "--id", vm_id])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {}", fc_bin))?;

    let pid = child.id().context("FC process exited immediately")?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if tokio::fs::metadata(sock_path).await.is_ok() { return Ok(pid); }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("firecracker socket {} not ready within 3s", sock_path);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

