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
