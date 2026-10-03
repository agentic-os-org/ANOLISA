//! Execute one protocol exchange without assigning scheduling or policy authority.

use crate::{CallRecord, Failure, NativeOutput, ProcessOutput, StepOutput};
use aw_exec::{CommandSpec, Limits};
use aw_provider::{Protocol, Reply, Request};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(crate) struct Exchange {
    pub record: CallRecord,
    pub result: Result<Reply, Failure>,
}

pub(crate) fn check(deadline: Instant, cancelled: &AtomicBool) -> Result<(), aw_exec::Error> {
    if cancelled.load(Ordering::Acquire) {
        Err(aw_exec::Error::Cancelled)
    } else if Instant::now() >= deadline {
        Err(aw_exec::Error::DeadlineExceeded)
    } else {
        Ok(())
    }
}

pub(crate) struct Transport<'a> {
    pub protocol: &'a Protocol,
    pub command: &'a CommandSpec,
    pub limits: Limits,
    pub deadline: Instant,
    pub cancelled: &'a AtomicBool,
}

impl Transport<'_> {
    pub fn native(
        &self,
        mut record: CallRecord,
        input: &[u8],
    ) -> (CallRecord, Result<StepOutput, Failure>) {
        let started = Instant::now();
        let result = (|| {
            check(self.deadline, self.cancelled)?;
            let output = aw_exec::run(
                self.command,
                input,
                self.limits,
                self.deadline,
                self.cancelled,
            )?;
            record.process = Some(ProcessOutput {
                status: output.status,
                stderr: output.stderr.clone(),
                stdout_bytes: output.stdout.len(),
                input_bytes_written: output.input_bytes_written,
            });
            check(self.deadline, self.cancelled)?;
            // Native hooks may deliberately ignore stdin or use a nonzero exit as
            // their host protocol. Neither condition becomes a Provider failure.
            Ok(StepOutput::Native(NativeOutput {
                stdout: output.stdout,
                stderr: output.stderr,
                status: output.status,
            }))
        })();
        record.elapsed = started.elapsed();
        (record, result)
    }

    pub fn exchange(
        &self,
        mut record: CallRecord,
        build: impl FnOnce(u64) -> Result<Request, aw_provider::Error>,
    ) -> Exchange {
        let started = Instant::now();
        let result = (|| {
            check(self.deadline, self.cancelled)?;
            let budget_ms =
                remaining_millis(self.deadline.saturating_duration_since(Instant::now()))?;
            let request = build(budget_ms)?;
            let input = serde_json::to_vec(request.as_value())
                .map_err(|_| aw_provider::Error::Invalid("request encoding failed"))?;
            check(self.deadline, self.cancelled)?;
            let output = aw_exec::run(
                self.command,
                &input,
                self.limits,
                self.deadline,
                self.cancelled,
            )?;
            record.process = Some(ProcessOutput {
                status: output.status,
                stderr: output.stderr,
                stdout_bytes: output.stdout.len(),
                input_bytes_written: output.input_bytes_written,
            });
            check(self.deadline, self.cancelled)?;
            if !output.status.success() {
                return Err(Failure::Exit);
            }
            if output.input_bytes_written != input.len() {
                return Err(Failure::IncompleteInput);
            }
            let reply = self.protocol.check_response(&request, &output.stdout);
            // A late validation result cannot restore an expired or cancelled call.
            check(self.deadline, self.cancelled)?;
            Ok(reply?)
        })();
        record.elapsed = started.elapsed();
        Exchange { record, result }
    }
}

fn remaining_millis(remaining: Duration) -> Result<u64, aw_exec::Error> {
    if remaining.is_zero() {
        return Err(aw_exec::Error::DeadlineExceeded);
    }
    // The protocol uses integer milliseconds; transport still enforces the exact
    // Instant, so rounding up never extends the actual execution deadline.
    Ok(remaining.as_nanos().div_ceil(1_000_000) as u64)
}

#[cfg(test)]
mod tests {
    use super::remaining_millis;
    use std::time::Duration;

    #[test]
    fn positive_submillisecond_budget_is_not_expired() {
        assert!(remaining_millis(Duration::ZERO).is_err());
        for (nanoseconds, milliseconds) in [(1, 1), (999_999, 1), (1_000_000, 1), (1_000_001, 2)] {
            assert_eq!(
                remaining_millis(Duration::from_nanos(nanoseconds)).unwrap(),
                milliseconds
            );
        }
    }
}
