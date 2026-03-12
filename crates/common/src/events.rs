use serde::{Deserialize, Serialize};

pub const STREAM_NAME: &str = "PLATFORM_EVENTS";
pub const SUBJECT_PROVISION:     &str = "platform.provision";
pub const SUBJECT_DEPROVISION:   &str = "platform.deprovision";
/// Fire-and-forget (plain NATS, not JetStream).
/// Published by the daemon after upstream_addr is written to the DB.
/// Consumed by Pingora proxy instances to update their in-memory route cache.
pub const SUBJECT_ROUTE_UPDATED: &str = "platform.route_updated";
/// Fire-and-forget. Published by the daemon after a service is torn down.
/// Consumed by Pingora proxy instances to evict the slug from the route cache.
pub const SUBJECT_ROUTE_REMOVED: &str = "platform.route_removed";

/// Published by the API when a service is created or redeployed.
/// Consumed by the daemon on the metal node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionEvent {
    pub tenant_id:  String,
    pub service_id: String,
    pub app_name:   String,
    /// Routing slug — carried through so the daemon can publish RouteUpdatedEvent
    /// after provisioning without an extra DB round-trip.
    #[serde(default)]
    pub slug:       String,
    pub engine:     Engine,
    pub spec:       EngineSpec,
}

/// Published by the daemon after upstream_addr is set (provision complete).
/// Consumed by proxy instances to warm their in-memory route cache without
/// a DB round-trip on every request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteUpdatedEvent {
    /// Routing slug — first subdomain label, e.g. "myapp" from "myapp.liquidmetal.dev".
    pub slug:          String,
    /// Resolved upstream address, e.g. "172.16.0.2:8080" or "127.0.0.1:54321".
    pub upstream_addr: String,
}

/// Published by the daemon after a service is fully torn down.
/// Consumed by proxy instances to evict the slug from the in-memory route cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRemovedEvent {
    pub slug: String,
}

/// Fire-and-forget. Published by Pingora on every proxied request (debounced 30s per slug).
/// Consumed by the daemon to update services.last_request_at, which drives idle timeout
/// enforcement for serverless Metal services.
pub const SUBJECT_TRAFFIC_PULSE: &str = "platform.traffic_pulse";

/// Published by the daemon every 60s for each running Metal service.
/// Consumed by the API billing aggregator to record compute usage.
pub const SUBJECT_USAGE_METAL: &str = "platform.usage_metal";

/// Published by the daemon every 60s with batched Wasm invocation counts.
/// Consumed by the API billing aggregator to record invocation usage.
pub const SUBJECT_USAGE_LIQUID: &str = "platform.usage_liquid";

/// Published by the daemon when a Firecracker VM exits unexpectedly.
/// Consumed by the API (or logged) for crash visibility.
pub const SUBJECT_SERVICE_CRASHED: &str = "platform.service_crashed";

/// Published by the API when a workspace balance reaches zero.
/// Consumed by the daemon to suspend all running services for that workspace.
pub const SUBJECT_SUSPEND: &str = "platform.suspend";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficPulseEvent {
    pub slug: String,
}

/// Published by the API when a service is stopped or deleted.
/// Consumed by the daemon to halt the VM and release resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeprovisionEvent {
    pub service_id: String,
    pub slug:       String,
    pub engine:     Engine,
}

/// Published by the daemon every 60s per running Metal service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetalUsageEvent {
    pub workspace_id:  String,
    pub service_id:    String,
    pub duration_secs: u64,
    pub vcpu:          u32,
    pub memory_mb:     u32,
}

/// Published by the daemon every 60s with accumulated Wasm invocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiquidUsageEvent {
    pub workspace_id: String,
    pub service_id:   String,
    pub invocations:  u64,
}

/// Published by the daemon when a Firecracker child process exits unexpectedly.
/// DB status is updated to 'crashed' and upstream_addr cleared before publishing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceCrashedEvent {
    pub service_id: String,
    pub slug:       String,
    pub exit_code:  Option<i32>,
}

/// Published by the API billing aggregator when workspace balance <= 0.
/// Daemon deprovisions all running services for the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuspendEvent {
    pub workspace_id: String,
    pub reason:       String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    /// Firecracker microVM — full Linux kernel, dedicated disk, ~100ms cold start.
    Metal,
    /// Wasmtime executor — sandboxed process, memory-only, <1ms cold start.
    Liquid,
}

impl Engine {
    pub fn as_str(&self) -> &'static str {
        match self {
            Engine::Metal  => "metal",
            Engine::Liquid => "liquid",
        }
    }
}

impl std::str::FromStr for Engine {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "metal"  => Ok(Engine::Metal),
            "liquid" => Ok(Engine::Liquid),
            other    => Err(format!("unknown engine: {other}")),
        }
    }
}

impl std::fmt::Display for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
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
