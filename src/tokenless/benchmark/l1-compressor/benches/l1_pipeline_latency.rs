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

//! Pipeline latency: in-process library micro-benchmarks.
//!
//! Measures the JsonCompressor + TOON encode pipeline as called from Rust.
//! Does NOT include subprocess overhead, network I/O, disk I/O, TOON decode,
//! RTK processing, or LLM inference time. For production end-to-end latency,
//! use adapter-level instrumentation (not this micro-benchmark).
//!
//! The response compressor (L1) and the TOON encoder (L2) are the two library
//! stages an agent chains per tool response. Every point here runs **in-process**
//! at the same microsecond magnitude and passes objects directly (no
//! serialize-to-string glue), so the numbers are mutually comparable and add up:
//!
//!   response_only                = compress(v)                 (L1)
//!   toon_encode_on_compressed     = encode(compress(v))         (L2, on L1's output)
//!   response_then_toon            = compress(v) then encode(.)  (L1 + L2)
//!
//! `toon_encode_on_compressed` encodes the exact value `response_only` emits, so
//! `response_then_toon` should ≈ `response_only + toon_encode_on_compressed`. This is an
//! engineering sanity check: the combined P50 and the sum of individual P50s
//! should be in the same ballpark (same order of magnitude). Two payload
//! sizes are measured: small (20 items) and large (200 items, past the 32-item
//! truncation knee).
//!
//! RTK is deliberately NOT part of this pipeline: it is an out-of-process
//! command rewrite (subprocess, millisecond scale) whose format contract lives
//! in `tests/l1_rtk_format_compat.rs`. Folding its subprocess cost into these
//! in-process microsecond figures would make the table non-comparable — the
//! exact mistake this layout avoids. Multi-round accumulations are also omitted:
//! an N-round loop is just N × the single-shot number (criterion already samples
//! heavily), so it adds no new verification dimension.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tokenless_bench::{compress_json, response_canonical, response_items, schema_canonical};
use tokenless_schema::SchemaCompressor;

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_toon_inprocess");
    group.sample_size(100);

    for (label, n) in [("small", 20usize), ("large", 200usize)] {
        let input = response_items(n);
        // Pre-compute L1's output once, outside the timed closures, so
        // `toon_only` encodes exactly what the combined pipeline feeds into L2.
        // This ensures the three measurement points share the same L1 output.
        let compressed = compress_json(&input);

        // L1 — response compression only.
        group.bench_function(format!("{label}/response_only"), |b| {
            b.iter(|| compress_json(black_box(&input)));
        });

        // "toon_encode_on_compressed": TOON-encodes the output of JsonCompressor
        // (L1 compressed value). Differs from metrics.rs "toon_only" which TOON-encodes raw input.
        // L2 — TOON encode only, on L1's output (object passing, no to_string).
        group.bench_function(format!("{label}/toon_encode_on_compressed"), |b| {
            b.iter(|| toon_format::encode_default(black_box(&compressed)).unwrap());
        });

        // L1 + L2 — full in-process pipeline, object passing.
        group.bench_function(format!("{label}/response_then_toon"), |b| {
            b.iter(|| {
                let c = compress_json(black_box(&input));
                toon_format::encode_default(&c).unwrap()
            });
        });
    }

    // Forced ablation: applies Response + Schema compression then TOON encode
    // on both outputs. This is NOT the production CLI path — the CLI applies a
    // token-gate and falls back to compressed-only when TOON inflates.
    let resp = response_canonical();
    let schema = schema_canonical();
    let schema_compressor = SchemaCompressor::new();

    group.bench_function("canonical/forced_all_stages", |b| {
        b.iter(|| {
            let rc = compress_json(black_box(&resp));
            let sc = schema_compressor.compress(black_box(&schema));
            let rt = toon_format::encode_default(&rc).unwrap();
            let st = toon_format::encode_default(&sc).unwrap();
            (black_box(rt), black_box(st))
        });
    });

    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
