// SPDX-License-Identifier: Apache-2.0
//! In-process counters surfaced via `GET /v1/metrics` (Prometheus text
//! exposition). v0.1 ships a small, fixed set; richer histograms wait
//! for an opinion-graded collector in Phase 2.

use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct Metrics {
    pub requests_total: AtomicU64,
    pub instances_created: AtomicU64,
    pub instances_destroyed: AtomicU64,
    pub policy_eval_failures: AtomicU64,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn inc(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Render every counter into the Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let series = [
            (
                "blaze_requests_total",
                "Total HTTP requests served by the blaze daemon",
                self.requests_total.load(Ordering::Relaxed),
            ),
            (
                "blaze_instances_created_total",
                "Total sandbox instances created",
                self.instances_created.load(Ordering::Relaxed),
            ),
            (
                "blaze_instances_destroyed_total",
                "Total sandbox instances destroyed",
                self.instances_destroyed.load(Ordering::Relaxed),
            ),
            (
                "blaze_policy_eval_failures_total",
                "Number of failed policy evaluations",
                self.policy_eval_failures.load(Ordering::Relaxed),
            ),
        ];
        for (name, help, value) in series {
            use std::fmt::Write;
            let _ = writeln!(&mut out, "# HELP {name} {help}");
            let _ = writeln!(&mut out, "# TYPE {name} counter");
            let _ = writeln!(&mut out, "{name} {value}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_keeps_active_counters_and_omits_removed_pool_counters() {
        let metrics = Metrics::new();
        metrics.inc(&metrics.requests_total);
        metrics.inc(&metrics.instances_created);

        let rendered = metrics.render();

        assert!(rendered.contains("blaze_requests_total 1"));
        assert!(rendered.contains("blaze_instances_created_total 1"));
        assert!(rendered.contains("blaze_instances_destroyed_total 0"));
        assert!(rendered.contains("blaze_policy_eval_failures_total 0"));
        for removed in [
            "blaze_instances_resets_total",
            "blaze_pool_hits_total",
            "blaze_pool_misses_total",
        ] {
            assert!(!rendered.contains(removed));
        }
    }
}
