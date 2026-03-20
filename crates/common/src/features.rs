/// Runtime feature flags — loaded from env vars at startup.
///
/// Each flag is a simple boolean. Flip them in .env or the deploy
/// environment to tune a production node.
///
/// | Env var            | Default | Description                              |
/// |--------------------|---------|------------------------------------------|
/// | REQUIRE_INVITE     | false   | Restrict signups to invite-code holders  |
/// | ENABLE_METAL       | true    | Accept Firecracker VM provision events   |
/// | ENABLE_LIQUID      | true    | Accept Wasm provision events             |
/// | ENFORCE_QUOTAS     | true    | Enforce per-tier service/resource limits |
/// | MAINTENANCE_MODE   | false   | Reject new deploys; drain only           |
#[derive(Debug, Clone)]
pub struct Features {
    pub require_invite:   bool,
    pub enable_metal:     bool,
    pub enable_liquid:    bool,
    pub enforce_quotas:   bool,
    pub maintenance_mode: bool,
}

impl Features {
    pub fn from_env() -> Self {
        Self {
            require_invite:   flag("REQUIRE_INVITE",   false),
            enable_metal:     flag("ENABLE_METAL",     true),
            enable_liquid:    flag("ENABLE_LIQUID",    true),
            enforce_quotas:   flag("ENFORCE_QUOTAS",   true),
            maintenance_mode: flag("MAINTENANCE_MODE", false),
        }
    }

    /// Emit a single structured log line summarising the active flag set.
    pub fn log_summary(&self) {
        tracing::info!(
            require_invite   = self.require_invite,
            enable_metal     = self.enable_metal,
            enable_liquid    = self.enable_liquid,
            enforce_quotas   = self.enforce_quotas,
            maintenance_mode = self.maintenance_mode,
            "feature flags"
        );
    }
}

/// Read an env var as a boolean flag.
/// Truthy: "1", "true", "yes", "on"  — Falsy: "0", "false", "no", "off"
/// Unrecognized values log a warning and fall back to `default`.
fn flag(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => match v.to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            other => {
                tracing::warn!(
                    key,
                    value = other,
                    default,
                    "unrecognized feature flag value — using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}
