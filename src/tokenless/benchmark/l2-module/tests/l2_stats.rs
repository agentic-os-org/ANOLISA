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

//! Unit checks for the L2 statistics helpers: Wilson interval against a
//! hand-computed reference, seeded-bootstrap reproducibility, and the CV→N
//! inference boundaries.

use tokenless_l2_bench::l2::probe::ProbeScore;
use tokenless_l2_bench::l2::stats::{
    bootstrap_ci_mean, infer_n, latency_percentiles, mean, percentile_sorted, std_dev,
    wilson_interval,
};

const TOL: f64 = 1e-3;

#[test]
fn semantic_score_conditions_on_the_original_answers() {
    // The baseline answered 2 questions and compression kept both.
    let kept = ProbeScore {
        correct_uncompressed: 2,
        correct_compressed: 2,
        retained: 2,
        total: 3,
    };
    assert_eq!(kept.semantic_score(), Some(1.0));

    // The regression this guards: compression destroys the one question the
    // original could answer but happens to answer a different one. Two
    // independent totals would read 1/1 = 1.0 and hide the loss outright.
    let swapped = ProbeScore {
        correct_uncompressed: 1,
        correct_compressed: 1,
        retained: 0,
        total: 2,
    };
    assert_eq!(swapped.semantic_score(), Some(0.0));

    // Half the baseline-answerable facts survive.
    let partial = ProbeScore {
        correct_uncompressed: 4,
        correct_compressed: 3,
        retained: 2,
        total: 5,
    };
    assert_eq!(partial.semantic_score(), Some(0.5));

    // Nothing to normalise against.
    let blind = ProbeScore {
        correct_uncompressed: 0,
        correct_compressed: 2,
        retained: 0,
        total: 2,
    };
    assert_eq!(blind.semantic_score(), None);
}

#[test]
fn wilson_matches_hand_computed_value() {
    // 8/10 successes, z = 1.96: standard worked example of the Wilson score
    // interval — (0.4902, 0.9433) to 4 decimal places.
    let (lo, hi) = wilson_interval(8, 10, 1.96);
    assert!((lo - 0.4902).abs() < TOL, "lo = {lo}");
    assert!((hi - 0.9433).abs() < TOL, "hi = {hi}");
}

#[test]
fn wilson_degenerate_cases() {
    assert_eq!(wilson_interval(0, 0, 1.96), (0.0, 1.0));
    let (lo, hi) = wilson_interval(0, 10, 1.96);
    assert!(
        lo >= 0.0 && hi < 0.5,
        "all-failure interval hugs zero: [{lo}, {hi}]"
    );
    let (lo, hi) = wilson_interval(10, 10, 1.96);
    assert!(
        lo > 0.5 && hi <= 1.0,
        "all-success interval hugs one: [{lo}, {hi}]"
    );
}

#[test]
fn bootstrap_is_reproducible_and_ordered() {
    let data = [0.10, 0.12, 0.11, 0.13, 0.09, 0.14, 0.10, 0.12];
    let first = bootstrap_ci_mean(&data);
    let second = bootstrap_ci_mean(&data);
    // Fixed seed: identical bounds on every invocation.
    assert_eq!(first, second);
    let (lo, hi) = first;
    let m = mean(&data);
    assert!(lo <= m && m <= hi, "mean {m} outside CI [{lo}, {hi}]");
    // Resampled means cannot leave the data range.
    assert!(lo >= 0.09 && hi <= 0.14);
}

#[test]
fn bootstrap_short_series_collapses_to_mean() {
    assert_eq!(bootstrap_ci_mean(&[0.5]), (0.5, 0.5));
    assert_eq!(bootstrap_ci_mean(&[]), (0.0, 0.0));
}

#[test]
fn infer_n_boundaries() {
    // Zero variance: pilot size is enough.
    assert_eq!(infer_n(&[10.0, 10.0, 10.0, 10.0, 10.0]), 5);
    // CV = 1.5811/10 = 0.15811 → ceil((1.96·0.15811/0.05)²) = 39.
    assert_eq!(infer_n(&[8.0, 9.0, 10.0, 11.0, 12.0]), 39);
    // Huge CV clamps at the upper bound.
    assert_eq!(infer_n(&[1.0, 100.0, 1.0, 100.0, 1.0]), 50);
    // Degenerate pilots fall back to the pilot size.
    assert_eq!(infer_n(&[]), 5);
    assert_eq!(infer_n(&[0.0, 0.0, 0.0]), 5);
    // An extreme CV must clamp in the f64 domain rather than relying on a
    // saturating f64->usize cast to land back in range.
    assert_eq!(infer_n(&[1e-300, 1e300, 1e-300]), 50);
    // A NaN-bearing pilot yields a non-finite target: fall back to the pilot
    // size instead of narrowing NaN to 0.
    assert_eq!(infer_n(&[1.0, f64::NAN, 3.0]), 5);
    assert_eq!(infer_n(&[f64::INFINITY, 1.0, 1.0]), 5);
}

#[test]
fn std_dev_uses_sample_variance() {
    // Variance of {8..12} with ddof=1 is 2.5 → std = 1.5811.
    assert!((std_dev(&[8.0, 9.0, 10.0, 11.0, 12.0]) - 2.5f64.sqrt()).abs() < TOL);
    assert_eq!(std_dev(&[42.0]), 0.0);
}

#[test]
fn percentiles_interpolate_linearly() {
    let sorted = [1.0, 2.0, 3.0, 4.0, 5.0];
    assert_eq!(percentile_sorted(&sorted, 0.0), 1.0);
    assert_eq!(percentile_sorted(&sorted, 50.0), 3.0);
    assert_eq!(percentile_sorted(&sorted, 100.0), 5.0);
    assert!((percentile_sorted(&sorted, 25.0) - 2.0).abs() < TOL);

    let p = latency_percentiles(&[5.0, 1.0, 3.0, 2.0, 4.0]);
    assert_eq!(p.p50, 3.0);
    assert!(p.p95 <= 5.0 && p.p95 >= 4.0);
    assert!(p.p99 <= 5.0 && p.p99 >= p.p95);
}
