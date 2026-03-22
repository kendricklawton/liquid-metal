//! Metal tier definitions and Liquid pricing constants.
//!
//! These are the canonical pricing values for the platform. The plans.html
//! page mirrors these numbers. If pricing changes, update both.

/// Metal VM tier specification.
pub struct MetalTierSpec {
    /// Tier identifier used in API requests and liquid-metal.toml.
    pub id: &'static str,
    /// Human-readable label (e.g. "1 vCPU").
    pub label: &'static str,
    /// Number of dedicated vCPUs.
    pub vcpu: u32,
    /// RAM in megabytes.
    pub memory_mb: u32,
    /// Base NVMe storage in gigabytes (top-up for more).
    pub disk_gb: u32,
    /// Monthly price in cents (e.g. 1700 = $17.00).
    pub price_cents: u32,
}

/// The three Metal tiers. Source of truth for pricing.
///
/// Economics (EPYC 7452, $215/mo, 60 sellable vCPUs, 368 GB RAM, 950 GB NVMe):
///   - 1 vCPU tier: 60 max VMs, $1,020 max revenue, 4.7x markup
///   - At 70% fill (mixed): ~$906/mo revenue, $691 margin
///   - RAM and disk are massively underutilized — competitive advantage
pub const METAL_TIERS: [MetalTierSpec; 3] = [
    MetalTierSpec {
        id: "one",
        label: "1 vCPU",
        vcpu: 1,
        memory_mb: 2048,
        disk_gb: 10,
        price_cents: 2000,
    },
    MetalTierSpec {
        id: "two",
        label: "2 vCPU",
        vcpu: 2,
        memory_mb: 4096,
        disk_gb: 20,
        price_cents: 4000,
    },
    MetalTierSpec {
        id: "four",
        label: "4 vCPU",
        vcpu: 4,
        memory_mb: 8192,
        disk_gb: 40,
        price_cents: 8000,
    },
];

/// Free Liquid invocations per workspace per month.
pub const FREE_LIQUID_INVOCATIONS: i64 = 1_000_000;

/// Liquid price per 1M invocations in micro-credits ($0.30 = 300_000 µcr).
pub const LIQUID_PRICE_PER_MILLION: i64 = 300_000;

/// Look up a Metal tier by its string identifier.
pub fn metal_tier(id: &str) -> Option<&'static MetalTierSpec> {
    METAL_TIERS.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_valid_tiers() {
        assert_eq!(metal_tier("one").unwrap().vcpu, 1);
        assert_eq!(metal_tier("two").unwrap().vcpu, 2);
        assert_eq!(metal_tier("four").unwrap().vcpu, 4);
    }

    #[test]
    fn lookup_invalid_tier() {
        assert!(metal_tier("eight").is_none());
        assert!(metal_tier("").is_none());
    }

    #[test]
    fn prices_scale_linearly() {
        let one = metal_tier("one").unwrap().price_cents;
        let two = metal_tier("two").unwrap().price_cents;
        let four = metal_tier("four").unwrap().price_cents;
        assert_eq!(two, one * 2);
        assert_eq!(four, two * 2);
    }

    #[test]
    fn memory_scales_with_vcpu() {
        let one = metal_tier("one").unwrap();
        let two = metal_tier("two").unwrap();
        let four = metal_tier("four").unwrap();
        assert_eq!(two.memory_mb, one.memory_mb * 2);
        assert_eq!(four.memory_mb, two.memory_mb * 2);
    }

    #[test]
    fn tier_count() {
        assert_eq!(METAL_TIERS.len(), 3);
    }
}
