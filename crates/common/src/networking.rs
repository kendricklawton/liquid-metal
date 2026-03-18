//! Shared networking conventions for Metal (Firecracker) services.
//!
//! The TAP index → IP mapping is an architectural contract that the daemon,
//! API, and proxy must all agree on. Centralizing it here prevents drift.

/// Maximum supported TAP index: 63 subnets × 63 hosts = 3969 concurrent VMs.
pub const MAX_TAP_INDEX: u32 = 63 * 63 - 1; // 3968

/// Derive the guest IP address for a TAP interface index.
///
/// Scheme: `172.16.{tap_idx/63}.{(tap_idx%63)*4+2}`
/// Gives 63 subnets × 63 hosts = 3969 concurrent Metal VMs per node.
///
/// Returns an error if `tap_idx` exceeds `MAX_TAP_INDEX` (3968), which would
/// produce IP octets outside the valid 0-254 range.
pub fn guest_ip(tap_idx: u32) -> anyhow::Result<String> {
    anyhow::ensure!(
        tap_idx <= MAX_TAP_INDEX,
        "TAP index {tap_idx} exceeds maximum ({MAX_TAP_INDEX}) — would produce invalid IP"
    );
    Ok(format!("172.16.{}.{}", tap_idx / 63, (tap_idx % 63) * 4 + 2))
}

/// Derive the TAP interface name for a given index.
///
/// Convention: `tap{idx}` — must match what the daemon creates via netlink
/// and what Firecracker is configured to use.
pub fn tap_name(tap_idx: u32) -> String {
    format!("tap{tap_idx}")
}
