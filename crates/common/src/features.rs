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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Feature flag tests manipulate env vars, which are process-global.
    // Use a mutex to prevent parallel tests from interfering with each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env(key: &str, val: Option<&str>, f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap();
        unsafe {
            match val {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        f();
        unsafe { std::env::remove_var(key); }
    }

    #[test]
    fn flag_true_values() {
        for val in &["1", "true", "yes", "on", "TRUE", "True", "YES", "On"] {
            with_env("TEST_FLAG_TRUE", Some(val), || {
                assert!(flag("TEST_FLAG_TRUE", false), "expected true for {val:?}");
            });
        }
    }

    #[test]
    fn flag_false_values() {
        for val in &["0", "false", "no", "off", "FALSE", "False", "NO", "Off"] {
            with_env("TEST_FLAG_FALSE", Some(val), || {
                assert!(!flag("TEST_FLAG_FALSE", true), "expected false for {val:?}");
            });
        }
    }

    #[test]
    fn flag_unrecognized_uses_default_true() {
        with_env("TEST_FLAG_UNKNOWN", Some("banana"), || {
            assert!(flag("TEST_FLAG_UNKNOWN", true));
        });
    }

    #[test]
    fn flag_unrecognized_uses_default_false() {
        with_env("TEST_FLAG_UNKNOWN2", Some("banana"), || {
            assert!(!flag("TEST_FLAG_UNKNOWN2", false));
        });
    }

    #[test]
    fn flag_unset_uses_default_true() {
        with_env("TEST_FLAG_UNSET", None, || {
            assert!(flag("TEST_FLAG_UNSET", true));
        });
    }

    #[test]
    fn flag_unset_uses_default_false() {
        with_env("TEST_FLAG_UNSET2", None, || {
            assert!(!flag("TEST_FLAG_UNSET2", false));
        });
    }

    #[test]
    fn flag_empty_string_uses_default() {
        with_env("TEST_FLAG_EMPTY", Some(""), || {
            assert!(flag("TEST_FLAG_EMPTY", true));
        });
    }

    #[test]
    fn features_from_env_defaults() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Clear all feature flag env vars to test defaults
        for key in &["REQUIRE_INVITE", "ENABLE_METAL", "ENABLE_LIQUID", "ENFORCE_QUOTAS", "MAINTENANCE_MODE"] {
            unsafe { std::env::remove_var(key); }
        }
        let f = Features::from_env();
        assert!(!f.require_invite);
        assert!(f.enable_metal);
        assert!(f.enable_liquid);
        assert!(f.enforce_quotas);
        assert!(!f.maintenance_mode);
    }
}
