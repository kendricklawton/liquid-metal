/// Per-tier limits. In the serverless model, the primary limit is the
/// invocation cap (Hobby) or credit balance (Pro/Team). Service count
/// is a secondary guardrail — idle services cost nothing.
pub struct TierLimits {
    pub max_services:      i64,
    pub free_invocations:  i64,
}

/// 1M free invocations for all tiers. Beyond this, Hobby is suspended
/// and Pro/Team are charged per-invocation from their credit balance.
const FREE_INVOCATIONS: i64 = 1_000_000;

pub fn limits_for(tier: &str) -> TierLimits {
    match tier {
        "pro" => TierLimits {
            max_services:     20,
            free_invocations: FREE_INVOCATIONS,
        },
        "team" => TierLimits {
            max_services:     50,
            free_invocations: FREE_INVOCATIONS,
        },
        _ => TierLimits { // hobby + unknown
            max_services:     5,
            free_invocations: FREE_INVOCATIONS,
        },
    }
}
