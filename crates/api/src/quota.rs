/// Global service limit per workspace. No tier-based limits —
/// everyone gets the same access. Pay for what you use.
pub const MAX_SERVICES_PER_WORKSPACE: i64 = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_services_is_reasonable() {
        assert!(MAX_SERVICES_PER_WORKSPACE > 0);
        assert!(MAX_SERVICES_PER_WORKSPACE <= 100);
    }
}
