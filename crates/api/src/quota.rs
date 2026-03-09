/// Per-tier resource limits enforced at deploy time.
pub struct TierLimits {
    pub max_services:     i64,
    pub max_vcpu:         u32,
    pub max_memory_mb:    u32,
    pub allows_always_on: bool,
}

pub fn limits_for(tier: &str) -> TierLimits {
    match tier {
        "pro" => TierLimits {
            max_services:     10,
            max_vcpu:         2,
            max_memory_mb:    512,
            allows_always_on: true,
        },
        "team" => TierLimits {
            max_services:     25,
            max_vcpu:         4,
            max_memory_mb:    1024,
            allows_always_on: true,
        },
        _ => TierLimits { // hobby + unknown — fail safe
            max_services:     2,
            max_vcpu:         1,
            max_memory_mb:    128,
            allows_always_on: false,
        },
    }
}
