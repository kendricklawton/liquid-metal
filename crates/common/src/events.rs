use serde::{Deserialize, Serialize};

pub const STREAM_NAME: &str = "PLATFORM_EVENTS";
pub const SUBJECT_PROVISION:   &str = "platform.provision";
pub const SUBJECT_DEPROVISION: &str = "platform.deprovision";

/// Published by the API when a service is created or redeployed.
/// Consumed by the daemon on the metal node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionEvent {
    pub tenant_id:  String,
    pub service_id: String,
    pub app_name:   String,
    pub engine:     Engine,
    pub spec:       EngineSpec,
}

/// Published by the API when a service is deleted.
/// Consumed by the daemon to halt the VM and release resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeprovisionEvent {
    pub service_id: String,
    pub engine:     Engine,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Firecracker microVM — full Linux kernel, dedicated disk, ~100ms cold start.
    Metal,
    /// Wasmtime executor — sandboxed process, memory-only, <1ms cold start.
    Liquid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EngineSpec {
    Metal(MetalSpec),
    Liquid(LiquidSpec),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetalSpec {
    pub vcpu:      u32,
    pub memory_mb: u32,
    /// TCP port the guest app listens on — used to build upstream_addr.
    pub port:      u16,
    /// Object Storage key for the rootfs image (e.g. `metal/{name}/{deploy_id}/rootfs.ext4`).
    /// Daemon downloads this to a temp path before booting Firecracker.
    pub artifact_key: String,
    /// SHA-256 hex of the rootfs image. Daemon verifies before boot.
    /// `None` skips the check — development/testing only.
    pub artifact_sha256: Option<String>,
    /// Resource quota enforced post-boot (IO + network). Defaults to unlimited.
    #[serde(default)]
    pub quota: ResourceQuota,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidSpec {
    /// Object Storage key for the .wasm binary (e.g. `liquid/{name}/{deploy_id}/main.wasm`).
    /// Daemon downloads this to a temp path before executing with Wasmtime.
    pub artifact_key: String,
    /// SHA-256 hex of the .wasm binary. Daemon verifies before execution.
    /// `None` skips the check — development/testing only.
    pub artifact_sha256: Option<String>,
}

/// Per-service resource limits — the Triple-Lock system:
///   Layer 1 — Hypervisor : vcpu + memory_mb in MetalSpec (Firecracker enforces at boot)
///   Layer 2 — Kernel IO  : disk_* fields   (cgroup v2 io.max, applied by daemon)
///   Layer 3 — Network    : net_* fields    (tc tbf for bandwidth; Aya eBPF TC for isolation)
///
/// `None` means unlimited for that dimension.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceQuota {
    // Layer 2: cgroup v2 io.max
    pub disk_read_bps:   Option<u64>,
    pub disk_write_bps:  Option<u64>,
    pub disk_read_iops:  Option<u32>,
    pub disk_write_iops: Option<u32>,

    // Layer 3: tc tbf bandwidth shaping on TAP (see tc.rs)
    pub net_ingress_kbps: Option<u32>,
    pub net_egress_kbps:  Option<u32>,
}
