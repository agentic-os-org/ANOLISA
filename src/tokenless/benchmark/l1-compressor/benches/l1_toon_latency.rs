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

//! TOON encode/decode latency benchmarks.
//!
//! Mirrors the 8 measurement points from the V5 report's toon_latency. Unlike
//! the 0.2.0 report — which shelled out to the `toon` CLI (adding subprocess
//! overhead) — these call `toon_format` in-process, so the numbers reflect the
//! library cost alone. No TOON CLI latency bench exists in this suite. The
//! pipeline benches (`l1_pipeline_latency.rs`) are also purely in-process and do
//! NOT measure CLI subprocess cost.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tokenless_bench::{response_canonical, schema_canonical, toon_flat, toon_nested, toon_table};

fn bench_toon(c: &mut Criterion) {
    let mut group = c.benchmark_group("toon_latency");
    group.sample_size(100);

    let flat = toon_flat();
    let nested = toon_nested();
    let table = toon_table(100);

    // encode: flat / nested / table
    group.bench_function("encode/flat", |b| {
        b.iter(|| toon_format::encode_default(black_box(&flat)).unwrap());
    });
    group.bench_function("encode/nested", |b| {
        b.iter(|| toon_format::encode_default(black_box(&nested)).unwrap());
    });
    group.bench_function("encode/table_100", |b| {
        b.iter(|| toon_format::encode_default(black_box(&table)).unwrap());
    });

    // decode: pre-encode once, then bench the decode leg alone.
    let flat_toon = toon_format::encode_default(&flat).unwrap();
    let nested_toon = toon_format::encode_default(&nested).unwrap();
    let table_toon = toon_format::encode_default(&table).unwrap();
    group.bench_function("decode/flat", |b| {
        b.iter(|| toon_format::decode_default::<serde_json::Value>(black_box(&flat_toon)).unwrap());
    });
    group.bench_function("decode/nested", |b| {
        b.iter(|| {
            toon_format::decode_default::<serde_json::Value>(black_box(&nested_toon)).unwrap()
        });
    });
    group.bench_function("decode/table_100", |b| {
        b.iter(|| {
            toon_format::decode_default::<serde_json::Value>(black_box(&table_toon)).unwrap()
        });
    });

    // roundtrip: encode + decode
    group.bench_function("roundtrip/flat", |b| {
        b.iter(|| {
            let t = toon_format::encode_default(black_box(&flat)).unwrap();
            toon_format::decode_default::<serde_json::Value>(&t).unwrap()
        });
    });
    group.bench_function("roundtrip/nested", |b| {
        b.iter(|| {
            let t = toon_format::encode_default(black_box(&nested)).unwrap();
            toon_format::decode_default::<serde_json::Value>(&t).unwrap()
        });
    });
    group.bench_function("roundtrip/table_100", |b| {
        b.iter(|| {
            let t = toon_format::encode_default(black_box(&table)).unwrap();
            toon_format::decode_default::<serde_json::Value>(&t).unwrap()
        });
    });

    // Canonical fixtures — same inputs used for compression-rate measurement.
    let canonical_resp = response_canonical();
    let canonical_schema = schema_canonical();
    group.bench_function("encode/canonical_response", |b| {
        b.iter(|| toon_format::encode_default(black_box(&canonical_resp)).unwrap());
    });
    group.bench_function("encode/canonical_schema", |b| {
        b.iter(|| toon_format::encode_default(black_box(&canonical_schema)).unwrap());
    });

    // decode: canonical fixtures
    let canonical_resp_toon = toon_format::encode_default(&canonical_resp).unwrap();
    let canonical_schema_toon = toon_format::encode_default(&canonical_schema).unwrap();

    group.bench_function("decode/canonical_response", |b| {
        let opts = toon_format::DecodeOptions::default().with_strict(false);
        b.iter(|| {
            toon_format::decode::<serde_json::Value>(black_box(&canonical_resp_toon), &opts)
                .unwrap()
        });
    });
    group.bench_function("decode/canonical_schema", |b| {
        b.iter(|| {
            toon_format::decode_default::<serde_json::Value>(black_box(&canonical_schema_toon))
                .unwrap()
        });
    });

    // roundtrip: canonical fixtures
    group.bench_function("roundtrip/canonical_response", |b| {
        b.iter(|| {
            let t = toon_format::encode_default(black_box(&canonical_resp)).unwrap();
            let opts = toon_format::DecodeOptions::default().with_strict(false);
            toon_format::decode::<serde_json::Value>(&t, &opts).unwrap()
        });
    });
    group.bench_function("roundtrip/canonical_schema", |b| {
        b.iter(|| {
            let t = toon_format::encode_default(black_box(&canonical_schema)).unwrap();
            toon_format::decode_default::<serde_json::Value>(&t).unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_toon);
criterion_main!(benches);
