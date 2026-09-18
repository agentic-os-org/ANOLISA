use crate::{RetryPolicy, StoreError};

/// # Errors
/// Rejects zero attempts/delay and a cap smaller than the base delay.
pub(crate) fn validate(policy: RetryPolicy) -> Result<(), StoreError> {
    if policy.max_attempts == 0
        || policy.base_delay_ms == 0
        || policy.max_delay_ms < policy.base_delay_ms
    {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

pub(crate) fn delay(policy: RetryPolicy, attempt: u32) -> u64 {
    policy
        .base_delay_ms
        .saturating_mul(
            1_u64
                .checked_shl(attempt.saturating_sub(1))
                .unwrap_or(u64::MAX),
        )
        .min(policy.max_delay_ms)
}
