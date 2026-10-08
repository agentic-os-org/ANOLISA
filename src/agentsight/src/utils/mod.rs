#[cfg(target_os = "linux")]
pub mod decompress;
#[cfg(target_os = "linux")]
pub mod process;
pub mod procfs;
pub mod thread;

use std::time::{SystemTime, UNIX_EPOCH};

/// Nanoseconds since the Unix epoch at `at`, 0 when the clock reads before it.
///
/// A host whose realtime clock is before 1970-01-01 (a failed RTC battery, a
/// snapshot restored with a stale clock) makes `duration_since(UNIX_EPOCH)`
/// fail; every hardened sibling in this crate (`server::system_audit::now_ns`,
/// `server::optimize::now_ns`, `enforcement::store::now_ns`,
/// `enforcement::transition::now_ns`, `analyzer::token::record`) maps that to
/// 0 instead of panicking, and this helper gives the remaining call sites the
/// same contract in one place. A 0 timestamp reads as "unknown/epoch" to every
/// consumer, which is the same degraded answer those siblings give.
pub fn epoch_nanos(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::epoch_nanos;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn epoch_nanos_maps_a_pre_epoch_clock_to_zero_without_panicking() {
        // The family contract: a clock before the epoch degrades to 0 instead
        // of unwrapping the Err side of `duration_since` and taking the
        // caller (a dashboard handler, a CLI report) down with it.
        assert_eq!(epoch_nanos(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert_eq!(
            epoch_nanos(UNIX_EPOCH - Duration::from_secs(1_000_000_000)),
            0
        );
        assert_eq!(epoch_nanos(UNIX_EPOCH), 0);
    }

    #[test]
    fn epoch_nanos_counts_forward_from_the_epoch() {
        assert_eq!(
            epoch_nanos(UNIX_EPOCH + Duration::from_secs(1)),
            1_000_000_000
        );
        assert_eq!(
            epoch_nanos(UNIX_EPOCH + Duration::from_millis(1)),
            1_000_000
        );
    }
}
