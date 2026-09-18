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

//! Statistics for the L3 report.
//!
//! Where the uncertainty comes from matters here, and it is not where L2's came
//! from. L3's payloads are committed static assets and both compressors are
//! deterministic, so re-running a scenario reproduces its rate exactly: a
//! per-scenario interval would be a point, and repeating runs to "tighten" it
//! would be pseudo-replication of the worst kind.
//!
//! So the two quantities are aggregated differently:
//!
//! - **Compression rate**: the population is the *scenarios* in a content-type
//!   group. `N` is how many scenarios, and the interval says how much the rate
//!   varies across payload shapes and sizes of that type — the only uncertainty
//!   actually present.
//! - **Latency**: genuinely varies between repetitions of identical work, so it
//!   is summarised over repetitions as percentiles.
//!
//! Retention is a count of successes out of trials, so it gets a Wilson score
//! interval rather than a bootstrap one.

use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Serialize;

/// Bootstrap resamples. Large enough that the interval is stable to the reported
/// precision, small enough to stay instant for the group sizes here.
pub const BOOTSTRAP_SAMPLES: usize = 10_000;

/// Fixed seed so a report is reproducible from the same inputs.
pub const BOOTSTRAP_SEED: u64 = 42;

/// Mean of a sample, or `None` when empty.
pub fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

/// A summary of one group of scenarios.
#[derive(Debug, Clone, Serialize)]
pub struct GroupStats {
    /// Independent observations, i.e. how many scenarios contributed.
    pub n: usize,
    /// Mean across those scenarios.
    pub mean: f64,
    /// Bootstrap 95% interval of the mean.
    ///
    /// With a handful of scenarios this is wide, and that width is the honest
    /// message: a content type sampled three ways cannot support a tight claim.
    pub ci: (f64, f64),
    /// Smallest and largest scenario value, so a group whose mean hides a
    /// bimodal split is visible rather than smoothed over.
    pub range: (f64, f64),
}

/// Summarise a group of per-scenario values.
///
/// Returns `None` for an empty group rather than a zero, so a content type with
/// nothing measurable stays out of the averages instead of dragging them down.
pub fn summarize(values: &[f64]) -> Option<GroupStats> {
    let m = mean(values)?;
    let ci = bootstrap_ci(values)?;
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(GroupStats {
        n: values.len(),
        mean: m,
        ci,
        range: (min, max),
    })
}

/// Percentile bootstrap 95% interval of the mean.
///
/// # Panics
///
/// Does not panic: an empty sample returns `None`, and the percentile indices
/// are clamped to the resample vector.
pub fn bootstrap_ci(values: &[f64]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    if values.len() == 1 {
        // One observation carries no information about spread. Reporting the
        // value as both bounds is honest; inventing a width would not be.
        return Some((values[0], values[0]));
    }

    let mut rng = StdRng::seed_from_u64(BOOTSTRAP_SEED);
    let mut means = Vec::with_capacity(BOOTSTRAP_SAMPLES);
    for _ in 0..BOOTSTRAP_SAMPLES {
        let mut sum = 0.0;
        for _ in 0..values.len() {
            let idx = next_index(&mut rng, values.len());
            sum += values[idx];
        }
        means.push(sum / values.len() as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let lo = percentile_of_sorted(&means, 2.5);
    let hi = percentile_of_sorted(&means, 97.5);
    Some((lo, hi))
}

/// Uniform index in `0..len`.
fn next_index(rng: &mut StdRng, len: usize) -> usize {
    use rand::Rng;
    rng.gen_range(0..len)
}

/// Percentile of an already-sorted sample, by nearest-rank.
fn percentile_of_sorted(sorted: &[f64], pct: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = (pct / 100.0) * (sorted.len() - 1) as f64;
    let idx = rank.round().clamp(0.0, (sorted.len() - 1) as f64) as usize;
    sorted[idx]
}

/// Latency percentiles over repetitions.
#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct Latency {
    /// Median.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile, where a tail problem shows up first.
    pub p99: f64,
}

/// Summarise repetition timings.
pub fn latency(samples: &mut [f64]) -> Option<Latency> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(Latency {
        p50: percentile_of_sorted(samples, 50.0),
        p95: percentile_of_sorted(samples, 95.0),
        p99: percentile_of_sorted(samples, 99.0),
    })
}

/// Wilson score 95% interval for a success count.
///
/// Preferred over the normal approximation because these counts are small and
/// often at the boundary: a 5/5 result must not report an interval of zero
/// width, which the naive formula does.
pub fn wilson_interval(passed: usize, total: usize) -> Option<(f64, f64)> {
    if total == 0 {
        return None;
    }
    const Z: f64 = 1.96;
    let n = total as f64;
    let p = passed as f64 / n;
    let denom = 1.0 + Z * Z / n;
    let centre = p + Z * Z / (2.0 * n);
    let spread = Z * ((p * (1.0 - p) / n) + (Z * Z / (4.0 * n * n))).sqrt();
    Some((
        ((centre - spread) / denom).max(0.0),
        ((centre + spread) / denom).min(1.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_group_has_no_summary() {
        assert!(summarize(&[]).is_none());
        assert!(mean(&[]).is_none());
    }

    #[test]
    fn single_scenario_reports_a_point_not_an_invented_width() {
        let s = summarize(&[0.42]).expect("one value summarises");
        assert_eq!(s.n, 1);
        assert_eq!(s.ci, (0.42, 0.42));
        assert_eq!(s.range, (0.42, 0.42));
    }

    #[test]
    fn interval_brackets_the_mean() {
        let values = [0.1, 0.4, 0.55, 0.62, 0.9];
        let s = summarize(&values).expect("summarises");
        assert!(s.ci.0 <= s.mean && s.mean <= s.ci.1, "{s:?}");
        assert_eq!(s.range, (0.1, 0.9));
    }

    #[test]
    fn bootstrap_is_reproducible() {
        let values = [0.2, 0.5, 0.9, 0.31];
        assert_eq!(bootstrap_ci(&values), bootstrap_ci(&values));
    }

    #[test]
    fn range_exposes_a_bimodal_group() {
        // A group whose mean sits between two clusters must not read as a
        // moderate result: the range is what reveals the split.
        let s = summarize(&[0.0, 0.0, 0.95, 0.97]).expect("summarises");
        assert!(s.mean > 0.4 && s.mean < 0.6);
        assert_eq!(s.range, (0.0, 0.97));
    }

    #[test]
    fn latency_percentiles_are_ordered() {
        let mut samples: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let l = latency(&mut samples).expect("has samples");
        assert!(l.p50 <= l.p95 && l.p95 <= l.p99);
    }

    #[test]
    fn wilson_gives_a_perfect_score_real_width() {
        let (lo, hi) = wilson_interval(5, 5).expect("has trials");
        assert!(lo < 1.0, "5/5 must not claim certainty, got lo={lo}");
        assert_eq!(hi, 1.0);
    }

    #[test]
    fn wilson_has_no_interval_without_trials() {
        assert!(wilson_interval(0, 0).is_none());
    }
}
