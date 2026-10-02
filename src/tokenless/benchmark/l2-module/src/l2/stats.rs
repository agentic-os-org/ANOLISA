// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Statistics for the L2 report: sample-size inference, paired differences,
//! bootstrap confidence intervals, Wilson score intervals and percentiles.
//!
//! Everything here is deterministic given the same inputs (the bootstrap is
//! seeded), so report numbers are reproducible run-to-run — a requirement
//! for regression-tracking the comparison over commits.

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Pilot size used before inferring the full sample count.
pub const PILOT_N: usize = 5;
/// Upper bound on the inferred sample count: a huge pilot CV must not turn
/// into an unaffordable run.
pub const MAX_N: usize = 50;
/// Bootstrap resample count.
pub const BOOTSTRAP_ITERS: usize = 10_000;
/// Fixed bootstrap seed: CI bounds must be reproducible across runs.
pub const BOOTSTRAP_SEED: u64 = 42;

/// Arithmetic mean; 0.0 for an empty slice (callers guard emptiness).
pub fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// Sample standard deviation (ddof = 1); 0.0 with fewer than two points.
pub fn std_dev(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    let var = values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() - 1) as f64;
    var.sqrt()
}

/// Infers the sample size N from a pilot run's coefficient of variation.
///
/// `N = ceil((1.96 * CV / 0.05)^2)` clamped to `[PILOT_N, MAX_N]`: targets a
/// 95% CI half-width of 5% of the mean, while the clamp keeps degenerate
/// pilots (CV≈0 or huge) from producing useless or unaffordable N.
pub fn infer_n(pilot: &[f64]) -> usize {
    let m = mean(pilot);
    if m == 0.0 {
        return PILOT_N;
    }
    let cv = std_dev(pilot) / m.abs();
    let target = (1.96 * cv / 0.05).powi(2).ceil();
    // Clamp while still in the f64 domain. A degenerate pilot can push the
    // intermediate past usize range, and narrowing first would depend on
    // saturating-cast semantics to land back in bounds. An infinite target
    // still means "as noisy as it gets", so it clamps up to MAX_N; only NaN
    // (from a NaN-bearing pilot) has no meaningful N and falls back to the
    // pilot size.
    if target.is_nan() {
        return PILOT_N;
    }
    target.clamp(PILOT_N as f64, MAX_N as f64) as usize
}

/// Element-wise paired differences `a[i] - b[i]`, truncated to the shorter
/// side so a degraded (partially missing) side never misaligns pairs.
pub fn paired_diff(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b.iter()).map(|(x, y)| x - y).collect()
}

/// Seeded percentile-bootstrap 95% CI of the mean.
///
/// Fixed `StdRng` seed (see [`BOOTSTRAP_SEED`]) so CI bounds are identical
/// across reruns on the same data. Returns `(mean, mean)` for slices with
/// fewer than two points — a CI is meaningless there but callers still want
/// a printable interval.
pub fn bootstrap_ci_mean(values: &[f64]) -> (f64, f64) {
    if values.len() < 2 {
        let m = mean(values);
        return (m, m);
    }
    let mut rng = StdRng::seed_from_u64(BOOTSTRAP_SEED);
    let mut means = Vec::with_capacity(BOOTSTRAP_ITERS);
    for _ in 0..BOOTSTRAP_ITERS {
        let resampled: f64 = (0..values.len())
            .map(|_| values[rng.gen_range(0..values.len())])
            .sum::<f64>()
            / values.len() as f64;
        means.push(resampled);
    }
    means.sort_by(|a, b| a.total_cmp(b));
    (
        percentile_sorted(&means, 2.5),
        percentile_sorted(&means, 97.5),
    )
}

/// Wilson score interval for a binomial proportion.
///
/// Chosen over the normal approximation because retention counts are small
/// (a handful of ground-truth items per sample) where Wald intervals are
/// badly mis-calibrated. Returns `(0.0, 1.0)` for zero trials.
pub fn wilson_interval(successes: usize, trials: usize, z: f64) -> (f64, f64) {
    if trials == 0 {
        return (0.0, 1.0);
    }
    let n = trials as f64;
    let p = successes as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let centre = p + z2 / (2.0 * n);
    let margin = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    (
        ((centre - margin) / denom).max(0.0),
        ((centre + margin) / denom).min(1.0),
    )
}

/// Linear-interpolation percentile over an ALREADY SORTED slice; `pct` in
/// [0, 100]. Returns 0.0 for an empty slice.
pub fn percentile_sorted(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let rank = pct / 100.0 * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// p50/p95/p99 of a latency series, in the same unit as the input.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct LatencyPercentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Computes p50/p95/p99 from an unsorted series.
pub fn latency_percentiles(values: &[f64]) -> LatencyPercentiles {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    LatencyPercentiles {
        p50: percentile_sorted(&sorted, 50.0),
        p95: percentile_sorted(&sorted, 95.0),
        p99: percentile_sorted(&sorted, 99.0),
    }
}
