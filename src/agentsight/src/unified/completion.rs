//! Pace complete raw captures before analysis expands their large fields.

use super::*;
use crate::aggregator::AggregatedResult;
use crate::utils::cpu_budget::{CpuBudget, WorkClass};
use std::collections::VecDeque;
use std::time::Instant;

struct Queued {
    result: AggregatedResult,
    bytes: usize,
    enqueued: Instant,
}

/// Queue ownership and accounting stay beside the shared CPU admission control.
#[derive(Default)]
pub(super) struct CompletionScheduler {
    pub(super) budget: Option<Arc<CpuBudget>>,
    queue: VecDeque<Queued>,
    pub(super) bytes: usize,
    next_attempt: Option<Instant>,
}

impl CompletionScheduler {
    pub(super) fn new(budget: Option<Arc<CpuBudget>>) -> Self {
        Self {
            budget,
            ..Self::default()
        }
    }

    pub(super) fn len(&self) -> usize {
        self.queue.len()
    }

    /// Return ownership unchanged when the shared count or byte allowance is full.
    pub(super) fn try_push(
        &mut self,
        result: AggregatedResult,
        max_bytes: usize,
        max_count: usize,
    ) -> Option<AggregatedResult> {
        let bytes = result.retained_bytes();
        if self.queue.len() >= max_count || self.bytes.saturating_add(bytes) > max_bytes {
            return Some(result);
        }
        self.bytes += bytes;
        self.queue.push_back(Queued {
            result,
            bytes,
            enqueued: Instant::now(),
        });
        None
    }

    fn pop(&mut self) -> Option<AggregatedResult> {
        let queued = self.queue.pop_front()?;
        self.bytes -= queued.bytes;
        metrics::histogram!("agentsight_completion_wait_seconds")
            .record(queued.enqueued.elapsed().as_secs_f64());
        Some(queued.result)
    }
}

impl AgentSight {
    pub(super) fn process_aggregated_results(&mut self, results: Vec<AggregatedResult>) {
        if self.completions.budget.is_none() {
            self.deliver_aggregated_results(results);
            return;
        }
        for mut result in results {
            loop {
                let available = self
                    .runtime_limits
                    .pending_genai_max_bytes
                    .saturating_sub(self.pending_genai.bytes)
                    .min(self.runtime_limits.pending_genai_max_bytes / 2);
                let count = self
                    .runtime_limits
                    .pending_genai_max_count
                    .saturating_sub(self.pending_genai.len());
                let Some(rejected) = self.completions.try_push(result, available, count) else {
                    self.drain_one_completion(false);
                    break;
                };
                result = rejected;
                if self.completions.len() > 0 {
                    // Preserve FIFO and apply backpressure without bypassing pacing.
                    self.drain_one_completion(true);
                    continue;
                }
                // One oversized task remains in-flight, outside the bounded
                // queue, and waits for CPU admission before expanding its fields.
                let budget = self.completions.budget.clone();
                let _permit =
                    budget
                        .as_ref()
                        .and_then(|budget| match budget.acquire(WorkClass::Capture) {
                            Ok(permit) => Some(permit),
                            Err(error) => {
                                log::error!("CPU pacing failed: {error}");
                                None
                            }
                        });
                self.deliver_aggregated_results(vec![result]);
                break;
            }
        }
    }

    /// Return whether an owned completion was delivered; waits only when applying backpressure.
    pub(super) fn drain_one_completion(&mut self, wait: bool) -> bool {
        if self.completions.len() == 0 {
            return false;
        }
        if !wait
            && self
                .completions
                .next_attempt
                .is_some_and(|next| Instant::now() < next)
        {
            return false;
        }
        let budget = self.completions.budget.clone();
        let _permit = loop {
            match budget
                .as_ref()
                .map(|budget| budget.try_acquire(WorkClass::Capture))
                .transpose()
            {
                Ok(Some(Some(permit))) => break Some(permit),
                Ok(None) => break None,
                Ok(Some(None)) if !wait => {
                    self.completions.next_attempt = Some(Instant::now() + Duration::from_millis(5));
                    return false;
                }
                Ok(Some(None)) => {
                    self.maybe_export_runtime_metrics();
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => {
                    log::error!(
                        "CPU pacing failed; preserving capture delivery without pacing: {error}"
                    );
                    break None;
                }
            }
        };
        self.completions.next_attempt = None;
        if let Some(result) = self.completions.pop() {
            self.deliver_aggregated_results(vec![result]);
            return true;
        }
        false
    }

    pub(super) fn flush_completions(&mut self) {
        while self.drain_one_completion(true) {}
    }

    /// PID-exit analysis must observe the calls queued before that lifecycle event.
    pub(super) fn flush_completions_for_pid(&mut self, pid: u32) {
        while self
            .completions
            .queue
            .iter()
            .any(|entry| entry.result.pid() == pid)
        {
            self.drain_one_completion(true);
        }
    }
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
