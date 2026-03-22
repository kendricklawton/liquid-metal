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
    /// User-defined environment variables injected at runtime.
    #[serde(default)]
    pub env_vars:   std::collections::HashMap<String, String>,
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

/// Published by the API when a suspended workspace's balance is restored (top-up).
/// Consumed by the daemon to re-provision all suspended services for that workspace.
pub const SUBJECT_UNSUSPEND: &str = "platform.unsuspend";

/// JetStream durable. Published by the proxy when a request arrives for a cold service
/// (status='ready', snapshot exists). Consumed by the daemon to restore the VM from snapshot.
pub const SUBJECT_WAKE: &str = "platform.wake";

/// Fire-and-forget. Published by the daemon at each provisioning step.
/// Subject: `platform.deploy_progress.{service_id}`.
/// Consumed by the API's SSE endpoint to stream live deploy status to `flux deploy`.
pub const SUBJECT_DEPLOY_PROGRESS: &str = "platform.deploy_progress";

/// Each discrete step the daemon reports during provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployStep {
    Queued,
    Downloading,
    Verifying,
    Building,     // Metal: assembling rootfs (template + binary + env vars)
    Booting,      // Metal: Firecracker VM starting
    Starting,     // Liquid: Wasmtime shim starting
    HealthCheck,   // TCP startup probe
    Snapshotting,  // Metal: creating VM snapshot after successful probe
    Ready,         // Metal terminal — snapshot stored, VM halted, awaiting first request
    Running,       // Liquid terminal — module loaded and serving
    Failed,        // Terminal — failure
}

/// Published by the daemon at each step of provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployProgressEvent {
    pub service_id: String,
    pub step: DeployStep,
    pub message: String,
}

/// Whether a provision failure is worth retrying.
/// Transient failures (S3 timeout, DB blip) retry with backoff.
/// Permanent failures (SHA mismatch, bad binary) stop immediately.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// Retry-eligible: S3 timeout, DB connection error, transient network failure.
    Transient,
    /// Do not retry: startup probe timeout, SHA mismatch, bad binary, Wasm compile error.
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficPulseEvent {
    pub slug: String,
}

/// Published by the proxy when a request arrives for a cold service.
/// Metal: restored from Firecracker snapshot. Liquid: re-compiled from cached Wasm module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeEvent {
    pub service_id:   String,
    pub slug:         String,
    pub engine:       Engine,
    /// S3 key prefix for snapshot files (Metal only). Empty for Liquid.
    #[serde(default)]
    pub snapshot_key: String,
}

/// Published by the API when a service is stopped or deleted.
/// Consumed by the daemon to halt the VM and release resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeprovisionEvent {
    pub service_id: String,
    pub slug:       String,
    pub engine:     Engine,
}

/// Published by the proxy every 60s with accumulated Metal usage per service.
/// Metal is billed on two dimensions: invocations + compute time (GB-seconds).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetalUsageEvent {
    pub workspace_id:  String,
    pub service_id:    String,
    pub invocations:   u64,
    /// Accumulated compute duration in milliseconds across all invocations.
    /// Converted to GB-sec by the billing aggregator: `duration_ms / 1000 * 0.128 GB`.
    pub duration_ms:   u64,
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

/// Published by the API when a suspended workspace regains positive balance
/// after a top-up. Daemon re-provisions all suspended services for that workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsuspendEvent {
    pub workspace_id: String,
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
///
/// `Default` provides conservative baseline limits so that a missing `quota`
/// field in a ProvisionEvent never results in an unlimited VM:
///   Disk:    100 MB/s read/write, 5000/2000 IOPS
///   Network: 100 Mbps ingress/egress
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for ResourceQuota {
    fn default() -> Self {
        Self {
            disk_read_bps:    Some(100 * 1024 * 1024), // 100 MB/s
            disk_write_bps:   Some(100 * 1024 * 1024), // 100 MB/s
            disk_read_iops:   Some(5000),
            disk_write_iops:  Some(2000),
            net_ingress_kbps: Some(100_000),            // 100 Mbps
            net_egress_kbps:  Some(100_000),            // 100 Mbps
        }
    }
}

/// Published by the API cert_manager after a TLS cert is stored in Postgres.
/// Consumed by proxy instances to hot-reload the cert into their in-memory SNI cache.
pub const SUBJECT_CERT_PROVISIONED: &str = "platform.cert_provisioned";

/// Published when a cert is provisioned or renewed for a custom domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertProvisionedEvent {
    /// Bare custom domain name, e.g. "api.mycorp.com".
    pub domain: String,
}

impl ResourceQuota {
    /// Build from env vars, falling back to compiled defaults for any unset var.
    ///
    ///   QUOTA_DISK_READ_BPS, QUOTA_DISK_WRITE_BPS     (bytes/s, 0 = unlimited)
    ///   QUOTA_DISK_READ_IOPS, QUOTA_DISK_WRITE_IOPS   (0 = unlimited)
    ///   QUOTA_NET_INGRESS_KBPS, QUOTA_NET_EGRESS_KBPS  (0 = unlimited)
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            disk_read_bps:    parse_quota_env("QUOTA_DISK_READ_BPS",    defaults.disk_read_bps),
            disk_write_bps:   parse_quota_env("QUOTA_DISK_WRITE_BPS",   defaults.disk_write_bps),
            disk_read_iops:   parse_quota_env("QUOTA_DISK_READ_IOPS",   defaults.disk_read_iops),
            disk_write_iops:  parse_quota_env("QUOTA_DISK_WRITE_IOPS",  defaults.disk_write_iops),
            net_ingress_kbps: parse_quota_env("QUOTA_NET_INGRESS_KBPS", defaults.net_ingress_kbps),
            net_egress_kbps:  parse_quota_env("QUOTA_NET_EGRESS_KBPS",  defaults.net_egress_kbps),
        }
    }
}

/// Parse a quota env var. Unset → use `fallback`. Set to `"0"` → `None` (unlimited).
/// Set to a positive number → `Some(n)`.
fn parse_quota_env<T: std::str::FromStr + PartialEq + Default>(key: &str, fallback: Option<T>) -> Option<T> {
    match std::env::var(key) {
        Err(_) => fallback,
        Ok(val) => match val.parse::<T>() {
            Ok(v) if v == T::default() => None, // 0 = unlimited
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(key, val, "invalid quota value, using default");
                fallback
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Engine ───────────────────────────────────────────────────────────

    #[test]
    fn engine_from_str_metal() {
        assert_eq!("metal".parse::<Engine>().unwrap(), Engine::Metal);
    }

    #[test]
    fn engine_from_str_liquid() {
        assert_eq!("liquid".parse::<Engine>().unwrap(), Engine::Liquid);
    }

    #[test]
    fn engine_from_str_unknown() {
        assert!("docker".parse::<Engine>().is_err());
    }

    #[test]
    fn engine_from_str_case_sensitive() {
        assert!("Metal".parse::<Engine>().is_err());
    }

    #[test]
    fn engine_as_str_roundtrip() {
        assert_eq!(Engine::Metal.as_str(), "metal");
        assert_eq!(Engine::Liquid.as_str(), "liquid");
    }

    #[test]
    fn engine_display() {
        assert_eq!(format!("{}", Engine::Metal), "metal");
        assert_eq!(format!("{}", Engine::Liquid), "liquid");
    }

    // ── Event serialization roundtrips ───────────────────────────────────

    #[test]
    fn provision_event_roundtrip() {
        let event = ProvisionEvent {
            tenant_id: "t1".to_string(),
            service_id: "s1".to_string(),
            app_name: "myapp".to_string(),
            slug: "myapp".to_string(),
            engine: Engine::Metal,
            spec: EngineSpec::Metal(MetalSpec {
                vcpu: 1,
                memory_mb: 512,
                port: 8080,
                artifact_key: "metal/p1/d1/app".to_string(),
                artifact_sha256: Some("abc123".to_string()),
                quota: ResourceQuota::default(),
            }),
            env_vars: [("PORT".to_string(), "8080".to_string())].into(),
        };

        let json = serde_json::to_string(&event).unwrap();
        let deserialized: ProvisionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.service_id, "s1");
        assert_eq!(deserialized.slug, "myapp");
        assert!(matches!(deserialized.engine, Engine::Metal));
        assert_eq!(deserialized.env_vars.get("PORT").unwrap(), "8080");
    }

    #[test]
    fn route_updated_event_roundtrip() {
        let event = RouteUpdatedEvent {
            slug: "myapp".to_string(),
            upstream_addr: "172.16.0.2:8080".to_string(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let d: RouteUpdatedEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(d.slug, "myapp");
        assert_eq!(d.upstream_addr, "172.16.0.2:8080");
    }

    #[test]
    fn deprovision_event_roundtrip() {
        let event = DeprovisionEvent {
            service_id: "s1".to_string(),
            slug: "myapp".to_string(),
            engine: Engine::Liquid,
        };
        let json = serde_json::to_string(&event).unwrap();
        let d: DeprovisionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(d.slug, "myapp");
        assert!(matches!(d.engine, Engine::Liquid));
    }

    #[test]
    fn deploy_step_serialization() {
        let step = DeployStep::HealthCheck;
        let json = serde_json::to_value(&step).unwrap();
        assert_eq!(json, "health_check");
    }

    #[test]
    fn deploy_step_all_variants_serialize() {
        let steps = vec![
            DeployStep::Queued,
            DeployStep::Downloading,
            DeployStep::Verifying,
            DeployStep::Building,
            DeployStep::Booting,
            DeployStep::Starting,
            DeployStep::HealthCheck,
            DeployStep::Snapshotting,
            DeployStep::Ready,
            DeployStep::Running,
            DeployStep::Failed,
        ];
        for step in steps {
            let json = serde_json::to_value(&step).unwrap();
            assert!(json.is_string(), "step {:?} should serialize as string", step);
            // Roundtrip
            let back: DeployStep = serde_json::from_value(json).unwrap();
            assert_eq!(
                serde_json::to_string(&step).unwrap(),
                serde_json::to_string(&back).unwrap()
            );
        }
    }

    #[test]
    fn failure_kind_roundtrip() {
        let t: FailureKind = serde_json::from_str("\"transient\"").unwrap();
        assert_eq!(t, FailureKind::Transient);
        let p: FailureKind = serde_json::from_str("\"permanent\"").unwrap();
        assert_eq!(p, FailureKind::Permanent);
    }

    #[test]
    fn engine_spec_tagged_serialization() {
        let spec = EngineSpec::Liquid(LiquidSpec {
            artifact_key: "wasm/p1/d1/main.wasm".to_string(),
            artifact_sha256: None,
        });
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["type"], "liquid");
        assert_eq!(json["artifact_key"], "wasm/p1/d1/main.wasm");
    }

    // ── ResourceQuota ────────────────────────────────────────────────────

    #[test]
    fn resource_quota_defaults_are_conservative() {
        let q = ResourceQuota::default();
        assert_eq!(q.disk_read_bps, Some(100 * 1024 * 1024));
        assert_eq!(q.disk_write_bps, Some(100 * 1024 * 1024));
        assert_eq!(q.disk_read_iops, Some(5000));
        assert_eq!(q.disk_write_iops, Some(2000));
        assert_eq!(q.net_ingress_kbps, Some(100_000));
        assert_eq!(q.net_egress_kbps, Some(100_000));
    }

    #[test]
    fn resource_quota_serializes_with_defaults() {
        let q = ResourceQuota::default();
        let json = serde_json::to_value(&q).unwrap();
        assert!(json["disk_read_bps"].is_number());
        assert!(json["net_egress_kbps"].is_number());
    }

    #[test]
    fn provision_event_defaults_env_vars() {
        // env_vars should default to empty when missing from JSON
        let json = r#"{
            "tenant_id": "t1",
            "service_id": "s1",
            "app_name": "app",
            "engine": "metal",
            "spec": {
                "type": "metal",
                "vcpu": 1,
                "memory_mb": 512,
                "port": 8080,
                "artifact_key": "k",
                "artifact_sha256": null
            }
        }"#;
        let event: ProvisionEvent = serde_json::from_str(json).unwrap();
        assert!(event.env_vars.is_empty());
        assert_eq!(event.slug, ""); // serde(default)
    }
}
