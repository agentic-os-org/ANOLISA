//! Same-machine monotonic deadlines include connection and framing time.

use std::time::{Duration, Instant};

use crate::{rejected, Result};

pub(crate) const MAX_BUDGET: Duration = Duration::from_secs(60);

fn monotonic() -> Result<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value points to a writable timespec for this synchronous system call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    u64::try_from(value.tv_sec)
        .ok()
        .and_then(|s| s.checked_mul(1_000_000_000))
        .and_then(|s| s.checked_add(value.tv_nsec as u64))
        .ok_or_else(|| rejected("clock_range"))
}

pub(crate) fn encode(deadline: Instant) -> Result<u64> {
    let now = monotonic()?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| *remaining <= MAX_BUDGET)
        .ok_or_else(|| rejected("deadline_range"))?;
    now.checked_add(remaining.as_nanos() as u64)
        .ok_or_else(|| rejected("clock_range"))
}

pub(crate) fn decode(value: u64) -> Result<Instant> {
    let instant = Instant::now();
    let remaining = value
        .checked_sub(monotonic()?)
        .map(Duration::from_nanos)
        .filter(|remaining| !remaining.is_zero() && *remaining <= MAX_BUDGET)
        .ok_or_else(|| rejected("deadline_range"))?;
    Ok(instant + remaining)
}

// Evidence records may describe an already elapsed deadline. RPC admission must
// continue using encode/decode; execution checks the original Instant directly.
pub(crate) fn recorded(value: Instant) -> Result<u64> {
    let clock = monotonic()?;
    let reference = Instant::now();
    let timestamp = if let Some(remaining) = value.checked_duration_since(reference) {
        clock.checked_add(u64::try_from(remaining.as_nanos()).map_err(|_| rejected("clock_range"))?)
    } else {
        clock.checked_sub(
            u64::try_from(reference.duration_since(value).as_nanos())
                .map_err(|_| rejected("clock_range"))?,
        )
    };
    timestamp.ok_or_else(|| rejected("clock_range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_metadata_deadlines_do_not_become_admissible_rpc_deadlines() {
        let expired = Instant::now() - Duration::from_millis(5);
        let timestamp = recorded(expired).unwrap();
        assert!(monotonic().unwrap() - timestamp >= 5_000_000);
        assert!(encode(expired).is_err());
        assert!(decode(timestamp).is_err());
    }
}
