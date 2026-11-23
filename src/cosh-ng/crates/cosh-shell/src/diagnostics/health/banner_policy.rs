//! Shared bounds for the Bash banner and its installed profile contract.

pub(crate) const BUDGET_MS: u64 = 300;
pub(crate) const SCAN_RESERVE_MS: u64 = 70;
pub(crate) const SEAL_RESERVE_MS: u64 = CAPTURE_RESERVE_US / 1000 + 10;
pub(crate) const OUTPUT_RESERVE_MS: u64 = 20;
pub(crate) const CAPTURE_RESERVE_US: u64 = 40_000;
pub(crate) const MAX_TEXT_BYTES: usize = 8192;
pub(crate) const BOOTSTRAP_BUDGET_MS: u64 = 20;

#[cfg(test)]
library_tests! {
mod tests {
    use super::*;

    #[test]
    fn installed_profile_uses_the_same_budget_and_output_bounds() {
        let profile = include_str!("../../../../../packaging/system-integration/profile.d/cosh.sh");
        for value in [
            format!("+ {}000))", BUDGET_MS),
            format!("- {CAPTURE_RESERVE_US}))"),
            format!("- {}000)", OUTPUT_RESERVE_MS),
            format!("-n {}", MAX_TEXT_BYTES + 1),
            format!("-le {MAX_TEXT_BYTES}"),
            format!("--budget-ms {BUDGET_MS}"),
        ] {
            assert!(profile.contains(&value), "profile contract drift: {value}");
        }
        const { assert!(SCAN_RESERVE_MS > SEAL_RESERVE_MS && SEAL_RESERVE_MS > OUTPUT_RESERVE_MS) };
    }
}
}
