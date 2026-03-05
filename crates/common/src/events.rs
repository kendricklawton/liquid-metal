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
    /// Local path on the node to the pre-staged ext4 rootfs image.
    pub rootfs_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashSpec {
    /// Local path on the node to the pre-staged .wasm binary.
    pub wasm_path: String,
}
