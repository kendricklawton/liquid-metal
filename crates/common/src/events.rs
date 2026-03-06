use serde::{Deserialize, Serialize};

pub const STREAM_NAME: &str = "PLATFORM_EVENTS";
pub const SUBJECT_PROVISION: &str = "platform.provision";

/// Published by the API when a service is created or redeployed.
/// Consumed by the daemon on the metal node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionEvent {
    pub tenant_id: String,
    pub service_id: String,
    pub app_name: String,
    pub engine: Engine,
    pub spec: EngineSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Firecracker microVM — full Linux kernel, dedicated disk, ~100ms cold start.
    Metal,
    /// Wasmtime executor — sandboxed process, memory-only, <1ms cold start.
    Flash,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EngineSpec {
    Metal(MetalSpec),
    Flash(FlashSpec),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetalSpec {
    pub vcpu: u32,
    pub memory_mb: u32,
    /// TCP port the guest app listens on — used to build upstream_addr.
    pub port: u16,
    /// Local path on the node to the pre-staged ext4 rootfs image.
    pub rootfs_path: String,
    /// SHA-256 hex of the rootfs image. Daemon verifies before boot.
    /// `None` skips the check — development/testing only.
    pub artifact_sha256: Option<String>,
    /// Resource quota enforced post-boot (IO + network). Defaults to unlimited.
    #[serde(default)]
    pub quota: ResourceQuota,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashSpec {
    /// Local path on the node to the pre-staged .wasm binary.
    pub wasm_path: String,
    /// SHA-256 hex of the .wasm binary. Daemon verifies before execution.
    /// `None` skips the check — development/testing only.
    pub artifact_sha256: Option<String>,
}

/// Per-service resource limits — the Triple-Lock system:
///   Layer 1 — Hypervisor : vcpu + memory_mb in MetalSpec (Firecracker enforces at boot)
///   Layer 2 — Kernel IO  : disk_* fields   (cgroup v2 io.max, applied by daemon)
///   Layer 3 — Network    : net_* fields    (tc tbf on TAP device; Cilium eBPF layers on top)
///
/// `None` means unlimited for that dimension.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceQuota {
    // Layer 2: cgroup v2 io.max
    pub disk_read_bps:   Option<u64>,
    pub disk_write_bps:  Option<u64>,
    pub disk_read_iops:  Option<u32>,
    pub disk_write_iops: Option<u32>,

    // Layer 3: tc tbf on TAP (Cilium eBPF layers on top when installed)
    pub net_ingress_kbps: Option<u32>,
    pub net_egress_kbps:  Option<u32>,
}
