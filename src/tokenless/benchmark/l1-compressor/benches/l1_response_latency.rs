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

//! Response compression latency benchmarks.
//!
//! Mirrors the 11 measurement points from the V5 report's response_latency:
//! small_1kb, medium_10kb, large_100kb, huge_1mb, deep_nesting_8,
//! high_repetition_100, items/{10,31,32,33,50,100,500,1000}. Plus a `canonical`
//! point that compresses the shared `fixtures/tool_response.json` payload — the
//! SAME bytes the Python compression-rate scripts measure — so latency and
//! compression rate are attributable to one input.
//!
//! NOTE: the default `truncate_arrays_at` is 32 in tokenless 0.7.4 (was 16 in
//! the 0.2.0 report), so the items/* curve flattens after 32 items rather than
//! 16 — the shape matches the report, the knee just moves.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tokenless_bench::{
    compress_json, response_canonical, response_deep_nesting, response_high_repetition,
    response_huge, response_items, response_small,
};

fn bench_response(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_latency");
    group.sample_size(100);

    let small = response_small();
    // Sanity check: response_small should be approximately 1KB (±20%).
    let small_bytes = serde_json::to_string(&small).unwrap().len();
    assert!(
        small_bytes > 800 && small_bytes < 1200,
        "small_1kb: actual={small_bytes}B"
    );
    group.bench_function("small_1kb", |b| {
        b.iter(|| compress_json(black_box(&small)));
    });

    // medium_10kb uses response_items(31) ≈ 10KB. This is the same input as the
    // items/31 benchmark below. The two serve different analytical purposes:
    //   - medium_10kb: validates that ~10KB payloads meet their size target
    //   - items/31: part of the truncation boundary curve (31→32→33)
    // Same data, different context in the report.
    let medium = response_items(31);
    let medium_bytes = serde_json::to_string(&medium).unwrap().len();
    assert!(
        medium_bytes > 9_000 && medium_bytes < 11_000,
        "medium_10kb: actual={medium_bytes}B"
    );
    group.bench_function("medium_10kb", |b| {
        b.iter(|| compress_json(black_box(&medium)));
    });

    // ~100KB: 307 records.
    let large = response_items(307);
    let large_bytes = serde_json::to_string(&large).unwrap().len();
    assert!(
        large_bytes > 90_000 && large_bytes < 110_000,
        "large_100kb: actual={large_bytes}B"
    );
    group.bench_function("large_100kb", |b| {
        b.iter(|| compress_json(black_box(&large)));
    });

    // ~1MB: records cycled to fill approximately one megabyte.
    let huge = response_huge();
    let huge_bytes = serde_json::to_string(&huge).unwrap().len();
    assert!(
        huge_bytes > 900_000 && huge_bytes < 1_200_000,
        "huge_1mb: actual={huge_bytes}B"
    );
    group.bench_function("huge_1mb", |b| {
        b.iter(|| compress_json(black_box(&huge)));
    });

    let deep = response_deep_nesting(8);
    group.bench_function("deep_nesting_8", |b| {
        b.iter(|| compress_json(black_box(&deep)));
    });

    let rep = response_high_repetition(100);
    group.bench_function("high_repetition_100", |b| {
        b.iter(|| compress_json(black_box(&rep)));
    });

    // Items curve including 31/32/33 around the truncation threshold.
    for n in [10usize, 31, 32, 33, 50, 100, 500, 1000] {
        let items = response_items(n);
        group.bench_function(format!("items/{n}"), |b| {
            b.iter(|| compress_json(black_box(&items)));
        });
    }

    // Shared canonical payload — identical bytes to what Python compresses.
    let canonical = response_canonical();
    group.bench_function("canonical", |b| {
        b.iter(|| compress_json(black_box(&canonical)));
    });

    group.finish();
}

criterion_group!(benches, bench_response);
criterion_main!(benches);
