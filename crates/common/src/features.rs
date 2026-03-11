/// Runtime feature flags — loaded from env vars at startup.
///
/// Each flag is a simple boolean. Defaults are chosen for local dev safety
/// (auth off, quotas off, both engines on). Flip them in .env or the deploy
/// environment to harden a production node.
///
/// | Env var            | Default | Description                              |
/// |--------------------|---------|------------------------------------------|
/// | DISABLE_AUTH       | false   | Bypass API key auth (dev only)           |
/// | REQUIRE_INVITE     | false   | Restrict signups to invite-code holders  |
/// | ENABLE_METAL       | true    | Accept Firecracker VM provision events   |
/// | ENABLE_LIQUID      | true    | Accept Wasm provision events             |
/// | ENFORCE_QUOTAS     | true    | Enforce per-tier service/resource limits |
/// | MAINTENANCE_MODE   | false   | Reject new deploys; drain only           |
#[derive(Debug, Clone)]
pub struct Features {
    pub disable_auth:     bool,
    pub require_invite:   bool,
    pub enable_metal:     bool,
    pub enable_liquid:    bool,
    pub enforce_quotas:   bool,
    pub maintenance_mode: bool,
}

impl Features {
    pub fn from_env() -> Self {
        Self {
            disable_auth:     flag("DISABLE_AUTH",     false),
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
            disable_auth     = self.disable_auth,
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
/// "1" / "true" (case-insensitive) → true; anything else → `default`.
fn flag(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(v) => matches!(v.to_lowercase().as_str(), "1" | "true"),
        Err(_) => default,
    }
}
