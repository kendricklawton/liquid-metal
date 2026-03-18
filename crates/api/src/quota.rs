/// Per-tier resource allocation assigned at deploy time.
///
/// `max_vcpu` and `max_memory_mb` are the resources each Metal service gets —
/// derived from the tier, not from user input. Customers don't choose VM specs;
/// they pick a plan and we handle the rest.
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
