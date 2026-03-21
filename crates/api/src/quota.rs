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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hobby_tier_limits() {
        let l = limits_for("hobby");
        assert_eq!(l.max_services, 5);
        assert_eq!(l.free_invocations, 1_000_000);
    }

    #[test]
    fn pro_tier_limits() {
        let l = limits_for("pro");
        assert_eq!(l.max_services, 20);
        assert_eq!(l.free_invocations, 1_000_000);
    }

    #[test]
    fn team_tier_limits() {
        let l = limits_for("team");
        assert_eq!(l.max_services, 50);
        assert_eq!(l.free_invocations, 1_000_000);
    }

    #[test]
    fn unknown_tier_falls_back_to_hobby() {
        let l = limits_for("enterprise");
        assert_eq!(l.max_services, 5);
    }

    #[test]
    fn empty_tier_falls_back_to_hobby() {
        let l = limits_for("");
        assert_eq!(l.max_services, 5);
    }

    #[test]
    fn free_invocations_same_for_all_tiers() {
        assert_eq!(limits_for("hobby").free_invocations, limits_for("pro").free_invocations);
        assert_eq!(limits_for("pro").free_invocations, limits_for("team").free_invocations);
    }

    #[test]
    fn service_limits_increase_with_tier() {
        let hobby = limits_for("hobby").max_services;
        let pro = limits_for("pro").max_services;
        let team = limits_for("team").max_services;
        assert!(hobby < pro);
        assert!(pro < team);
    }
}
